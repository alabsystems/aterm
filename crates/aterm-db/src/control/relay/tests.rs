// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{control::Failure, table::tests::Lab};
use aterm_secrets::commons::{
    binding::Binding,
    hearing::{LIFTED, RELAYS, Version},
    roster::Role,
};
use aterm_types::network_state::RepairCause;
use std::{
    io::{self, Cursor},
    sync::Arc,
    time::Duration,
};

/// A session's channel after its handoff, in memory: the bytes the peer sent, and what this
/// side wrote.
struct Channel {
    peer: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.peer.read(buf)
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A flight's body as its frame goes on the wire.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

/// Mac 1 of a lab of two writers: its endpoint, whose store keeps its own report and trades
/// with a peer that reports what a test binary does, and Mac 2's binding.
struct Trading {
    _lab: Lab,
    endpoint: Endpoint,
    peer: Binding,
}

impl Trading {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer; 2]);
        let bootstrap = lab.bootstrap(0);
        let own = bootstrap.request().unwrap().binding().clone();
        let peer = lab.bootstrap(1).request().unwrap().binding().clone();
        let now_ms = aterm_time::unix_millis().unwrap();
        let mut reports = Reports::new(bootstrap.trust(), &own);
        reports.set_roster([&own, &peer], now_ms);
        let signed = bootstrap
            .device()
            .with_seed(|seed| reports.sign_own(seed, RELAYS, now_ms));
        assert!(signed.unwrap());
        let endpoint = Endpoint::from_bootstrap(&bootstrap)
            .unwrap()
            .unwrap()
            .with_reports(Arc::new(Mutex::new(reports)))
            .relaying_from(LIFTED);
        Self {
            _lab: lab,
            endpoint,
            peer,
        }
    }

    /// One trade in `side`'s seat after `repair`'s round, the peer having sent `sent`: the
    /// bytes this side wrote, and how many of the peer's it read.
    fn trade(&self, repair: &mut Repair, side: Side, sent: &[u8], until: Instant) -> [usize; 2] {
        let mut channel = Channel {
            peer: Cursor::new(sent.to_vec()),
            written: Vec::new(),
        };
        self.endpoint
            .trade_relayed(&mut channel, self.peer.actor(), side, repair, until);
        [
            channel.written.len(),
            usize::try_from(channel.peer.position()).unwrap(),
        ]
    }
}

/// A repair state as a round leaves it once its application exchange, the peer's version
/// report and the credential handoff completed.
fn completed() -> Repair {
    Repair {
        said: Said::Version(Version::own()),
        handoff: Some(Ok(0)),
        ..Repair::default()
    }
}

/// The trade runs only after a round whose credential handoff did not fail and whose peer's
/// report verified at the floor or later, and only before the exchange's deadline. A failed
/// handoff can leave its bytes unread on the channel, so after one nothing is written and
/// nothing is read, in either seat, though the peer sent a whole offer; so too where the peer's
/// report was absent, unread or of a release below the floor, and once the deadline passed.
/// None of those rounds changes what the state keeps of the trade. A round with no handoff at
/// all (an endpoint without credentials custody) trades as one whose handoff succeeded.
///
/// NEGATIVE CONTROL: with `Endpoint::trade_relayed` not reading `repair.handoff`, the dialler
/// writes its offer after a failed handoff: `` assertion `left == right` failed: Initiator a
/// failed handoff `` with `left: [34, 10]`, `right: [0, 0]`.
#[test]
fn nothing_is_traded_after_a_failed_handoff_an_unread_release_or_the_deadline() {
    let lab = Trading::new();
    let until = Instant::now() + Duration::from_secs(60);
    let offered = frame(&hearing::offer_flight(&[]));
    // What either seat moves where the trade runs. The dialler writes its offer of one report
    // and reads the peer's frame, which is no answer; the listener reads the frame as the peer's
    // offer, answers with its own offer of one report and meets the end.
    let trades = [4 + 4 + 2 + 24, offered.len()];
    let failed = || Some(Err(Failure::new(RepairCause::Other, "handoff")));
    let below = Version(LIFTED.0, LIFTED.1 - 1, 0);
    for side in [Side::Initiator, Side::Responder] {
        let moved = lab.trade(&mut completed(), side, &offered, until);
        assert_eq!(moved, trades, "{side:?}");
        let mut unhanded = completed();
        unhanded.handoff = None;
        let moved = lab.trade(&mut unhanded, side, &offered, until);
        assert_eq!(moved, trades, "{side:?} no handoff");

        let held: [(&str, Box<dyn Fn(&mut Repair)>, Instant); 5] = [
            (
                "a failed handoff",
                Box::new(|repair| repair.handoff = failed()),
                until,
            ),
            (
                "no report",
                Box::new(|repair| repair.said = Said::Nothing),
                until,
            ),
            (
                "an unread report",
                Box::new(|repair| repair.said = Said::Unknown),
                until,
            ),
            (
                "a release below the floor",
                Box::new(|repair| repair.said = Said::Version(below)),
                until,
            ),
            ("the deadline", Box::new(|_| ()), Instant::now()),
        ];
        for (case, change, until) in held {
            let mut repair = completed();
            change(&mut repair);
            let moved = lab.trade(&mut repair, side, &offered, until);
            assert_eq!(moved, [0, 0], "{side:?} {case}");
            assert_eq!(repair.relay, Relay::default(), "{side:?} {case}");
        }
    }
}

/// What a repair state keeps of the trade is its peer's alone. The mark an answered offer
/// left and the note that the last offer went unanswered stay with the state while it is taken
/// up for the same peer, and both go when it is taken up for another, so that peer is offered
/// every kept report and is not held to the last peer's silence.
///
/// NEGATIVE CONTROL: with `Repair::prepare` leaving `relay` as it was for a new peer, the next
/// peer inherits the mark and the note: `` left: Relay { mark: Mark { epoch: 1, stamp: 1 },
/// unanswered: true } right: Relay { mark: Mark { epoch: 0, stamp: 0 }, unanswered: false } ``.
#[test]
fn a_repair_state_taken_up_for_another_peer_keeps_nothing_of_the_last_peers_trade() {
    let lab = Trading::new();
    let until = Instant::now() + Duration::from_secs(60);
    let trust = lab.endpoint.trust;
    let peer = lab.peer.actor();
    let mut repair = completed();
    repair.prepare(trust, peer);
    assert_eq!(repair.relay, Relay::default());

    // The peer answers the offer of one report, wanting nothing and offering nothing: the
    // mark moves past the report.
    let answer = frame(&hearing::answer_flight(&[false], &[]));
    let moved = lab.trade(&mut repair, Side::Initiator, &answer, until);
    assert_eq!(moved, [4 + 4 + 2 + 24, answer.len()]);
    let mark = repair.relay.mark;
    assert_ne!(mark, Mark::default());
    assert!(!repair.relay.unanswered);
    // Then it leaves an offer, of nothing new, unanswered.
    let moved = lab.trade(&mut repair, Side::Initiator, &[], until);
    assert_eq!(moved, [4 + 4 + 2, 0]);
    let held = Relay {
        mark,
        unanswered: true,
    };
    assert_eq!(repair.relay, held);

    repair.prepare(trust, peer);
    assert_eq!(repair.relay, held);
    let mut other = peer;
    other[47] ^= 1;
    repair.prepare(trust, other);
    assert_eq!(repair.relay, Relay::default());
}

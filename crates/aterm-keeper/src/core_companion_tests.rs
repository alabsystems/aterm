// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companion rules of [`KeeperCore`] (the crash-restore plan §5), one test per rule.

use super::*;
use std::collections::BTreeSet;

/// A world the tests describe: who lives, who holds what.
#[derive(Default)]
struct World {
    held: BTreeMap<Rdev, Vec<u32>>,
    live_pids: Vec<u32>,
}

impl KeeperEnv for World {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        match self.held.get(&rdev) {
            Some(p) if !p.is_empty() => Holders::Held(p.clone()),
            _ => Holders::None,
        }
    }
    fn shell_alive(&mut self, _pid: u32, _birth: Birth) -> bool {
        true
    }
    fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
        self.live_pids.contains(&pid)
    }
}

const R: Rdev = 0x0f00_0042;
const LEN: u64 = 8 << 20;

fn hdr() -> MasterHeader {
    MasterHeader {
        rdev: R,
        shell_pid: 900,
        shell_birth: Birth::default(),
        local_id: 1,
    }
}

/// A window process `pid` on `conn`, its HELLO stating `caps` beside BYE.
fn connect(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, caps: u8) -> Vec<Out> {
    assert!(k.accept(conn, pid, None));
    if !w.live_pids.contains(&pid) {
        w.live_pids.push(pid);
    }
    k.hello(conn, PeerClass::App, CAP_SENDS_BYE | caps, 0, w)
}

/// `pid` is SIGKILLed: its connection's EOF, then its status.
fn kill(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32) -> Vec<Out> {
    w.live_pids.retain(|p| *p != pid);
    let mut out = k.eof(conn, 1, w);
    out.extend(k.peer_exit(pid, 9, 1, w));
    out
}

fn closed(outs: &[Out]) -> Vec<CompanionId> {
    outs.iter()
        .filter_map(|o| match o {
            Out::CloseCompanion(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// The journal fields of the OFFER of `R` in `outs`.
fn offer_of(outs: &[Out]) -> Option<Option<OfferJournal>> {
    outs.iter().find_map(|o| match o {
        Out::Offer { rdev, journal, .. } if *rdev == R => Some(*journal),
        _ => None,
    })
}

fn companion(id: CompanionId) -> Companion {
    Companion {
        id,
        kind: COMPANION_JOURNAL,
        len: LEN,
    }
}

fn view(k: &KeeperCore) -> CompanionView {
    k.companion_view(R).expect("the record")
}

/// A journaling window that registered `R` on `conn` and sent its region `id`.
fn journaled(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, id: CompanionId) {
    connect(k, w, conn, pid, CAP_JOURNAL);
    k.register(conn, hdr(), vec![], R);
    assert_eq!(
        k.companion(conn, R, COMPANION_JOURNAL, LEN, id),
        vec![Out::KeepCompanion(id)]
    );
}

/// The happy path: a region is kept, rides the next window's offer with the brake at 0, stays
/// while that window's claim is unsettled, and closes when its own region arrives.
#[test]
fn a_region_rides_its_offer_and_the_next_window_s_own_supersedes_it() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    assert_eq!(
        view(&k),
        CompanionView {
            offered: Some(1),
            reader_settled: Some(true),
            attempts: 0,
            outgoing: None
        }
    );
    let outs = kill(&mut k, &mut w, 1, 500);
    assert!(closed(&outs).is_empty(), "the dead reader's region is kept");
    assert_eq!(k.records()[&R].state, RecordState::Orphaned);
    assert_eq!(view(&k).offered, Some(1));

    let outs = connect(&mut k, &mut w, 2, 600, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 0,
            companion: Some(companion(1))
        }))
    );
    k.register(2, hdr(), vec![], R);
    assert_eq!(
        view(&k),
        CompanionView {
            offered: Some(1),
            reader_settled: Some(false),
            attempts: 0,
            outgoing: None
        },
        "a claim not yet settled leaves the dead window's region on offer"
    );
    assert_eq!(
        k.companion(2, R, COMPANION_JOURNAL, LEN, 2),
        vec![Out::CloseCompanion(1), Out::KeepCompanion(2)]
    );
    assert_eq!(view(&k).offered, Some(2));
    assert_eq!(
        k.companion(2, R, COMPANION_JOURNAL, LEN, 3),
        vec![Out::CloseCompanion(2), Out::KeepCompanion(3)]
    );
}

/// §3.2 N4 and the cap: a restorer that dies after its claim, before its COMPANION, costs one
/// attempt (the next offer says 1); one that dies before its claim costs another; at two the
/// region closes and the offer carries none.
#[test]
fn restorers_that_die_before_their_companion_spend_the_brake_then_the_region() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    kill(&mut k, &mut w, 1, 500);

    connect(&mut k, &mut w, 2, 600, CAP_JOURNAL);
    k.register(2, hdr(), vec![], R);
    let outs = kill(&mut k, &mut w, 2, 600);
    assert!(closed(&outs).is_empty());
    assert_eq!(view(&k).attempts, 1);

    let outs = connect(&mut k, &mut w, 3, 700, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 1,
            companion: Some(companion(1))
        })),
        "the next window replays the keyframe alone"
    );
    w.live_pids.retain(|p| *p != 700);
    let outs = k.eof(3, 2, &mut w);
    assert_eq!(closed(&outs), [1], "the second death closes the region");
    k.peer_exit(700, 9, 2, &mut w);
    assert_eq!(view(&k).attempts, 2);
    assert_eq!(view(&k).offered, None);

    let outs = connect(&mut k, &mut w, 4, 800, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 2,
            companion: None
        })),
        "the master is still offered, on the META rung"
    );
    k.register(4, hdr(), vec![], R);
    assert_eq!(
        k.companion(4, R, COMPANION_JOURNAL, LEN, 9),
        vec![Out::KeepCompanion(9)]
    );
    assert_eq!(view(&k).attempts, 0);
}

/// §3.2 J2 and N5: a window that does not journal is offered the master alone (its strict loop
/// meets no COMPANION_OFFER), its offer's return costs no attempt, and its claim closes the
/// region, which would otherwise show a screen from before its whole life after its death.
#[test]
fn a_window_that_does_not_journal_gets_no_region_and_its_claim_closes_it() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    kill(&mut k, &mut w, 1, 500);

    let outs = connect(&mut k, &mut w, 2, 600, 0);
    assert_eq!(offer_of(&outs), Some(None), "an OFFER as it always was");
    kill(&mut k, &mut w, 2, 600);
    assert_eq!(view(&k).attempts, 0, "it never had the region to die on");

    connect(&mut k, &mut w, 3, 700, 0);
    let outs = k.register(3, hdr(), vec![], R);
    assert_eq!(outs, vec![Out::CloseCompanion(1), Out::DropDuplicate(R)]);
    assert_eq!(view(&k).offered, None);
    kill(&mut k, &mut w, 3, 700);
    let outs = connect(&mut k, &mut w, 4, 800, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs).flatten().and_then(|j| j.companion),
        None,
        "nothing from before the old window's life"
    );
}

/// COMPANION_RELEASE settles a claim with no region: the claim's and the retained region close,
/// the brake resets, and its death offers no region.
#[test]
fn a_released_companion_settles_the_claim_with_no_region() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    kill(&mut k, &mut w, 1, 500);
    connect(&mut k, &mut w, 2, 600, CAP_JOURNAL);
    k.register(2, hdr(), vec![], R);
    assert_eq!(
        k.companion_release(2, R, COMPANION_JOURNAL),
        vec![Out::CloseCompanion(1)]
    );
    assert_eq!(
        view(&k),
        CompanionView {
            offered: None,
            reader_settled: Some(true),
            attempts: 0,
            outgoing: None
        }
    );
    kill(&mut k, &mut w, 2, 600);
    let outs = connect(&mut k, &mut w, 3, 700, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 0,
            companion: None
        }))
    );
}

/// Only a window that stated CAP_JOURNAL, claiming the master, may send a companion of a kind
/// this build knows; every refusal keeps nothing.
#[test]
fn companions_are_refused_by_name() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    connect(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(), vec![], R);
    connect(&mut k, &mut w, 2, 600, CAP_JOURNAL);
    let refused = |code, rdev, conn| vec![Out::Refused { conn, code, rdev }];
    assert_eq!(
        k.companion(1, R, COMPANION_JOURNAL, LEN, 1),
        refused(RefuseCode::WrongClass, R, 1),
        "a window that does not journal"
    );
    assert_eq!(
        k.companion(2, R, COMPANION_JOURNAL, LEN, 2),
        refused(RefuseCode::NotClaimant, R, 2)
    );
    assert_eq!(
        k.companion(2, R + 1, COMPANION_JOURNAL, LEN, 3),
        refused(RefuseCode::NoSuchRecord, R + 1, 2)
    );
    k.register(2, hdr(), vec![], R);
    assert_eq!(
        k.companion(2, R, 9, LEN, 4),
        refused(RefuseCode::NotAJournal, R, 2)
    );
    assert_eq!(
        k.companion_release(2, R + 1, COMPANION_JOURNAL),
        refused(RefuseCode::NoSuchRecord, R + 1, 2)
    );
    assert!(k.records()[&R].journal.own.is_empty());
    assert!(k.accept(3, 700, None));
    k.hello(3, PeerClass::Status, CAP_JOURNAL, 0, &mut w);
    assert_eq!(
        k.companion_release(3, R, COMPANION_JOURNAL),
        refused(RefuseCode::WrongClass, R, 3)
    );
}

/// A link that reconnects and registers again keeps its process's region and its settling: a
/// reconnect is no new reader, and the window's later death offers its region at attempts 0.
#[test]
fn a_reconnected_link_keeps_its_region_and_its_settling() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    connect(&mut k, &mut w, 2, 500, CAP_JOURNAL | CAP_LINK);
    assert_eq!(k.register(2, hdr(), vec![], R), vec![Out::DropDuplicate(R)]);
    assert_eq!(k.records()[&R].claimants, vec![2]);
    assert_eq!(
        view(&k),
        CompanionView {
            offered: Some(1),
            reader_settled: Some(true),
            attempts: 0,
            outgoing: None
        }
    );
    assert_eq!(
        k.companion(2, R, COMPANION_JOURNAL, LEN, 2),
        vec![Out::CloseCompanion(1), Out::KeepCompanion(2)]
    );
    w.live_pids.clear();
    k.eof(1, 1, &mut w);
    k.eof(2, 1, &mut w);
    k.peer_exit(500, 9, 1, &mut w);
    let outs = connect(&mut k, &mut w, 3, 600, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 0,
            companion: Some(companion(2))
        }))
    );
}

/// An update's two claimants. The outgoing window's region is the fallback until its successor
/// settles, then superseded; whichever order the successor's COMPANION and the outgoing window's
/// end come in, the successor's crash offers the successor's region.
#[test]
fn an_update_hands_the_region_on_whichever_comes_first() {
    for successor_first in [false, true] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        journaled(&mut k, &mut w, 1, 500, 1);
        connect(&mut k, &mut w, 2, 600, CAP_JOURNAL | CAP_LINK);
        k.register(2, hdr(), vec![], R);
        assert_eq!(k.records()[&R].claimants, vec![1, 2]);
        assert_eq!(view(&k).reader_settled, Some(false), "the successor reads");
        if successor_first {
            k.companion(2, R, COMPANION_JOURNAL, LEN, 2);
            assert_eq!(k.release(1, R), vec![Out::CloseCompanion(1)]);
        } else {
            w.live_pids.retain(|p| *p != 500);
            let mut outs = k.eof(1, 1, &mut w);
            outs.extend(k.peer_exit(500, 0, 1, &mut w));
            assert!(closed(&outs).is_empty(), "{outs:?}");
            assert_eq!(view(&k).offered, Some(1));
            assert_eq!(
                k.companion(2, R, COMPANION_JOURNAL, LEN, 2),
                vec![Out::CloseCompanion(1), Out::KeepCompanion(2)]
            );
        }
        kill(&mut k, &mut w, 2, 600);
        let outs = connect(&mut k, &mut w, 3, 700, CAP_JOURNAL);
        assert_eq!(
            offer_of(&outs),
            Some(Some(OfferJournal {
                attempts: 0,
                companion: Some(companion(2))
            })),
            "successor_first={successor_first}"
        );
    }
}

/// A hold that ends with no successor's claim (the successor died before its REGISTER) orphans
/// the master with no region: whoever held it may have read what no region saw.
#[test]
fn a_hold_that_ends_without_a_claim_offers_no_region() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    w.live_pids.push(700);
    k.pending(1, 700, 1, None);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 2, &mut w);
    k.peer_exit(500, 0, 2, &mut w);
    assert!(matches!(k.records()[&R].state, RecordState::Held { .. }));
    assert_eq!(view(&k).offered, Some(1), "kept for the successor's claim");
    w.live_pids.retain(|p| *p != 700);
    let outs = k.peer_exit(700, 74 << 8, 4, &mut w);
    assert_eq!(k.records()[&R].state, RecordState::Orphaned);
    assert_eq!(closed(&outs), [1]);
    let outs = connect(&mut k, &mut w, 2, 800, CAP_JOURNAL);
    assert_eq!(offer_of(&outs).flatten().and_then(|j| j.companion), None);
}

/// Teardown: a RELEASE (the tab closed) removes the record, so its master's `Close` closes every
/// companion beside it, and the core holds none any more. (That the server's descriptors follow
/// is `server.rs`'s test, and the kernel lane's.)
#[test]
fn a_record_s_end_takes_its_companions() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    journaled(&mut k, &mut w, 1, 500, 1);
    assert_eq!(k.companions_held(), BTreeSet::from([1]));
    assert_eq!(k.release(1, R), vec![Out::Close(R)]);
    assert_eq!(k.companion_view(R), None);
    assert!(k.companions_held().is_empty());
}

/// The fault point the conformance's negative control plants: armed, no death counts.
#[test]
fn the_uncounted_fault_point_removes_the_brake_and_only_while_armed() {
    let run = |armed: bool| {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        journaled(&mut k, &mut w, 1, 500, 1);
        kill(&mut k, &mut w, 1, 500);
        connect(&mut k, &mut w, 2, 600, CAP_JOURNAL);
        k.register(2, hdr(), vec![], R);
        if armed {
            fault::with_attempts_uncounted(|| kill(&mut k, &mut w, 2, 600));
        } else {
            kill(&mut k, &mut w, 2, 600);
        }
        view(&k).attempts
    };
    assert_eq!(run(false), 1);
    assert_eq!(run(true), 0);
    assert_eq!(run(false), 1, "disarmed after");
}

/// An update: the outgoing window `O` (connection 1, pid 500) journals region 1 and names its
/// successor `S` (pid 600) in a PENDING; `S` says HELLO as a journaling window on connection 2
/// and registers the master at its Commit, so both claim it and `S` reads it from now on. With
/// `settled`, `S`'s own region 2 has arrived.
fn committed_update(k: &mut KeeperCore, w: &mut World, settled: bool) {
    journaled(k, w, 1, 500, 1);
    w.live_pids.push(600);
    k.pending(1, 600, 0, None);
    connect(k, w, 2, 600, CAP_JOURNAL | CAP_LINK);
    k.register(2, hdr(), vec![], R);
    assert_eq!(k.records()[&R].claimants, vec![1, 2]);
    if settled {
        assert_eq!(
            k.companion(2, R, COMPANION_JOURNAL, LEN, 2),
            vec![Out::KeepCompanion(2)]
        );
    }
}

/// What the next journaling window is offered once an update's successor ended unsettled
/// (`O`'s region, the fallback, with the attempt `S`'s end costs: `S` read output after its
/// Commit that `O`'s region never saw) or settled (`S`'s own region, exact).
fn after_the_successor(settled: bool) -> OfferJournal {
    if settled {
        OfferJournal {
            attempts: 0,
            companion: Some(companion(2)),
        }
    } else {
        OfferJournal {
            attempts: 1,
            companion: Some(companion(1)),
        }
    }
}

/// §5 "orphaning" and §3.2 N4 across an update: from its Commit REGISTER the successor is the
/// reader, and the outgoing window never becomes the reader again, whichever of the two deaths
/// the keeper judges first. Settled, the successor's region is offered at attempts 0; unsettled,
/// the outgoing window's region with one attempt counted, never at attempts 0 (which a restorer
/// would replay to a stale screen and call exact). Before the fix, the successor judged first
/// left the outgoing window the reader: its region was offered at attempts 0 both ways.
#[test]
fn the_outgoing_window_never_reads_again_after_its_successor_s_commit() {
    for settled in [false, true] {
        for successor_first in [true, false] {
            let mut k = KeeperCore::new(true);
            let mut w = World::default();
            committed_update(&mut k, &mut w, settled);
            if successor_first {
                kill(&mut k, &mut w, 2, 600);
                assert_eq!(view(&k).reader_settled, None, "no claim reads the master");
                kill(&mut k, &mut w, 1, 500);
            } else {
                kill(&mut k, &mut w, 1, 500);
                kill(&mut k, &mut w, 2, 600);
            }
            assert_eq!(k.records()[&R].state, RecordState::Orphaned);
            let outs = connect(&mut k, &mut w, 3, 700, CAP_JOURNAL);
            assert_eq!(
                offer_of(&outs),
                Some(Some(after_the_successor(settled))),
                "settled={settled} successor_first={successor_first}"
            );
        }
    }
}

/// The same update whose outgoing window then ends at its Commit (`exit(0)`, inside the death
/// grace): the hold the keeper judges by the successor's end keeps what that end already left,
/// as the successor's death alone would have (its region at attempts 0, or the fallback with the
/// attempt). Before the fix the hold closed every region, losing the settled successor's exact
/// screen to the META rung.
#[test]
fn a_hold_judged_by_a_successor_that_registered_keeps_what_its_end_left() {
    for settled in [false, true] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        committed_update(&mut k, &mut w, settled);
        kill(&mut k, &mut w, 2, 600);
        w.live_pids.retain(|p| *p != 500);
        k.eof(1, 2, &mut w);
        let outs = k.peer_exit(500, 0, 2, &mut w);
        assert_eq!(k.records()[&R].state, RecordState::Orphaned, "{outs:?}");
        let outs = connect(&mut k, &mut w, 3, 700, CAP_JOURNAL);
        assert_eq!(
            offer_of(&outs),
            Some(Some(after_the_successor(settled))),
            "settled={settled}"
        );
    }
}

/// The outgoing window's link reconnecting after the Commit (its REGISTER again moves its claim
/// to the newer connection, the last in the list) does not make it the reader either: its
/// region told again is only the fallback, and settles nothing.
#[test]
fn a_reconnected_outgoing_link_is_no_reader() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    committed_update(&mut k, &mut w, false);
    connect(&mut k, &mut w, 3, 500, CAP_JOURNAL | CAP_LINK);
    assert_eq!(k.register(3, hdr(), vec![], R), vec![Out::DropDuplicate(R)]);
    assert_eq!(k.records()[&R].claimants, vec![2, 3]);
    assert_eq!(view(&k).reader_settled, Some(false), "the successor reads");
    assert_eq!(
        k.companion(3, R, COMPANION_JOURNAL, LEN, 4),
        vec![Out::CloseCompanion(1), Out::KeepCompanion(4)]
    );
    assert_eq!(
        view(&k),
        CompanionView {
            offered: Some(4),
            reader_settled: Some(false),
            attempts: 0,
            outgoing: Some(4)
        },
        "the outgoing window's region is the fallback, and settles no claim"
    );
    kill(&mut k, &mut w, 2, 600);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 3, &mut w);
    k.eof(3, 3, &mut w);
    k.peer_exit(500, 9, 3, &mut w);
    let outs = connect(&mut k, &mut w, 4, 700, CAP_JOURNAL);
    assert_eq!(
        offer_of(&outs),
        Some(Some(OfferJournal {
            attempts: 1,
            companion: Some(companion(4))
        }))
    );
}

/// The successor closing a tab (its RELEASE) while the outgoing window still claims ends the
/// reader's claim the same way its death would: judged at once, settled or not, and the outgoing
/// window is left claiming with no reader.
#[test]
fn a_successor_s_release_is_judged_as_the_reader_s_end() {
    for settled in [false, true] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        committed_update(&mut k, &mut w, settled);
        let outs = k.release(2, R);
        assert_eq!(k.records()[&R].claimants, vec![1]);
        assert_eq!(k.records()[&R].journal.reader_gone, Some(600));
        let (closed_now, offered, attempts) = if settled {
            (vec![1], Some(2), 0)
        } else {
            (vec![], Some(1), 1)
        };
        assert_eq!(closed(&outs), closed_now, "settled={settled}");
        assert_eq!(
            view(&k),
            CompanionView {
                offered,
                reader_settled: None,
                attempts,
                outgoing: None
            },
            "settled={settled}"
        );
    }
}

/// A missing exit watch leaves an EOF-only claim during the death grace. A new connection of
/// the same PID may reuse the old journal only when its known birth does not contradict the
/// old one. The world is RAM facts; no process, socket, exit watch or clock is exercised.
#[test]
fn a_known_different_birth_is_no_reconnect_even_after_eof() {
    let a = Birth {
        seconds: 10,
        micros: 20,
    };
    let births = [
        Birth {
            seconds: 11,
            micros: 20,
        },
        Birth {
            seconds: 10,
            micros: 21,
        },
    ];
    let run = |old: Option<Birth>, new: Option<Birth>, eof: bool| {
        let mut k = KeeperCore::new(false);
        let mut w = World {
            live_pids: vec![500],
            ..World::default()
        };
        assert!(k.accept(1, 500, old));
        k.hello(1, PeerClass::App, CAP_SENDS_BYE | CAP_JOURNAL, 0, &mut w);
        assert_eq!(k.register(1, hdr(), vec![], R), [Out::Keep(R)]);
        assert_eq!(
            k.companion(1, R, COMPANION_JOURNAL, LEN, 1),
            [Out::KeepCompanion(1)]
        );
        assert_eq!(view(&k).reader_settled, Some(true));
        if eof {
            assert!(k.eof(1, 10, &mut w).is_empty());
            assert_eq!(k.peers()[&1].eof_at, Some(10));
            assert_eq!(k.peers()[&1].exit, None, "no exit-watch result yet");
            assert_eq!(k.records()[&R].claimants, [1]);
            assert_eq!(
                k.records()[&R].state,
                RecordState::Claimed,
                "EOF alone has not ended the claim"
            );
        }
        assert!(k.accept(2, 500, new));
        k.hello(
            2,
            PeerClass::App,
            CAP_SENDS_BYE | CAP_JOURNAL | CAP_LINK,
            11,
            &mut w,
        );
        assert_eq!(k.register(2, hdr(), vec![], R), [Out::DropDuplicate(R)]);
        k
    };
    for eof in [false, true] {
        let k = run(Some(a), Some(a), eof);
        assert_eq!(k.records()[&R].claimants, [2]);
        assert_eq!(k.records()[&R].journal.reader, Some(2));
        assert_eq!(k.records()[&R].journal.own.get(&2), Some(&companion(1)));
        assert_eq!(view(&k).reader_settled, Some(true));
    }
    for (old, new) in [(None, Some(a)), (Some(a), None), (None, None)] {
        let k = run(old, new, true);
        assert_eq!(k.records()[&R].claimants, [2]);
        assert_eq!(view(&k).reader_settled, Some(true));
        assert_eq!(k.records()[&R].journal.own.get(&2), Some(&companion(1)));
    }
    let k = run(Some(a), Some(births[0]), false);
    assert_eq!(k.records()[&R].claimants, [1, 2]);
    assert_eq!(view(&k).reader_settled, Some(false));
    assert_eq!(k.records()[&R].journal.own.get(&1), Some(&companion(1)));
    assert!(!k.records()[&R].journal.own.contains_key(&2));

    let mut wrong = Vec::new();
    for b in births {
        let mut k = run(Some(a), Some(b), true);
        let before = (
            k.records()[&R].claimants.clone(),
            k.records()[&R].journal.clone(),
        );
        let new_reader = k.records()[&R].claimants == [1, 2]
            && k.records()[&R].journal.reader == Some(2)
            && view(&k).reader_settled == Some(false)
            && k.records()[&R].journal.own.get(&1) == Some(&companion(1))
            && !k.records()[&R].journal.own.contains_key(&2);
        let ended = k.release(2, R);
        let after = k.companion_view(R);
        let fallback = after
            .is_some_and(|v| v.offered == Some(1) && v.reader_settled.is_none() && v.attempts == 1);
        if !new_reader || !fallback {
            wrong.push(format!(
                "{a:?} -> {b:?}: before={before:?} ended={ended:?} after={after:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "a known different birth inherited a settled region through EOF: {wrong:#?}"
    );
}

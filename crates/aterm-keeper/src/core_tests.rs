// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A world the tests describe directly.
#[derive(Default)]
struct World {
    /// Who holds each master; an empty list is a scan that could not decide.
    held: BTreeMap<Rdev, Vec<u32>>,
    dead_shells: Vec<u32>,
    live_pids: Vec<u32>,
    /// What each window's marker says, by the window's pid.
    markers: BTreeMap<u32, MarkerEvidence>,
    /// How many looks at who holds what were taken: each one, in the
    /// server, is a walk of every process of the uid.
    scans: usize,
}

impl World {
    fn answer(&self, rdev: Rdev) -> Holders {
        match self.held.get(&rdev) {
            Some(p) if p.is_empty() => Holders::Unknown,
            Some(p) => Holders::Held(p.clone()),
            None => Holders::None,
        }
    }
}

impl KeeperEnv for World {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        self.scans += 1;
        self.answer(rdev)
    }
    fn holders_of(&mut self, rdevs: &[Rdev]) -> Vec<Holders> {
        if !rdevs.is_empty() {
            self.scans += 1;
        }
        rdevs.iter().map(|rdev| self.answer(*rdev)).collect()
    }
    fn shell_alive(&mut self, pid: u32, _birth: Birth) -> bool {
        !self.dead_shells.contains(&pid)
    }
    fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
        self.live_pids.contains(&pid)
    }
    fn marker(&mut self, pid: u32, _marker: &MarkerRef) -> MarkerEvidence {
        self.markers
            .get(&pid)
            .copied()
            .unwrap_or(MarkerEvidence::Unknown)
    }
}

fn hdr(rdev: Rdev, shell: u32) -> MasterHeader {
    MasterHeader {
        rdev,
        shell_pid: shell,
        shell_birth: Birth::default(),
        local_id: rdev,
    }
}

fn window(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, now: u64) -> Vec<Out> {
    assert!(k.accept(conn, pid, None));
    w.live_pids.push(pid);
    k.hello(conn, PeerClass::App, CAP_SENDS_BYE, now, w)
}

#[test]
fn the_classifier_reads_its_table_in_order() {
    let e = |successor_holds, bye, sends_bye, status| DeathEvidence {
        successor_holds,
        bye,
        sends_bye,
        status,
        marker: MarkerEvidence::Unknown,
    };
    let m = |sends_bye, status, marker| DeathEvidence {
        successor_holds: false,
        bye: false,
        sends_bye,
        status,
        marker,
    };
    assert_eq!(
        classify_death(e(true, true, true, Some(0))),
        Verdict::Handoff
    );
    assert_eq!(classify_death(e(false, true, true, Some(9))), Verdict::Quit);
    assert_eq!(
        classify_death(e(false, false, false, Some(0))),
        Verdict::CleanPreKeeper
    );
    assert_eq!(
        classify_death(e(false, false, false, Some(0x0100))),
        Verdict::Crash
    );
    assert_eq!(
        classify_death(e(false, false, false, Some(9))),
        Verdict::Crash
    );
    assert_eq!(
        classify_death(e(false, false, true, Some(0))),
        Verdict::Crash
    );
    assert_eq!(classify_death(e(false, false, true, None)), Verdict::Crash);
    use MarkerEvidence::{Dead, Held, Released, Unknown};
    assert_eq!(
        classify_death(m(true, Some(0), Released)),
        Verdict::CleanExit
    );
    assert_eq!(
        classify_death(m(false, Some(0), Released)),
        Verdict::CleanExit
    );
    for (status, marker) in [
        (Some(101 << 8), Released),
        (Some(9), Released),
        (None, Released),
        (Some(0), Held),
        (Some(0), Unknown),
        (Some(0), Dead),
        (Some(9), Dead),
    ] {
        assert_eq!(
            classify_death(m(true, status, marker)),
            Verdict::Crash,
            "{status:?} {marker:?}"
        );
    }
    assert_eq!(
        classify_death(m(false, Some(0), Unknown)),
        Verdict::CleanPreKeeper
    );
    assert_eq!(classify_death(m(false, Some(0), Dead)), Verdict::Crash);
    assert_eq!(
        classify_death(DeathEvidence {
            bye: true,
            ..m(true, Some(9), Dead)
        }),
        Verdict::Quit
    );
    assert!(exited_zero(0));
    assert!(!exited_zero(0x0700));
    assert!(!exited_zero(0x9));
    assert!(!exited_zero(101 << 8));
}

#[test]
fn the_brake_backs_off_then_holds_until_a_manual_launch() {
    let mut b = RelaunchBrake::new();
    assert_eq!(b.decide(100, true), BrakeDecision::Nothing, "a quit");
    assert_eq!(b.decide(100, false), BrakeDecision::RelaunchAt(100));
    assert_eq!(b.decide(200, false), BrakeDecision::RelaunchAt(10_200));
    assert_eq!(b.decide(300, false), BrakeDecision::RelaunchAt(60_300));
    assert_eq!(b.decide(400, false), BrakeDecision::Hold);
    assert!(b.held());
    assert_eq!(b.decide(500, false), BrakeDecision::Hold, "still held");
    b.manual_launch();
    assert_eq!((b.streak(), b.held()), (0, false));
    assert_eq!(b.decide(600, false), BrakeDecision::RelaunchAt(600));
    b.healthy();
    assert_eq!((b.streak(), b.held()), (0, false));
    for t in [700, 800, 900] {
        let _ = b.decide(t, false);
    }
    assert_eq!(b.decide(1000, false), BrakeDecision::Hold);
    assert!(b.held());
    b.healthy();
    assert_eq!((b.streak(), b.held()), (0, false));
}

#[test]
fn a_crash_orphans_and_a_relaunch_offers_keeping_the_copy() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    assert_eq!(k.register(1, hdr(10, 900), vec![], 10), vec![Out::Keep(10)]);
    assert_eq!(k.register(1, hdr(11, 901), vec![], 11), vec![Out::Keep(11)]);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 5, &mut w);
    k.peer_exit(500, 9, 6, &mut w);
    assert_eq!(k.orphan_count(), 2);
    assert_eq!(k.tick(7, &mut w), vec![Out::Relaunch]);
    let outs = window(&mut k, &mut w, 2, 600, 8);
    assert_eq!(
        outs,
        vec![
            Out::Welcome { conn: 2, offers: 2 },
            Out::Offer {
                conn: 2,
                rdev: 10,
                journal: None
            },
            Out::Offer {
                conn: 2,
                rdev: 11,
                journal: None
            },
        ]
    );
    assert!(k.records().contains_key(&10), "the keeper keeps its copy");
    assert_eq!(
        k.register(2, hdr(10, 900), vec![], 10),
        vec![Out::DropDuplicate(10)]
    );
    assert_eq!(k.records()[&10].state, RecordState::Claimed);
    assert!(k.peers()[&2].relaunched);
    k.tick(8 + 30 * 60_000 - 1, &mut w);
    assert_eq!(
        (k.relaunch_state(), k.brake().streak()),
        (
            RelaunchState::InFlight {
                since: 7,
                hello: Some((2, 8))
            },
            1
        )
    );
    k.tick(8 + 30 * 60_000, &mut w);
    assert_eq!(
        (k.relaunch_state(), k.brake().streak()),
        (RelaunchState::Idle, 0)
    );
}

#[test]
fn a_crashed_desk_is_judged_and_offered_after_one_holder_scan() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    for rdev in 1..=200u64 {
        k.register(1, hdr(rdev, 10_000 + rdev as u32), vec![], rdev);
    }
    w.live_pids.retain(|p| *p != 500);
    w.scans = 0;
    k.eof(1, 5, &mut w);
    k.peer_exit(500, 9, 6, &mut w);
    assert_eq!(k.orphan_count(), 200);
    assert!(w.scans <= 1, "the crash took {} holder scans", w.scans);
    w.scans = 0;
    let outs = window(&mut k, &mut w, 2, 600, 8);
    assert_eq!(
        outs.first(),
        Some(&Out::Welcome {
            conn: 2,
            offers: 200
        })
    );
    assert_eq!(outs.len(), 201, "the WELCOME and every offer");
    assert!(w.scans <= 1, "one HELLO took {} holder scans", w.scans);
}

#[test]
fn a_long_hold_is_rescanned_once_per_pass() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 10, 0);
    for rdev in 1..=100u64 {
        k.register(1, hdr(rdev, 10_000 + rdev as u32), vec![], rdev);
        w.held.insert(rdev, vec![20]);
    }
    w.live_pids.retain(|p| *p != 10);
    w.scans = 0;
    k.eof(1, 0, &mut w);
    k.peer_exit(10, 0, 0, &mut w);
    let held = |k: &KeeperCore| {
        k.records()
            .values()
            .filter(|r| matches!(r.state, RecordState::Held { .. }))
            .count()
    };
    assert_eq!(held(&k), 100, "the successor holds every master");
    assert!(w.scans <= 2, "the Commit took {} holder scans", w.scans);
    w.scans = 0;
    let passes = 30;
    for pass in 1..=passes {
        assert!(k.tick(pass as u64 * HOLD_RESCAN_MS, &mut w).is_empty());
    }
    assert_eq!(held(&k), 100, "still held: the successor lives");
    assert!(
        w.scans <= passes,
        "{} holder scans in {passes} passes",
        w.scans
    );
}

#[test]
fn an_eof_alone_is_not_a_death() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.eof(1, 5, &mut w);
    assert!(k.tick(5_000, &mut w).is_empty(), "the process lives");
    assert_eq!(k.records()[&10].state, RecordState::Claimed);
    w.live_pids.clear();
    k.peer_exit(500, 9, 6_000, &mut w);
    assert_eq!(k.records()[&10].state, RecordState::Orphaned);
}

#[test]
fn a_quit_closes_and_relaunches_nothing() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.bye(1);
    k.eof(1, 5, &mut w);
    let out = k.peer_exit(500, 0, 5, &mut w);
    assert_eq!(out, vec![Out::Close(10)]);
    assert!(k.tick(100_000, &mut w).is_empty());
    assert_eq!(k.relaunches(), 0);
}

/// A quit's BYE that names a master to keep (the Mac Setup tab's installer, mid-install)
/// leaves it orphaned with its shell alive, closing the rest as any quit does, owing no
/// relaunch and telling no window; the next window's HELLO, a person's or macOS's launch, is
/// offered it. One whose shell is gone by the quit's end closes with the rest.
///
/// NEGATIVE CONTROL: `a_quit_closes_and_relaunches_nothing`, the same quit keeping
/// nothing, closes every master.
#[test]
fn a_quit_keeps_the_masters_its_bye_names_for_the_next_launch() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(1, hdr(11, 901), vec![], 11);
    k.register(1, hdr(12, 902), vec![], 12);
    w.dead_shells.push(902);
    k.bye_keeping(1, &[11, 12]);
    k.eof(1, 5, &mut w);
    w.live_pids.clear();
    let out = k.peer_exit(500, 0, 5, &mut w);
    assert_eq!(
        out,
        vec![Out::Close(10), Out::WatchPid(901), Out::Close(12)],
        "the installer's master outlives the quit for the next launch"
    );
    assert_eq!(k.records()[&11].state, RecordState::Orphaned);
    assert!(
        k.records()[&11].kept,
        "an orphan the quit kept, not a crash's"
    );
    assert!(k.status_lines().iter().any(|l| l.ends_with("kept=1")));
    assert!(
        k.tick(100_000, &mut w).is_empty(),
        "no relaunch after a quit"
    );
    assert_eq!(k.relaunches(), 0);
    let outs = window(&mut k, &mut w, 2, 600, 100_001);
    assert!(outs.contains(&Out::Offer {
        conn: 2,
        rdev: 11,
        journal: None
    }));
    assert!(k.records()[&11].kept, "an offer of it says it was kept");
    k.register(2, hdr(11, 901), vec![], 11);
    assert!(
        !k.records()[&11].kept,
        "claimed again, it is that window's like any other"
    );
    k.eof(2, 100_002, &mut w);
    w.live_pids.clear();
    k.peer_exit(600, 9, 100_003, &mut w);
    assert_eq!(k.records()[&11].state, RecordState::Orphaned);
    assert!(
        !k.records()[&11].kept,
        "CONTROL: the crash of the window that adopted it orphans it as a crash's"
    );
}

#[test]
fn a_pre_keeper_clean_exit_closes() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    assert!(k.accept(1, 500, None));
    w.live_pids.push(500);
    k.hello(1, PeerClass::App, 0, 0, &mut w);
    k.register(1, hdr(10, 900), vec![], 10);
    k.eof(1, 5, &mut w);
    assert_eq!(k.peer_exit(500, 0, 5, &mut w), vec![Out::Close(10)]);
}

#[test]
fn an_update_is_a_hold_and_a_failed_successor_orphans() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.live_pids.push(700);
    assert_eq!(k.pending(1, 700, 1, None), vec![Out::WatchPid(700)]);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 2, &mut w);
    k.peer_exit(500, 9, 2, &mut w);
    assert!(matches!(k.records()[&10].state, RecordState::Held { .. }));
    assert!(
        k.tick(3, &mut w).is_empty(),
        "no relaunch over a live successor"
    );
    w.live_pids.retain(|p| *p != 700);
    k.peer_exit(700, 74 << 8, 4, &mut w);
    assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    assert_eq!(k.tick(5, &mut w), vec![Out::Relaunch]);
}

#[test]
fn a_quit_beside_another_windows_update_closes_its_masters() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    window(&mut k, &mut w, 2, 510, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(2, hdr(20, 920), vec![], 20);
    w.live_pids.push(700);
    let _ = k.pending(1, 700, 1, None);
    k.bye(2);
    w.live_pids.retain(|p| *p != 510);
    let _ = k.eof(2, 2, &mut w);
    assert_eq!(k.peer_exit(510, 0, 2, &mut w), vec![Out::Close(20)]);
    assert_eq!(k.records()[&10].state, RecordState::Claimed);
}

#[test]
fn a_commit_whose_successor_died_first_orphans_the_master() {
    for (aware, read_first, rolled_back, expired, beside) in [
        (false, true, false, false, 0),
        (false, false, false, false, 0),
        (true, true, false, false, 0),
        (true, false, false, false, 0),
        (false, true, true, false, 0),
        (false, false, false, true, 0),
        (false, true, false, false, 1),
        (false, false, false, false, 1),
        (true, true, false, false, 1),
        (true, true, false, false, 2),
    ] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        if beside > 0 {
            window(&mut k, &mut w, 3, 510, 0);
            k.register(3, hdr(20, 920), vec![], 20);
            w.live_pids.push(710);
        }
        w.markers.insert(500, MarkerEvidence::Held);
        let marker = MarkerRef {
            nanos: 1,
            dir: b"/m".to_vec(),
        };
        assert!(k.name_marker(1, marker, &mut w));
        k.register(1, hdr(10, 900), vec![], 10);
        k.register(1, hdr(11, 901), vec![], 11);
        w.live_pids.push(700);
        let _ = k.pending(1, 700, 1, None);
        if beside == 1 {
            let _ = k.pending(3, 710, 1, None);
        }
        let mut now = 3;
        if expired {
            now += PENDING_TTL_MS;
            let _ = k.tick(now, &mut w);
        }
        if aware {
            window(&mut k, &mut w, 2, 700, 2);
            k.register(2, hdr(10, 900), vec![], 10);
        }
        if beside == 2 {
            let _ = k.pending(3, 710, 2, None);
            assert!(k.accept(4, 710, None));
            k.hello(4, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 2, &mut w);
            k.register(4, hdr(20, 920), vec![], 20);
        }
        w.live_pids.retain(|p| *p != 700);
        let successor_ends = |k: &mut KeeperCore, w: &mut World, now| {
            if aware {
                let _ = k.eof(2, now, w);
            }
            let _ = k.peer_exit(700, 9, now, w);
        };
        if read_first {
            successor_ends(&mut k, &mut w, now);
        }
        if rolled_back {
            now += 10 * DEATH_GRACE_MS;
        }
        w.live_pids.retain(|p| *p != 500);
        w.markers.insert(500, MarkerEvidence::Released);
        let _ = k.eof(1, now, &mut w);
        let _ = k.peer_exit(500, 0, now, &mut w);
        if !read_first {
            successor_ends(&mut k, &mut w, now);
        }
        if beside == 1 {
            let _ = k.tick(now + PENDING_TTL_MS, &mut w);
        }
        let state = |rdev| k.records().get(&rdev).map(|r| r.state.clone());
        let want = (!rolled_back).then_some(RecordState::Orphaned);
        assert_eq!(
            ([state(10), state(11)], k.ends().crash, state(20)),
            (
                [want.clone(), want],
                2 * u64::from(!rolled_back),
                (beside > 0).then_some(RecordState::Claimed)
            ),
            "aware={aware} read_first={read_first} rolled_back={rolled_back} expired={expired} \
             beside={beside}"
        );
    }
}

#[test]
fn a_committed_successor_holding_the_master_is_a_hold() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.held.insert(10, vec![700]);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 2, &mut w);
    k.peer_exit(500, 0, 2, &mut w);
    assert!(matches!(k.records()[&10].state, RecordState::Held { .. }));
    let outs = window(&mut k, &mut w, 2, 600, 3);
    assert_eq!(outs, vec![Out::Welcome { conn: 2, offers: 0 }]);
}

#[test]
fn a_quiet_successor_that_hands_the_master_on_is_judged_by_the_next_ones_end() {
    for (status, orphaned) in [(9, true), (0, false)] {
        let mut k = KeeperCore::new(false);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.push(700);
        let _ = k.pending(1, 700, 1, None);
        w.held.insert(10, vec![700]);
        w.live_pids.retain(|p| *p != 500);
        let _ = k.eof(1, 2, &mut w);
        let _ = k.peer_exit(500, 0, 2, &mut w);
        let _ = k.tick(2 + PENDING_TTL_MS, &mut w);
        w.held.insert(10, vec![800]);
        let outs = k.peer_exit(700, 0, 3 + PENDING_TTL_MS, &mut w);
        assert!(outs.contains(&Out::WatchPid(800)), "{outs:?}");
        w.held.remove(&10);
        let _ = k.peer_exit(800, status, 4 + PENDING_TTL_MS, &mut w);
        let state = k.records().get(&10).map(|r| r.state.clone());
        let want = orphaned.then_some(RecordState::Orphaned);
        assert_eq!(state, want, "800 ended with {status}");
    }
}

#[test]
fn possession_is_proved_and_caps_hold() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    assert_eq!(
        k.register(1, hdr(10, 900), vec![], 11),
        vec![
            Out::Refused {
                conn: 1,
                code: RefuseCode::RdevMismatch,
                rdev: 10
            },
            Out::DropDuplicate(10)
        ]
    );
    window(&mut k, &mut w, 2, 501, 0);
    window(&mut k, &mut w, 3, 502, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(2, hdr(10, 900), vec![], 10);
    assert!(matches!(
        k.register(3, hdr(10, 900), vec![], 10)[0],
        Out::Refused {
            code: RefuseCode::TooManyClaimants,
            ..
        }
    ));
    assert!(k.accept(9, 503, None));
    k.hello(9, PeerClass::Status, 0, 0, &mut w);
    assert!(matches!(
        k.register(9, hdr(12, 900), vec![], 12)[0],
        Out::Refused {
            code: RefuseCode::WrongClass,
            ..
        }
    ));
    assert!(matches!(k.release(9, 10)[0], Out::Refused { .. }));
    assert_eq!(
        k.release(1, 10),
        vec![],
        "the other claimant still holds it"
    );
    assert_eq!(k.records()[&10].claimants, vec![2]);
    assert!(
        matches!(k.release(1, 10)[0], Out::Refused { .. }),
        "no claim left"
    );
    assert_eq!(k.release(2, 10), vec![Out::Close(10)]);
    assert_eq!(
        k.release(2, 10),
        vec![Out::Refused {
            conn: 2,
            code: RefuseCode::NoSuchRecord,
            rdev: 10
        }],
        "a master with no record is told apart from one held for another claim"
    );
}

#[test]
fn a_late_release_from_the_outgoing_window_keeps_the_successors_copy() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    assert_eq!(k.register(1, hdr(10, 900), vec![], 10), vec![Out::Keep(10)]);
    k.pending(1, 600, 1, None);
    w.live_pids.push(600);
    window(&mut k, &mut w, 2, 600, 2);
    assert_eq!(
        k.register(2, hdr(10, 900), vec![], 10),
        vec![Out::DropDuplicate(10)]
    );
    assert_eq!(k.release(1, 10), vec![]);
    w.live_pids.retain(|p| *p != 500);
    assert!(k.eof(1, 3, &mut w).is_empty());
    assert!(k.peer_exit(500, 0, 3, &mut w).is_empty());
    assert!(k.records().contains_key(&10), "the successor's copy stays");
    w.live_pids.retain(|p| *p != 600);
    k.eof(2, 4, &mut w);
    k.peer_exit(600, 9, 4, &mut w);
    assert_eq!(k.records()[&10].state, RecordState::Orphaned);
}

#[test]
fn a_release_from_a_reconnected_link_is_the_last_claim() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    assert!(k.accept(2, 500, None));
    k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 1, &mut w);
    k.register(2, hdr(10, 900), vec![], 10);
    assert_eq!(k.records()[&10].claimants, vec![2]);
    assert_eq!(k.release(2, 10), vec![Out::Close(10)]);
}

#[test]
fn a_stale_release_from_an_older_link_leaves_the_newer_links_claim() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    assert!(k.accept(2, 500, None));
    k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 1, &mut w);
    assert_eq!(
        k.release(2, 10),
        vec![Out::Close(10)],
        "the replay ends the old link's claim"
    );
    k.register(2, hdr(10, 901), vec![], 10);
    assert_eq!(
        k.records()[&10].claimants,
        vec![2],
        "the new tab is claimed"
    );
    assert_eq!(
        k.release(1, 10),
        vec![Out::Refused {
            conn: 1,
            code: RefuseCode::NotClaimant,
            rdev: 10
        }]
    );
    assert_eq!(
        k.records()[&10].claimants,
        vec![2],
        "the new tab stays in custody"
    );
}

#[test]
fn a_release_keeps_the_claim_of_a_reused_pid() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    for conn in [1, 2] {
        assert!(k.accept(
            conn,
            500,
            Some(Birth {
                seconds: conn,
                micros: 0
            })
        ));
        k.hello(conn, PeerClass::App, CAP_SENDS_BYE, conn, &mut w);
        k.register(conn, hdr(10, 900), vec![], 10);
    }
    assert_eq!(k.records()[&10].claimants, vec![1, 2]);
    assert!(k.release(1, 10).is_empty());
    assert_eq!(k.records()[&10].claimants, vec![2]);
    assert_eq!(k.release(2, 10), vec![Out::Close(10)]);
}

#[test]
fn a_reconnected_link_leaves_room_for_the_update_successors_claim() {
    let mut k = KeeperCore::new(false);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    assert!(k.accept(2, 500, None));
    k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 1, &mut w);
    k.register(2, hdr(10, 900), vec![], 10);
    let _ = k.eof(1, 2, &mut w);
    w.live_pids.push(700);
    let _ = k.pending(2, 700, 3, None);
    assert!(k.accept(3, 700, None));
    k.hello(3, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 3, &mut w);
    let outs = k.register(3, hdr(10, 900), vec![], 10);
    assert!(
        !outs.iter().any(|out| matches!(
            out,
            Out::Refused {
                code: RefuseCode::TooManyClaimants,
                ..
            }
        )),
        "{outs:?}"
    );
    w.held.insert(10, vec![700]);
    w.live_pids.retain(|p| *p != 500);
    let _ = k.eof(2, 4, &mut w);
    let _ = k.peer_exit(500, 0, 4, &mut w);
    k.bye(3);
    w.held.remove(&10);
    w.live_pids.retain(|p| *p != 700);
    let _ = k.eof(3, 5, &mut w);
    let _ = k.peer_exit(700, 0, 5, &mut w);
    assert_eq!(
        k.records().get(&10).map(|rec| rec.state.clone()),
        None,
        "the quit closes the shell it held"
    );
    assert!(k.tick(100_000, &mut w).is_empty(), "nothing relaunches");
}

#[test]
fn a_stale_register_from_an_older_link_leaves_the_claim_with_the_newer_one() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    assert!(k.accept(2, 500, None));
    k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 1, &mut w);
    assert_eq!(k.register(2, hdr(10, 900), vec![], 10), vec![Out::Keep(10)]);
    assert_eq!(
        k.register(1, hdr(10, 900), vec![], 10),
        vec![Out::DropDuplicate(10)]
    );
    assert_eq!(k.records()[&10].claimants, vec![2]);
    assert!(k.meta(2, 10, b"m".to_vec()).is_empty());
}

#[test]
fn a_dead_shell_is_closed_not_offered() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(1, hdr(11, 901), vec![], 11);
    w.live_pids.clear();
    k.eof(1, 1, &mut w);
    k.peer_exit(500, 9, 1, &mut w);
    w.dead_shells.push(900);
    let outs = window(&mut k, &mut w, 2, 600, 2);
    assert_eq!(
        outs,
        vec![
            Out::Close(10),
            Out::Welcome { conn: 2, offers: 1 },
            Out::Offer {
                conn: 2,
                rdev: 11,
                journal: None
            }
        ]
    );
}

#[test]
fn an_orphan_whose_shell_dies_is_pruned_at_once() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(1, hdr(11, 901), vec![], 11);
    w.live_pids.clear();
    k.eof(1, 1, &mut w);
    let outs = k.peer_exit(500, 9, 1, &mut w);
    assert!(outs.contains(&Out::WatchPid(900)), "{outs:?}");
    assert!(outs.contains(&Out::WatchPid(901)), "{outs:?}");
    assert_eq!(k.orphan_count(), 2);
    w.dead_shells.push(900);
    let outs = k.peer_exit(900, 1, 5, &mut w);
    assert_eq!(outs, vec![Out::Close(10)]);
    assert_eq!(k.orphan_count(), 1);
    assert_eq!(k.ends().pruned, 1);
    let status = k.status_lines().join("\n");
    assert!(!status.contains("shell=900"), "{status}");
    assert!(status.contains("shell=901"), "{status}");
    assert!(status.contains("pruned=1"), "{status}");
    w.dead_shells.push(901);
    assert_eq!(k.prune_dead_orphans(&mut w), vec![Out::Close(11)]);
    assert_eq!(k.records().len(), 0);
    assert!(k.tick(5, &mut w).is_empty());
    assert_eq!(k.relaunch_state(), RelaunchState::Idle);
}

#[test]
fn a_lost_bye_with_the_marker_gone_is_a_quit() {
    let marker = MarkerRef {
        nanos: 7,
        dir: b"/logs".to_vec(),
    };
    let run = |at_hello: MarkerEvidence, at_death: MarkerEvidence, status: i32| {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        w.markers.insert(500, at_hello);
        let kept = k.name_marker(1, marker.clone(), &mut w);
        assert_eq!(kept, at_hello == MarkerEvidence::Held);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        w.markers.insert(500, at_death);
        k.eof(1, 1, &mut w);
        k.peer_exit(500, status, 1, &mut w);
        (
            k.last_verdict(),
            k.records().contains_key(&10),
            k.relaunch_state(),
        )
    };
    use MarkerEvidence::{Dead, Held, Released};
    assert_eq!(
        run(Held, Released, 0),
        (Some(Verdict::CleanExit), false, RelaunchState::Idle),
        "a clean quit is never a crash, and relaunches nothing"
    );
    assert_eq!(run(Held, Dead, 9).0, Some(Verdict::Crash));
    assert_eq!(
        run(Held, Released, 101 << 8).0,
        Some(Verdict::Crash),
        "a panic's exit path also removes the marker: a crash, never a quit"
    );
    assert_eq!(
        run(Released, Released, 0).0,
        Some(Verdict::Crash),
        "a marker not held at HELLO proves nothing"
    );
    assert_eq!(run(Dead, Released, 0).0, Some(Verdict::Crash));
}

#[test]
fn the_relaunch_lane_waits_for_hello_and_backs_off() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.live_pids.clear();
    k.eof(1, 0, &mut w);
    k.peer_exit(500, 9, 0, &mut w);
    assert_eq!(k.tick(0, &mut w), vec![Out::Relaunch]);
    assert!(k.tick(14_999, &mut w).is_empty());
    assert!(k.tick(15_000, &mut w).is_empty());
    assert_eq!(k.relaunch_state(), RelaunchState::Due { at: 25_000 });
    assert!(k.tick(24_999, &mut w).is_empty());
    assert_eq!(k.tick(25_000, &mut w), vec![Out::Relaunch]);
    assert!(k.tick(40_000, &mut w).is_empty());
    assert_eq!(k.relaunch_state(), RelaunchState::Due { at: 100_000 });
    assert_eq!(k.tick(100_000, &mut w), vec![Out::Relaunch]);
    assert!(k.tick(115_000, &mut w).is_empty());
    assert_eq!(
        k.relaunch_state(),
        RelaunchState::Held,
        "the streak is spent"
    );
    assert!(
        k.tick(10_000_000, &mut w).is_empty(),
        "held until a manual launch"
    );
    let outs = window(&mut k, &mut w, 2, 600, 10_000_001);
    assert!(outs.contains(&Out::Offer {
        conn: 2,
        rdev: 10,
        journal: None
    }));
    assert_eq!(k.brake().streak(), 0);
    k.register(2, hdr(10, 900), vec![], 10);
    w.live_pids.clear();
    k.eof(2, 10_000_002, &mut w);
    k.peer_exit(600, 9, 10_000_002, &mut w);
    assert_eq!(k.tick(10_000_002, &mut w), vec![Out::Relaunch]);
}

#[test]
fn a_relaunched_window_that_lives_on_does_not_spend_the_brake() {
    const WEEK: u64 = 7 * 24 * 3_600_000;
    for update in [true, false] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 100, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        let (mut pid, mut conns, mut next_conn, mut now) = (100u32, vec![1], 2, 0);
        for cycle in 0..4u32 {
            now += WEEK;
            let _ = k.tick(now, &mut w);
            w.live_pids.retain(|p| *p != pid);
            for c in &conns {
                let _ = k.eof(*c, now, &mut w);
            }
            let _ = k.peer_exit(pid, 9, now, &mut w);
            assert_eq!(
                k.tick(now, &mut w),
                vec![Out::Relaunch],
                "update={update} cycle {cycle}"
            );
            let (r, rc, lc) = (1_000 + cycle * 10, next_conn, next_conn + 1);
            next_conn += 2;
            window(&mut k, &mut w, rc, r, now + 1);
            k.register(rc, hdr(10, 900), vec![], 10);
            now += 600_000;
            if update {
                w.live_pids.push(r + 1);
                let _ = k.pending(rc, r + 1, now, None);
                w.held.insert(10, vec![r + 1]);
                w.live_pids.retain(|p| *p != r);
                let _ = k.eof(rc, now, &mut w);
                let _ = k.peer_exit(r, 0, now, &mut w);
                (pid, conns) = (r + 1, vec![lc]);
            } else {
                let _ = k.eof(rc, now, &mut w);
                (pid, conns) = (r, vec![rc, lc]);
            }
            assert!(k.accept(lc, pid, None));
            k.hello(lc, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, now, &mut w);
            k.register(lc, hdr(10, 900), vec![], 10);
            w.held.remove(&10);
            assert_eq!(k.records()[&10].state, RecordState::Claimed);
        }
    }
}

#[test]
fn a_keeper_without_a_bundle_never_relaunches() {
    let mut k = KeeperCore::new(false);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.live_pids.clear();
    k.eof(1, 0, &mut w);
    k.peer_exit(500, 9, 0, &mut w);
    assert!(k.tick(1_000_000, &mut w).is_empty());
    assert_eq!(k.orphan_count(), 1, "held for the next manual launch");
}

#[test]
fn an_offer_whose_recipient_dies_is_an_orphan_again() {
    for eof_read in [true, false] {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        window(&mut k, &mut w, 2, 600, 1);
        assert_eq!(k.records()[&10].state, RecordState::Offered { to: 2 });
        w.live_pids.clear();
        let mut outs = Vec::new();
        if eof_read {
            outs.extend(k.eof(2, 2, &mut w));
        }
        outs.extend(k.peer_exit(600, 9, 2, &mut w));
        outs.extend(k.tick(2 + DEATH_GRACE_MS, &mut w));
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
        assert!(
            outs.contains(&Out::WatchPid(900)),
            "the orphan's leader is watched again (EOF read: {eof_read}): {outs:?}"
        );
        assert!(
            outs.contains(&Out::Relaunch),
            "a recipient that crashed before it registered still owes the orphan a window \
             (EOF read: {eof_read})"
        );
    }
}

#[test]
fn an_offer_whose_link_drops_while_its_window_lives_is_offered_again() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 10, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.live_pids.clear();
    k.eof(1, 0, &mut w);
    k.peer_exit(10, 9, 0, &mut w);
    assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    assert_eq!(k.tick(0, &mut w), vec![Out::Relaunch]);
    let outs = window(&mut k, &mut w, 2, 20, 1);
    assert!(
        outs.contains(&Out::Offer {
            conn: 2,
            rdev: 10,
            journal: None
        }),
        "{outs:?}"
    );
    assert_eq!(k.records()[&10].state, RecordState::Offered { to: 2 });
    let outs = k.eof(2, 2, &mut w);
    assert!(
        outs.contains(&Out::WatchPid(900)),
        "the orphan's leader is watched again: {outs:?}"
    );
    assert!(k.tick(2 + DEATH_GRACE_MS + 1, &mut w).is_empty());
    assert_eq!(
        k.records()[&10].state,
        RecordState::Orphaned,
        "no offer stands to a connection that is gone"
    );
    assert!(k.accept(3, 20, None));
    assert_eq!(
        k.hello(3, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 3, &mut w),
        vec![Out::Welcome { conn: 3, offers: 0 }]
    );
    for holders in [vec![20], Vec::new()] {
        w.held.insert(10, holders);
        assert_eq!(
            k.adopt(3, &mut w),
            vec![Out::Welcome { conn: 3, offers: 0 }]
        );
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    }
    w.held.clear();
    let outs = k.adopt(3, &mut w);
    assert!(
        outs.contains(&Out::Offer {
            conn: 3,
            rdev: 10,
            journal: None
        }),
        "{outs:?}"
    );
    k.bye(3);
    let now = 100_000;
    k.eof(3, now, &mut w);
    w.live_pids.clear();
    k.peer_exit(20, 0, now, &mut w);
    for t in [now, now + 1_000_000, now + 10_000_000] {
        assert!(
            !k.tick(t, &mut w).contains(&Out::Relaunch),
            "a window the person quit is never brought back"
        );
    }
    assert_eq!(
        k.relaunch_state(),
        RelaunchState::Idle,
        "the relaunched window quit: the lane is settled"
    );
}

fn a_tab_closed_while_its_link_was_down(eof_first: bool) {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    k.register(1, hdr(11, 901), vec![], 11);
    if eof_first {
        assert!(
            k.eof(1, 5, &mut w).is_empty(),
            "an EOF alone decides nothing"
        );
    }
    assert!(k.accept(2, 500, None));
    assert_eq!(
        k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 6, &mut w),
        vec![Out::Welcome { conn: 2, offers: 0 }]
    );
    assert_eq!(
        k.register(2, hdr(11, 901), vec![], 11),
        vec![Out::DropDuplicate(11)]
    );
    assert_eq!(k.release(2, 10), vec![Out::Close(10)], "the RELEASE lands");
    assert!(!k.records().contains_key(&10));
    if !eof_first {
        assert!(
            k.eof(1, 7, &mut w).is_empty(),
            "an EOF alone decides nothing"
        );
    }
    k.bye(2);
    k.eof(2, 8, &mut w);
    w.live_pids.clear();
    assert_eq!(k.peer_exit(500, 0, 9, &mut w), vec![Out::Close(11)]);
    assert!(k.records().is_empty(), "nothing left in custody");
    assert!(
        k.tick(100_000, &mut w).is_empty(),
        "no relaunch after a quit"
    );
    assert_eq!(k.relaunches(), 0);
}

#[test]
fn a_release_on_the_reconnected_link_reaches_what_the_dropped_link_claimed() {
    a_tab_closed_while_its_link_was_down(true);
}

#[test]
fn a_release_on_the_new_link_lands_before_the_old_links_eof_is_read() {
    a_tab_closed_while_its_link_was_down(false);
}

#[test]
fn another_process_may_not_release_what_a_dropped_link_claimed() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    assert!(k.eof(1, 5, &mut w).is_empty());
    window(&mut k, &mut w, 2, 600, 6);
    let refused = |conn| {
        vec![Out::Refused {
            conn,
            code: RefuseCode::NotClaimant,
            rdev: 10,
        }]
    };
    assert_eq!(k.release(2, 10), refused(2), "another window");
    let other_birth = Birth {
        seconds: 1,
        ..Birth::default()
    };
    assert!(k.accept(3, 500, Some(other_birth)));
    k.hello(3, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 7, &mut w);
    assert_eq!(k.release(3, 10), refused(3), "another birth at the pid");
    assert_eq!(k.records()[&10].claimants, vec![1]);
}

fn an_end_after_its_offer_went_back(
    claimed_since: bool,
    marker_at_death: MarkerEvidence,
) -> RelaunchState {
    let marker = MarkerRef {
        nanos: 7,
        dir: b"/logs".to_vec(),
    };
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    window(&mut k, &mut w, 1, 500, 0);
    k.register(1, hdr(10, 900), vec![], 10);
    w.live_pids.retain(|p| *p != 500);
    k.eof(1, 1, &mut w);
    k.peer_exit(500, 9, 1, &mut w);
    w.markers.insert(600, MarkerEvidence::Held);
    window(&mut k, &mut w, 2, 600, 2);
    assert!(k.name_marker(2, marker.clone(), &mut w));
    assert_eq!(k.records()[&10].state, RecordState::Offered { to: 2 });
    assert!(k.accept(3, 600, None));
    k.hello(3, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 3, &mut w);
    assert!(k.name_marker(3, marker, &mut w));
    k.eof(2, 4, &mut w);
    assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    if claimed_since {
        assert_eq!(
            k.register(3, hdr(10, 900), vec![], 10),
            vec![Out::DropDuplicate(10)]
        );
    }
    window(&mut k, &mut w, 4, 700, 5);
    k.register(4, hdr(20, 920), vec![], 20);
    assert!(k.accept(5, 800, None));
    k.hello(5, PeerClass::Status, 0, 5, &mut w);
    w.live_pids.retain(|p| *p != 700);
    k.eof(4, 6, &mut w);
    let told: Vec<(ConnId, u16)> = k
        .peer_exit(700, 9, 6, &mut w)
        .iter()
        .filter_map(|o| match o {
            Out::Orphans { conn, n } => Some((*conn, *n)),
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        vec![(3, if claimed_since { 1 } else { 2 })],
        "the live link hears how many orphans are on offer; the dropped connection and the \
         status client do not"
    );
    k.tick(7, &mut w);
    assert_eq!(k.relaunch_state(), RelaunchState::Idle, "window 600 lives");
    w.markers.insert(600, marker_at_death);
    k.eof(3, 8, &mut w);
    w.live_pids.retain(|p| *p != 600);
    k.peer_exit(600, 0, 8, &mut w);
    k.relaunch_state()
}

#[test]
fn a_quit_whose_bye_was_lost_owes_its_returned_offers_nothing() {
    use MarkerEvidence::{Dead, Released};
    for claimed_since in [false, true] {
        assert_eq!(
            an_end_after_its_offer_went_back(claimed_since, Released),
            RelaunchState::Idle,
            "a quit relaunches nothing (claimed since: {claimed_since})"
        );
    }
    assert_eq!(
        an_end_after_its_offer_went_back(false, Dead),
        RelaunchState::Due { at: 8 },
        "a crash's returned offer owes a window, at once: with no relaunch yet, the crash \
         beside a live window spent no streak"
    );
}

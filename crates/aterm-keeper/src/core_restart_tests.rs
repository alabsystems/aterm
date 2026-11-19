// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's RESTART_WHEN_SAFE ([`KeeperCore::restart_when_safe`]): the keeper exits only once
//! nothing only it holds would be lost and another file stands at its executable's path.
//!
//! NEGATIVE CONTROLS (run by hand, each restored): with the orphan count dropped from
//! `custody_hold`, [`an_orphan_holds_a_restart_until_its_shell_ends`] sees an exit beside the
//! orphan; with `restart_ready` reading every look at the executable as replaced,
//! [`an_executable_still_in_place_holds_a_restart_however_safe_its_custody`] sees an exit
//! beside the same file and [`an_older_build_at_the_path_holds_a_restart_however_safe_its_custody`]
//! one for an older build.

use super::*;

const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const R1: Rdev = 0x1000_0001;
const R2: Rdev = 0x1000_0002;

struct World {
    held: BTreeMap<Rdev, Vec<u32>>,
    dead_shells: Vec<u32>,
    live_pids: Vec<u32>,
    image: ImageLook,
}

impl Default for World {
    fn default() -> Self {
        Self {
            held: BTreeMap::new(),
            dead_shells: Vec::new(),
            live_pids: Vec::new(),
            image: ImageLook::Replaced(EXE.to_string()),
        }
    }
}

impl KeeperEnv for World {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        self.held
            .get(&rdev)
            .map_or(Holders::None, |p| Holders::Held(p.clone()))
    }
    fn shell_alive(&mut self, pid: u32, _birth: Birth) -> bool {
        !self.dead_shells.contains(&pid)
    }
    fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
        self.live_pids.contains(&pid)
    }
    fn image(&mut self) -> ImageLook {
        self.image.clone()
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

fn window(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32) -> Vec<Out> {
    assert!(k.accept(conn, pid, None));
    w.live_pids.push(pid);
    k.hello(conn, PeerClass::App, CAP_SENDS_BYE, 0, w)
}

/// A window on `conn` that registered `rdev`, whose shell is `shell`.
fn claim(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, rdev: Rdev, shell: u32) {
    window(k, w, conn, pid);
    assert_eq!(
        k.register(conn, hdr(rdev, shell), Vec::new(), rdev),
        [Out::Keep(rdev)]
    );
}

/// The window `pid` on `conn` ended with `status`, both halves of its death read at `now`.
fn ends(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, status: i32, now: u64) {
    w.live_pids.retain(|p| *p != pid);
    k.eof(conn, now, w);
    k.peer_exit(pid, status, now, w);
}

fn held(hold: RestartHold) -> Vec<Out> {
    vec![Out::Restart(RestartNote::Held(hold))]
}

fn exit() -> Vec<Out> {
    vec![Out::Exit {
        replaced: EXE.to_string(),
    }]
}

#[test]
fn a_restart_beside_live_windows_masters_alone_exits_at_once() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    assert_eq!(
        k.restart_when_safe(2),
        [Out::Restart(RestartNote::Wanted { pid: 200 })]
    );
    assert!(
        k.restart_when_safe(2).is_empty(),
        "asked twice on one connection, said once"
    );
    assert_eq!(k.tick(1, &mut w), exit(), "the turn that read the request");
}

#[test]
fn an_executable_still_in_place_holds_a_restart_however_safe_its_custody() {
    let mut k = KeeperCore::new(true);
    let mut w = World {
        image: ImageLook::Same,
        ..World::default()
    };
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    k.restart_when_safe(1);
    assert_eq!(k.tick(1, &mut w), held(RestartHold::SameImage));
    for now in 2..10 {
        assert!(
            k.tick(now, &mut w).is_empty(),
            "the same build would start again; what holds it is said once"
        );
    }
    w.image = ImageLook::Unknown;
    assert_eq!(
        k.tick(10, &mut w),
        held(RestartHold::ImageUnknown),
        "a path that does not read is never read as replaced"
    );
    assert!(
        k.status_lines()
            .contains(&"restart=wanted askers=1 held_by=image-unknown".to_string())
    );
    w.image = ImageLook::Replaced(EXE.to_string());
    assert_eq!(k.tick(11, &mut w), exit());
}

/// A file at the path that is not a build at least as new as the keeper (a downgrade, or one
/// whose version does not read: [`crate::code::ExeImage::look`]) holds the restart however safe
/// custody is, so launchd never starts an older keeper; a newer file then lets it go.
#[test]
fn an_older_build_at_the_path_holds_a_restart_however_safe_its_custody() {
    let mut k = KeeperCore::new(true);
    let mut w = World {
        image: ImageLook::Older(EXE.to_string()),
        ..World::default()
    };
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    k.restart_when_safe(1);
    assert_eq!(k.tick(1, &mut w), held(RestartHold::OlderImage));
    assert!(k.tick(2, &mut w).is_empty(), "what holds it is said once");
    assert!(
        k.status_lines()
            .contains(&"restart=wanted askers=1 held_by=older-image".to_string())
    );
    w.image = ImageLook::Replaced(EXE.to_string());
    assert_eq!(k.tick(3, &mut w), exit());
}

#[test]
fn an_orphan_holds_a_restart_until_its_shell_ends() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    k.restart_when_safe(1);
    ends(&mut k, &mut w, 2, 200, 9, 5);
    assert_eq!(k.orphan_count(), 1);
    assert_eq!(k.tick(6, &mut w), held(RestartHold::Orphaned(1)));
    assert!(k.tick(7, &mut w).is_empty(), "still held, said once");
    w.dead_shells.push(2002);
    assert!(k.peer_exit(2002, 0, 8, &mut w).contains(&Out::Close(R2)));
    assert_eq!(k.tick(9, &mut w), exit(), "nothing left that only it holds");
}

#[test]
fn an_offer_holds_a_restart_until_its_window_registers_it() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    k.restart_when_safe(1);
    ends(&mut k, &mut w, 2, 200, 9, 5);
    let offered = window(&mut k, &mut w, 3, 300);
    assert!(
        offered
            .iter()
            .any(|o| matches!(o, Out::Offer { rdev: R2, .. }))
    );
    assert_eq!(k.tick(6, &mut w), held(RestartHold::Offered(1)));
    k.register(3, hdr(R2, 2002), Vec::new(), R2);
    assert_eq!(k.tick(7, &mut w), exit());
}

#[test]
fn a_quits_keep_set_holds_a_restart_from_its_bye_until_a_window_claims_it() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    k.restart_when_safe(1);
    k.bye_keeping(2, &[R2]);
    assert_eq!(
        k.tick(1, &mut w),
        held(RestartHold::Kept(1)),
        "its window is still connected, but its quit will leave the shell to the keeper"
    );
    ends(&mut k, &mut w, 2, 200, 0, 2);
    assert!(k.records()[&R2].kept);
    assert!(
        k.tick(3, &mut w).is_empty(),
        "the kept orphan holds it still"
    );
    window(&mut k, &mut w, 3, 300);
    assert_eq!(k.tick(4, &mut w), held(RestartHold::Offered(1)));
    k.register(3, hdr(R2, 2002), Vec::new(), R2);
    assert_eq!(k.tick(5, &mut w), exit());
}

#[test]
fn a_pending_successor_holds_a_restart_until_its_outgoing_window_is_judged() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    assert_eq!(k.pending(1, 500, 1, None), [Out::WatchPid(500)]);
    window(&mut k, &mut w, 2, 500);
    k.restart_when_safe(2);
    assert_eq!(k.tick(2, &mut w), held(RestartHold::Update));
    assert_eq!(
        k.register(2, hdr(R1, 1001), Vec::new(), R1),
        [Out::DropDuplicate(R1)]
    );
    assert!(
        k.tick(3, &mut w).is_empty(),
        "both claim it: the update is under way"
    );
    // The outgoing window's Commit: `exit(0)` with no BYE, its successor holding on.
    ends(&mut k, &mut w, 1, 100, 0, 4);
    assert_eq!(k.records()[&R1].claimants, [2]);
    assert_eq!(k.tick(5, &mut w), exit());
}

#[test]
fn a_crashed_windows_claim_holds_a_restart_until_its_end_is_judged() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    k.restart_when_safe(1);
    w.live_pids.retain(|p| *p != 200);
    k.eof(2, 5, &mut w);
    assert_eq!(k.tick(5, &mut w), held(RestartHold::Unjudged(1)));
    k.peer_exit(200, 9, 6, &mut w);
    assert_eq!(k.tick(6, &mut w), held(RestartHold::Orphaned(1)));
}

#[test]
fn a_want_lapses_with_the_last_window_that_asked_and_never_comes_back_by_itself() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    claim(&mut k, &mut w, 1, 100, R1, 1001);
    claim(&mut k, &mut w, 2, 200, R2, 2002);
    k.restart_when_safe(2);
    ends(&mut k, &mut w, 1, 100, 9, 5);
    assert_eq!(k.tick(6, &mut w), held(RestartHold::Orphaned(1)));
    k.bye(2);
    ends(&mut k, &mut w, 2, 200, 0, 7);
    assert_eq!(k.tick(8, &mut w), [Out::Restart(RestartNote::Lapsed)]);
    assert_eq!(k.restart(), &Restart::Unwanted);
    w.dead_shells.push(1001);
    k.peer_exit(1001, 0, 9, &mut w);
    for now in 10..20 {
        assert!(
            k.tick(now, &mut w).is_empty(),
            "safe now, but nobody stands behind the stale want"
        );
    }
    claim(&mut k, &mut w, 3, 300, R1, 1003);
    k.restart_when_safe(3);
    assert_eq!(
        k.tick(21, &mut w),
        exit(),
        "a later window asks on its own connection"
    );
}

#[test]
fn only_a_connected_window_asks() {
    let mut k = KeeperCore::new(true);
    let mut w = World::default();
    assert!(k.accept(1, 100, None));
    let refused = [Out::Refused {
        conn: 1,
        code: RefuseCode::WrongClass,
        rdev: 0,
    }];
    assert_eq!(k.restart_when_safe(1), refused, "before its HELLO");
    k.hello(1, PeerClass::Status, 0, 0, &mut w);
    assert_eq!(k.restart_when_safe(1), refused, "a status client");
    assert_eq!(k.restart(), &Restart::Unwanted);
    assert!(k.tick(1, &mut w).is_empty());
}

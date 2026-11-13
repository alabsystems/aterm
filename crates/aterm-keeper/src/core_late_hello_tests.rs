// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Alive(Vec<u32>);
impl KeeperEnv for Alive {
    fn holders(&mut self, _: Rdev) -> Holders {
        Holders::None
    }
    fn shell_alive(&mut self, _: u32, _: Birth) -> bool {
        true
    }
    fn pid_alive(&mut self, pid: u32, _: Option<Birth>) -> bool {
        self.0.contains(&pid)
    }
}

fn hdr(rdev: Rdev) -> MasterHeader {
    MasterHeader {
        rdev,
        shell_pid: 900,
        shell_birth: Birth::default(),
        local_id: rdev,
    }
}

#[test]
fn a_slow_booting_crash_loop_is_still_braked() {
    const MIN: u64 = 60_000;
    for (boots_in, lives_for, answering, braked) in [
        (20_000, 5_000, true, true),
        (4 * MIN, 5_000, true, true),
        (5 * MIN, 5_000, false, false),
        (20_000, 25 * MIN, true, true),
        (20_000, 30 * MIN - 20_000, true, false),
    ] {
        let row = format!("HELLO {boots_in} ms after open, crash {lives_for} ms after it");
        let mut k = KeeperCore::new(true);
        let mut w = Alive(vec![500]);
        assert!(k.accept(1, 500, None));
        k.hello(1, PeerClass::App, CAP_SENDS_BYE, 0, &mut w);
        k.register(1, hdr(10), vec![], 10);
        w.0.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        let mut now = 0u64;
        let mut relaunches = 0;
        let conns = 2u64..22;
        let cycles = conns.end - conns.start;
        for conn in conns {
            let mut fired = false;
            for _ in 0..200 {
                if k.tick(now, &mut w).contains(&Out::Relaunch) {
                    fired = true;
                    break;
                }
                now += 1_000;
            }
            if !fired {
                break;
            }
            let opened = now;
            relaunches += 1;
            now += boots_in;
            k.tick(now, &mut w);
            let pid = 600 + u32::try_from(conn).expect("small");
            assert!(k.accept(conn, pid, None));
            w.0.push(pid);
            k.hello(conn, PeerClass::App, CAP_SENDS_BYE, now, &mut w);
            let answered = RelaunchState::InFlight {
                since: opened,
                hello: Some((conn, now)),
            };
            assert_eq!(
                k.relaunch_state(),
                if answering {
                    answered
                } else {
                    RelaunchState::Idle
                },
                "{row}: the HELLO is the relaunch answering: {answering}"
            );
            k.register(conn, hdr(10), vec![], 10);
            now += lives_for;
            w.0.clear();
            k.eof(conn, now, &mut w);
            k.peer_exit(pid, 9, now, &mut w);
        }
        if braked {
            assert!(
                relaunches <= u64::from(MAX_STREAK),
                "{row}: relaunched {relaunches} times: the brake never held"
            );
        } else {
            assert_eq!(relaunches, cycles, "{row}: the brake is reset each time");
        }
        assert_eq!(k.orphan_count(), 1, "{row}: the orphan is still kept");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the ask (`disk-free@v1`): the election, one line an episode across hosts, the
//! hand-off's bounds, the cooldown, the floor an episode is held to, the release of a pick the
//! seat rule bars, the lease file and the line.

use std::path::PathBuf;

use super::*;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:16777234";
const T0: i64 = 1_792_000_000_000;

fn host(pid: u32) -> HostId {
    HostId {
        pid,
        start: format!("start-{pid}"),
    }
}

fn seat(sid: &str, held_gib: u64, at_break: bool) -> Seat {
    Seat {
        sid: sid.to_owned(),
        held: held_gib * GIB,
        at_break,
    }
}

/// The floor [`At::short`] reads the volume short against.
const FLOOR: u64 = 25 * GIB;

/// The inode line every host reads an emergency below.
const FILES_FLOOR: u64 = crate::harness::disk::EMERGENCY_FILES_FREE;

/// One host's look at `now`: `seats` are its candidates, every one of them supervised and live.
struct At<'a> {
    host: &'a HostId,
    level: Level,
    free: u64,
    floor: u64,
    files_free: Option<u64>,
    done: Option<Done>,
    seats: Vec<Seat>,
    supervised: Vec<String>,
    barred: Vec<String>,
    live: Vec<String>,
    held: Vec<String>,
    /// What holds [`Self::held`]: a usage-limit wall unless a test says otherwise.
    hold: HeldBy,
    dead: Vec<u32>,
}

impl<'a> At<'a> {
    fn short(host: &'a HostId, seats: Vec<Seat>) -> At<'a> {
        let live: Vec<String> = seats.iter().map(|s| s.sid.clone()).collect();
        At {
            host,
            level: Level::Short,
            free: 20 * GIB,
            floor: FLOOR,
            files_free: None,
            done: Some(Done::Spent),
            seats,
            supervised: live.clone(),
            barred: Vec::new(),
            live,
            held: Vec::new(),
            hold: HeldBy::Limit,
            dead: Vec::new(),
        }
    }

    /// [`Self::held`], each with [`Self::hold`]: the look's [`Look::held`].
    fn held_by(&self) -> Vec<(String, HeldBy)> {
        self.held.iter().map(|s| (s.clone(), self.hold)).collect()
    }

    /// A look at `free` bytes free, its level judged against its own floor as a host's is.
    fn reads(&mut self, free: u64) -> &mut Self {
        self.free = free;
        self.level = if free < self.floor {
            Level::Short
        } else {
            Level::Ok
        };
        self
    }

    fn step(&self, lease: &mut Lease, now_ms: i64) -> Said {
        let dead = self.dead.clone();
        let alive = move |h: &HostId| !dead.contains(&h.pid);
        let held = self.held_by();
        step(
            lease,
            &Look {
                now_ms,
                host: self.host,
                key: KEY,
                level: self.level,
                free: self.free,
                floor: self.floor,
                files_free: self.files_free,
                files_floor: FILES_FLOOR,
                done: self.done,
                seats: &self.seats,
                supervised: &self.supervised,
                barred: &self.barred,
                live: &self.live,
                held: &held,
                alive: &alive,
            },
        )
    }
}

impl At<'_> {
    /// This host's take-over before its look's pass ([`take_over`]), over the same roster.
    fn take_over(&self, lease: &mut Lease) -> Vec<String> {
        let dead = self.dead.clone();
        let alive = move |h: &HostId| !dead.contains(&h.pid);
        let held = self.held_by();
        take_over(
            lease,
            &Roster {
                host: self.host,
                key: KEY,
                supervised: &self.supervised,
                barred: &self.barred,
                live: &self.live,
                held: &held,
                alive: &alive,
            },
        )
    }
}

fn phase(lease: &Lease) -> &Phase {
    &lease.entry(KEY).expect("the volume's entry").phase
}

/// Opens an episode with host 1's seats and host 2's, and elects at the round's end.
fn elected(lease: &mut Lease, one: &At<'_>, two: &At<'_>) -> Said {
    assert!(matches!(one.step(lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    one.step(lease, T0 + NOMINATE_MS)
}

#[test]
fn rank_puts_whole_gib_held_first_then_a_turn_break_then_bytes_then_the_lower_sid() {
    assert_eq!(
        rank((40 * GIB, false, "s-b"), (3 * GIB, true, "s-a")),
        Ordering::Greater,
        "the checkout holding more wins over one at a turn break"
    );
    assert_eq!(
        rank((3 * GIB + 5, false, "s-a"), (3 * GIB, true, "s-b")),
        Ordering::Less,
        "within the same GiB a turn break wins"
    );
    assert_eq!(
        rank((3 * GIB + 5, true, "s-b"), (3 * GIB, true, "s-a")),
        Ordering::Greater
    );
    assert_eq!(
        rank((0, true, "s-a"), (0, true, "s-b")),
        Ordering::Greater,
        "the lower sid breaks a tie, so every host elects the same one"
    );
}

#[test]
fn nothing_opens_while_aterm_may_still_free_space_or_above_the_floor() {
    let h = host(1);
    let mut lease = Lease::default();
    let mut at = At::short(&h, vec![seat("s-a", 5, true)]);
    at.done = None;
    assert_eq!(at.step(&mut lease, T0), Said::Freeing);
    at.level = Level::Low;
    at.done = Some(Done::Spent);
    assert_eq!(
        at.step(&mut lease, T0),
        Said::Quiet,
        "low is not below the floor: aterm still holds its deeper tiers"
    );
    at.level = Level::Unknown;
    assert_eq!(at.step(&mut lease, T0), Said::Quiet);
    assert!(
        lease
            .entry(KEY)
            .is_none_or(|e| e.episode_ms.is_none() && e.phase == Phase::Idle)
    );
}

#[test]
fn one_session_machine_wide_is_elected_from_every_hosts_nominee() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 2, true), seat("s-b", 9, false)]);
    let two = At::short(&h2, vec![seat("s-c", 30, false), seat("s-d", 1, true)]);
    let said = elected(&mut lease, &one, &two);
    assert_eq!(
        said,
        Said::Elected {
            pick: Pick {
                host: h2.clone(),
                sid: "s-c".to_owned()
            },
            held: 30 * GIB
        }
    );
    assert!(said.moves());
    assert_eq!(
        two.step(&mut lease, T0 + NOMINATE_MS + 1),
        Said::Owed {
            pick: Pick {
                host: h2.clone(),
                sid: "s-c".to_owned()
            },
            until_ms: T0 + NOMINATE_MS + DELIVER_MS
        }
    );
    assert_eq!(
        claim(&mut lease, KEY, &h1, "s-b", T0 + NOMINATE_MS + 2),
        Claim::Gone,
        "host 1 never types: its sessions were not elected"
    );
    assert_eq!(
        claim(&mut lease, KEY, &h2, "s-d", T0 + NOMINATE_MS + 2),
        Claim::Gone,
        "nor does host 2 type for its other session"
    );
    assert_eq!(
        claim(&mut lease, KEY, &h2, "s-c", T0 + NOMINATE_MS + 2),
        Claim::Yours
    );
}

/// A nominee whose host a look reads ended stands [`ADOPT_MS`] from that first read: a seamless
/// update's successor, which runs the session under a new host, looks within it and nominates
/// the session again, and the round elects it there; with no successor, the next best is elected
/// once that time is over. A look that dropped the ended host's nominee at once
/// (`Fault::DropEndedNominee`) elected a weaker seat, or ran out with nobody, before the
/// successor's first look.
#[test]
fn an_ended_hosts_nominee_stands_for_its_successor_then_gives_way() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 2, true)]);
    let two = At::short(&h2, vec![seat("s-c", 30, false)]);
    one.dead = vec![2];
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    let round = lease.clone();
    let at = T0 + NOMINATE_MS;
    let waits = Said::Nominating {
        until_ms: at + ADOPT_MS,
        seat: Some("s-a".to_owned()),
    };
    assert_eq!(
        one.step(&mut lease, at),
        waits,
        "s-c outranks s-a, and its host's successor may yet nominate it"
    );
    assert_eq!(
        one.step(&mut lease, at + ADOPT_MS - 1),
        waits,
        "the wait runs from the look that first read its host ended"
    );
    let said = one.step(&mut lease.clone(), at + ADOPT_MS);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "no successor looked: the next best: {said:?}"
    );
    // A successor runs s-c now, and its first look nominates it again: elected there.
    let mut three = At::short(&h3, vec![seat("s-c", 30, false)]);
    three.dead = vec![2];
    assert_eq!(
        three.step(&mut lease, at + ADOPT_MS - 1),
        Said::Elected {
            pick: Pick {
                host: h3.clone(),
                sid: "s-c".to_owned()
            },
            held: 30 * GIB
        }
    );
    let dropped = fault::with(fault::Fault::DropEndedNominee, || {
        one.step(&mut round.clone(), at)
    });
    assert!(
        matches!(&dropped, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "the defect: the weaker seat, before the successor looked: {dropped:?}"
    );
    // A look of a host with no seat of its own waits too, where it ran out with nobody.
    let mut lease = Lease::default();
    let mut none = At::short(&h1, Vec::new());
    none.dead = vec![2];
    assert!(matches!(none.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    assert_eq!(
        none.step(&mut lease, at),
        Said::Nominating {
            until_ms: at + ADOPT_MS,
            seat: None
        }
    );
}

/// A session held where it cannot take a line now (`Look::held`: an unspent usage-limit wall, a
/// box only its owner can answer) is not among its host's seats, so it is never nominated, however
/// much its checkout holds; and an elected one that comes to such a wall before its turn break
/// goes back to the episode at its host's next look, at no election's cost. Kept owed
/// (`Fault::KeepHeld`), it waited out its delivery window and was handed off as undelivered,
/// spending an election with no line typed.
#[test]
fn a_pick_its_host_holds_at_a_wall_goes_back_to_the_episode_at_no_elections_cost() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-b", 9, true)]);
    one.held = vec!["s-a".to_owned()];
    one.live = vec!["s-a".to_owned(), "s-b".to_owned()];
    one.supervised = one.live.clone();
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    let said = elected(&mut lease, &one, &two);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-b"),
        "{said:?}"
    );
    one.seats.clear();
    one.held = vec!["s-a".to_owned(), "s-b".to_owned()];
    let t = T0 + NOMINATE_MS + 60_000;
    let kept = fault::with(fault::Fault::KeepHeld, || one.step(&mut lease.clone(), t));
    assert!(
        matches!(&kept, Said::Owed { pick, .. } if pick.sid == "s-b"),
        "the defect: owed on: {kept:?}"
    );
    let back = one.step(&mut lease, t);
    assert_eq!(
        back,
        Said::Returned {
            from: Pick {
                host: h1.clone(),
                sid: "s-b".to_owned()
            },
            why: HeldBy::Limit
        }
    );
    assert!(back.moves(), "the host that moved it journals it");
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.picks.len(), e.released, e.asks, e.returned.as_slice()),
        (0, 0, 0, ["s-b".to_owned()].as_slice()),
        "out of the picks, so it costs no election"
    );
    assert!(matches!(
        two.step(&mut lease, t + 60_000),
        Said::Nominating { .. }
    ));
    let said = one.step(&mut lease, t + NOMINATE_MS);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-c"),
        "{said:?}"
    );
}

/// A session alone on the machine is elected mid-turn and comes to a usage-limit wall before its
/// turn break: its host's next look sends it back to the episode, and the round, with nobody
/// else to elect, waits for it (`said: waiting`) and elects it again at the first look after it
/// is past, so its line goes once it rests. A session goes back once an episode: held again
/// after that second election, it is released and stays a pick, so the next round, with nobody
/// left, runs out. A release that kept it a pick at its first wall (`Fault::HoldKeepsPick`, the
/// release before held picks went back) ran the round out there with no line typed, and the
/// episode parked for its cooldown though the session was past twenty minutes later.
#[test]
fn a_pick_held_after_its_election_is_waited_for_and_elected_again_once_past() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 30, false)]);
    let two = At::short(&h2, Vec::new());
    let said = elected(&mut lease, &one, &two);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "{said:?}"
    );
    one.seats.clear();
    one.held = vec!["s-a".to_owned()];
    let t = T0 + NOMINATE_MS + 60_000;
    let walled = lease.clone();
    assert!(matches!(one.step(&mut lease, t), Said::Returned { .. }));
    let round = t + NOMINATE_MS;
    let mut kept = walled.clone();
    let out = fault::with(fault::Fault::HoldKeepsPick, || {
        let released = one.step(&mut kept, t);
        assert!(
            matches!(
                released,
                Said::Handoff {
                    why: Miss::Released,
                    ..
                }
            ),
            "{released:?}"
        );
        one.step(&mut kept, round)
    });
    assert_eq!(
        out,
        Said::Out {
            last: Some(Miss::Released),
            asked: false
        },
        "the defect: out at its first wall"
    );
    let waiting = Said::Waiting {
        held: vec!["s-a".to_owned()],
        why: vec![HeldBy::Limit],
    };
    assert_eq!(two.step(&mut lease, round), waiting);
    assert_eq!(one.step(&mut lease, round + 20 * 60_000), waiting);
    let e = lease.entry(KEY).expect("entry").clone();
    assert!(
        owner_line(&e, round + 20 * 60_000).is_some_and(|l| l.contains("usage limit")),
        "{:?}",
        owner_line(&e, round + 20 * 60_000)
    );
    assert!(
        why_lines(&lease, &[], round)
            .iter()
            .any(|l| l.contains("back in the episode") && l.contains("@s-a")),
        "{:#?}",
        why_lines(&lease, &[], round)
    );
    one.held.clear();
    one.seats = vec![seat("s-a", 30, false)];
    let past = round + 21 * 60_000;
    let said = one.step(&mut lease, past);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "past its wall, elected again: {said:?}"
    );
    let mut typed = lease.clone();
    assert_eq!(
        at_break(&mut typed, KEY, &h1, "s-a", past + 60_000, false),
        Break::Yours,
        "its line goes at its turn break"
    );
    // Held again after that second election: released, and it stays a pick.
    one.seats.clear();
    one.held = vec!["s-a".to_owned()];
    assert_eq!(
        one.step(&mut lease, past + 60_000),
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Released
        }
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.picks.len(), e.released, e.returned.len()),
        (1, 1, 1),
        "released at no election's cost, and back no more"
    );
    assert_eq!(
        two.step(&mut lease, past + 60_000 + NOMINATE_MS),
        Said::Out {
            last: Some(Miss::Released),
            asked: false
        },
        "nobody left to elect or wait for"
    );
}

/// The lease's record of every host's held sessions is kept only while an episode is open: it
/// goes with the episode, and the next one starts from the opening look's own. A record kept past
/// its episode was never replaced while the volume read clear (no look steps it then), so
/// `harness disk why` said, days on, that a session long past its wall was at it, and the next
/// episode's first owner line said every session was at a usage limit before the host whose
/// record it was looked again.
#[test]
fn the_record_of_held_sessions_goes_with_its_episode() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 3, true)]);
    let mut two = At::short(&h2, Vec::new());
    two.held = vec!["s-c".to_owned()];
    two.live = two.held.clone();
    two.supervised = two.held.clone();
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    assert_eq!(lease.entry(KEY).map(|e| e.held.len()), Some(1));
    let mut clear = At::short(&h1, Vec::new());
    clear.reads(FLOOR + END_MARGIN);
    assert!(matches!(
        clear.step(&mut lease, T0 + 120_000),
        Said::Ended { .. }
    ));
    let e = lease.entry(KEY).expect("entry").clone();
    assert!(e.held.is_empty(), "{:?}", e.held);
    assert!(
        !why_lines(&lease, &[], T0 + 3_600_000)
            .iter()
            .any(|l| l.contains("usage limit")),
        "{:#?}",
        why_lines(&lease, &[], T0 + 3_600_000)
    );
    // A day on the volume falls short again; host 1 opens with no seat of its own, before host 2
    // (whose session is long past its wall) looks.
    let none = At::short(&h1, Vec::new());
    let later = T0 + 86_400_000;
    assert!(matches!(none.step(&mut lease, later), Said::Opened { .. }));
    let e = lease.entry(KEY).expect("entry").clone();
    assert!(e.held.is_empty(), "{:?}", e.held);
    assert_eq!(
        owner_line(&e, later + NOMINATE_MS),
        None,
        "no stale wall told to the owner"
    );
    // A look of a host whose own session is held at the opening records it.
    let mut lease = Lease::default();
    let mut walled = At::short(&h1, Vec::new());
    walled.held = vec!["s-a".to_owned()];
    walled.live = walled.held.clone();
    walled.supervised = walled.held.clone();
    assert!(matches!(walled.step(&mut lease, T0), Said::Opened { .. }));
    assert_eq!(
        lease.entry(KEY).map(|e| e.held.clone()),
        Some(vec![Held {
            host: h1.clone(),
            sid: "s-a".to_owned(),
            why: Some(HeldBy::Limit),
            orphan_ms: None
        }])
    );
}

/// When the round's time is over and every session not yet elected is held at a wall or a box,
/// the round waits for one to be past, and the owner's line says so; the first look after one is
/// past elects it. A round that ran out on them (`Fault::OutWhileHeld`) parked the episode for
/// its cooldown while a session that could free space was a reset away.
#[test]
fn a_round_whose_sessions_are_all_held_waits_and_elects_the_first_past() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, Vec::new());
    one.held = vec!["s-a".to_owned()];
    one.live = one.held.clone();
    one.supervised = one.held.clone();
    let mut two = At::short(&h2, Vec::new());
    two.held = vec!["s-c".to_owned()];
    two.live = two.held.clone();
    two.supervised = two.held.clone();
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    let at = T0 + NOMINATE_MS;
    let out = fault::with(fault::Fault::OutWhileHeld, || {
        one.step(&mut lease.clone(), at)
    });
    assert!(matches!(out, Said::Out { .. }), "the defect: {out:?}");
    let waiting = Said::Waiting {
        held: vec!["s-a".to_owned(), "s-c".to_owned()],
        why: vec![HeldBy::Limit],
    };
    assert_eq!(one.step(&mut lease, at), waiting);
    assert!(!waiting.moves(), "a standing word");
    assert_eq!(two.step(&mut lease, at + 3_600_000), waiting);
    let e = lease.entry(KEY).expect("entry").clone();
    assert!(
        owner_line(&e, at + 3_600_000).is_some_and(|l| l.contains("usage limit")),
        "{:?}",
        owner_line(&e, at + 3_600_000)
    );
    assert!(
        why_lines(&lease, &[], at).iter().any(|l| l
            .contains("@s-a (host pid 1) at a usage limit, @s-c (host pid 2) at a usage limit")),
        "{:#?}",
        why_lines(&lease, &[], at)
    );
    assert_eq!(
        Lease::decode(&lease.encode()),
        Lease {
            schema: 1,
            ..lease.clone()
        },
        "the held sessions round-trip the lease file"
    );
    two.held.clear();
    two.seats = vec![seat("s-c", 4, true)];
    let said = two.step(&mut lease, at + 3_660_000);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-c"),
        "past its wall, elected: {said:?}"
    );
    assert_eq!(
        lease.entry(KEY).map(|e| e.held.len()),
        Some(1),
        "s-a is held still"
    );
}

#[test]
fn the_line_goes_once_then_free_space_rising_settles_the_episode_and_rising_past_the_floor_ends_it()
{
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let t = T0 + NOMINATE_MS + 10_000;
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", t), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    assert_eq!(
        claim(&mut lease, KEY, &h1, "s-a", t + 1),
        Claim::Gone,
        "its line went: it is not typed again"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!((e.asks, e.last_ask_ms), (1, Some(t)));
    one.free = 20 * GIB + GIB / 2;
    assert!(matches!(
        one.step(&mut lease, t + 60_000),
        Said::Asked { .. }
    ));
    one.free = 20 * GIB + GIB / 4;
    assert!(
        matches!(one.step(&mut lease, t + 120_000), Said::Asked { rose, .. } if rose == GIB / 2),
        "the best figure since the line counts, not the latest"
    );
    one.free = 22 * GIB;
    assert_eq!(
        one.step(&mut lease, t + 180_000),
        Said::Freed {
            sid: "s-a".to_owned(),
            freed: 2 * GIB
        }
    );
    assert_eq!(two.step(&mut lease, t + 190_000), Said::Settled);
    assert_eq!(one.step(&mut lease, t + 240_000), Said::Settled);
    one.reads(FLOOR + END_MARGIN - 1);
    assert_eq!(
        one.step(&mut lease, t + 270_000),
        Said::Settled,
        "back at its floor, but not past it by the margin"
    );
    one.reads(FLOOR + END_MARGIN);
    assert_eq!(one.step(&mut lease, t + 300_000), Said::Ended { asks: 1 });
    let e = lease.entry(KEY).expect("entry kept for its cooldown");
    assert_eq!(
        (e.episode_ms, &e.phase, e.last_ask_ms),
        (None, &Phase::Idle, Some(t))
    );
    assert_eq!(
        two.step(&mut lease, t + 360_000),
        Said::Cooldown { last_ms: t }
    );
    assert!(
        matches!(
            two.step(&mut lease, t + COOLDOWN_MS),
            Said::Opened { done: Done::Spent }
        ),
        "the cooldown is over"
    );
}

/// What each look of `looks` said, in order: host `h` at `free` bytes free, a minute apart.
fn said_over(lease: &mut Lease, hosts: &mut [At<'_>], looks: &[(usize, u64)]) -> Vec<Said> {
    looks
        .iter()
        .zip(0i64..)
        .map(|(&(h, free), i)| hosts[h].reads(free).step(lease, T0 + i * 60_000))
        .collect()
}

fn count(said: &[Said], word: &str) -> usize {
    said.iter().filter(|s| s.word() == word).count()
}

#[test]
fn two_hosts_whose_floors_differ_hold_one_episode_open_until_an_election() {
    let (h1, h2) = (host(1), host(2));
    let mut high = At::short(&h1, vec![seat("s-a", 2, true)]);
    high.floor = 48 * GIB;
    let mut low = At::short(&h2, vec![seat("s-c", 30, true)]);
    low.floor = 32 * GIB;
    let mut hosts = [high, low];
    // 40 GiB free: short against the first host's floor, not against the second's.
    let looks: Vec<(usize, u64)> = (0..10).map(|i| (i % 2, 40 * GIB)).collect();
    let mut lease = Lease::default();
    let said = said_over(&mut lease, &mut hosts, &looks);
    assert_eq!(count(&said, "opened"), 1, "{said:#?}");
    assert_eq!(count(&said, "ended"), 0, "{said:#?}");
    assert!(
        said.iter().any(|s| matches!(s,
            Said::Elected { pick, .. } if pick.sid == "s-c" && pick.host == h2)),
        "the host that does not read it short still nominates, and its session is elected: \
         {said:#?}"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        e.floor,
        Some(Floor {
            host: h1.clone(),
            bytes: 48 * GIB
        })
    );
    assert!(matches!(&e.phase, Phase::Owed { pick, .. } if pick.sid == "s-c"));
    let late = T0 + 10 * 60_000;
    assert!(
        matches!(
            hosts[1]
                .reads(48 * GIB + END_MARGIN - 1)
                .step(&mut lease, late),
            Said::Owed { .. }
        ),
        "past both floors, but within the margin of the one it is held to"
    );
    assert_eq!(
        hosts[1]
            .reads(48 * GIB + END_MARGIN)
            .step(&mut lease, late + 60_000),
        Said::Ended { asks: 0 },
        "any host ends it once free space clears the floor it is held to by the margin"
    );
    // The defect: each host held the episode to its own floor, so the second ended what the
    // first opened at every look, inside the round, and nobody was ever elected.
    let mut flapping = Lease::default();
    let said = fault::with(fault::Fault::EndAtOwnFloor, || {
        said_over(&mut flapping, &mut hosts, &looks)
    });
    assert_eq!(count(&said, "opened"), 5, "{said:#?}");
    assert_eq!(count(&said, "ended"), 5, "{said:#?}");
    assert_eq!(count(&said, "elected"), 0, "{said:#?}");
}

#[test]
fn a_lower_floors_episode_is_raised_to_a_higher_one_that_reads_it_short() {
    let (h1, h2) = (host(1), host(2));
    let mut high = At::short(&h1, vec![seat("s-a", 2, true)]);
    high.floor = 48 * GIB;
    let mut low = At::short(&h2, vec![seat("s-c", 30, true)]);
    low.floor = 32 * GIB;
    let mut hosts = [high, low];
    let mut lease = Lease::default();
    let said = said_over(
        &mut lease,
        &mut hosts,
        &[(1, 30 * GIB), (0, 30 * GIB), (1, 40 * GIB), (1, 40 * GIB)],
    );
    assert!(matches!(said[0], Said::Opened { .. }), "{said:#?}");
    assert_eq!(count(&said, "ended"), 0, "{said:#?}");
    assert_eq!(
        lease.entry(KEY).and_then(|e| e.floor.clone()),
        Some(Floor {
            host: h1.clone(),
            bytes: 48 * GIB
        }),
        "the host that read it short against a higher floor holds it there"
    );
    hosts[1].dead = vec![1];
    assert_eq!(
        hosts[1].reads(40 * GIB).step(&mut lease, T0 + 600_000),
        Said::Ended { asks: 0 },
        "a floor whose host ended holds nothing: the looking host's own does"
    );
}

#[test]
fn free_space_hovering_a_gib_either_side_of_the_floor_is_one_episode() {
    let h1 = host(1);
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    one.floor = 32 * GIB;
    let mut hosts = [one];
    let looks: Vec<(usize, u64)> = (0..12)
        .map(|i| (0, if i % 2 == 0 { 31 * GIB } else { 33 * GIB }))
        .collect();
    let mut lease = Lease::default();
    let said = said_over(&mut lease, &mut hosts, &looks);
    assert_eq!(count(&said, "opened"), 1, "{said:#?}");
    assert_eq!(count(&said, "ended"), 0, "{said:#?}");
    assert_eq!(count(&said, "elected"), 1, "{said:#?}");
    let mut flapping = Lease::default();
    let said = fault::with(fault::Fault::EndAtOwnFloor, || {
        said_over(&mut flapping, &mut hosts, &looks)
    });
    assert_eq!(
        (count(&said, "opened"), count(&said, "ended")),
        (6, 6),
        "the defect: with no margin every look above the floor ended it: {said:#?}"
    );
}

#[test]
fn free_inodes_hovering_at_their_line_with_bytes_to_spare_is_one_episode() {
    let h1 = host(1);
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    one.floor = 32 * GIB;
    one.free = 200 * GIB;
    // A build creates and removes small files: free inodes read either side of their line at
    // each look, with 200 GiB free.
    let over = |one: &mut At<'_>, lease: &mut Lease| -> Vec<Said> {
        (0..12i64)
            .map(|i| {
                let short = i % 2 == 0;
                one.files_free = Some(if short {
                    FILES_FLOOR - 10
                } else {
                    FILES_FLOOR + 10
                });
                one.level = if short { Level::Emergency } else { Level::Ok };
                one.done = short.then_some(Done::Inodes);
                one.step(lease, T0 + i * 60_000)
            })
            .collect()
    };
    let mut lease = Lease::default();
    let said = over(&mut one, &mut lease);
    assert_eq!(count(&said, "opened"), 1, "{said:#?}");
    assert_eq!(count(&said, "ended"), 0, "{said:#?}");
    assert_eq!(count(&said, "elected"), 1, "{said:#?}");
    let said = fault::with(fault::Fault::EndAtHeldFloor, || {
        over(&mut one, &mut Lease::default())
    });
    assert_eq!(
        (
            count(&said, "opened"),
            count(&said, "ended"),
            count(&said, "elected")
        ),
        (6, 6, 0),
        "the defect: with no inode margin every look past the line ended it: {said:#?}"
    );
    one.files_free = Some(FILES_FLOOR + END_FILES_MARGIN - 1);
    assert!(
        !matches!(one.step(&mut lease, T0 + 13 * 60_000), Said::Ended { .. }),
        "past the line, but within the margin of it"
    );
    one.files_free = Some(FILES_FLOOR + END_FILES_MARGIN);
    assert_eq!(
        one.step(&mut lease, T0 + 14 * 60_000),
        Said::Ended { asks: 0 }
    );
}

#[test]
fn an_episode_parked_out_or_settled_gives_way_to_a_new_one_after_the_cooldown() {
    let h1 = host(1);
    let mut one = At::short(&h1, Vec::new());
    one.floor = 32 * GIB;
    let mut lease = Lease::default();
    // Nobody to ask at 31 GiB: the episode runs out.
    assert!(matches!(
        one.reads(31 * GIB).step(&mut lease, T0),
        Said::Opened { .. }
    ));
    assert_eq!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Out {
            last: None,
            asked: false
        }
    );
    // Free space then sits within the margin above the floor for days: ordinary churn.
    let mut now = T0 + NOMINATE_MS;
    for gib in [33, 36, 34, 35, 33, 36] {
        now += 12 * 3_600_000;
        assert_eq!(one.reads(gib * GIB).step(&mut lease, now), Said::Spent);
    }
    // Later builds take it down to an emergency, while sessions sit there to ask.
    one.seats = vec![seat("s-a", 12, true), seat("s-b", 2, true)];
    one.supervised = vec!["s-a".to_owned(), "s-b".to_owned()];
    one.live = one.supervised.clone();
    let parked = lease.clone();
    one.reads(3 * GIB).level = Level::Emergency;
    now += 60_000;
    let kept = fault::with(fault::Fault::ParkForever, || {
        one.step(&mut parked.clone(), now)
    });
    assert_eq!(
        kept,
        Said::Spent,
        "the defect: an out episode stayed parked, and nobody was asked again"
    );
    assert_eq!(
        one.step(&mut lease, now),
        Said::Opened { done: Done::Spent },
        "the parked episode gives way to a new one"
    );
    assert_eq!(
        lease.entry(KEY).and_then(|e| e.floor.clone()),
        Some(Floor {
            host: h1.clone(),
            bytes: 32 * GIB
        })
    );
    let said = one.step(&mut lease, now + NOMINATE_MS);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "{said:?}"
    );
    // Settled: the asked session frees space, free stays within the margin above the floor,
    // then drops below it again.
    let t = now + NOMINATE_MS + 1_000;
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 3 * GIB));
    one.reads(31 * GIB);
    assert!(matches!(
        one.step(&mut lease, t + 60_000),
        Said::Freed { .. }
    ));
    one.reads(32 * GIB + GIB / 2);
    assert_eq!(one.step(&mut lease, t + 120_000), Said::Settled);
    one.reads(31 * GIB);
    assert_eq!(
        one.step(&mut lease, t + COOLDOWN_MS - 1),
        Said::Settled,
        "within the cooldown of its line it stays settled"
    );
    let said = one.step(&mut lease, t + 60_000 + COOLDOWN_MS);
    assert_eq!(said, Said::Opened { done: Done::Spent });
    let e = lease.entry(KEY).expect("entry");
    assert!(
        e.picks.is_empty() && e.asks == 0 && e.last_ask_ms == Some(t),
        "a new episode, its cooldown kept: {e:?}"
    );
    // aterm's own passes may free again: the parked episode waits for them.
    let mut lease = Lease::default();
    one.seats.clear();
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert_eq!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Out {
            last: None,
            asked: false
        }
    );
    one.done = None;
    assert_eq!(
        one.step(&mut lease, T0 + NOMINATE_MS + COOLDOWN_MS),
        Said::Spent
    );
}

#[test]
fn a_look_that_steps_an_episode_records_the_floor_it_holds_it_to_short_or_not() {
    let (h1, h2) = (host(1), host(2));
    let mut high = At::short(&h1, vec![seat("s-a", 30, true)]);
    high.floor = 48 * GIB;
    let mut low = At::short(&h2, vec![seat("s-c", 2, true)]);
    low.floor = 32 * GIB;
    let floor = |l: &Lease| l.entry(KEY).and_then(|e| e.floor.clone());
    let held = |host: &HostId, gib: u64| {
        Some(Floor {
            host: host.clone(),
            bytes: gib * GIB,
        })
    };
    let mut lease = Lease::default();
    assert!(matches!(
        low.reads(30 * GIB).step(&mut lease, T0),
        Said::Opened { .. }
    ));
    assert_eq!(floor(&lease), held(&h2, 32));
    // The lower floor's host ends before the higher one reads the volume short: free space is
    // past the higher floor, but within its margin, at every look of the higher one.
    high.dead = vec![2];
    assert!(matches!(
        high.reads(50 * GIB).step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    assert_eq!(
        floor(&lease),
        held(&h1, 48),
        "held to the looking host's floor, so the record says so"
    );
    let said = high.step(&mut lease, T0 + NOMINATE_MS);
    assert!(matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"));
    // While the lower one lives, the higher one's look not short raises the record too.
    let mut lease = Lease::default();
    high.dead.clear();
    assert!(matches!(
        low.reads(30 * GIB).step(&mut lease, T0),
        Said::Opened { .. }
    ));
    assert!(matches!(
        high.reads(50 * GIB).step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    assert_eq!(floor(&lease), held(&h1, 48));
    assert!(matches!(
        low.reads(50 * GIB).step(&mut lease, T0 + 120_000),
        Said::Nominating { .. }
    ));
    assert_eq!(
        floor(&lease),
        held(&h1, 48),
        "a lower floor's look keeps a live host's higher one"
    );
}

#[test]
fn a_lease_written_before_floors_were_recorded_holds_its_episode_to_the_looking_hosts_floor() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    let mut text: Value = aterm_json::from_str(&lease.encode()).expect("json");
    let entry = text
        .as_object_mut()
        .and_then(|o| o.get_mut("volumes"))
        .and_then(Value::as_array_mut)
        .and_then(|v| v.get_mut(0))
        .and_then(Value::as_object_mut)
        .expect("an entry");
    assert!(entry.remove("floor").is_some(), "the lease records it now");
    let mut old = Lease::decode(&aterm_json::to_string(&text).expect("json"));
    assert_eq!(old.entry(KEY).map(|e| e.floor.clone()), Some(None));
    assert!(
        matches!(
            old.entry(KEY).map(|e| &e.phase),
            Some(Phase::Nominating { .. })
        ),
        "an old lease is read, not dropped"
    );
    assert!(matches!(
        one.reads(FLOOR + END_MARGIN - 1)
            .step(&mut old, T0 + 60_000),
        Said::Nominating { .. }
    ));
    assert_eq!(
        one.reads(FLOOR + END_MARGIN).step(&mut old, T0 + 120_000),
        Said::Ended { asks: 0 }
    );
}

#[test]
fn an_elected_session_whose_task_is_done_before_its_turn_break_is_released_at_the_next_look() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    let mut two = At::short(&h2, vec![seat("s-c", 3, true)]);
    let said = elected(&mut lease, &one, &two);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "{said:?}"
    );
    // Its task turns done: the seat rule drops its seat and bars it.
    one.seats.retain(|s| s.sid != "s-a");
    one.barred = vec!["s-a".to_owned()];
    let at = T0 + NOMINATE_MS + 60_000;
    let kept = fault::with(fault::Fault::KeepBarred, || {
        let mut l = lease.clone();
        (
            one.step(&mut l, at),
            one.step(&mut l, T0 + NOMINATE_MS + DELIVER_MS),
        )
    });
    assert!(
        matches!(
            kept,
            (
                Said::Owed { .. },
                Said::Handoff {
                    why: Miss::Undelivered,
                    ..
                }
            )
        ),
        "the defect: it stayed owed for the delivery window and then cost an election: {kept:?}"
    );
    two.barred = vec!["s-a".to_owned()];
    assert!(
        matches!(two.step(&mut lease, at - 1), Said::Owed { .. }),
        "another host does not release this host's pick"
    );
    assert_eq!(
        one.step(&mut lease, at),
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Released
        }
    );
    let e = lease.entry(KEY).expect("entry").clone();
    assert_eq!(
        (e.picks.len(), e.released, e.asks),
        (1, 1, 0),
        "no election spent"
    );
    two.step(&mut lease, at + 1_000);
    let said = one.step(&mut lease, at + NOMINATE_MS);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-c"),
        "the next best session is elected: {said:?}"
    );
}

#[test]
fn an_elected_session_whose_agent_exited_is_released_though_it_left_supervision() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected { pick, .. } if pick.sid == "s-a"
    ));
    // Its Claude Code exits with no relaunch: the tab lives on, its task record says exited,
    // and it is no longer on the supervised roster.
    one.seats.retain(|s| s.sid != "s-a");
    one.supervised.retain(|s| s != "s-a");
    one.barred = vec!["s-a".to_owned()];
    let at = T0 + NOMINATE_MS + 60_000;
    let gone_first = fault::with(fault::Fault::GoneFirst, || one.step(&mut lease.clone(), at));
    assert!(
        matches!(
            gone_first,
            Said::Handoff {
                why: Miss::Undelivered,
                ..
            }
        ),
        "the defect: an exited pick cost an election: {gone_first:?}"
    );
    assert!(matches!(
        one.step(&mut lease, at),
        Said::Handoff {
            why: Miss::Released,
            ..
        }
    ));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!((e.picks.len(), e.released), (1, 1), "no election spent");
    // A pick that left supervision with nothing barring it (its Autopilot turned Off) is still
    // handed off as undelivered.
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    one.step(&mut lease, T0);
    one.step(&mut lease, T0 + NOMINATE_MS);
    one.supervised.retain(|s| s != "s-a");
    assert!(matches!(
        one.step(&mut lease, at),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
}

#[test]
fn a_turn_break_releases_a_line_the_seat_rule_bars_and_claims_one_it_admits() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected { .. }
    ));
    let at = T0 + NOMINATE_MS + 1_000;
    let blind = fault::with(fault::Fault::TypeBarred, || {
        at_break(&mut lease.clone(), KEY, &h1, "s-a", at, true)
    });
    assert_eq!(
        blind,
        Break::Yours,
        "the defect: a barred session was claimed"
    );
    assert_eq!(
        at_break(&mut lease.clone(), KEY, &h1, "s-a", at, false),
        Break::Yours
    );
    assert_eq!(
        at_break(&mut lease.clone(), KEY, &h1, "s-b", at, false),
        Break::Gone
    );
    assert!(matches!(
        at_break(&mut lease, KEY, &h1, "s-a", at, true),
        Break::Released(Said::Handoff {
            why: Miss::Released,
            ..
        })
    ));
    assert_eq!(
        at_break(&mut lease, KEY, &h1, "s-a", at + 1, true),
        Break::Gone,
        "released once"
    );
}

#[test]
fn a_session_that_frees_nothing_hands_off_to_one_other_then_the_episode_is_out() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 1, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let t = T0 + NOMINATE_MS + 10_000;
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", t), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    assert!(matches!(
        two.step(&mut lease, t + ANSWER_MS - 1),
        Said::Asked { .. }
    ));
    assert_eq!(
        two.step(&mut lease, t + ANSWER_MS),
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::NothingFreed
        }
    );
    let t2 = t + ANSWER_MS;
    one.step(&mut lease, t2 + 1_000);
    two.step(&mut lease, t2 + 2_000);
    let said = one.step(&mut lease, t2 + NOMINATE_MS);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-c"),
        "s-a was picked this episode, so the next best is: {said:?}"
    );
    assert_eq!(
        claim(&mut lease, KEY, &h2, "s-c", t2 + NOMINATE_MS + 1),
        Claim::Yours
    );
    assert!(typed(
        &mut lease,
        KEY,
        &h2,
        "s-c",
        t2 + NOMINATE_MS + 1,
        20 * GIB
    ));
    let t3 = t2 + NOMINATE_MS + 1 + ANSWER_MS;
    assert_eq!(
        one.step(&mut lease, t3),
        Said::Out {
            last: Some(Miss::NothingFreed),
            asked: true
        },
        "two lines this episode: no third session is asked"
    );
    assert_eq!(two.step(&mut lease, t3 + 60_000), Said::Spent);
    assert_eq!(lease.entry(KEY).expect("entry").asks, MAX_ASKS);
}

#[test]
fn a_pick_that_never_takes_its_line_costs_an_election_and_the_elections_are_bounded() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let seats: Vec<Seat> = (0..6)
        .map(|i| seat(&format!("s-{i}"), 10 - i, false))
        .collect();
    let one = At::short(&h1, seats);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    let mut now = T0 + NOMINATE_MS;
    let mut elected_sids = Vec::new();
    loop {
        match one.step(&mut lease, now) {
            Said::Elected { pick, .. } => {
                elected_sids.push(pick.sid);
                now += DELIVER_MS;
            }
            Said::Handoff {
                why: Miss::Undelivered,
                ..
            } => now += NOMINATE_MS,
            Said::Out {
                last: Some(Miss::Undelivered),
                asked: false,
            } => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(elected_sids, ["s-0", "s-1", "s-2", "s-3"]);
    assert_eq!(lease.entry(KEY).expect("entry").asks, 0);
}

/// An episode that runs out with no line typed says that no session took its line, as the
/// owner's line does, not that the agents asked did not free it: the ledger row and `harness
/// disk why` read this sentence. Only an episode whose lines went says the agents asked did not
/// free it.
#[test]
fn an_episode_out_with_no_line_typed_says_no_session_took_its_line() {
    let h1 = host(1);
    let seats: Vec<Seat> = (0..6)
        .map(|i| seat(&format!("s-{i}"), 10 - i, false))
        .collect();
    let one = At::short(&h1, seats);
    let run_out = |type_lines: bool| {
        let mut lease = Lease::default();
        assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
        let mut now = T0 + NOMINATE_MS;
        loop {
            match one.step(&mut lease, now) {
                Said::Elected { pick, .. } if type_lines => {
                    assert!(typed(&mut lease, KEY, &h1, &pick.sid, now, 20 * GIB));
                    now += ANSWER_MS;
                }
                Said::Elected { .. } => now += DELIVER_MS,
                Said::Handoff { .. } => now += NOMINATE_MS,
                said @ Said::Out { .. } => return (said, lease),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let (out, lease) = run_out(false);
    let text = out.describe();
    assert!(
        text.starts_with("no session took its line (last: undelivered)"),
        "{text}"
    );
    let row = row(T0 / 1000, KEY, &out, Level::Short, 0, &h1);
    assert!(row.contains("no session took its line"), "{row}");
    assert!(
        owner_line(lease.entry(KEY).expect("entry"), T0 + COOLDOWN_MS)
            .is_some_and(|l| l.contains("none took its line")),
        "the owner's line says the same"
    );
    let (out, _) = run_out(true);
    assert!(
        out.describe()
            .starts_with("the agents asked did not free it (last: nothing-freed)"),
        "{}",
        out.describe()
    );
}

#[test]
fn a_released_pick_costs_no_election_so_a_session_that_may_be_asked_still_is() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let seats: Vec<Seat> = (0..6)
        .map(|i| seat(&format!("s-{i}"), 10 - i, true))
        .collect();
    let one = At::short(&h1, seats);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    let mut now = T0 + NOMINATE_MS;
    for i in 0..5 {
        let sid = format!("s-{i}");
        let said = one.step(&mut lease, now);
        assert!(
            matches!(&said, Said::Elected { pick, .. } if pick.sid == sid),
            "{said:?}"
        );
        assert!(
            matches!(
                release(&mut lease, KEY, &h1, &sid, now + 1),
                Some(Said::Handoff {
                    why: Miss::Released,
                    ..
                })
            ),
            "release {i} hands the ask on"
        );
        now += NOMINATE_MS + 1;
    }
    let said = one.step(&mut lease, now);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-5"),
        "five releases cost no election: {said:?}"
    );
    assert_eq!(claim(&mut lease, KEY, &h1, "s-5", now + 1), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h1, "s-5", now + 1, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!((e.picks.len(), e.released, e.asks), (6, 5, 1));
}

#[test]
fn repeated_claims_hold_a_pick_no_longer_than_its_time_and_one_grace() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected { .. }
    ));
    let cap = T0 + NOMINATE_MS + DELIVER_MS + TYPE_GRACE_MS;
    let mut now = T0 + NOMINATE_MS + DELIVER_MS - 1_000;
    while claim(&mut lease, KEY, &h1, "s-a", now) == Claim::Yours {
        assert!(
            matches!(phase(&lease), Phase::Owed { until_ms, .. } if *until_ms <= cap),
            "{:?}",
            phase(&lease)
        );
        now += 60_000;
    }
    assert!(now >= cap, "a claim stood past the cap at {now}");
    assert!(matches!(one.step(&mut lease, cap - 1), Said::Owed { .. }));
    assert!(matches!(
        one.step(&mut lease, cap),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
}

#[test]
fn space_freed_in_time_but_first_read_after_the_answer_deadline_still_settles() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    one.step(&mut lease, T0 + NOMINATE_MS);
    let t = T0 + NOMINATE_MS + 1_000;
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", t), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    one.free = 23 * GIB;
    assert_eq!(
        one.step(&mut lease, t + ANSWER_MS + 30_000),
        Said::Freed {
            sid: "s-a".to_owned(),
            freed: 3 * GIB
        },
        "a rise is read before the deadline is"
    );
}

#[test]
fn a_line_whose_record_the_lease_lost_is_recorded_once_by_a_later_step() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    elected(
        &mut lease,
        &At::short(&h1, vec![seat("s-a", 12, true)]),
        &At::short(&h2, vec![seat("s-c", 3, true)]),
    );
    let t = T0 + NOMINATE_MS + 5_000;
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    assert!(
        matches!(phase(&lease), Phase::Asked { typed_ms, .. } if *typed_ms == t),
        "{:?}",
        phase(&lease)
    );
    assert!(
        !typed_unrecorded(&mut lease, KEY, &h1, "s-a", t, 20 * GIB),
        "a recorded line is not recorded again"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!((e.asks, e.last_ask_ms), (1, Some(t)));
}

#[test]
fn a_host_that_finds_its_pick_may_not_be_asked_releases_it_and_a_host_with_no_seat_runs_out() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 4, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        one.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected { .. }
    ));
    assert_eq!(
        release(&mut lease, KEY, &host(2), "s-a", T0 + NOMINATE_MS + 1),
        None,
        "another host cannot release it"
    );
    assert_eq!(
        release(&mut lease, KEY, &h1, "s-a", T0 + NOMINATE_MS + 1),
        Some(Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Released
        })
    );
    assert_eq!(
        one.step(&mut lease, T0 + 2 * NOMINATE_MS + 1),
        Said::Out {
            last: Some(Miss::Released),
            asked: false
        },
        "its one session was picked already: nobody is left to ask"
    );
    let mut lease = Lease::default();
    let none = At::short(&h1, Vec::new());
    assert!(matches!(none.step(&mut lease, T0), Said::Opened { .. }));
    assert_eq!(
        none.step(&mut lease, T0 + NOMINATE_MS),
        Said::Out {
            last: None,
            asked: false
        }
    );
}

#[test]
fn an_owed_pick_whose_session_left_its_hosts_supervision_is_handed_off_at_once() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    one.seats.clear();
    assert!(
        matches!(
            one.step(&mut lease, T0 + NOMINATE_MS + 30_000),
            Said::Owed { .. }
        ),
        "a session whose seat could not be read this look is still supervised"
    );
    one.supervised.clear();
    assert!(matches!(
        one.step(&mut lease, T0 + NOMINATE_MS + 60_000),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let mut two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    assert!(matches!(
        two.step(&mut lease, T0 + NOMINATE_MS + 60_000),
        Said::Owed { .. }
    ));
    two.dead = vec![1];
    assert!(
        matches!(
            two.step(&mut lease, T0 + NOMINATE_MS + 120_000),
            Said::Owed { .. }
        ),
        "its host ended, but a successor may yet adopt it"
    );
    assert!(
        matches!(
            two.step(&mut lease, T0 + NOMINATE_MS + 120_000 + ADOPT_MS),
            Said::Handoff {
                why: Miss::Undelivered,
                ..
            }
        ),
        "its host ended before the line, and nobody adopted it"
    );
}

#[test]
fn an_asked_session_that_ends_hands_off_and_a_claim_holds_the_pick_while_it_is_typed() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    one.step(&mut lease, T0 + NOMINATE_MS);
    let late = T0 + NOMINATE_MS + DELIVER_MS - 1_000;
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", late), Claim::Yours);
    assert!(
        matches!(phase(&lease), Phase::Owed { until_ms, .. }
            if *until_ms == T0 + NOMINATE_MS + DELIVER_MS + TYPE_GRACE_MS - 1_000),
        "{:?}",
        phase(&lease)
    );
    assert!(
        matches!(one.step(&mut lease, late + 2_000), Said::Owed { .. }),
        "no host hands it off while it is typed"
    );
    assert!(typed(&mut lease, KEY, &h1, "s-a", late + 3_000, 20 * GIB));
    one.live = vec!["s-b".to_owned()];
    assert!(matches!(
        one.step(&mut lease, late + 60_000),
        Said::Handoff {
            why: Miss::Ended,
            ..
        }
    ));
}

#[test]
fn a_successor_that_supervises_a_pick_whose_host_ended_adopts_it_owed_or_asked() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let mut successor = At::short(&h3, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    successor.dead = vec![1];
    let t = T0 + NOMINATE_MS + 60_000;
    let adopted = Pick {
        host: h3.clone(),
        sid: "s-a".to_owned(),
    };
    assert_eq!(
        successor.step(&mut lease, t),
        Said::Owed {
            pick: adopted.clone(),
            until_ms: T0 + NOMINATE_MS + DELIVER_MS
        },
        "a seamless update's successor adopts its session's pick, not hands it off"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.picks.as_slice(), e.asks),
        ([adopted.clone()].as_slice(), 0)
    );
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", t + 1), Claim::Gone);
    assert_eq!(claim(&mut lease, KEY, &h3, "s-a", t + 1), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h3, "s-a", t + 2, 20 * GIB));
    // Its successor in turn ends: the next one judges the asked session's answer.
    let h4 = host(4);
    let mut next = At::short(&h4, vec![seat("s-a", 12, true)]);
    next.dead = vec![1, 3];
    assert!(matches!(
        next.step(&mut lease, t + 60_000),
        Said::Asked { pick, .. } if pick.host == h4
    ));
    next.live.clear();
    next.supervised.clear();
    next.seats.clear();
    assert!(
        matches!(
            next.step(&mut lease, t + 120_000),
            Said::Handoff {
                why: Miss::Ended,
                ..
            }
        ),
        "the host that adopted it sees it end"
    );
    // A host that does not supervise the session cannot adopt it: its host ended, so it is
    // handed off once a successor has had the adoption window to look.
    let mut lease = Lease::default();
    elected(&mut lease, &one, &two);
    let mut other = At::short(&h2, vec![seat("s-c", 3, true)]);
    other.dead = vec![1];
    assert!(matches!(other.step(&mut lease, t), Said::Owed { .. }));
    assert!(matches!(
        other.step(&mut lease, t + ADOPT_MS),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
    // Nor is a pick whose host still runs adopted by another host holding the same sid.
    let mut lease = Lease::default();
    elected(&mut lease, &one, &two);
    let twin = At::short(&h3, vec![seat("s-a", 12, true)]);
    assert!(matches!(
        twin.step(&mut lease, t),
        Said::Owed { pick, .. } if pick.host == h1
    ));
    assert_eq!(
        fault::with(fault::Fault::NoAdopt, || {
            let mut lease = Lease::default();
            elected(&mut lease, &one, &two);
            successor.step(&mut lease, t);
            successor.step(&mut lease, t + ADOPT_MS)
        }),
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Undelivered
        },
        "the planted defect the conformance control arms"
    );
}

/// A pick whose agent exited around a seamless update: its task record says exited, so the
/// successor's seat rule bars it, and it has left the successor's supervised roster. The
/// successor adopts it all the same and releases it at once, at no election's cost, where the
/// look before handed it off as undelivered once the adoption window was over.
#[test]
fn a_successor_adopts_and_releases_an_exited_pick_of_the_host_it_replaced() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    let mut successor = At::short(&h3, vec![seat("s-b", 2, true)]);
    successor.supervised = vec!["s-b".to_owned()];
    successor.barred = vec!["s-a".to_owned()];
    successor.live = vec!["s-a".to_owned(), "s-b".to_owned()];
    successor.dead = vec![1];
    let t = T0 + NOMINATE_MS + 2 * 60_000;
    let mut lease = Lease::default();
    elected(&mut lease, &one, &two);
    assert_eq!(
        successor.step(&mut lease, t),
        Said::Handoff {
            from: Pick {
                host: h3.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Released
        },
        "adopted, then released by the seat rule that bars it"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.picks.len(), e.released, e.asks),
        (1, 1, 0),
        "no election spent"
    );
    assert!(matches!(e.phase, Phase::Nominating { .. }));
    let unadopted = fault::with(fault::Fault::AdoptSupervisedOnly, || {
        let mut lease = Lease::default();
        elected(&mut lease, &one, &two);
        let first = successor.step(&mut lease, t);
        let later = successor.step(&mut lease, t + ADOPT_MS);
        (first, later, lease.entry(KEY).map(|e| e.released))
    });
    assert!(
        matches!(&unadopted.0, Said::Owed { pick, .. } if pick.host == h1),
        "{unadopted:?}"
    );
    assert_eq!(
        (unadopted.1, unadopted.2),
        (
            Said::Handoff {
                from: Pick {
                    host: h1.clone(),
                    sid: "s-a".to_owned()
                },
                why: Miss::Undelivered
            },
            Some(0)
        ),
        "the planted defect the conformance control arms: an election spent"
    );
}

#[test]
fn another_hosts_look_leaves_an_ended_hosts_pick_owed_until_its_successor_adopts_it() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    // The first host is replaced by a seamless update; the second host's looks come first.
    let mut other = At::short(&h2, vec![seat("s-c", 3, true)]);
    other.dead = vec![1];
    let ended = T0 + NOMINATE_MS + 30_000;
    assert!(
        matches!(other.step(&mut lease, ended), Said::Owed { pick, .. } if pick.host == h1),
        "the session may still run under a successor that has not looked yet"
    );
    assert!(
        matches!(
            phase(&lease),
            Phase::Owed { orphan_ms: Some(at), .. } if *at == ended
        ),
        "{:?}",
        phase(&lease)
    );
    assert!(matches!(
        other.step(&mut lease, ended + ADOPT_MS - 1),
        Said::Owed { .. }
    ));
    assert!(
        why_lines(&lease, &[], ended)
            .iter()
            .any(|l| l.contains("read ended")),
        "{:?}",
        why_lines(&lease, &[], ended)
    );
    let mut successor = At::short(&h3, vec![seat("s-a", 12, true)]);
    successor.dead = vec![1];
    let adopted = Pick {
        host: h3.clone(),
        sid: "s-a".to_owned(),
    };
    assert_eq!(
        successor.step(&mut lease, ended + ADOPT_MS - 1),
        Said::Owed {
            pick: adopted.clone(),
            until_ms: T0 + NOMINATE_MS + DELIVER_MS
        }
    );
    assert!(
        matches!(
            phase(&lease),
            Phase::Owed {
                orphan_ms: None,
                ..
            }
        ),
        "adopted: its host runs"
    );
    assert!(
        matches!(other.step(&mut lease, ended + 2 * ADOPT_MS), Said::Owed { pick, .. } if pick == adopted),
        "a pick its successor adopted is no other host's to hand on"
    );
    assert_eq!(
        lease.entry(KEY).map(|e| e.picks.clone()),
        Some(vec![adopted])
    );
    // A pick whose host is read ended and then running again (a misread) is owed afresh.
    let mut lease = Lease::default();
    elected(&mut lease, &one, &two);
    other.step(&mut lease, ended);
    two.step(&mut lease, ended + 60_000);
    assert!(matches!(
        phase(&lease),
        Phase::Owed {
            orphan_ms: None,
            ..
        }
    ));
    assert!(matches!(
        other.step(&mut lease, ended + ADOPT_MS),
        Said::Owed { .. }
    ));
    assert_eq!(
        fault::with(fault::Fault::OrphanAtOnce, || {
            let mut lease = Lease::default();
            elected(&mut lease, &one, &two);
            other.step(&mut lease, ended)
        }),
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Undelivered
        },
        "the planted defect the conformance control arms"
    );
}

/// A seamless update's successor takes its session's owed pick over as its first look begins,
/// before that look's pass, so another host's look while the pass runs, past `ADOPT_MS` from
/// the look that first read the ended host, reads the pick's host running and keeps it owed; the
/// successor's step after its pass keeps it and its line goes there. With the take-over at the
/// step after the pass alone (`Fault::NoTakeOver`), a first look's pass longer than about a
/// minute (discovery, profile walks) let that other look hand the pick off as undelivered,
/// spending an election on the session the successor runs.
#[test]
fn a_successor_takes_its_sessions_pick_over_before_its_pass_so_no_other_look_hands_it_off() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let mut other = At::short(&h2, vec![seat("s-c", 3, true)]);
    other.dead = vec![1];
    let ended = T0 + NOMINATE_MS + 30_000;
    assert!(matches!(other.step(&mut lease, ended), Said::Owed { .. }));
    let mut successor = At::short(&h3, vec![seat("s-a", 12, true)]);
    successor.dead = vec![1];
    let begins = ended + 60_000;
    let before = lease.clone();
    assert_eq!(
        other.take_over(&mut lease),
        Vec::<String>::new(),
        "not its session"
    );
    assert_eq!(lease, before, "another host takes nothing over");
    let skipped = fault::with(fault::Fault::NoTakeOver, || {
        let mut l = lease.clone();
        assert!(successor.take_over(&mut l).is_empty());
        other.step(&mut l, ended + ADOPT_MS)
    });
    assert_eq!(
        skipped,
        Said::Handoff {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            why: Miss::Undelivered
        },
        "the defect the conformance control arms: handed off while the successor's pass ran"
    );
    assert_eq!(successor.take_over(&mut lease), vec!["s-a".to_owned()]);
    let adopted = Pick {
        host: h3.clone(),
        sid: "s-a".to_owned(),
    };
    assert!(
        matches!(phase(&lease), Phase::Owed { pick, orphan_ms: None, .. } if *pick == adopted),
        "{:?}",
        phase(&lease)
    );
    assert_eq!(
        lease
            .entry(KEY)
            .map(|e| (e.picks.clone(), e.released, e.asks)),
        Some((vec![adopted.clone()], 0, 0)),
        "moved, nothing else"
    );
    assert_eq!(
        successor.take_over(&mut lease),
        Vec::<String>::new(),
        "nothing left to take"
    );
    // The successor's pass runs on; the other host's looks keep the pick owed.
    for at in [ended + ADOPT_MS, ended + 2 * ADOPT_MS] {
        assert!(
            matches!(other.step(&mut lease, at), Said::Owed { pick, .. } if pick == adopted),
            "{at}"
        );
    }
    assert!(matches!(
        successor.step(&mut lease, begins + 3 * ADOPT_MS),
        Said::Owed { pick, .. } if pick == adopted
    ));
    assert_eq!(
        at_break(
            &mut lease,
            KEY,
            &h3,
            "s-a",
            begins + 3 * ADOPT_MS + 1,
            false
        ),
        Break::Yours
    );
    // No episode open: nothing to take.
    let mut idle = Lease::default();
    assert!(successor.take_over(&mut idle).is_empty());
    assert_eq!(idle, Lease::default());
}

/// A successor's take-over before its pass moves the ended host's nominee of a session it may
/// nominate to itself, figures kept, so the other host's look past the round's time elects it
/// there instead of waiting for, or dropping, the ended host's; it drops the nominee of a session
/// its seat rule bars, and records one it holds as held. It moves the ended host's record of
/// every held session it runs, whether or not its seat rule reads that session held before its
/// pass (D), as that reading can lack what holds it: the step after the pass replaces the records
/// with its own reading, under one lock.
#[test]
fn a_successor_takes_over_its_sessions_nominee_and_held_record_before_its_pass() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 30, true)]);
    one.held = vec!["s-b".to_owned(), "s-d".to_owned()];
    one.live.extend(one.held.clone());
    one.supervised = one.live.clone();
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        two.step(&mut lease, T0 + 60_000),
        Said::Nominating { .. }
    ));
    let mut successor = At::short(&h3, Vec::new());
    successor.live = ["s-a", "s-b", "s-d"].map(str::to_owned).to_vec();
    successor.supervised = successor.live.clone();
    successor.held = vec!["s-b".to_owned()];
    successor.dead = vec![1];
    let round = lease.clone();
    assert_eq!(
        successor.take_over(&mut lease),
        ["s-a", "s-b", "s-d"].map(str::to_owned).to_vec()
    );
    let e = lease.entry(KEY).expect("entry").clone();
    let Phase::Nominating { nominees, .. } = &e.phase else {
        panic!("{:?}", e.phase);
    };
    assert_eq!(
        nominees
            .iter()
            .map(|n| (n.host.pid, n.sid.as_str(), n.held, n.orphan_ms))
            .collect::<Vec<_>>(),
        [(3, "s-a", 30 * GIB, None), (2, "s-c", 3 * GIB, None)],
        "A's nominee moved with its figures"
    );
    let records = |e: &Entry| {
        e.held
            .iter()
            .map(|h| (h.host.pid, h.sid.clone(), h.why))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        records(&e),
        [
            (3, "s-b".to_owned(), Some(HeldBy::Limit)),
            (3, "s-d".to_owned(), Some(HeldBy::Limit))
        ],
        "B's and D's records moved, D's though the successor's rule does not read it held yet"
    );
    let mut other = At::short(&h2, vec![seat("s-c", 3, true)]);
    other.dead = vec![1];
    assert_eq!(
        other.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected {
            pick: Pick {
                host: h3.clone(),
                sid: "s-a".to_owned()
            },
            held: 30 * GIB
        },
        "elected on the successor at the round's time"
    );
    // A successor whose seat rule bars A drops its nominee.
    let mut barring = round.clone();
    successor.barred = vec!["s-a".to_owned()];
    assert_eq!(
        successor.take_over(&mut barring),
        ["s-b", "s-d"].map(str::to_owned).to_vec()
    );
    assert!(
        matches!(
            phase(&barring),
            Phase::Nominating { nominees, .. } if nominees.iter().all(|n| n.sid != "s-a")
        ),
        "{:?}",
        phase(&barring)
    );
    // A successor whose seat rule holds A, and B, at a draft left in its prompt: A's nominee
    // becomes a record of A held there, and B's record says the draft.
    let mut holding = round;
    successor.barred.clear();
    successor.held = vec!["s-a".to_owned(), "s-b".to_owned()];
    successor.hold = HeldBy::Draft;
    assert_eq!(
        successor.take_over(&mut holding),
        ["s-a", "s-b", "s-d"].map(str::to_owned).to_vec()
    );
    let e = holding.entry(KEY).expect("entry").clone();
    assert!(
        matches!(
            &e.phase,
            Phase::Nominating { nominees, .. } if nominees.iter().all(|n| n.sid != "s-a")
        ),
        "{:?}",
        e.phase
    );
    assert_eq!(
        records(&e),
        [
            (3, "s-b".to_owned(), Some(HeldBy::Draft)),
            (3, "s-d".to_owned(), Some(HeldBy::Limit)),
            (3, "s-a".to_owned(), Some(HeldBy::Draft))
        ]
    );
    let mut other = At::short(&h2, vec![seat("s-c", 3, true)]);
    other.dead = vec![1];
    assert_eq!(
        other.step(&mut holding, T0 + NOMINATE_MS),
        Said::Elected {
            pick: Pick {
                host: h2.clone(),
                sid: "s-c".to_owned()
            },
            held: 3 * GIB
        },
        "C, as A's successor holds A"
    );
}

/// A round waits past its time on an ended host's record of a held session alone when that
/// host's successor begins a look whose seat rule, before its pass, does not read the session
/// held (before its first pass a process places no session on the volume, and reads no draft as
/// standing): its take-over moves the record to itself, so another host's look while the pass
/// runs, past `ADOPT_MS` too, still waits, and the successor's step after its pass, reading the
/// session fit, replaces the record and elects it. A take-over that dropped the record by that
/// reading (`Fault::TakeOverByRule`, the first take-over) left the round with nobody to elect or
/// wait for, and the other host's look ran the episode out, parking it for its cooldown.
#[test]
fn a_successor_keeps_a_held_record_it_takes_over_whatever_its_rule_reads_before_its_pass() {
    let (h1, h2, h3) = (host(1), host(2), host(3));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, Vec::new());
    one.held = vec!["s-a".to_owned()];
    one.hold = HeldBy::Draft;
    one.live = one.held.clone();
    one.supervised = one.held.clone();
    let two = At::short(&h2, Vec::new());
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    let round = T0 + NOMINATE_MS;
    let waiting = Said::Waiting {
        held: vec!["s-a".to_owned()],
        why: vec![HeldBy::Draft],
    };
    assert_eq!(two.step(&mut lease, round), waiting);
    let mut successor = At::short(&h3, Vec::new());
    successor.live = vec!["s-a".to_owned()];
    successor.supervised = successor.live.clone();
    successor.dead = vec![1];
    let mut other = At::short(&h2, Vec::new());
    other.dead = vec![1];
    let begins = round + 120_000;
    let dropped = fault::with(fault::Fault::TakeOverByRule, || {
        let mut l = lease.clone();
        assert!(successor.take_over(&mut l).is_empty());
        other.step(&mut l, begins + 60_000)
    });
    assert_eq!(
        dropped,
        Said::Out {
            last: None,
            asked: false
        },
        "the defect: run out while the successor's pass ran"
    );
    assert_eq!(successor.take_over(&mut lease), vec!["s-a".to_owned()]);
    assert_eq!(
        lease.entry(KEY).map(|e| e.held.clone()),
        Some(vec![Held {
            host: h3.clone(),
            sid: "s-a".to_owned(),
            why: Some(HeldBy::Draft),
            orphan_ms: None
        }]),
        "moved, its hold kept"
    );
    for at in [begins + 60_000, begins + ADOPT_MS + 60_000] {
        assert_eq!(other.step(&mut lease, at), waiting, "{at}");
    }
    successor.seats = vec![seat("s-a", 9, true)];
    let said = successor.step(&mut lease, begins + ADOPT_MS + 90_000);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.host == h3 && pick.sid == "s-a"),
        "the step after the pass reads A fit: {said:?}"
    );
    assert!(
        lease.entry(KEY).is_some_and(|e| e.held.is_empty()),
        "{lease:?}"
    );
}

#[test]
fn a_late_record_that_spends_the_episodes_lines_withdraws_a_pick_elected_since() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 6, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    // A is asked and frees nothing; B is elected and its line goes, but its record is lost; the
    // second host hands B off at its deadline and elects C before B's host looks again.
    elected(&mut lease, &one, &two);
    let t = T0 + NOMINATE_MS + 1_000;
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", t), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    assert!(matches!(
        one.step(&mut lease, t + ANSWER_MS),
        Said::Handoff {
            why: Miss::NothingFreed,
            ..
        }
    ));
    let round = t + ANSWER_MS + NOMINATE_MS;
    one.step(&mut lease, round - 1_000);
    assert!(matches!(
        one.step(&mut lease, round),
        Said::Elected { pick, .. } if pick.sid == "s-b"
    ));
    let at = round + DELIVER_MS - 60_000;
    let handed = round + DELIVER_MS;
    assert!(matches!(
        two.step(&mut lease, handed),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
    two.step(&mut lease, handed + 60_000);
    assert!(matches!(
        two.step(&mut lease, handed + NOMINATE_MS),
        Said::Elected { pick, .. } if pick.sid == "s-c"
    ));
    let mut late = lease.clone();
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-b", at, 21 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.asks, e.asked.as_slice()),
        (2, ["s-a".to_owned(), "s-b".to_owned()].as_slice())
    );
    assert_eq!(
        e.phase,
        Phase::Asked {
            pick: Pick {
                host: h1.clone(),
                sid: "s-b".to_owned()
            },
            typed_ms: at,
            until_ms: at + ANSWER_MS,
            free_at: 21 * GIB,
            best_free: 21 * GIB
        },
        "B's line is the episode's second: C is withdrawn, and B's answer awaited"
    );
    assert_eq!(
        claim(&mut lease, KEY, &h2, "s-c", handed + NOMINATE_MS + 1_000),
        Claim::Gone,
        "no third line"
    );
    assert_eq!(
        one.step(&mut lease, at + ANSWER_MS),
        Said::Out {
            last: Some(Miss::NothingFreed),
            asked: true
        }
    );
    // The planted defect the conformance control arms: C stays owed, and takes a third line.
    fault::with(fault::Fault::OwedPastTwo, || {
        assert!(typed_unrecorded(&mut late, KEY, &h1, "s-b", at, 21 * GIB));
    });
    assert_eq!(
        claim(&mut late, KEY, &h2, "s-c", handed + NOMINATE_MS + 1_000),
        Claim::Yours
    );
}

#[test]
fn a_late_record_after_a_later_pick_was_handed_off_waits_on_its_answer() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let mut two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let at = T0 + NOMINATE_MS + DELIVER_MS - 60_000;
    let handed = T0 + NOMINATE_MS + DELIVER_MS;
    two.step(&mut lease, handed);
    two.step(&mut lease, handed + 60_000);
    assert!(matches!(
        two.step(&mut lease, handed + NOMINATE_MS),
        Said::Elected { pick, .. } if pick.sid == "s-c"
    ));
    // C may not be asked: its host releases it, at no election's cost, and nobody else is there.
    two.barred = vec!["s-c".to_owned()];
    assert!(matches!(
        two.step(&mut lease, handed + NOMINATE_MS + 60_000),
        Said::Handoff {
            why: Miss::Released,
            ..
        }
    ));
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.asks, e.released),
        (1, 1),
        "C's release is C's: it stands"
    );
    assert!(
        matches!(&e.phase, Phase::Asked { pick, .. } if pick.sid == "s-a"),
        "no line went since A's hand-off: A's answer is awaited before anyone else is elected: {:?}",
        e.phase
    );
}

#[test]
fn a_lost_record_is_counted_after_its_pick_was_handed_off_and_waits_on_its_answer_again() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true), seat("s-d", 1, true)]);
    elected(&mut lease, &one, &two);
    // A's line went just before its deadline; its record (and its claim) never reached the lease.
    let at = T0 + NOMINATE_MS + DELIVER_MS - 60_000;
    assert!(matches!(
        two.step(&mut lease, T0 + NOMINATE_MS + DELIVER_MS),
        Said::Handoff {
            why: Miss::Undelivered,
            ..
        }
    ));
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.asks, e.asked.as_slice(), e.last_ask_ms),
        (1, ["s-a".to_owned()].as_slice(), Some(at)),
        "the line counts as one of the episode's two"
    );
    assert_eq!(
        e.phase,
        Phase::Asked {
            pick: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned()
            },
            typed_ms: at,
            until_ms: at + ANSWER_MS,
            free_at: 20 * GIB,
            best_free: 20 * GIB
        },
        "nobody was elected since its hand-off: the episode waits on its answer"
    );
    assert!(
        !typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB),
        "counted once"
    );
    // A frees nothing: one other session is asked, then the episode is out at two lines.
    assert!(matches!(
        one.step(&mut lease, at + ANSWER_MS),
        Said::Handoff {
            why: Miss::NothingFreed,
            ..
        }
    ));
    two.step(&mut lease, at + ANSWER_MS + 1_000);
    assert!(matches!(
        two.step(&mut lease, at + ANSWER_MS + NOMINATE_MS + 1_000),
        Said::Elected { pick, .. } if pick.sid == "s-c"
    ));
    let t = at + ANSWER_MS + NOMINATE_MS + 2_000;
    assert_eq!(claim(&mut lease, KEY, &h2, "s-c", t), Claim::Yours);
    assert!(typed(&mut lease, KEY, &h2, "s-c", t, 20 * GIB));
    assert_eq!(
        two.step(&mut lease, t + ANSWER_MS),
        Said::Out {
            last: Some(Miss::NothingFreed),
            asked: true
        },
        "two lines went: nobody else is asked"
    );
}

#[test]
fn a_lost_record_counts_though_another_was_elected_or_a_later_line_went() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let at = T0 + NOMINATE_MS + DELIVER_MS - 60_000;
    two.step(&mut lease, T0 + NOMINATE_MS + DELIVER_MS);
    two.step(&mut lease, T0 + NOMINATE_MS + DELIVER_MS + 1_000);
    let elected_c = T0 + 2 * NOMINATE_MS + DELIVER_MS + 1_000;
    assert!(matches!(
        two.step(&mut lease, elected_c),
        Said::Elected { pick, .. } if pick.sid == "s-c"
    ));
    assert!(typed(
        &mut lease,
        KEY,
        &h2,
        "s-c",
        elected_c + 1_000,
        20 * GIB
    ));
    assert!(
        typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB),
        "a later line hides no line the episode has not counted"
    );
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.asks, e.asked.len(), e.last_ask_ms),
        (2, 2, Some(elected_c + 1_000)),
        "both lines count, and the cooldown runs from the later"
    );
    assert!(
        matches!(&e.phase, Phase::Asked { pick, .. } if pick.sid == "s-c"),
        "C was elected after A's hand-off, so C's answer is the one awaited: {:?}",
        e.phase
    );
}

#[test]
fn a_lost_record_undoes_a_release_its_own_host_made_before_recording_it() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 2, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    one.step(&mut lease, T0 + NOMINATE_MS);
    let at = T0 + NOMINATE_MS + 10_000;
    assert!(release(&mut lease, KEY, &h1, "s-a", at + 5_000).is_some());
    assert_eq!(lease.entry(KEY).expect("entry").released, 1);
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(
        (e.released, e.asks),
        (0, 1),
        "its line went: it was no release"
    );
    assert!(matches!(&e.phase, Phase::Asked { pick, .. } if pick.sid == "s-a"));
}

#[test]
fn a_lost_record_of_an_earlier_episode_counts_for_the_cooldown_alone() {
    let h1 = host(1);
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true)]);
    assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
    one.step(&mut lease, T0 + NOMINATE_MS);
    let at = T0 + NOMINATE_MS + 10_000;
    // The line went unrecorded; the episode ended and, once its cooldown was over, a new one
    // elected the same session before its host recorded the old line.
    one.reads(100 * GIB);
    assert!(matches!(
        one.step(&mut lease, at + 1_000),
        Said::Ended { .. }
    ));
    one.reads(20 * GIB);
    let later = at + COOLDOWN_MS;
    assert!(matches!(one.step(&mut lease, later), Said::Opened { .. }));
    one.step(&mut lease, later + NOMINATE_MS);
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!(e.last_ask_ms, Some(at));
    assert_eq!(
        e.asks, 0,
        "a line before the episode opened is none of its lines"
    );
}

/// A clear look rests (steps nothing, takes no lock) only with no episode open and no line of
/// its host's for the lease to record. A's line goes but its record is lost, and another host's
/// look ends the episode before A's host looks again: that host's clear look still records the
/// line, so the cooldown holds a new episode shut, where the look before rested and left the
/// cooldown unstarted, so another host's short look asked a second session inside it.
#[test]
fn a_clear_look_records_a_lost_line_whose_episode_another_host_ended_before_it_rests() {
    let (h1, h2) = (host(1), host(2));
    let mut empty = Lease::default();
    assert!(rests(&empty, KEY, None), "nothing to step");
    assert!(!rests(&empty, KEY, Some(T0)), "a line the lease never saw");
    typed(&mut empty, KEY, &h1, "s-a", T0, 20 * GIB);
    assert!(rests(&empty, KEY, Some(T0)), "recorded");
    assert!(!rests(&empty, KEY, Some(T0 + 1)), "a later line is not");

    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    let opened = |lease: &mut Lease| {
        elected(lease, &one, &two);
        assert!(!rests(lease, KEY, None), "an episode is open");
    };
    let at = T0 + NOMINATE_MS + 60_000;
    let mut clear = At::short(&h2, vec![seat("s-c", 3, true)]);
    clear.reads(100 * GIB);
    let mut lease = Lease::default();
    opened(&mut lease);
    // A's line went at `at`, its record lost to a full disk: the lease still owes it.
    assert!(matches!(
        clear.step(&mut lease, at + 60_000),
        Said::Ended { asks: 0 }
    ));
    assert!(
        !rests(&lease, KEY, Some(at)),
        "A's host's clear look must step, to record its line"
    );
    assert!(typed_unrecorded(&mut lease, KEY, &h1, "s-a", at, 20 * GIB));
    assert!(rests(&lease, KEY, Some(at)), "and then rests");
    let mut short = At::short(&h2, vec![seat("s-c", 3, true)]);
    short.reads(20 * GIB);
    assert_eq!(
        short.step(&mut lease, at + 10 * 60_000),
        Said::Cooldown { last_ms: at },
        "no second session is asked within the cooldown of A's line"
    );
    assert!(
        fault::with(fault::Fault::RestUnrecorded, || rests(
            &Lease::default(),
            KEY,
            Some(at)
        )),
        "the planted defect the conformance control arms rests with the line unrecorded"
    );
    let mut unrecorded = Lease::default();
    opened(&mut unrecorded);
    clear.step(&mut unrecorded, at + 60_000);
    assert!(
        matches!(
            short.step(&mut unrecorded, at + 10 * 60_000),
            Said::Opened { .. }
        ),
        "left unrecorded, a new episode opens inside the cooldown"
    );
}

#[test]
fn a_hosts_start_is_judged_in_its_own_spelling_with_no_child_for_a_boot_clock_one() {
    let boot = format!("{BOOT_START}12345");
    assert!(
        alive_in(
            &boot,
            || Some(boot.clone()),
            || panic!("no ps for a boot-clock start")
        ),
        "the same boot-clock start: the same process, whatever the wall clock did"
    );
    assert!(!alive_in(
        &boot,
        || Some(format!("{BOOT_START}999")),
        || panic!("no ps for a boot-clock start")
    ));
    assert!(!alive_in(
        &boot,
        || None,
        || panic!("no ps for a boot-clock start")
    ));
    let lstart = "Sat Oct 24 10:00:00 2026";
    assert!(
        alive_in(
            lstart,
            || panic!("no boot read for an older spelling"),
            || true
        ),
        "a lease an older build wrote: its live host is judged by that spelling"
    );
    assert!(!alive_in(lstart, || None, || false));
    assert!(alive_in("", || None, || false), "unsaid: read as running");
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn on_linux_a_host_is_named_by_its_boot_clock_start() {
    let this = HostId::this();
    assert!(this.start.starts_with(BOOT_START), "{this:?}");
    assert!(this.alive());
    assert!(
        !HostId {
            start: format!("{BOOT_START}1"),
            ..this.clone()
        }
        .alive(),
        "another start of this pid is no one's host"
    );
}

#[test]
fn a_line_typed_after_the_lease_moved_on_counts_for_the_cooldown_alone() {
    let h1 = host(1);
    let mut lease = Lease::default();
    assert!(!typed(&mut lease, KEY, &h1, "s-a", T0, 20 * GIB));
    let e = lease.entry(KEY).expect("entry");
    assert_eq!((e.asks, e.last_ask_ms), (0, Some(T0)));
}

#[test]
fn the_lease_round_trips_its_file_and_a_foreign_schema_reads_as_empty() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    elected(
        &mut lease,
        &At::short(&h1, vec![seat("s-a", 12, true)]),
        &At::short(&h2, vec![seat("s-c", 3, true)]),
    );
    let text = lease.encode();
    let back = Lease::decode(&text);
    assert_eq!(back.volumes, lease.volumes);
    assert_eq!(back.schema, 1);
    assert_eq!(
        Lease::decode("{\"schema\":2,\"volumes\":[]}"),
        Lease::default()
    );
    assert_eq!(Lease::decode("not json"), Lease::default());
}

#[test]
fn the_lease_keeps_at_most_its_volumes_dropping_an_idle_one_first() {
    let mut lease = Lease::default();
    for i in 0..MAX_VOLUMES {
        lease.entry_mut(&format!("dev:{i}")).last_ask_ms = Some(i64::try_from(i).unwrap_or(0));
    }
    lease.entry_mut("dev:0").episode_ms = Some(T0);
    lease.entry_mut("dev:new");
    assert_eq!(lease.volumes.len(), MAX_VOLUMES);
    assert!(lease.entry("dev:0").is_some(), "an open episode is kept");
    assert!(lease.entry("dev:1").is_none(), "the oldest idle one goes");
}

#[test]
fn transact_writes_a_changed_lease_and_skips_while_another_holds_the_lock() {
    let tmp = aterm_tempfile::TempDir::new().expect("owned scratch directory");
    let dir = tmp.path().join("harness");
    let h1 = host(1);
    let at = At::short(&h1, vec![seat("s-a", 12, true)]);
    let said = transact(&dir, &LOCK_LADDER, |l| at.step(l, T0)).expect("transact");
    assert!(matches!(said, Some(Said::Opened { .. })), "{said:?}");
    assert!(matches!(
        read(&dir).entry(KEY).map(|e| &e.phase),
        Some(Phase::Nominating { .. })
    ));
    let held = lock_within(&dir, &[]).expect("lock").expect("free");
    assert_eq!(
        transact(&dir, &[], |l| at.step(l, T0 + NOMINATE_MS)).expect("transact"),
        None,
        "a held lease is not read or written"
    );
    drop(held);
    assert!(matches!(
        transact(&dir, &[], |l| at.step(l, T0 + NOMINATE_MS)).expect("transact"),
        Some(Said::Elected { .. })
    ));
    assert!(matches!(
        read(&dir).entry(KEY).map(|e| &e.phase),
        Some(Phase::Owed { .. })
    ));
}

fn hog(path: &str, gib: u64, serves: Option<&str>) -> Hog {
    Hog {
        path: PathBuf::from(path),
        bytes: gib * GIB,
        bytes_partial: false,
        why: "cargo build output".to_owned(),
        serves: serves.map(PathBuf::from),
    }
}

#[test]
fn the_line_names_the_volume_the_holders_and_the_rule_within_its_cap() {
    let holders = [
        hog(
            "/Users//u/Library/Caches/aterm/cargo-build.noindex/x/.cargo-build",
            212,
            Some("/Users//u/aterm"),
        ),
        hog("/Users//u/trust/build", 94, Some("/Users//u/trust")),
    ];
    let text = line(&Line {
        mount: Path::new("/System/Volumes/Data"),
        total: 1000 * GIB,
        free: 18 * GIB,
        floor: 25 * GIB,
        emergency: false,
        done: Done::Spent,
        holders: &holders,
        checkout: Some(Path::new("/Users//u/aterm")),
        home: Some(Path::new("/Users//u")),
    });
    assert!(text.starts_with("[aterm harness] aterm: "), "{text}");
    assert!(text.ends_with("(rule disk-free@v1)"), "{text}");
    assert!(
        text.contains("(/System/Volumes/Data) is 98% full"),
        "{text}"
    );
    assert!(text.contains("~/aterm's builds 212"), "{text}");
    assert!(text.contains("(this tab's)"), "{text}");
    assert!(text.contains("$HOME/trust's builds 94"), "{text}");
    assert!(text.contains("never source"), "{text}");
    assert!(text.contains("aterm harness disk"), "{text}");
    assert!(text.contains("Only this tab was asked."), "{text}");
    assert!(text.chars().count() <= LINE_CAP, "{}", text.chars().count());
    assert!(!text.contains('\n'));
}

#[test]
fn a_line_owed_while_the_episode_is_held_past_its_floor_says_how_near_it_is() {
    let text = line(&Line {
        mount: Path::new("/"),
        total: 1000 * GIB,
        free: 50 * GIB,
        floor: 48 * GIB,
        emergency: false,
        done: Done::Spent,
        holders: &[],
        checkout: None,
        home: None,
    });
    assert!(
        text.contains("50.0 GiB free, within 5.0 GiB of its 48.0 GiB floor"),
        "{text}"
    );
    assert!(!text.contains("below"), "{text}");
    assert!(text.chars().count() <= LINE_CAP, "{}", text.chars().count());
}

#[test]
fn a_holder_path_with_any_character_beyond_the_plain_ones_is_not_said() {
    let holders = [hog(
        "/Users//u/x`; rm -rf ~ #/target",
        50,
        Some("/Users//u/x`; rm -rf ~ #"),
    )];
    let text = line(&Line {
        mount: Path::new("/"),
        total: 100 * GIB,
        free: GIB,
        floor: 10 * GIB,
        emergency: false,
        done: Done::CleanupOff,
        holders: &holders,
        checkout: None,
        home: Some(Path::new("/Users//u")),
    });
    assert!(!text.contains("rm -rf"), "{text}");
    assert!(text.contains("a build directory's builds 50"), "{text}");
    assert!(text.contains("auto_free_gib"), "{text}");
}

#[test]
fn a_long_holder_path_is_clipped_and_holders_drop_before_the_line_passes_its_cap() {
    let long = format!("/Users//u/{}/target", "deep/".repeat(60));
    let holders = [hog(&long, 50, None), hog(&long, 40, None)];
    let text = line(&Line {
        mount: Path::new("/"),
        total: 100 * GIB,
        free: GIB,
        floor: 10 * GIB,
        emergency: false,
        done: Done::Inodes,
        holders: &holders,
        checkout: None,
        home: Some(Path::new("/Users//u")),
    });
    assert!(text.contains('…'), "{text}");
    assert!(text.chars().count() <= LINE_CAP, "{}", text.chars().count());
    assert!(text.contains("inodes"), "{text}");
}

#[test]
fn rows_say_each_decision_and_the_reader_explains_the_lease() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let said = elected(
        &mut lease,
        &At::short(&h1, vec![seat("s-a", 12, true)]),
        &At::short(&h2, vec![seat("s-c", 3, true)]),
    );
    let r = row(T0 / 1000, KEY, &said, Level::Short, 20 * GIB, &h1);
    let v: Value = aterm_json::from_str(&r).expect("json");
    assert_eq!(v.get("kind").and_then(Value::as_str), Some("free-ask"));
    assert_eq!(v.get("said").and_then(Value::as_str), Some("elected"));
    assert_eq!(v.get("sid").and_then(Value::as_str), Some("s-a"));
    assert_eq!(v.get("rule").and_then(Value::as_str), Some(RULE_DISK_FREE));
    let s = step_row(T0 / 1000, "s-a", KEY, "wait:busy", &h1);
    let lines = why_lines(
        &lease,
        &[r.clone(), s, "{\"kind\":\"pass\"}".to_owned()],
        T0,
    );
    let all = lines.join("\n");
    assert!(all.contains("owed @s-a (host pid 1)"), "{all}");
    assert!(all.contains("lines 0/2; picked @s-a; no line yet"), "{all}");
    assert!(
        all.contains("elected @s-a dev:16777234 — @s-a (host pid 1) is asked"),
        "{all}"
    );
    assert!(all.contains("step @s-a wait:busy (dev:16777234)"), "{all}");
    assert!(
        !all.contains("pass"),
        "another kind's row is not this rule's: {all}"
    );
    assert_eq!(row_line(&r).map(|l| l.starts_with("2026-")), Some(true));
    assert!(
        why_lines(&Lease::default(), &[], T0)
            .join("\n")
            .contains("the lease is empty")
    );
}

#[test]
fn the_owners_line_says_who_was_asked_and_what_came_of_it() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let one = At::short(&h1, vec![seat("s-a", 12, true)]);
    let two = At::short(&h2, vec![seat("s-c", 3, true)]);
    elected(&mut lease, &one, &two);
    let entry = |l: &Lease| l.entry(KEY).cloned().expect("entry");
    assert_eq!(
        owner_line(&entry(&lease), T0 + NOMINATE_MS),
        None,
        "no line typed yet"
    );
    let t = T0 + NOMINATE_MS + 1;
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    let said = owner_line(&entry(&lease), t).expect("asked");
    assert!(said.starts_with("aterm asked @s-a at 2026-"), "{said}");
    two.step(&mut lease, t + ANSWER_MS);
    let said = owner_line(&entry(&lease), t + ANSWER_MS).expect("handing off");
    assert!(said.contains("one other session is asked"), "{said}");
    one.step(&mut lease, t + ANSWER_MS + 1);
    two.step(&mut lease, t + ANSWER_MS + 2);
    one.step(&mut lease, t + ANSWER_MS + NOMINATE_MS);
    assert!(typed(
        &mut lease,
        KEY,
        &h2,
        "s-c",
        t + ANSWER_MS + NOMINATE_MS,
        20 * GIB
    ));
    one.step(&mut lease, t + 2 * ANSWER_MS + NOMINATE_MS);
    let said = owner_line(&entry(&lease), t + 2 * ANSWER_MS + NOMINATE_MS).expect("out");
    assert!(said.contains("asked @s-a and @s-c"), "{said}");
    assert!(said.contains("no more agents are asked"), "{said}");
    let mut lease = Lease::default();
    let none = At::short(&h1, Vec::new());
    none.step(&mut lease, T0);
    none.step(&mut lease, T0 + NOMINATE_MS);
    assert!(
        owner_line(&entry(&lease), T0 + NOMINATE_MS)
            .is_some_and(|s| s.contains("only you can free it"))
    );
}

/// After a line went and freed nothing, a round whose sessions are all held at a wall or a box
/// waits (`said: waiting`), for days at a weekly limit: the owner's line says so, and who was
/// asked. It said one other session was being asked, while nobody was, as it read the hand-off
/// before the wait. Before the round's time is over, other hosts may yet nominate, so it says the
/// hand-off.
#[test]
fn the_owners_line_says_a_round_after_a_line_waits_on_held_sessions() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 12, true), seat("s-b", 3, true)]);
    let mut two = At::short(&h2, vec![seat("s-c", 5, true)]);
    let said = elected(&mut lease, &one, &two);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "{said:?}"
    );
    let t = T0 + NOMINATE_MS + 1;
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    // B and C come to walls before A's answer is due.
    one.seats = vec![seat("s-a", 12, true)];
    one.held = vec!["s-b".to_owned()];
    two.seats.clear();
    two.held = vec!["s-c".to_owned()];
    let missed = t + ANSWER_MS;
    assert!(matches!(
        one.step(&mut lease, missed),
        Said::Handoff {
            why: Miss::NothingFreed,
            ..
        }
    ));
    assert!(matches!(
        two.step(&mut lease, missed + 60_000),
        Said::Nominating { .. }
    ));
    let said = owner_line(lease.entry(KEY).expect("entry"), missed + 60_000).expect("handing off");
    assert!(
        said.contains("so one other session is asked"),
        "within the round's time: {said}"
    );
    let over = missed + NOMINATE_MS;
    let waiting = Said::Waiting {
        held: vec!["s-b".to_owned(), "s-c".to_owned()],
        why: vec![HeldBy::Limit],
    };
    assert_eq!(one.step(&mut lease, over), waiting);
    let days = over + 3 * 86_400_000;
    assert_eq!(two.step(&mut lease, days), waiting);
    let said = owner_line(lease.entry(KEY).expect("entry"), days).expect("waiting");
    assert!(
        said.starts_with("aterm asked @s-a; it freed under 1"),
        "{said}"
    );
    assert!(said.contains("usage limit"), "{said}");
    assert!(
        !said.contains("one other session is asked"),
        "nobody is asked: {said}"
    );
}

/// A round that waits on a session held by the owner's hold (or a person's take-over) or by a
/// draft left in its prompt says what holds it and what ends the wait, on the owner's line, in
/// `harness disk why`, in the journal's sentence and in its row: it is no usage limit and no
/// box, and the owner, told to look for one, was never told that lifting the hold, or sending or
/// clearing the draft, lets the ask go on. A session given back for either says so too. A
/// record an older build wrote names no hold, and is said as the two that build knew, as is a
/// hold word this build does not know.
#[test]
fn a_round_that_waits_on_a_hold_or_a_draft_says_what_holds_it_and_what_ends_the_wait() {
    let (h1, h2) = (host(1), host(2));
    for (hold, at, past) in [
        (
            HeldBy::Hold,
            "on hold (yours, or a person's take-over)",
            "its hold is lifted",
        ),
        (
            HeldBy::Draft,
            "with a draft left in its prompt",
            "its draft is sent or cleared",
        ),
    ] {
        let mut lease = Lease::default();
        let mut one = At::short(&h1, Vec::new());
        one.held = vec!["s-a".to_owned()];
        one.hold = hold;
        one.live = one.held.clone();
        one.supervised = one.held.clone();
        let two = At::short(&h2, Vec::new());
        assert!(matches!(one.step(&mut lease, T0), Said::Opened { .. }));
        let round = T0 + NOMINATE_MS;
        let said = two.step(&mut lease, round);
        assert_eq!(
            said,
            Said::Waiting {
                held: vec!["s-a".to_owned()],
                why: vec![hold]
            }
        );
        let e = lease.entry(KEY).cloned().expect("entry");
        let line = owner_line(&e, round).expect("waiting");
        assert_eq!(
            line,
            format!(
                "Every agent session that could free space is {at}: aterm asks one once {past} \
                 (`aterm harness disk why`)"
            )
        );
        let text = said.describe();
        let why = why_lines(&lease, &[], round).join("\n");
        assert!(text.contains(hold.at(false)), "{text}");
        assert!(
            why.contains(&format!("@s-a (host pid 1) {}", hold.at(false))),
            "{why}"
        );
        for said in [&line, &text, &why] {
            for not in ["usage limit", "box"] {
                assert!(!said.contains(not), "{hold:?}: {said}");
            }
        }
        let r = row(round / 1000, KEY, &said, Level::Short, 20 * GIB, &h2);
        let v: Value = aterm_json::from_str(&r).expect("json");
        assert_eq!(v.get("why").and_then(Value::as_str), Some(hold.word()));
        assert_eq!(
            Lease::decode(&lease.encode()),
            Lease {
                schema: 1,
                ..lease.clone()
            },
            "the hold round-trips the lease file"
        );
        let back = Said::Returned {
            from: Pick {
                host: h1.clone(),
                sid: "s-a".to_owned(),
            },
            why: hold,
        }
        .describe();
        assert!(
            back.contains(hold.at(false)) && back.contains(past),
            "{back}"
        );
    }
    // An older build's record names no hold; another session waits on a draft.
    let mut e = Entry {
        key: KEY.to_owned(),
        episode_ms: Some(T0),
        phase: Phase::Nominating {
            until_ms: T0,
            nominees: Vec::new(),
            after: None,
        },
        held: vec![
            Held {
                host: h1.clone(),
                sid: "s-a".to_owned(),
                why: None,
                orphan_ms: None,
            },
            Held {
                host: h2.clone(),
                sid: "s-c".to_owned(),
                why: Some(HeldBy::Draft),
                orphan_ms: None,
            },
        ],
        ..Entry::default()
    };
    assert_eq!(
        owner_line(&e, T0).as_deref(),
        Some(
            "Every agent session that could free space is at a usage limit, at a box only you can \
             answer or with a draft left in its prompt: aterm asks one once it is past or its \
             draft is sent or cleared (`aterm harness disk why`)"
        )
    );
    e.held.truncate(1);
    let lease = Lease {
        schema: 1,
        volumes: vec![e],
    };
    let unknown = lease
        .encode()
        .replace("\"sid\":\"s-a\"", "\"sid\":\"s-a\",\"why\":\"telepathy\"");
    assert!(unknown.contains("telepathy"), "{unknown}");
    assert_eq!(
        Lease::decode(&unknown),
        lease,
        "a hold this build does not know reads as none, not as an empty lease"
    );
}

#[test]
fn a_blind_claim_is_the_planted_defect_the_conformance_control_arms() {
    let h1 = host(1);
    let mut lease = Lease::default();
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", T0), Claim::Gone);
    assert_eq!(
        fault::with(fault::Fault::ClaimBlind, || claim(
            &mut lease, KEY, &h1, "s-a", T0
        )),
        Claim::Yours
    );
    assert_eq!(claim(&mut lease, KEY, &h1, "s-a", T0), Claim::Gone);
}

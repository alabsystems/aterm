// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the ask (`disk-free@v1`): the election, one line an episode across hosts, the
//! hand-off's bounds, the cooldown, the lease file and the line.

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

/// One host's look at `now`: `seats` are its candidates, every one of them supervised and live.
struct At<'a> {
    host: &'a HostId,
    level: Level,
    free: u64,
    done: Option<Done>,
    seats: Vec<Seat>,
    supervised: Vec<String>,
    live: Vec<String>,
    dead: Vec<u32>,
}

impl<'a> At<'a> {
    fn short(host: &'a HostId, seats: Vec<Seat>) -> At<'a> {
        let live: Vec<String> = seats.iter().map(|s| s.sid.clone()).collect();
        At {
            host,
            level: Level::Short,
            free: 20 * GIB,
            done: Some(Done::Spent),
            seats,
            supervised: live.clone(),
            live,
            dead: Vec::new(),
        }
    }

    fn step(&self, lease: &mut Lease, now_ms: i64) -> Said {
        let dead = self.dead.clone();
        let alive = move |h: &HostId| !dead.contains(&h.pid);
        step(
            lease,
            &Look {
                now_ms,
                host: self.host,
                key: KEY,
                level: self.level,
                free: self.free,
                done: self.done,
                seats: &self.seats,
                supervised: &self.supervised,
                live: &self.live,
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

#[test]
fn a_dead_hosts_nominee_is_not_elected() {
    let (h1, h2) = (host(1), host(2));
    let mut lease = Lease::default();
    let mut one = At::short(&h1, vec![seat("s-a", 2, true)]);
    let two = At::short(&h2, vec![seat("s-c", 30, false)]);
    one.dead = vec![2];
    let said = elected(&mut lease, &one, &two);
    assert!(
        matches!(&said, Said::Elected { pick, .. } if pick.sid == "s-a"),
        "{said:?}"
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
    one.level = Level::Low;
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
            last: Some(Miss::NothingFreed)
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
            } => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(elected_sids, ["s-0", "s-1", "s-2", "s-3"]);
    assert_eq!(lease.entry(KEY).expect("entry").asks, 0);
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
            last: Some(Miss::Released)
        },
        "its one session was picked already: nobody is left to ask"
    );
    let mut lease = Lease::default();
    let none = At::short(&h1, Vec::new());
    assert!(matches!(none.step(&mut lease, T0), Said::Opened { .. }));
    assert_eq!(
        none.step(&mut lease, T0 + NOMINATE_MS),
        Said::Out { last: None }
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
            Said::Handoff {
                why: Miss::Undelivered,
                ..
            }
        ),
        "its host ended before the line"
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
    assert_eq!(owner_line(&entry(&lease)), None, "no line typed yet");
    let t = T0 + NOMINATE_MS + 1;
    assert!(typed(&mut lease, KEY, &h1, "s-a", t, 20 * GIB));
    let said = owner_line(&entry(&lease)).expect("asked");
    assert!(said.starts_with("aterm asked @s-a at 2026-"), "{said}");
    two.step(&mut lease, t + ANSWER_MS);
    let said = owner_line(&entry(&lease)).expect("handing off");
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
    let said = owner_line(&entry(&lease)).expect("out");
    assert!(said.contains("asked @s-a and @s-c"), "{said}");
    assert!(said.contains("no more agents are asked"), "{said}");
    let mut lease = Lease::default();
    let none = At::short(&h1, Vec::new());
    none.step(&mut lease, T0);
    none.step(&mut lease, T0 + NOMINATE_MS);
    assert!(owner_line(&entry(&lease)).is_some_and(|s| s.contains("only you can free it")));
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

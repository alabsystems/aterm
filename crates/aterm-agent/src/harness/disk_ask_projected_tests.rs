// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the ask before the disk fills (the design's §8.3): a look pressed by a projection
//! opens an episode on its plain floor and records its projection apart; no look ends an episode
//! while a projection record stands, and a warm look that projects nothing drops its own; a
//! projected episode's windows fit the time its crossing leaves; a rise settles it only once the
//! crossing is gone or its answer window ends; and the line says the projection.

use std::path::Path;

use super::*;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:16777234";
const T0: i64 = 1_792_000_000_000;
const MIN: i64 = 60_000;

/// The design's night: 130.8 GiB an hour.
const RATE_BPS: u64 = 39_012_620;

/// The volume's size: at 75 GiB free it is 84% full.
const TOTAL: u64 = 470 * GIB;

/// The plain floor of the projecting host's looks.
const FLOOR: u64 = 47 * GIB;

/// The inode line every host reads an emergency below.
const FILES_FLOOR: u64 = crate::harness::disk::EMERGENCY_FILES_FREE;

fn host(pid: u32) -> HostId {
    HostId {
        pid,
        start: format!("start-{pid}"),
    }
}

/// A projection at the design's rate whose crossing of `floor` is `eta_ms` away.
fn crossing_in(eta_ms: i64, floor: u64) -> Projection {
    Projection {
        rate_bps: RATE_BPS,
        eta_ms,
        need: RATE_BPS.saturating_mul(7_200),
        floor,
    }
}

fn seat_a() -> Seat {
    Seat {
        sid: "s-a".to_owned(),
        held: 5 * GIB,
        at_break: true,
    }
}

/// One host's look: its free figure, its plain floor (it reads the volume short below it), what
/// its ring projects, whether that ring is cold, its seats (each supervised and live), and the
/// hosts whose processes ended.
struct At<'a> {
    host: &'a HostId,
    free: u64,
    floor: u64,
    projected: Option<Projection>,
    cold: bool,
    seats: Vec<Seat>,
    dead: Vec<u32>,
}

impl<'a> At<'a> {
    fn new(host: &'a HostId, floor: u64, free: u64) -> At<'a> {
        At {
            host,
            free,
            floor,
            projected: None,
            cold: false,
            seats: Vec::new(),
            dead: Vec::new(),
        }
    }

    /// Projecting a crossing `eta_ms` away.
    fn projecting(mut self, eta_ms: i64) -> At<'a> {
        self.projected = Some(crossing_in(eta_ms, self.floor));
        self
    }

    fn step(&self, lease: &mut Lease, now_ms: i64) -> Said {
        let dead = self.dead.clone();
        let alive = move |h: &HostId| !dead.contains(&h.pid);
        let live: Vec<String> = self.seats.iter().map(|s| s.sid.clone()).collect();
        step(
            lease,
            &Look {
                now_ms,
                host: self.host,
                key: KEY,
                level: if self.free < self.floor {
                    Level::Short
                } else {
                    Level::Low
                },
                free: self.free,
                floor: self.floor,
                files_free: None,
                files_floor: FILES_FLOOR,
                done: Some(Done::Spent),
                seats: &self.seats,
                supervised: &live,
                barred: &[],
                live: &live,
                held: &[],
                reclaimed: &[],
                passing: false,
                alive: &alive,
                ahead: Ahead {
                    projected: self.projected,
                    cold: self.cold,
                    ..Ahead::default()
                },
            },
        )
    }
}

fn entry(lease: &Lease) -> &Entry {
    lease.entry(KEY).expect("the volume's entry")
}

fn ended(said: &Said) -> bool {
    matches!(said, Said::Ended { .. })
}

#[test]
fn a_look_pressed_by_a_projection_above_its_floor_opens_an_episode_on_its_plain_floor() {
    let p = host(1);
    let mut lease = Lease::default();
    let mut at = At::new(&p, FLOOR, 75 * GIB);
    assert_eq!(
        at.step(&mut lease, T0),
        Said::Quiet,
        "past its floor and projecting nothing, nothing opens"
    );
    assert!(lease.entry(KEY).is_none());
    let crossing = Projection::of(RATE_BPS, at.free, FLOOR, Some(TOTAL));
    at.projected = Some(crossing);
    assert_eq!(
        at.step(&mut lease, T0),
        Said::Opened {
            done: Done::Spent,
            projected: Some(crossing)
        }
    );
    let e = entry(&lease);
    assert_eq!(
        e.floor,
        Some(Floor {
            host: p.clone(),
            bytes: FLOOR
        }),
        "the floor is the plain one, never the floor plus the horizon's fill"
    );
    assert_eq!(
        e.projected,
        [Projected {
            host: p.clone(),
            rate_bps: crossing.rate_bps,
            eta_ms: crossing.eta_ms,
            floor: FLOOR,
            since_ms: T0,
            at_ms: T0,
        }]
    );
    // Its next projecting look refreshes its record, keeping when its projection began.
    at.free = 73 * GIB;
    let later = Projection::of(RATE_BPS, at.free, FLOOR, Some(TOTAL));
    at.projected = Some(later);
    assert!(!ended(&at.step(&mut lease, T0 + MIN)));
    let e = entry(&lease);
    assert_eq!(
        e.projected
            .iter()
            .map(|r| (r.since_ms, r.at_ms, r.eta_ms))
            .collect::<Vec<_>>(),
        [(T0, T0 + MIN, later.eta_ms)]
    );
    assert_eq!(e.floor.as_ref().map(|f| f.bytes), Some(FLOOR));
}

#[test]
fn no_look_ends_an_episode_while_another_live_hosts_projection_stands() {
    let (p, q) = (host(1), host(2));
    let mut lease = Lease::default();
    let pa = At::new(&p, FLOOR, 75 * GIB).projecting(40 * MIN);
    assert!(matches!(
        pa.step(&mut lease, T0),
        Said::Opened {
            projected: Some(_),
            ..
        }
    ));
    // Q's floor the volume clears by far, and Q projects nothing: P's record holds the episode.
    let qa = At::new(&q, 10 * GIB, 75 * GIB);
    for after in [MIN, 10 * MIN, COLD_HOLD_MS - MIN] {
        let said = qa.step(&mut lease, T0 + after);
        assert!(!ended(&said), "{after} ms after P's look: {said:?}");
        assert_eq!(entry(&lease).projected.len(), 1, "P's record stands");
    }
    // Unrefreshed for COLD_HOLD_MS, it stands no more: Q's look drops it and ends the episode.
    assert_eq!(
        qa.step(&mut lease, T0 + COLD_HOLD_MS),
        Said::Ended { asks: 0 }
    );
}

#[test]
fn an_ended_hosts_projection_stands_only_at_a_cold_look_and_only_within_the_cold_hold() {
    let (p, q, successor) = (host(1), host(2), host(3));
    let pa = At::new(&p, FLOOR, 75 * GIB).projecting(40 * MIN);
    // At another host's warm look an ended host's record stands no more: the episode ends.
    let mut lease = Lease::default();
    assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
    let mut qa = At::new(&q, 10 * GIB, 75 * GIB);
    qa.dead = vec![1];
    assert_eq!(qa.step(&mut lease, T0 + MIN), Said::Ended { asks: 0 });
    // A seamless update's successor, its ring cold, keeps the record for COLD_HOLD_MS after it was
    // last refreshed, then ends the episode.
    let mut lease = Lease::default();
    assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
    let mut sa = At::new(&successor, FLOOR, 75 * GIB);
    sa.cold = true;
    sa.dead = vec![1];
    for after in [2 * MIN, 20 * MIN, COLD_HOLD_MS - MIN] {
        let said = sa.step(&mut lease, T0 + after);
        assert!(!ended(&said), "{after} ms after P's look: {said:?}");
        assert_eq!(
            entry(&lease)
                .projected
                .iter()
                .map(|r| r.host.pid)
                .collect::<Vec<_>>(),
            [1],
            "the ended host's record stands at the cold look"
        );
    }
    assert_eq!(
        sa.step(&mut lease, T0 + COLD_HOLD_MS),
        Said::Ended { asks: 0 }
    );
}

#[test]
fn a_warm_look_that_projects_nothing_drops_its_own_record_and_then_ends_the_episode() {
    let p = host(1);
    let mut lease = Lease::default();
    let mut pa = At::new(&p, FLOOR, 75 * GIB).projecting(40 * MIN);
    assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
    // Its projection lapsed within the margin past its floor: its record goes, the episode stays.
    pa.projected = None;
    pa.free = 50 * GIB;
    let said = pa.step(&mut lease, T0 + MIN);
    assert!(!ended(&said), "{said:?}");
    assert!(
        entry(&lease).projected.is_empty(),
        "its own record is dropped"
    );
    assert!(entry(&lease).episode_ms.is_some());
    // Past its floor by the margin nothing holds the episode: it ends.
    pa.free = 60 * GIB;
    assert_eq!(pa.step(&mut lease, T0 + 2 * MIN), Said::Ended { asks: 0 });
    // Its own record never holds its own warm look: past the margin the look ends it at once.
    let mut lease = Lease::default();
    let mut pa = At::new(&p, FLOOR, 75 * GIB).projecting(40 * MIN);
    assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
    pa.projected = None;
    assert_eq!(pa.step(&mut lease, T0 + MIN), Said::Ended { asks: 0 });
}

#[test]
fn a_projected_episodes_windows_are_a_third_of_its_crossing_and_at_least_five_minutes() {
    let p = host(1);
    for (eta, owed, asked) in [
        // A crossing 39 minutes away: 13 minutes to come to a turn break; typed a minute later,
        // a third of the 38 left to answer.
        (39 * MIN, 13 * MIN, 38 * MIN / 3),
        // 6 minutes: a third is 2, so the least window.
        (6 * MIN, FLOOR_WINDOW_MS, FLOOR_WINDOW_MS),
        // 119 minutes: a third is past each cap.
        (119 * MIN, DELIVER_MS, ANSWER_MS),
    ] {
        let mut lease = Lease::default();
        let mut pa = At::new(&p, FLOOR, 75 * GIB).projecting(eta);
        pa.seats = vec![seat_a()];
        assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
        let now = T0 + NOMINATE_MS;
        let said = pa.step(&mut lease, now);
        assert!(matches!(said, Said::Elected { .. }), "{said:?}");
        let Phase::Owed { until_ms, .. } = entry(&lease).phase else {
            panic!("owed: {:?}", entry(&lease).phase)
        };
        assert_eq!(
            until_ms - now,
            owed,
            "the line owed, crossing {eta} ms away"
        );
        assert!(typed(&mut lease, KEY, &p, "s-a", now + MIN, 75 * GIB));
        let Phase::Asked { until_ms, .. } = entry(&lease).phase else {
            panic!("asked: {:?}", entry(&lease).phase)
        };
        assert_eq!(
            until_ms - (now + MIN),
            asked,
            "the line asked, crossing {eta} ms away"
        );
    }
    // With no projection recorded, below the floor, the windows are revision 1's.
    let mut lease = Lease::default();
    let mut pa = At::new(&p, FLOOR, 40 * GIB);
    pa.seats = vec![seat_a()];
    assert!(matches!(
        pa.step(&mut lease, T0),
        Said::Opened {
            projected: None,
            ..
        }
    ));
    let now = T0 + NOMINATE_MS;
    assert!(matches!(pa.step(&mut lease, now), Said::Elected { .. }));
    let Phase::Owed { until_ms, .. } = entry(&lease).phase else {
        panic!("owed: {:?}", entry(&lease).phase)
    };
    assert_eq!(until_ms - now, DELIVER_MS);
    assert!(typed(&mut lease, KEY, &p, "s-a", now + MIN, 40 * GIB));
    let Phase::Asked { until_ms, .. } = entry(&lease).phase else {
        panic!("asked: {:?}", entry(&lease).phase)
    };
    assert_eq!(until_ms - (now + MIN), ANSWER_MS);
}

/// A projected episode on host `p` whose one seat, `s-a`, took its line at 49 GiB free (past the
/// floor, within the margin), its crossing 39 minutes away: the lease and its answer window's end.
fn asked_projected(p: &HostId) -> (Lease, i64) {
    let mut lease = Lease::default();
    let mut pa = At::new(p, FLOOR, 49 * GIB).projecting(39 * MIN);
    pa.seats = vec![seat_a()];
    assert!(matches!(pa.step(&mut lease, T0), Said::Opened { .. }));
    assert!(matches!(
        pa.step(&mut lease, T0 + NOMINATE_MS),
        Said::Elected { .. }
    ));
    assert!(typed(
        &mut lease,
        KEY,
        p,
        "s-a",
        T0 + NOMINATE_MS + MIN,
        49 * GIB
    ));
    let Phase::Asked { until_ms, .. } = entry(&lease).phase else {
        panic!("asked: {:?}", entry(&lease).phase)
    };
    (lease, until_ms)
}

#[test]
fn a_rise_settles_a_projected_episode_only_once_its_crossing_is_gone_or_its_answer_window_ends() {
    let p = host(1);
    let mut pa = At::new(&p, FLOOR, 49 * GIB + FREED_MIN).projecting(30 * MIN);
    pa.seats = vec![seat_a()];
    // Still projecting, inside the answer window: a rise of FREED_MIN settles nothing.
    let (mut lease, until) = asked_projected(&p);
    let said = pa.step(&mut lease, until - MIN);
    assert!(
        matches!(said, Said::Asked { rose, .. } if rose == FREED_MIN),
        "{said:?}"
    );
    assert!(matches!(entry(&lease).phase, Phase::Asked { .. }));
    // At the window's end, still projecting, it settles: the session freed something.
    assert_eq!(
        pa.step(&mut lease, until),
        Said::Freed {
            sid: "s-a".to_owned(),
            freed: FREED_MIN
        }
    );
    // Once the crossing is gone, the same rise settles it at once.
    let (mut lease, until) = asked_projected(&p);
    pa.projected = None;
    assert_eq!(
        pa.step(&mut lease, until - 5 * MIN),
        Said::Freed {
            sid: "s-a".to_owned(),
            freed: FREED_MIN
        }
    );
    // Less than FREED_MIN by the window's end hands the ask on, as in revision 1.
    let (mut lease, until) = asked_projected(&p);
    let mut pa = At::new(&p, FLOOR, 49 * GIB + FREED_MIN / 2).projecting(30 * MIN);
    pa.seats = vec![seat_a()];
    assert!(matches!(
        pa.step(&mut lease, until),
        Said::Handoff {
            why: Miss::NothingFreed,
            ..
        }
    ));
}

/// The asked tab's own build cache, 61 GiB, for its checkout `~/aterm`.
fn own_holder() -> Hog {
    Hog {
        path: "/Users//u/Library/Caches/aterm/cargo-build.noindex/aterm/.cargo-build".into(),
        bytes: 61 * GIB,
        bytes_partial: false,
        why: "cargo build output".to_owned(),
        serves: Some("/Users//u/aterm".into()),
    }
}

/// The line at `mount` with `free` bytes free, naming `holders`, while the design's fill
/// projects its crossing of [`FLOOR`] 13 minutes from 75 GiB.
fn projected_line(mount: &str, free: u64, holders: &[Hog]) -> String {
    line(&Line {
        mount: Path::new(mount),
        total: TOTAL,
        free,
        floor: FLOOR,
        emergency: false,
        done: Done::Spent,
        holders,
        checkout: Some(Path::new("/Users//u/aterm")),
        shared: false,
        home: Some(Path::new("/Users//u")),
        projected: Some(Projection::of(RATE_BPS, 75 * GIB, FLOOR, Some(TOTAL))),
    })
}

#[test]
fn the_line_says_the_projection_in_place_of_how_near_the_floor_is() {
    let own = [own_holder()];
    let above = projected_line("/", 75 * GIB, &own);
    assert!(
        above.contains(
            "is 84% full, 75.0 GiB free, falling 130.8 GiB/h: at that rate it is under its \
             47.0 GiB floor in ~13 min, and aterm has removed every build cache it may. Largest \
             left: ~/aterm's builds 61.0 GiB (this tab's)."
        ),
        "{above}"
    );
    assert!(!above.contains("within"), "{above}");
    assert!(
        above.chars().count() <= LINE_CAP,
        "{}",
        above.chars().count()
    );
    let below = projected_line("/", 40 * GIB, &own);
    assert!(
        below.contains(
            "40.0 GiB free, below its 47.0 GiB floor and falling 130.8 GiB/h, and aterm has"
        ),
        "{below}"
    );
    assert!(!below.contains("at that rate"), "{below}");
    assert!(
        below.chars().count() <= LINE_CAP,
        "{}",
        below.chars().count()
    );
}

/// On the home volume's ordinary macOS mount the projection's words with its fill rate run the
/// line past its cap: the volume's name gives way first (clipped in its middle), then the rate,
/// so every line keeps within the cap and names the asked tab's own holder whole. Before this the
/// line ran past the cap a composer holds as typed text (646 characters with no holder), and
/// with the holder it named none, as no ladder step had room for its path.
#[test]
fn a_projected_line_too_long_with_its_fill_rate_says_its_crossing_without_it_within_the_cap() {
    let own = [own_holder()];
    let rule = |line: &str| {
        assert!(line.chars().count() <= LINE_CAP, "{}", line.chars().count());
        assert!(line.ends_with("(rule disk-free@v1)"), "{line}");
    };
    let held = projected_line("/System/Volumes/Data", 75 * GIB, &own);
    rule(&held);
    assert!(
        held.contains(
            "(/System/…/Data) is 84% full, 75.0 GiB free, falling 130.8 GiB/h: at that rate it is \
             under its 47.0 GiB floor in ~13 min, and aterm has removed every build cache it may. \
             Largest left: ~/aterm's builds 61.0 GiB (this tab's). Free what"
        ),
        "the volume gives way, the rate and the holder stay: {held}"
    );
    let bare = projected_line("/System/Volumes/Data", 75 * GIB, &[]);
    rule(&bare);
    assert!(
        bare.contains(
            "75.0 GiB free, under its 47.0 GiB floor in ~13 min, and aterm has removed every build \
             cache it may. aterm's report names no holder it can size"
        ),
        "with no holder even the volume's room is not enough, and the rate goes: {bare}"
    );
    for holders in [&own[..], &[][..]] {
        let below = projected_line("/System/Volumes/Data", 40 * GIB, holders);
        rule(&below);
        assert!(
            below.contains("40.0 GiB free, below its 47.0 GiB floor and falling 130.8 GiB/h"),
            "{below}"
        );
    }
    // The fill rate goes only from a line too long with it: at `/` the holder still leaves room.
    assert!(projected_line("/", 75 * GIB, &own).contains("falling 130.8 GiB/h"));
}

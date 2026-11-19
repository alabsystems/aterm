// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::events::{Detector, EventKind};
use crate::model::{
    MemCounters, MemRates, MemSample, MemView, ProcView, Sample, Snapshot, TabView, Vitals,
};

const GIB: u64 = 1 << 30;
const PAGE: u64 = 16_384;
/// The Mac of 2026-11-15: 128 GiB.
const RAM: u64 = 128 * GIB;
const S: u64 = 1_000_000_000;

/// The incident's counters as its first WindowServer report read them at 23:16:09.8 (pages).
#[derive(Clone, Copy)]
struct Counters {
    compressions: u64,
    decompressions: u64,
    reactivations: u64,
    swapins: u64,
    swapouts: u64,
    pageins: u64,
}

const AT_231609: Counters = Counters {
    compressions: 1_131_136_030,
    decompressions: 960_115_961,
    reactivations: 1_116_900_559,
    swapins: 5_077_767,
    swapouts: 24_800_003,
    pageins: 1_583_396_492,
};

/// …and as its second, at 23:20:24.7, 254.93 s later.
const AT_232024: Counters = Counters {
    compressions: 1_135_357_722,
    decompressions: 960_164_489,
    reactivations: 1_118_299_206,
    swapins: 5_081_147,
    swapouts: 24_831_383,
    pageins: 1_583_753_674,
};

/// The counters `back_ms` before 23:16:09.8, run back at the rates the two reports read between
/// them (16 560 compressions, 190 decompressions and 5 486 reactivations a second). No reading
/// exists before 23:16:09.8: these stand in for what a reading then would have read, on the
/// research's inference that compression had run for minutes by then.
fn before_231609(back_ms: u64) -> Counters {
    let back = |at: u64, then: u64| at - (then - at) * back_ms / 254_930;
    Counters {
        compressions: back(AT_231609.compressions, AT_232024.compressions),
        decompressions: back(AT_231609.decompressions, AT_232024.decompressions),
        reactivations: back(AT_231609.reactivations, AT_232024.reactivations),
        swapins: back(AT_231609.swapins, AT_232024.swapins),
        swapouts: back(AT_231609.swapouts, AT_232024.swapouts),
        pageins: back(AT_231609.pageins, AT_232024.pageins),
    }
}

/// A Mac's memory reading as the host makes it of `aterm_sysprobe`'s (`vitals_host::mem_sample`):
/// `headroom` and `kept_permille` as the macOS reader derives them from the page queues (its own
/// tests replay the same report), the counters in pages, svp unread so every compression counts.
fn mac(headroom: u64, kept_permille: u16, c: Counters) -> MemSample {
    MemSample {
        total: RAM,
        headroom: Some(headroom),
        kept_permille: Some(kept_permille),
        counters: Some(MemCounters {
            swapin_bytes: c.swapins * PAGE,
            swapout_bytes: c.swapouts * PAGE,
            decompressions: c.decompressions,
            compressions: c.compressions,
            pageins: c.pageins,
            page_bytes: PAGE,
            pushed: Some(c.compressions),
            refaulted: Some(c.decompressions + c.reactivations),
            stall_us: None,
        }),
        ..MemSample::default()
    }
}

/// A machine-only reading at `unix_ms` (`mono` its monotonic clock, ms).
fn reading(mono_ms: u64, unix_ms: i64, mem: MemSample) -> Sample {
    Sample {
        at_ns: mono_ms * 1_000_000,
        mono_ms,
        at_unix_ms: unix_ms,
        cadence_ms: 10_000,
        machine_only: true,
        mem: Some(mem),
        ..Sample::default()
    }
}

fn starving_edges(snap: &Snapshot) -> Vec<&Edge> {
    snap.events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Starving(edge) => Some(edge.as_ref()),
            _ => None,
        })
        .collect()
}

/// 2026-11-15 23:16:09.8, unix ms (PST).
const UNIX_231609: i64 = 1_763_277_369_800;

/// THE INCIDENT OPENS AN EPISODE BEFORE WINDOWSERVER'S FIRST KILL (23:16:11). At 23:16:09.8 the
/// Mac read 5.8 GiB of headroom (382 055 pages: its free ones, the stackshot's free count read
/// as `host_statistics64`'s, speculative pages among them, which is not checked against xnu; and
/// its purgeable ones, its file cache under the kernel's floor), under the 8 GiB reserve, and
/// pushed 259 MiB/s out and read 89 MiB/s back, over both floors; readings ten seconds apart from
/// 23:15:49.8 (the hidden cadence) open the episode at 23:16:09.8, its second starving reading,
/// for the thrash rule alone (no wall read, the kernel's level unread). The 23:20:24.7 report
/// keeps it open. The records hold no reading before 23:16:09.8: the two earlier readings take
/// their counters run back at the rates between the two reports and keep 23:16:09.8's queues
/// (its headroom and its kernel's share), so whether it would have opened before the sweep
/// stalled (23:12:22) cannot be said. The stackshots carry no single-value split of their
/// compressions, so every compression counts as pushed here; the shipped reader subtracts the
/// single-value ones (`aterm_sysprobe`'s `mac_pushed`), which makes 259 MiB/s an upper bound
/// (the_incident_opens_whatever_share_of_its_compressions_held_a_single_value holds the opening
/// without it). NEGATIVE CONTROLS: the first starving reading alone opens nothing, and a 32nd of
/// memory as the reserve (4 GiB) finds 23:16:09.8's look starving by no rule.
#[test]
fn the_incident_opens_an_episode_before_windowserver_was_killed() {
    let headroom = 382_055 * PAGE;
    let kept = 468;
    let mut v = Vitals::new();
    let at = |back_ms: u64| {
        reading(1_000_000 - back_ms, UNIX_231609 - back_ms as i64, {
            mac(headroom, kept, before_231609(back_ms))
        })
    };
    assert!(
        starving_edges(&v.absorb(&at(20_000))).is_empty(),
        "23:15:49.8: no rates"
    );
    let second = v.absorb(&at(10_000));
    assert!(
        starving_edges(&second).is_empty(),
        "one starving reading opens nothing"
    );
    assert!(second.starving.is_none());
    let third = v.absorb(&reading(
        1_000_000,
        UNIX_231609,
        mac(headroom, kept, AT_231609),
    ));
    let edges = starving_edges(&third);
    let [Edge::Opened(e)] = edges.as_slice() else {
        panic!("23:16:09.8 opens the episode: {edges:?}");
    };
    assert_eq!(e.why, Why::Thrash);
    assert_eq!(e.opened_unix_ms, UNIX_231609);
    let push = e.at_open.push_bps.expect("pushed");
    let back = e.at_open.refault_bps.expect("back");
    assert_eq!((push / 1_048_576.0).round(), 259.0, "{push}");
    assert_eq!((back / 1_048_576.0).round(), 89.0, "{back}");
    assert_eq!(third.starving.as_deref(), Some(e));
    let mut narrower = e.at_open;
    narrower.headroom = narrower
        .headroom
        .map(|h| h + (reserve_bytes(RAM) - RAM / 32));
    assert_eq!(
        narrower.starving(),
        None,
        "the headroom over a 32nd of memory: no rule holds"
    );
    let fourth = v.absorb(&reading(
        1_254_930,
        UNIX_231609 + 254_930,
        mac(7_694 * PAGE, 403, AT_232024),
    ));
    assert!(
        starving_edges(&fourth).is_empty(),
        "23:20:24.7 keeps it open"
    );
    assert!(fourth.starving.is_some());
}

/// A look's memory with these rates over `window_s`.
#[derive(Clone, Copy, Default)]
struct At {
    ram: u64,
    push: f64,
    back: f64,
    anon: f64,
    headroom: Option<u64>,
    wall: Option<(u64, u64)>,
    kept: Option<u16>,
    pressure: Pressure,
    stall: Option<(f64, f64)>,
    swap_used: Option<u64>,
    compressed: Option<u64>,
}

fn mem(a: At, window_s: u64) -> MemView {
    MemView {
        sample: MemSample {
            total: a.ram,
            headroom: a.headroom,
            wall: a.wall,
            kept_permille: a.kept,
            pressure: a.pressure,
            swap_used: a.swap_used,
            compressed: a.compressed,
            ..MemSample::default()
        },
        rates: Some(MemRates {
            push_bps: Some(a.push),
            refault_bps: Some(a.back),
            anon_back_bps: a.anon,
            stall: a.stall,
            window_ns: window_s * S,
            ..MemRates::default()
        }),
        ..MemView::default()
    }
}

fn snap_at(t_s: u64, m: MemView) -> Snapshot {
    Snapshot {
        at_ns: t_s * S,
        at_unix_ms: i64::try_from(t_s * 1000).unwrap(),
        mem: Some(m),
        ..Snapshot::default()
    }
}

/// Feeds `looks` at `every_s` apart from `from_s` and returns each reading's edge.
fn run(w: &mut Watch, from_s: u64, every_s: u64, looks: &[At]) -> Vec<Option<Edge>> {
    looks
        .iter()
        .enumerate()
        .map(|(i, a)| w.step(&snap_at(from_s + every_s * i as u64, mem(*a, every_s))))
        .collect()
}

fn opened(edges: &[Option<Edge>]) -> Option<&Episode> {
    edges.iter().flatten().find_map(|e| match e {
        Edge::Opened(e) => Some(e),
        Edge::Ended(_) => None,
    })
}

fn ended(edges: &[Option<Edge>]) -> Option<&Episode> {
    edges.iter().flatten().find_map(|e| match e {
        Edge::Ended(e) => Some(e),
        Edge::Opened(_) => None,
    })
}

const MIB: f64 = 1_048_576.0;

/// The incident's look at 23:16:09.8 (its pages pushed counted whole).
const INCIDENT: At = At {
    ram: RAM,
    push: 259.0 * MIB,
    back: 89.0 * MIB,
    anon: 3.0 * MIB,
    headroom: Some(382_055 * PAGE),
    wall: None,
    kept: Some(468),
    pressure: Pressure::Unknown,
    stall: None,
    swap_used: None,
    compressed: None,
};

/// THE INCIDENT OPENS WHATEVER SHARE OF ITS COMPRESSIONS HELD A SINGLE VALUE. The stackshots
/// record no single-value split, and the shipped reader pushes only the rest: this Mac's own
/// counters since its post-panic boot read 78 % single-value. At that share 23:16:09.8 pushed
/// 57 MiB/s, under the 64 MiB/s thrash floor of 128 GiB, so the thrash rule alone would not have
/// opened; the incident's logs and footprints add what carries it: the kernel delivered critical
/// from 23:15:16, and its compressor stood at about 95 % of its segments (an estimate from the
/// processes' footprints; no reading of it survives), so the wall's rule opens it at the same
/// reading, and the kernel's where the wall is unread (a macOS before 27.2). NEGATIVE CONTROL:
/// the same look at 78 % with neither the wall nor the reported level opens none.
#[test]
fn the_incident_opens_whatever_share_of_its_compressions_held_a_single_value() {
    for share in [0.0, 0.5, 0.78, 0.95] {
        let pushed = At {
            push: INCIDENT.push * (1.0 - share),
            ..INCIDENT
        };
        for (a, why) in [
            (
                At {
                    wall: Some((950, 1000)),
                    pressure: Pressure::Critical,
                    ..pushed
                },
                if share < 0.75 { Why::Thrash } else { Why::Wall },
            ),
            (
                At {
                    pressure: Pressure::Critical,
                    ..pushed
                },
                if share < 0.75 {
                    Why::Thrash
                } else {
                    Why::Kernel
                },
            ),
        ] {
            let mut w = Watch::default();
            let edges = run(&mut w, 0, 10, &[a; 2]);
            let e = opened(&edges).unwrap_or_else(|| panic!("share {share} opens"));
            assert_eq!((e.why, e.opened_unix_ms), (why, 10_000), "share {share}");
        }
    }
    let bare = At {
        push: INCIDENT.push * 0.22,
        ..INCIDENT
    };
    let mut w = Watch::default();
    assert!(opened(&run(&mut w, 0, 10, &[bare; 30])).is_none());
}

/// A HEALTHY MACHINE OPENS NO EPISODE, however full. Recorded: this Mac idle at 18:47 on
/// 2026-11-16 (78 GiB of headroom, 0 compressions and 0.4 decompressions a second). Constructed
/// just under a floor, not recorded (no reading of a busy full-cache Mac or of an 8 or 16 GiB
/// one in ordinary use exists here): a build with its headroom under the reserve, the
/// compressor taking idle pages at 8 MiB/s and 6 MiB/s coming back; a large file cache (its
/// headroom over the reserve) with pages pushed and read back over their floors; a stale
/// compressor and 40 GiB of stale swap with nothing moving; a wall of swapped segments at 92 %
/// with nothing moving, with a build reading its file cache back (1 100 reactivations a second,
/// no anonymous page among them), or with idle pages compressed at 6 MiB/s; the kernel critical
/// with nothing moving; a 16 GiB Mac in heavy use (warn, 0.5 GiB of headroom, 23 MiB/s pushed
/// and 6 back); Linux's PSI `some` at 30 % with `full` under 1 %. Every one held for ten minutes
/// of readings at 1, 5 and 10 s. NEGATIVE CONTROL: the incident's look, held the same, opens one.
#[test]
fn a_healthy_machine_opens_no_episode() {
    let stale_wall = At {
        ram: RAM,
        headroom: Some(2 * GIB),
        wall: Some((920, 1000)),
        pressure: Pressure::Warn,
        ..At::default()
    };
    let healthy = [
        At {
            ram: RAM,
            push: 0.0,
            back: 0.4 * 16_384.0,
            anon: 0.4 * 16_384.0,
            headroom: Some(78 * GIB),
            kept: Some(988),
            pressure: Pressure::Normal,
            ..At::default()
        },
        At {
            ram: RAM,
            push: 8.0 * MIB,
            back: 6.0 * MIB,
            anon: 2.0 * MIB,
            headroom: Some(6 * GIB),
            kept: Some(960),
            pressure: Pressure::Normal,
            ..At::default()
        },
        At {
            ram: RAM,
            push: 300.0 * MIB,
            back: 100.0 * MIB,
            anon: 20.0 * MIB,
            headroom: Some(40 * GIB),
            kept: Some(900),
            pressure: Pressure::Normal,
            ..At::default()
        },
        At {
            ram: RAM,
            headroom: Some(2 * GIB),
            compressed: Some(GIB * 3 / 2),
            swap_used: Some(40 * GIB),
            kept: Some(700),
            pressure: Pressure::Warn,
            ..At::default()
        },
        stale_wall,
        At {
            back: 1_100.0 * 16_384.0,
            ..stale_wall
        },
        At {
            push: 6.0 * MIB,
            ..stale_wall
        },
        At {
            ram: RAM,
            headroom: Some(GIB / 2),
            kept: Some(330),
            pressure: Pressure::Critical,
            ..At::default()
        },
        At {
            ram: 16 * GIB,
            push: 23.0 * MIB,
            back: 6.0 * MIB,
            anon: 1.0 * MIB,
            headroom: Some(GIB / 2),
            kept: Some(480),
            pressure: Pressure::Warn,
            ..At::default()
        },
        At {
            ram: 32 * GIB,
            headroom: Some(20 * GIB),
            stall: Some((0.30, 0.005)),
            pressure: Pressure::Warn,
            ..At::default()
        },
    ];
    for (i, a) in healthy.iter().enumerate() {
        for every in [1, 5, 10] {
            let mut w = Watch::default();
            let edges = run(&mut w, 0, every, &vec![*a; (600 / every) as usize]);
            assert!(opened(&edges).is_none(), "healthy look {i} at {every} s");
            assert!(w.open().is_none());
        }
    }
    let mut w = Watch::default();
    assert!(opened(&run(&mut w, 0, 10, &[INCIDENT; 60])).is_some());
}

/// A BURST OPENS NONE: three seconds of the incident's rates at a one-second cadence; one
/// ten-second reading whose window holds a burst, then quiet; at five seconds, a burst two
/// windows hold (about six seconds of it straddling them), since the hold counts only the
/// windows after the first starving reading. NEGATIVE CONTROLS: at one a second it opens at the
/// eleventh starving reading (ten seconds after the first), at five seconds at the third, and at
/// the hidden deck's ten seconds at the second: there two windows' averages hold it.
#[test]
fn a_burst_opens_no_episode() {
    let burst = At {
        ram: RAM,
        push: 900.0 * MIB,
        back: 300.0 * MIB,
        headroom: Some(GIB),
        ..At::default()
    };
    let calm = At {
        ram: RAM,
        headroom: Some(GIB),
        ..At::default()
    };
    let mut looks = vec![burst; 3];
    looks.extend(vec![calm; 60]);
    let mut w = Watch::default();
    assert!(opened(&run(&mut w, 0, 1, &looks)).is_none());
    let mut w = Watch::default();
    assert!(opened(&run(&mut w, 0, 10, &[burst, calm, burst, calm, calm])).is_none());
    let mut w = Watch::default();
    assert!(
        opened(&run(
            &mut w,
            0,
            5,
            &[calm, burst, burst, calm, burst, burst, calm]
        ))
        .is_none()
    );
    let mut w = Watch::default();
    let edges = run(&mut w, 0, 1, &[burst; 11]);
    assert!(edges[..10].iter().all(Option::is_none));
    assert!(matches!(edges[10], Some(Edge::Opened(_))));
    let mut w = Watch::default();
    let edges = run(&mut w, 0, 5, &[burst; 3]);
    assert!(edges[..2].iter().all(Option::is_none));
    assert!(matches!(edges[2], Some(Edge::Opened(_))));
    let mut w = Watch::default();
    let edges = run(&mut w, 0, 10, &[burst; 2]);
    assert!(edges[0].is_none());
    assert!(matches!(edges[1], Some(Edge::Opened(_))));
}

/// EACH RULE OPENS AN EPISODE ON ITS OWN: anonymous pages moving with the wall at 93 % or 98 %
/// (headroom ample); moving with the kernel's level critical as it reports it, or as its own
/// test reads the queues (a share of 300 ‰); every task stalled for 8 % of the window.
/// NEGATIVE CONTROLS: each with nothing moving (or the stall at 0.5 %) opens none; the wall at
/// 85 % opens none with pages moving; under the reserve, one thrash rate over its floor and the
/// other under it opens none, either way round, nor both over with the headroom over the
/// reserve.
#[test]
fn each_rule_opens_an_episode_on_its_own() {
    let moving = At {
        ram: RAM,
        push: 70.0 * MIB,
        headroom: Some(60 * GIB),
        ..At::default()
    };
    let cases = [
        (
            At {
                wall: Some((930, 1000)),
                ..moving
            },
            Why::Wall,
        ),
        (
            At {
                pressure: Pressure::Critical,
                ..moving
            },
            Why::Kernel,
        ),
        (
            At {
                kept: Some(300),
                ..moving
            },
            Why::Kernel,
        ),
        (
            At {
                wall: Some((980, 1000)),
                ..moving
            },
            Why::Wall,
        ),
        (
            At {
                push: 0.0,
                anon: 9.0 * MIB,
                wall: Some((930, 1000)),
                ..moving
            },
            Why::Wall,
        ),
        (
            At {
                ram: 32 * GIB,
                headroom: Some(20 * GIB),
                stall: Some((0.4, 0.08)),
                ..At::default()
            },
            Why::Stall,
        ),
    ];
    for (a, why) in cases {
        let mut w = Watch::default();
        let e = opened(&run(&mut w, 0, 5, &[a; 4]))
            .cloned()
            .unwrap_or_else(|| panic!("{why:?} opens"));
        assert_eq!(e.why, why);
        let still = At {
            push: 0.0,
            back: 0.0,
            anon: 0.0,
            stall: a.stall.map(|(some, _)| (some, 0.005)),
            ..a
        };
        let mut w = Watch::default();
        assert!(
            opened(&run(&mut w, 0, 5, &[still; 40])).is_none(),
            "{why:?} with nothing moving opens none"
        );
    }
    let short = |push: f64, back: f64, headroom: u64| Look {
        ram: RAM,
        push_bps: Some(push * MIB),
        refault_bps: Some(back * MIB),
        headroom: Some(headroom),
        ..Look::default()
    };
    for (look, what) in [
        (
            Look {
                wall_permille: Some(850),
                ..short(70.0, 0.0, 60 * GIB)
            },
            "the wall at 85 %",
        ),
        (short(64.0, 0.0, 60 * GIB), "pushed alone, room left"),
        (short(259.0, 15.0, 5 * GIB), "read back under its floor"),
        (short(63.0, 89.0, 5 * GIB), "pushed under its floor"),
        (short(259.0, 89.0, 9 * GIB), "the headroom over the reserve"),
    ] {
        assert_eq!(look.starving(), None, "{what}");
    }
    assert_eq!(short(64.0, 16.0, 8 * GIB).starving(), Some(Why::Thrash));
}

/// A SLOWER STARVATION OPENS ONE AT THE WALL: near its wall the compressor can take little more,
/// so the push falls under the thrash floors, which grow with the machine's memory; the wall's
/// and the kernel's rules ask only that anonymous pages move. A 128 GiB Mac at 97 % of its
/// segments, critical, with 0.3 GiB of headroom, pushing 50 MiB/s and reading 12 back (0.6 of it
/// anonymous); a 512 GiB Mac at 95 %, critical, pushing 130 and reading 45 back; a fifth of the
/// incident's rates at 95 %. NEGATIVE CONTROL: the first at 89 % with its level at warn opens
/// none.
#[test]
fn a_slower_starvation_opens_one_at_the_wall() {
    let slow = At {
        ram: RAM,
        push: 50.0 * MIB,
        back: 12.0 * MIB,
        anon: 0.6 * MIB,
        headroom: Some(GIB * 3 / 10),
        wall: Some((970, 1000)),
        kept: Some(400),
        pressure: Pressure::Critical,
        ..At::default()
    };
    for a in [
        slow,
        At {
            ram: 512 * GIB,
            push: 130.0 * MIB,
            back: 45.0 * MIB,
            headroom: Some(20 * GIB),
            wall: Some((950, 1000)),
            ..slow
        },
        At {
            push: INCIDENT.push / 5.0,
            back: INCIDENT.back / 5.0,
            anon: INCIDENT.anon / 5.0,
            wall: Some((950, 1000)),
            ..INCIDENT
        },
    ] {
        let mut w = Watch::default();
        let e = opened(&run(&mut w, 0, 5, &[a; 120])).cloned();
        assert_eq!(e.map(|e| (e.why, e.opened_ns)), Some((Why::Wall, 10 * S)));
    }
    let mut w = Watch::default();
    let off = At {
        wall: Some((890, 1000)),
        pressure: Pressure::Warn,
        ..slow
    };
    assert!(opened(&run(&mut w, 0, 5, &[off; 120])).is_none());
}

/// AN EPISODE FROZEN AT ITS EDGE STAYS OPEN. From 23:20:24.7 to the panic the incident's queues
/// all but stood still (its compressor 4 728 204 to 4 728 343 pages, free 7 694 to 914), its
/// compressor at its wall, so no page was pushed or read back: a minute of zero rates there is
/// not quiet. Each edge alone holds it: the compressor at 98 %, the kernel critical as it reports
/// it, its share under 343 ‰, the headroom at the free target. NEGATIVE CONTROL: the same zero
/// rates off every edge (97 %, warn, a share of 400 ‰, 2 GiB of headroom) end it after the
/// minute, and the record says how long it lasted.
#[test]
fn an_episode_frozen_at_its_edge_stays_open() {
    let off = At {
        ram: RAM,
        headroom: Some(2 * GIB),
        wall: Some((970, 1000)),
        kept: Some(400),
        pressure: Pressure::Warn,
        ..At::default()
    };
    let frozen = [
        At {
            wall: Some((1000, 1000)),
            pressure: Pressure::Critical,
            kept: Some(403),
            headroom: Some(20 << 20),
            ..off
        },
        At {
            wall: Some((980, 1000)),
            ..off
        },
        At {
            pressure: Pressure::Critical,
            ..off
        },
        At {
            kept: Some(330),
            ..off
        },
        At {
            headroom: Some(FROZEN_HEADROOM),
            ..off
        },
    ];
    for (i, a) in frozen.iter().enumerate() {
        let mut w = Watch::default();
        assert!(opened(&run(&mut w, 0, 5, &[INCIDENT; 3])).is_some());
        let edges = run(&mut w, 15, 5, &vec![*a; 36]);
        assert!(ended(&edges).is_none(), "frozen look {i} stays open");
        assert!(w.open().is_some());
        let edges = run(&mut w, 195, 5, &[off; 12]);
        assert!(edges[..11].iter().all(Option::is_none), "look {i}");
        let e = ended(&edges).unwrap_or_else(|| panic!("look {i}: a quiet minute ends it"));
        assert_eq!(e.for_ms(), Some(250_000 - 10_000));
    }
}

/// A STALL OR ANONYMOUS MOVEMENT AT THE WALL BREAKS THE QUIET MINUTE: an open episode whose
/// quiet readings are broken by one with every task stalled for 2 % of its window, or by one with
/// anonymous pages moving at 5 MiB/s while the wall is near, ends a minute after the break, not
/// before. NEGATIVE CONTROL: the stall at 0.5 % and the movement at 3 MiB/s break nothing.
#[test]
fn a_stall_or_movement_at_the_wall_breaks_the_quiet_minute() {
    let quiet = At {
        ram: RAM,
        headroom: Some(30 * GIB),
        wall: Some((920, 1000)),
        stall: Some((0.1, 0.0)),
        ..At::default()
    };
    for (breaks, a) in [
        (
            true,
            At {
                stall: Some((0.2, 0.02)),
                ..quiet
            },
        ),
        (
            true,
            At {
                anon: 5.0 * MIB,
                ..quiet
            },
        ),
        (
            false,
            At {
                stall: Some((0.2, 0.005)),
                ..quiet
            },
        ),
        (
            false,
            At {
                anon: 3.0 * MIB,
                ..quiet
            },
        ),
    ] {
        let mut w = Watch::default();
        assert!(opened(&run(&mut w, 0, 5, &[INCIDENT; 3])).is_some());
        let mut looks = vec![quiet; 6];
        looks.push(a);
        looks.extend(vec![quiet; 12]);
        let edges = run(&mut w, 15, 5, &looks);
        let at = edges.iter().position(Option::is_some);
        assert_eq!(at, Some(if breaks { 18 } else { 11 }), "breaks={breaks}");
    }
}

fn ty(pid: u32, gib: f64, scope: RowScope) -> ProcView {
    ProcView {
        group: None,
        energy_mw: None,
        energy_read_span_ns: None,
        scheduling: None,
        cpu_accounting: crate::model::CpuAccounting::default(),
        pid,
        ppid: 1,
        pgid: pid,
        name: "ty".into(),
        cores: Some(1.0),
        cpu_read_span_ns: None,
        wait: None,
        footprint: Some((gib * GIB as f64) as u64),
        peak: None,
        read_bps: None,
        write_bps: None,
        state: crate::model::ProcState::Run,
        scope,
        refused: false,
        gpu: None,
        birth_ms: None,
    }
}

/// AN EPISODE ENDS AFTER A MINUTE QUIET AND NAMES THE LARGEST HOLDERS AND THEIR TABS from the
/// newest sweep the vitals hold: the incident's five `ty` runs (133, 83, 65, 41 and 18 GiB, four
/// of them a tab's) give the three largest at the opening, the sweep stalled since (no process
/// rows) leaves them named with their age, a reading that moves again restarts the quiet minute,
/// and the end names the newest sweep's holders. The stale compressor and swap left after it do
/// not hold it open.
#[test]
fn an_episode_ends_after_a_quiet_minute_and_names_the_largest_holders() {
    let tab = RowScope::Tab(7);
    let swept = |t_s: u64, a: At, procs: Vec<ProcView>| Snapshot {
        procs,
        tabs: vec![TabView {
            key: 7,
            label: "Tab 3".into(),
            ..TabView::default()
        }],
        ..snap_at(t_s, mem(a, 5))
    };
    let starving = At {
        ram: RAM,
        push: 259.0 * MIB,
        back: 89.0 * MIB,
        headroom: Some(5 * GIB),
        ..At::default()
    };
    let quiet = At {
        ram: RAM,
        headroom: Some(70 * GIB),
        compressed: Some(64 * GIB),
        swap_used: Some(80 * GIB),
        ..At::default()
    };
    let five = vec![
        ty(77_936, 133.37, RowScope::Others),
        ty(77_700, 82.96, tab),
        ty(77_701, 64.67, tab),
        ty(77_702, 41.43, tab),
        ty(77_703, 18.31, tab),
    ];
    let mut d = Detector::default();
    assert!(d.detect(&swept(0, starving, five.clone()), &[]).is_empty());
    assert!(d.detect(&swept(5, starving, Vec::new()), &[]).is_empty());
    let e = d.detect(&swept(10, starving, Vec::new()), &[]);
    let [ev] = e.as_slice() else { panic!("{e:?}") };
    let EventKind::Starving(edge) = &ev.kind else {
        panic!("{ev:?}")
    };
    let Edge::Opened(open) = edge.as_ref() else {
        panic!("{edge:?}")
    };
    let named: Vec<(u32, Option<&str>)> = open
        .holders
        .iter()
        .map(|h| (h.pid, h.label.as_deref()))
        .collect();
    assert_eq!(
        named,
        [
            (77_936, None),
            (77_700, Some("Tab 3")),
            (77_701, Some("Tab 3"))
        ]
    );
    assert_eq!(
        open.holders_age_ms,
        Some(10_000),
        "the sweep ten seconds before"
    );
    assert!(d.starving().is_some());
    let mut t = 15;
    for _ in 0..11 {
        assert!(
            d.detect(&swept(t, quiet, Vec::new()), &[]).is_empty(),
            "{t} s"
        );
        t += 5;
    }
    assert!(
        d.detect(&swept(t, starving, Vec::new()), &[]).is_empty(),
        "moving again"
    );
    t += 5;
    let left = vec![ty(77_703, 18.31, tab)];
    for i in 0..11 {
        let procs = if i == 0 { left.clone() } else { Vec::new() };
        assert!(d.detect(&swept(t, quiet, procs), &[]).is_empty(), "{t} s");
        t += 5;
    }
    let e = d.detect(&swept(t, quiet, Vec::new()), &[]);
    let [ev] = e.as_slice() else {
        panic!("the twelfth quiet reading, a minute, ends it: {e:?}")
    };
    let EventKind::Starving(edge) = &ev.kind else {
        panic!("{ev:?}")
    };
    let Edge::Ended(ended) = edge.as_ref() else {
        panic!("{edge:?}")
    };
    assert_eq!(ended.holders.len(), 1);
    assert_eq!(ended.holders[0].pid, 77_703);
    assert_eq!(ended.holders_age_ms, Some(55_000));
    assert_eq!(ended.for_ms(), Some((t - 10) * 1000));
    assert_eq!(ended.worst.headroom, Some(5 * GIB));
    assert!(d.starving().is_none());
}

/// A READING THAT CANNOT JUDGE MOVES NEITHER WAY: no rates yet, or none of the activity counters
/// read (an OS that reads none), keeps the count toward an opening and the quiet minute as they
/// stood, and a Snapshot with no memory at all likewise. NEGATIVE CONTROLS: the opening comes at
/// the third starving reading, ten seconds of windows after the first, as with no unread reading
/// between; the end at the twelfth quiet one, a minute of windows, unread ones between them
/// counting none of it.
#[test]
fn a_reading_that_cannot_judge_moves_neither_way() {
    let unread = MemView {
        sample: MemSample {
            total: RAM,
            headroom: Some(GIB),
            ..MemSample::default()
        },
        rates: Some(MemRates {
            window_ns: 5 * S,
            ..MemRates::default()
        }),
        ..MemView::default()
    };
    assert!(Look::of(&unread).is_none());
    assert!(Look::of(&MemView::default()).is_none());
    let burst = At {
        ram: RAM,
        push: 900.0 * MIB,
        back: 300.0 * MIB,
        headroom: Some(GIB),
        ..At::default()
    };
    let quiet = At {
        ram: RAM,
        headroom: Some(70 * GIB),
        ..At::default()
    };
    let mut w = Watch::default();
    assert!(w.step(&snap_at(0, mem(burst, 5))).is_none());
    assert!(w.step(&snap_at(5, unread.clone())).is_none());
    assert!(w.step(&Snapshot::default()).is_none());
    assert!(w.step(&snap_at(10, mem(burst, 5))).is_none());
    assert!(matches!(
        w.step(&snap_at(15, mem(burst, 5))),
        Some(Edge::Opened(_))
    ));
    let mut t = 20;
    for i in 0..11 {
        assert!(w.step(&snap_at(t, mem(quiet, 5))).is_none(), "quiet {i}");
        t += 5;
        if i == 5 {
            assert!(w.step(&snap_at(t, unread.clone())).is_none());
            assert!(w.step(&Snapshot::default()).is_none());
            t += 5;
        }
    }
    assert!(matches!(
        w.step(&snap_at(t, mem(quiet, 5))),
        Some(Edge::Ended(_))
    ));
}

/// THE READER KEEPS WATCHING MEMORY WHILE IT IS TIGHT: a reading whose headroom is at its
/// reserve, whose wall is near, whose kernel reads warn or worse (as it reports it or as its
/// share reads), or whose tasks stall on memory keeps the reader reading on its own, and so does
/// any reading while an episode is open; the incident's 23:16:09.8 look is tight. NEGATIVE
/// CONTROL: a healthy Mac with room (78 GiB of headroom, its share at 988 ‰, its level normal)
/// and a reading with no memory keep none.
#[test]
fn the_reader_keeps_watching_memory_while_it_is_tight() {
    let roomy = At {
        ram: RAM,
        headroom: Some(78 * GIB),
        kept: Some(988),
        pressure: Pressure::Normal,
        ..At::default()
    };
    assert!(!tight(&mem(roomy, 10)));
    assert!(!watching(&snap_at(0, mem(roomy, 10))));
    assert!(!watching(&Snapshot::default()));
    for (a, what) in [
        (INCIDENT, "the incident"),
        (
            At {
                headroom: Some(8 * GIB),
                ..roomy
            },
            "at the reserve",
        ),
        (
            At {
                wall: Some((900, 1000)),
                ..roomy
            },
            "the wall near",
        ),
        (
            At {
                pressure: Pressure::Warn,
                ..roomy
            },
            "warn reported",
        ),
        (
            At {
                kept: Some(499),
                ..roomy
            },
            "warn computed",
        ),
        (
            At {
                stall: Some((0.2, 0.01)),
                ..roomy
            },
            "tasks stalled",
        ),
    ] {
        assert!(tight(&mem(a, 10)), "{what}");
    }
    let mut open = snap_at(0, mem(roomy, 10));
    open.starving = Some(Box::new(Episode {
        why: Why::Thrash,
        opened_ns: 0,
        opened_unix_ms: 0,
        at_open: Look::default(),
        worst: Worst::default(),
        holders: Vec::new(),
        holders_age_ms: None,
        ended: None,
    }));
    assert!(watching(&open), "an open episode");
}

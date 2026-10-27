// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GIB: u64 = 1 << 30;
const M3: u64 = 128 * GIB;

fn fseventsd(footprint: u64) -> Service {
    Service {
        coalition: 7,
        pid: 537,
        start_s: 1_758_000_000,
        start_us: 12,
        uid: 0,
        name: "fseventsd".into(),
        path: Some(FSEVENTSD.path.into()),
        live: 1,
        footprint,
    }
}

fn reading(services: Vec<Service>) -> Reading {
    Reading {
        total: M3,
        services,
        ..Reading::default()
    }
}

/// What a judge tells of each service it follows: `(pid, size, held)`.
fn seen(told: Option<&Told>) -> Vec<(u32, Size, bool)> {
    told.expect("a reading")
        .services
        .iter()
        .map(|s| (s.service.pid, s.size, s.held))
        .collect()
}

/// The bars scale with memory past their floors: the 2026-10-26 Mac's (128 GiB) are 8 and
/// 32 GiB, a 16 GiB Mac's the floors (8 and 16 GiB: a WindowServer of large displays at
/// 4.5 GiB is not large there).
#[test]
fn bars_scale_with_memory_past_their_floors() {
    assert_eq!((large_bar(M3), huge_bar(M3)), (8 * GIB, 32 * GIB));
    assert_eq!(
        (large_bar(16 * GIB), huge_bar(16 * GIB)),
        (8 * GIB, 16 * GIB)
    );
    assert_eq!(
        size(4_608 << 20, 16 * GIB),
        Size::Usual,
        "a busy WindowServer"
    );
    assert_eq!(size(3_900_000, M3), Size::Usual, "a fresh fseventsd");
    assert_eq!(size(5 * GIB, M3), Size::Usual, "mds at its peak");
    assert_eq!(size(8 * GIB, M3), Size::Large);
    assert_eq!(size(44 * GIB, M3), Size::Huge, "the first jetsam kill");
    assert_eq!(size(152_000_000_000, M3), Size::Huge, "the incident");
    assert_eq!((leave_bar(8 * GIB), followed_from(M3)), (7 * GIB, 7 * GIB));
}

/// A service is told only once it has held past the large bar for the hold, and a new
/// process in its place (a restart) starts over.
#[test]
fn a_service_is_told_after_the_hold_and_a_restart_starts_over() {
    let mut judge = Judge::new();
    assert_eq!(judge.word(), "-");
    let big = reading(vec![fseventsd(40 * GIB)]);
    assert_eq!(
        seen(judge.absorb(0, Some(&big))),
        [(537, Size::Huge, false)]
    );
    assert_eq!(judge.word(), "ok", "read, nothing told yet");
    assert_eq!(
        seen(judge.absorb(HOLD_MS - 1, Some(&big))),
        [(537, Size::Huge, false)]
    );
    let told = judge.absorb(HOLD_MS, Some(&big)).unwrap().clone();
    let over: Vec<&Seen> = told.told().collect();
    assert_eq!(over.len(), 1);
    assert_eq!((over[0].size, over[0].since_ms), (Size::Huge, Some(0)));
    assert_eq!(over[0].restart, Some(&FSEVENTSD));
    assert_eq!(judge.word(), "huge");

    assert_eq!(
        judge.absorb(HOLD_MS + 1, None).unwrap().told().count(),
        1,
        "a failed reading changes nothing"
    );

    let mut fresh = fseventsd(40 * GIB);
    fresh.pid = 90_001;
    assert_eq!(
        seen(judge.absorb(HOLD_MS + 2, Some(&reading(vec![fresh])))),
        [(90_001, Size::Huge, false)],
        "the old episode ended; the new process holds anew"
    );
    assert!(seen(judge.absorb(2 * HOLD_MS, Some(&reading(vec![fseventsd(3_700_000)])))).is_empty());
    assert_eq!(judge.word(), "ok");
}

/// At a bar's edge a service keeps its size: past the large bar it stays large (and held)
/// until it falls an eighth under it, and past the huge bar huge until an eighth under that.
/// NEGATIVE CONTROL: a fall past the margin leaves the size, and under the large one ends the
/// episode, so the hold starts over.
#[test]
fn a_service_keeps_its_size_at_a_bars_edge_until_it_falls_past_the_margin() {
    let mut judge = Judge::new();
    let at = |fp: u64| reading(vec![fseventsd(fp)]);
    judge.absorb(0, Some(&at(32 * GIB + (1 << 20))));
    judge.absorb(HOLD_MS, Some(&at(32 * GIB + (1 << 20))));
    for (step, fp) in [32 * GIB - (1 << 20), 32 * GIB + (1 << 20), 29 * GIB]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            seen(judge.absorb(HOLD_MS + 60_000 * (step as u64 + 1), Some(&at(fp)))),
            [(537, Size::Huge, true)],
            "{fp}"
        );
    }
    assert_eq!(
        seen(judge.absorb(HOLD_MS * 2, Some(&at(27 * GIB)))),
        [(537, Size::Large, true)],
        "under the huge bar's margin"
    );
    assert_eq!(
        seen(judge.absorb(HOLD_MS * 3, Some(&at(7 * GIB + (1 << 20))))),
        [(537, Size::Large, true)],
        "within the large bar's margin: still the episode, still held"
    );
    assert!(
        seen(judge.absorb(HOLD_MS * 4, Some(&at(7 * GIB - (1 << 20))))).is_empty(),
        "under it: the episode ends"
    );
    assert_eq!(
        seen(judge.absorb(HOLD_MS * 5, Some(&at(9 * GIB)))),
        [(537, Size::Large, false)],
        "a new episode holds anew"
    );
}

/// A judge newer than the episode (aterm relaunched mid-episode) still follows the service
/// from its first reading, not yet held: what it tells is no end. NEGATIVE CONTROL: one under
/// the margin is not followed, and one whose footprint did not read keeps what it was.
#[test]
fn a_relaunched_judge_follows_a_large_service_from_its_first_reading() {
    let mut judge = Judge::new();
    assert_eq!(
        seen(judge.absorb(0, Some(&reading(vec![fseventsd(141 * GIB)])))),
        [(537, Size::Huge, false)]
    );
    let mut band = Judge::new();
    assert_eq!(
        seen(band.absorb(0, Some(&reading(vec![fseventsd(7 * GIB + (1 << 20))])))),
        [(537, Size::Usual, false)],
        "within the margin under the bar: followed, nothing reached"
    );
    let mut small = Judge::new();
    assert!(seen(small.absorb(0, Some(&reading(vec![fseventsd(GIB)])))).is_empty());

    judge.absorb(HOLD_MS, Some(&reading(vec![fseventsd(141 * GIB)])));
    let unread = Reading {
        total: M3,
        unread: vec![Member {
            pid: 537,
            uid: 0,
            name: "fseventsd".into(),
            start_s: 1_758_000_000,
            start_us: 12,
            coalition: 7,
        }],
        ..Reading::default()
    };
    let told = judge.absorb(HOLD_MS * 2, Some(&unread)).unwrap();
    assert_eq!(told.told().count(), 1, "an unread footprint is no end");
    assert_eq!(told.unread.len(), 1);
}

/// A relaunched judge holds a service an earlier aterm process told from its last reading on
/// (carried), at the size it was told: no new hold, and a dip within the huge bar's margin stays
/// huge. NEGATIVE CONTROL: a new process in its place inherits nothing, and a service the judge
/// does not follow is not carried.
#[test]
fn a_carried_service_is_held_from_the_judges_last_reading() {
    let mut judge = Judge::new();
    let at = |fp: u64| reading(vec![fseventsd(fp)]);
    judge.absorb(0, Some(&at(30 * GIB)));
    assert_eq!(judge.word(), "ok", "followed, in its hold");
    let episode = fseventsd(0).episode();
    assert!(judge.carry(&episode, Size::Huge));
    assert_eq!(seen(judge.told()), [(537, Size::Huge, true)]);
    assert_eq!(judge.word(), "huge");
    assert!(!judge.carry(&episode, Size::Huge), "held already");
    assert_eq!(
        seen(judge.absorb(60_000, Some(&at(29 * GIB)))),
        [(537, Size::Huge, true)],
        "within the huge bar's margin"
    );
    assert_eq!(
        seen(judge.absorb(120_000, Some(&at(20 * GIB)))),
        [(537, Size::Large, true)]
    );
    let mut fresh = fseventsd(30 * GIB);
    fresh.pid = 90_001;
    assert_eq!(
        seen(judge.absorb(180_000, Some(&reading(vec![fresh])))),
        [(90_001, Size::Large, false)],
        "a new process holds anew"
    );
    let mut low = Judge::new();
    low.absorb(0, Some(&at(GIB)));
    assert!(!low.carry(&episode, Size::Huge), "not followed");
    assert!(!Judge::new().carry(&episode, Size::Huge), "nothing read");
}

/// Memory is under pressure at the kernel's level past normal, or with swap moving at a
/// megabyte a second; a quiet swap counter at a normal level is not.
#[test]
fn pressure_is_the_kernels_level_or_swap_moving() {
    let strained = |warn: Option<bool>, kib: Option<u64>| Reading {
        pressure_warn: warn,
        swapped_kib: kib,
        ..reading(vec![fseventsd(40 * GIB)])
    };
    let mut judge = Judge::new();
    assert!(
        !judge
            .absorb(0, Some(&strained(Some(false), Some(1_000))))
            .unwrap()
            .pressed
    );
    assert!(
        !judge
            .absorb(60_000, Some(&strained(Some(false), Some(2_000))))
            .unwrap()
            .pressed,
        "a KiB a minute"
    );
    assert!(
        judge
            .absorb(120_000, Some(&strained(Some(false), Some(2_000 + 60_000))))
            .unwrap()
            .pressed,
        "about a megabyte a second"
    );
    assert!(
        judge
            .absorb(180_000, Some(&strained(Some(true), None)))
            .unwrap()
            .pressed
    );
    assert!(
        !judge
            .absorb(240_000, Some(&strained(None, None)))
            .unwrap()
            .pressed
    );
}

/// The restart names only the service it was measured on: a look-alike at another path or
/// uid gets none, as does one that shares its coalition; one whose image this reading did not
/// read keeps the restart its process was matched to.
#[test]
fn the_restart_is_held_to_its_name_uid_image_and_coalition() {
    let mut other = fseventsd(20 * GIB);
    other.path = Some("/tmp/fseventsd".into());
    other.coalition = 3;
    other.pid = 600;
    let mut mds = fseventsd(30 * GIB);
    mds.name = "mds".into();
    mds.path = Some("/System/Library/Frameworks/CoreServices.framework/mds".into());
    mds.coalition = 9;
    mds.pid = 400;
    let mut shared = fseventsd(25 * GIB);
    shared.live = 2;
    shared.coalition = 11;
    shared.pid = 700;
    let mut judge = Judge::new();
    let all = reading(vec![other, mds, shared]);
    judge.absorb(0, Some(&all));
    let told: Vec<(String, bool)> = judge
        .absorb(HOLD_MS, Some(&all))
        .unwrap()
        .told()
        .map(|o| (o.service.name.clone(), o.restart.is_some()))
        .collect();
    assert_eq!(
        told,
        [
            ("mds".to_owned(), false),
            ("fseventsd".to_owned(), false),
            ("fseventsd".to_owned(), false)
        ]
    );
    assert_eq!(restart_named("fseventsd"), Some(&FSEVENTSD));
    assert_eq!(restart_named("mds"), None);

    let mut judge = Judge::new();
    judge.absorb(0, Some(&reading(vec![fseventsd(40 * GIB)])));
    let mut unread_path = fseventsd(40 * GIB);
    unread_path.path = None;
    let told = judge
        .absorb(HOLD_MS, Some(&reading(vec![unread_path])))
        .unwrap();
    assert_eq!(told.services[0].restart, Some(&FSEVENTSD));
}

fn lead(exe: &str, uid: u32, voucher: Voucher, signing_id: &str) -> Lead {
    Lead {
        pid: 537,
        birth_ms: 1,
        uid: Some(uid),
        exe: Some(exe.into()),
        voucher,
        signing_id: Some(signing_id.into()),
        team: None,
    }
}

fn row(id: u64, part: Part, footprint: u64, lead: Lead) -> CoalRow {
    CoalRow {
        id,
        part,
        footprint: Some(footprint),
        live: 1,
        lead: Some(lead),
        ..CoalRow::default()
    }
}

/// Of a split's rows, only services of the system's accounts past the bar are over; the
/// user's own (keyboxd at 4.28 GB on the incident Mac) and the kernel's coalition (led by
/// kernel_task or launchd) never are, and the restart needs Apple's signature and a lead alone
/// in its coalition.
#[test]
fn rows_over_holds_services_of_system_accounts() {
    let apple = |exe: &str, uid: u32| lead(exe, uid, Voucher::Apple, "com.apple.fseventsd");
    let mut kernel = apple("kernel_task", 0);
    kernel.pid = 0;
    let mut launchd = apple("launchd", 0);
    launchd.pid = 1;
    let mut shared = row(8, Part::Services, 40 * GIB, apple("fseventsd", 0));
    shared.live = 2;
    let rows = vec![
        row(1, Part::Services, 40 * GIB, apple("fseventsd", 0)),
        row(2, Part::Others, 40 * GIB, apple("keyboxd", 501)),
        row(3, Part::Services, 40 * GIB, apple("someone", 502)),
        row(4, Part::Services, 90 * GIB, kernel),
        row(5, Part::Services, GIB, apple("mds", 0)),
        row(6, Part::Services, 9 * GIB, apple("_ws", 88)),
        row(
            7,
            Part::Services,
            50 * GIB,
            lead("fseventsd", 0, Voucher::AdHoc, "com.apple.fseventsd"),
        ),
        shared,
        row(9, Part::Services, 90 * GIB, launchd),
    ];
    let over: Vec<(u64, Size, bool)> = rows_over(&rows, M3)
        .iter()
        .map(|o| (o.coalition, o.size, o.restart.is_some()))
        .collect();
    assert_eq!(
        over,
        [
            (7, Size::Huge, false),
            (1, Size::Huge, true),
            (8, Size::Huge, false),
            (6, Size::Large, false)
        ]
    );
}

/// A direct read names the same services: past the bar, largest first, the restart by the
/// measured image.
#[test]
fn a_direct_read_names_the_services_past_the_bar() {
    let mut mds = fseventsd(GIB);
    mds.name = "mds".into();
    let over = reading_over(&reading(vec![fseventsd(141 * GIB), mds]));
    let named: Vec<(Option<&str>, Size, bool)> = over
        .iter()
        .map(|o| (o.lead.exe.as_deref(), o.size, o.restart.is_some()))
        .collect();
    assert_eq!(named, [(Some("fseventsd"), Size::Huge, true)]);
}

#[test]
fn system_accounts_are_below_500_and_nobody() {
    assert!(system_uid(0) && system_uid(88) && system_uid(499) && system_uid(NOBODY_UID));
    assert!(!system_uid(500) && !system_uid(501));
    assert!(kernel_member(0) && kernel_member(1) && !kernel_member(2));
}

fn member(pid: u32, uid: u32, name: &str, start_s: u64, coalition: u64) -> Member {
    Member {
        pid,
        uid,
        name: name.into(),
        start_s,
        start_us: 0,
        coalition,
    }
}

/// The fold keeps the coalitions of system accounts alone, led by the earliest-born member,
/// leaves out the kernel's coalition, names a coalition whose footprint does not read by its
/// lead, and reads an image only from the followed footprint.
#[test]
fn the_fold_keeps_coalitions_of_system_accounts_alone() {
    let members = [
        member(537, 0, "fseventsd", 100, 7),
        member(88, 0, "mds", 90, 9),
        member(89, 0, "mds_stores", 80, 9),
        member(700, 501, "keyboxd", 50, 11),
        member(701, 0, "helper", 40, 11),
        member(702, 0, "unread", 40, 12),
        member(160, 88, "WindowServer", 60, 13),
        member(0, 0, "kernel_task", 0, 1),
        member(1, 0, "launchd", 1, 1),
    ];
    let mut paths = Vec::new();
    let reading = fold(
        &members,
        M3,
        |id| match id {
            1 => Some(140 * GIB),
            7 => Some(40 * GIB),
            9 => Some(GIB),
            11 => Some(50 * GIB),
            13 => Some(900 << 20),
            _ => None,
        },
        |pid| {
            paths.push(pid);
            Some(format!("/path/of/{pid}"))
        },
    );
    let leads: Vec<(u64, u32, &str, u32)> = reading
        .services
        .iter()
        .map(|s| (s.coalition, s.pid, s.name.as_str(), s.live))
        .collect();
    assert_eq!(
        leads,
        [
            (7, 537, "fseventsd", 1),
            (9, 89, "mds_stores", 2),
            (13, 160, "WindowServer", 1)
        ]
    );
    assert_eq!(paths, [537], "only the service followed");
    assert_eq!(reading.total, M3);
    assert_eq!(reading.services[0].path.as_deref(), Some("/path/of/537"));
    let unread: Vec<(u64, u32)> = reading
        .unread
        .iter()
        .map(|m| (m.coalition, m.pid))
        .collect();
    assert_eq!(unread, [(12, 702)]);
}

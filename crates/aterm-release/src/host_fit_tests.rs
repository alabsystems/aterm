// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_capacity::book::Key;

use super::*;

const GIB: u64 = 1 << 30;
const TENTH: u64 = GIB / 10;

fn reading(physical: u64, wired: u64, cores: u32) -> Reading {
    Reading {
        cores,
        physical: Some(physical),
        wired: Some(wired),
        available: Some(physical / 2),
        ..Reading::default()
    }
}

/// A serial parent that fits any reading these tests judge, standing for a measured book.
fn light() -> Option<Need> {
    Some(Need::new("release serial slice memory budget").mem(GIB))
}

fn volume(free: u64) -> aterm_capacity::Volume {
    aterm_capacity::Volume {
        mount: "/Volumes/cut".into(),
        total: 1000 * GIB,
        free,
        floor: 25 * GIB,
        room: 0,
    }
}

fn records(rows: &[(&str, &str, u64)]) -> Records {
    let mut records = Records::default();
    for (at, (stage, variant, bytes)) in rows.iter().enumerate() {
        records.push(
            Key {
                stage: (*stage).to_string(),
                checkout: "/c/aterm".into(),
                crates: Some(vec!["aterm-gui".into()]),
                variant: Some((*variant).to_string()),
            },
            *bytes,
            at as u64 + 1,
        );
    }
    records
}

/// THE 0.199.0 HOST, REFUSED BEFORE THE LINE, in the words its qualify refused with hours in. A
/// 32 GiB Mac with 2.6 GiB wired can never have more than 29.4 GiB available; the compiler unit
/// it recorded at 27.3 GiB claims 32.8 GiB with the driver's 2 GiB spare beside it, which can
/// never fit, and the refusal names the unit, the shortfall, the memory and the cores. The Mac
/// that recorded the same unit with 256 GiB is fit, and says what it judged.
#[test]
fn a_recorded_compiler_unit_that_can_never_fit_refuses_the_host() {
    // peak_claim(27 1/3 GiB) = 32.8 GiB: the peak and a fifth more.
    let unit = [(UNIT_STAGE, "aterm_gui lib", 82 * GIB / 3)];
    assert_eq!(
        aterm_capacity::gib(aterm_capacity::peak_claim(82 * GIB / 3)),
        "32.8 GiB"
    );
    let small = reading(32 * GIB, 26 * TENTH, 12);
    let refusal = judge(&small, &records(&unit), light(), None).unwrap_err();
    for part in [
        "the heaviest compiler unit recorded here (aterm_gui lib, in /c/aterm)",
        "needs 32.8 GiB and 2 GiB spare beside it",
        "can never have more than 29.4 GiB of its 32 GiB of memory available",
        "The host: 32 GiB of memory (29.4 GiB of it can ever be available), 12 cores",
    ] {
        assert!(refusal.why.contains(part), "{part}: {}", refusal.why);
    }
    assert!(
        !refusal.could_not_run,
        "a verdict on the host, not a resumable stop"
    );
    assert!(
        refusal.failing.is_none(),
        "no red of the source: no repair worker"
    );
    assert!(
        refusal.remedy.contains("on a Mac that fits")
            && refusal.remedy.contains("the pump on the cut host"),
        "{}",
        refusal.remedy
    );
    assert!(
        refusal.remedy.contains("still resumes here"),
        "{}",
        refusal.remedy
    );

    let large = reading(256 * GIB, 6 * GIB, 36);
    let note = judge(&large, &records(&unit), light(), None).unwrap();
    assert!(
        note.contains("256 GiB of memory") && note.contains("36 cores"),
        "{note}"
    );
    assert!(
        note.contains("aterm_gui lib") && note.contains("32.8 GiB"),
        "{note}"
    );
}

/// The table: only an indivisible claim that can never fit refuses, and its margin counts.
#[test]
fn only_an_indivisible_claim_that_can_never_fit_refuses() {
    let mac = reading(64 * GIB, 4 * GIB, 16);
    // Reachable 60 GiB. A unit claims its peak and a fifth more with 2 GiB spare beside it, so
    // it refuses from a 48 1/3 GiB peak, which claims 58 GiB.
    let fits = |rows: &[(&str, &str, u64)], serial: Option<u64>| {
        let serial = serial.map_or_else(light, |bytes| {
            Some(
                Need::new("release serial slice memory budget")
                    .mem(bytes)
                    .margin(1),
            )
        });
        // A unit of this Mac's own, so that only the rows under test decide.
        let mut rows = rows.to_vec();
        rows.push((UNIT_STAGE, "small", GIB));
        judge(&mac, &records(&rows), serial, None).is_ok()
    };
    assert!(fits(&[(UNIT_STAGE, "u", 48 * GIB)], None));
    assert!(
        !fits(&[(UNIT_STAGE, "u", 146 * GIB / 3)], None),
        "the claim is the peak and a fifth more, and its margin counts"
    );
    assert!(
        !fits(&[(UNIT_STAGE, "u", 50 * GIB)], None),
        "a peak that fits bare does not fit as it is claimed"
    );
    assert!(
        !fits(
            &[
                (UNIT_STAGE, "light", 2 * GIB),
                (UNIT_STAGE, "heavy", 70 * GIB)
            ],
            None
        ),
        "the heaviest unit is the one judged"
    );
    assert!(
        fits(&[("TestCompile", "whole", 90 * GIB)], None),
        "a stage's total is divisible: its units run one after another"
    );
    assert!(fits(&[], Some(59 * GIB)));
    assert!(
        !fits(&[], Some(60 * GIB)),
        "the slices' serial parent is indivisible too"
    );
    let alone = Need::new("release cold slice memory budget")
        .mem(90 * GIB)
        .margin(1)
        .alone();
    assert!(
        judge(
            &mac,
            &records(&[(UNIT_STAGE, "small", GIB)]),
            Some(alone),
            None
        )
        .is_ok(),
        "a claim admitted alone takes what the machine has"
    );
}

/// NO RECORD IS NEVER A PASS: THE 0.199.0 HOST AS IT TOOK ITS LINE. A 32 GiB, 12-core Mac that
/// never built the tree holds no record of it, and is judged by what a cut host measured: the
/// bundle's heaviest unit peaks at 32.15 GiB, which the slices claim at 39 GiB with their 2
/// GiB driver beside it, and that can never fit 29.4 GiB. The refusal carries those numbers and
/// the remedy; a 48 GiB Mac with no record fits and says what it was judged by.
/// NEGATIVE CONTROL: the same Mac with records of its own that fit is judged by them and passes
/// (without the stand-in, the Mac with no record passed the same way, unjudged).
#[test]
fn a_mac_with_no_record_is_judged_by_a_cut_hosts_measurement() {
    let m9 = reading(32 * GIB, 26 * TENTH, 12);
    let refusal = judge(&m9, &Records::default(), None, None).unwrap_err();
    for part in [
        "the bundle's heaviest compiler unit as a cut host measured it",
        "a 32.2 GiB peak",
        "this Mac has recorded none",
        "needs 41 GiB",
        "can never have more than 29.4 GiB of its 32 GiB of memory available",
        "The host: 32 GiB of memory (29.4 GiB of it can ever be available), 12 cores",
    ] {
        assert!(refusal.why.contains(part), "{part}: {}", refusal.why);
    }
    assert!(
        !refusal.why.contains("aterm_gui test"),
        "the test unit's 24.4 GiB claim and 2 GiB spare fit 29.4 GiB: {}",
        refusal.why
    );
    assert!(
        refusal.remedy.contains("the pump on the cut host"),
        "{}",
        refusal.remedy
    );

    let fits = reading(48 * GIB, 3 * GIB, 12);
    let note = judge(&fits, &Records::default(), None, None).unwrap();
    for part in [
        "48 GiB of memory",
        "as a cut host measured it (aterm_gui test, a 20.3 GiB peak",
        "the slices' parent (41 GiB)",
        "10 cores or more",
    ] {
        assert!(note.contains(part), "{part}: {note}");
    }

    let own = judge(
        &m9,
        &records(&[(UNIT_STAGE, "aterm_gui test", 20 * GIB)]),
        light(),
        None,
    )
    .unwrap();
    assert!(own.contains("recorded here (aterm_gui test"), "{own}");

    // An unread machine is refused nothing: no number to judge.
    let unread = judge(&Reading::default(), &Records::default(), None, None).unwrap();
    assert!(unread.contains("memory unread"), "{unread}");
}

/// TOO FEW CORES TO END INSIDE ONE LEASE REFUSE THE LINE, with the measurement in the refusal:
/// the qualify tree's 39 core-hours over the four-hour lease is 10 cores.
/// NEGATIVE CONTROL: ten cores, and the 0.199.0 host's twelve, are not refused for cores.
#[test]
fn a_mac_with_too_few_cores_to_end_inside_a_lease_is_refused() {
    assert_eq!(least_cores(), 10);
    let unit = [(UNIT_STAGE, "u", GIB)];
    let few = reading(128 * GIB, 4 * GIB, 8);
    let refusal = judge(&few, &records(&unit), light(), None).unwrap_err();
    for part in [
        "the qualify tree ran 1h05m on the 36 cores it was measured on",
        "at most 39 core-hours of work",
        "which 8 cores cannot end inside the 4h00m",
        "needs 10 cores",
        "The host: 128 GiB of memory (124 GiB of it can ever be available), 8 cores",
    ] {
        assert!(refusal.why.contains(part), "{part}: {}", refusal.why);
    }
    for cores in [10, 12] {
        let enough = reading(128 * GIB, 4 * GIB, cores);
        assert!(judge(&enough, &records(&unit), light(), None).is_ok());
    }
}

/// THE RELEASE'S DISK CLAIM IS JUDGED BEFORE THE LINE against what its clone's volume has free
/// above its floor now, with both numbers and what sized the claim in the refusal, and every
/// shortfall is named at once.
/// NEGATIVE CONTROL: the same claim on a volume with the room, or with no volume read, passes.
#[test]
fn a_volume_without_room_for_the_release_refuses_the_host() {
    let unit = [(UNIT_STAGE, "u", GIB)];
    let claim = || {
        Some((
            39 * GIB,
            "the floor where the clone's build is cold".to_string(),
        ))
    };
    let mut full = reading(128 * GIB, 4 * GIB, 16);
    full.volume = Some(volume(55 * GIB));
    let refusal = judge(&full, &records(&unit), light(), claim()).unwrap_err();
    assert!(
        refusal.why.contains(
            "the release claims 39 GiB of disk on /Volumes/cut (the floor where the clone's \
             build is cold), which has 30 GiB free above its 25 GiB floor"
        ),
        "{}",
        refusal.why
    );
    assert!(
        refusal.remedy.contains("ends when space is freed"),
        "{}",
        refusal.remedy
    );

    let mut roomy = reading(128 * GIB, 4 * GIB, 16);
    roomy.volume = Some(volume(64 * GIB));
    let note = judge(&roomy, &records(&unit), light(), claim()).unwrap();
    assert!(note.contains("the release's 39 GiB of disk"), "{note}");
    let unread = reading(128 * GIB, 4 * GIB, 16);
    assert!(judge(&unread, &records(&unit), light(), claim()).is_ok());

    // Every shortfall at once: memory, cores and disk.
    let mut small = reading(32 * GIB, 26 * TENTH, 8);
    small.volume = Some(volume(55 * GIB));
    let all = judge(&small, &Records::default(), None, claim())
        .unwrap_err()
        .why;
    for part in ["needs 41 GiB", "needs 10 cores", "claims 39 GiB of disk"] {
        assert!(all.contains(part), "{part}: {all}");
    }
}

/// The live judgment reads this machine and names it, whichever way it judges.
#[test]
fn the_live_judgment_names_this_host() {
    let repo = std::env::current_dir().unwrap();
    let said = live(&repo, (GIB, "a test's claim".into())).unwrap_or_else(|refusal| refusal.why);
    assert!(said.contains(" cores"), "{said}");
}

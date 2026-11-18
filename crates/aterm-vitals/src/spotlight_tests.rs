// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use super::*;
use crate::explain::CoalRow;

/// `mdutil -s -a` on m42 with the data volume's indexing off: each status line starts with a
/// tab and ends with a space. Nothing in it asks for root.
const ONE_OFF: &str = "/:\n\tIndexing enabled. \n/System/Volumes/Data:\n\tIndexing disabled. \n\
                       /System/Volumes/Preboot:\n\tIndexing enabled. \n";

thread_local! {
    static RUNS: Cell<u32> = const { Cell::new(0) };
}

/// A port that counts its runs and prints [`ONE_OFF`].
const COUNTING: MdutilProbe = MdutilProbe {
    status_all: || {
        RUNS.with(|n| n.set(n.get() + 1));
        Ok(ONE_OFF.to_owned())
    },
};

fn runs() -> u32 {
    RUNS.with(Cell::get)
}

/// The runs `busy` (by the second of each reading) makes over `secs` readings a second apart.
fn runs_over(secs: u64, busy: impl Fn(u64) -> bool) -> u32 {
    RUNS.with(|n| n.set(0));
    let mut cadence = Cadence::default();
    for t in 0..secs {
        let read = cadence.step(t * 1000, busy(t), &COUNTING);
        assert!(read.is_some(), "the first run read the volumes, t={t}");
    }
    runs()
}

/// Each volume's indexing is read from one `mdutil -s -a` (the host's argv, no `sudo`): three
/// volumes, the data volume off. Over three minutes of readings a second apart with the family
/// busy, `mdutil` runs three times, once a minute; a bar crossed at 30 s makes no run before
/// the minute; a quiet family's runs are five minutes apart.
/// NEGATIVE CONTROL: the same three busy minutes through a cadence whose floors are zero (the
/// fault a cadence that ignored them would have) run `mdutil` on every reading, 180 times.
#[test]
fn each_volumes_spotlight_indexing_is_read_without_root_at_most_once_a_minute() {
    assert_eq!(
        MDUTIL_ARGS,
        ["-s", "-a"],
        "a status read, which needs no root"
    );
    let v = volumes(ONE_OFF).expect("the fixture reads");
    assert_eq!(
        v.iter()
            .map(|v| (v.path.as_str(), v.indexing.clone()))
            .collect::<Vec<_>>(),
        [
            ("/", Indexing::Enabled),
            ("/System/Volumes/Data", Indexing::Disabled),
            ("/System/Volumes/Preboot", Indexing::Enabled),
        ]
    );
    assert_eq!(
        state(0, 0, Some(&v)),
        State::Idle,
        "one volume on is not off"
    );

    assert_eq!(runs_over(180, |_| true), 3, "busy: once a minute");
    assert_eq!(
        runs_over(60, |t| t == 30),
        1,
        "a crossing at 30 s waits for the minute"
    );
    assert_eq!(
        runs_over(61, |t| t == 30),
        2,
        "and runs at the minute after it"
    );
    assert_eq!(runs_over(600, |_| false), 2, "quiet: every five minutes");

    RUNS.with(|n| n.set(0));
    let mut no_floor = Cadence {
        every_ms: 0,
        quiet_ms: 0,
        ..Cadence::default()
    };
    for t in 0..180 {
        assert!(no_floor.step(t * 1000, true, &COUNTING).is_some());
    }
    assert_eq!(runs(), 180, "the control: no floor runs each reading");

    // The single-volume form, the other disabled sentence, and an error line.
    assert_eq!(
        volumes("/System/Volumes/Data:\n\tIndexing enabled. \n"),
        Ok(vec![Volume {
            path: "/System/Volumes/Data".into(),
            indexing: Indexing::Enabled,
        }])
    );
    assert_eq!(
        volumes("/Volumes/X:\n\tIndexing and searching disabled.\n").map(|v| v[0].indexing.clone()),
        Ok(Indexing::Disabled)
    );
    assert_eq!(
        volumes("/Volumes/X:\n\tError: unknown indexing state.\n").map(|v| v[0].indexing.clone()),
        Ok(Indexing::Unknown("Error: unknown indexing state.".into()))
    );
    assert_eq!(
        volumes("/Volumes/X:\n").map(|v| v[0].indexing.clone()),
        Ok(Indexing::Unknown(String::new()))
    );
    assert!(volumes("").is_err());
    assert!(
        volumes("\tIndexing enabled.\n").is_err(),
        "a status before any volume"
    );
    assert!(volumes("Indexing enabled.\nIndexing disabled.").is_err());

    // An inert port leaves the volumes unread, which never reads off.
    let mut inert = Cadence::default();
    assert!(inert.step(0, false, &MdutilProbe::INERT).is_none());
    let all_off =
        volumes("/:\n\tIndexing disabled.\n/System/Volumes/Data:\n\tIndexing disabled.\n")
            .expect("reads");
    assert_eq!(state(0, 0, Some(&all_off)), State::Off);
    assert_eq!(state(0, 0, None), State::Idle);
    assert_eq!(state(0, 0, Some(&[])), State::Idle);
}

/// A volume reading older than ten minutes is not carried into a reading.
#[test]
fn a_stale_volume_reading_is_dropped() {
    let read = VolumesRead {
        volumes: volumes(ONE_OFF).expect("reads"),
        at_ms: 0,
    };
    let r = reading(&[], Some(&read), VOLUMES_STALE_MS, 0, 5000);
    assert_eq!(r.volumes_age_ms, Some(VOLUMES_STALE_MS));
    let r = reading(&[], Some(&read), VOLUMES_STALE_MS + 1, 0, 5000);
    assert_eq!((r.volumes, r.volumes_age_ms), (None, None));
}

/// A family row of coalition `id` led by `signing_id` under `voucher`, at 300 millicores.
fn row(id: u64, signing_id: &str, voucher: crate::explain::Voucher) -> CoalRow {
    CoalRow {
        id,
        cpu_mc: 300,
        measured: true,
        read_bps: 1_000_000,
        footprint: Some(10 << 20),
        lead: Some(crate::explain::Lead {
            pid: u32::try_from(id).unwrap_or(0),
            birth_ms: 0,
            uid: Some(501),
            exe: None,
            voucher,
            signing_id: Some(signing_id.into()),
            team: None,
        }),
        ..CoalRow::default()
    }
}

/// The reading counts a row by the cause engine's own rule: an Apple-signed member's row whose
/// ownership is not mixed. A row of mixed ownership, an ad-hoc lead under a member's signing id
/// and a look-alike signing id are each left out.
/// CONTROL: the same rows, honest, each count.
#[test]
fn the_family_counts_only_rows_the_cause_engine_names() {
    use crate::explain::Voucher;
    let honest = row(10, "com.apple.mdworker", Voucher::Apple);
    let r = reading(std::slice::from_ref(&honest), None, 0, 0, 5000);
    assert_eq!(
        (r.shares.len(), r.cpu_mc, r.footprint_bytes()),
        (1, 300, Some(10 << 20))
    );

    let mixed = CoalRow {
        ownership_mixed: true,
        ..row(11, "com.apple.mds_stores", Voucher::Apple)
    };
    let liar = row(12, "com.apple.mds", Voucher::AdHoc);
    let look_alike = row(
        13,
        "com.apple.SpotlightSettingsIntentsExtension",
        Voucher::Apple,
    );
    let r = reading(&[mixed.clone(), liar, look_alike], None, 0, 0, 5000);
    assert_eq!((r.shares.len(), r.cpu_mc, r.read_bps), (0, 0, 0));
    assert_eq!(r.state, State::Idle);

    let unmixed = CoalRow {
        ownership_mixed: false,
        ..mixed
    };
    let r = reading(&[honest, unmixed], None, 0, 0, 5000);
    assert_eq!((r.shares.len(), r.cpu_mc), (2, 600), "the control");
    assert_eq!(r.state, State::Indexing);
}

/// A split reading is carried for a minute on the monotonic clock, which no wall clock set
/// back can stretch, and its footprint reads unread where no share's was read.
#[test]
fn a_carried_reading_ages_on_the_monotonic_clock() {
    let mut facts = crate::explain::WhyFacts {
        at_unix_ms: 1_800_000_000_000,
        window_ns: 5_000_000_000,
        ..crate::explain::WhyFacts::default()
    };
    facts.split.rows = vec![CoalRow {
        footprint: None,
        ..row(10, "com.apple.mdworker", crate::explain::Voucher::Apple)
    }];
    let mut family = Family::default();
    let made = family.absorb(Some(&facts), None, 1_000).expect("read");
    assert!(made.footprint_unread() && made.footprint_bytes().is_none());
    assert!(family.busy());
    assert!(
        family
            .absorb(None, None, 1_000 + READING_CARRIED_MS)
            .is_some()
    );
    assert!(
        family
            .absorb(None, None, 1_001 + READING_CARRIED_MS)
            .is_none()
    );
    assert!(!family.busy());
}

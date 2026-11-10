// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

const T0: i64 = 1_762_500_000_000;

/// A4 §8's law: a lid change is the closed bit changing, never a message. The root domain sends
/// its clamshell message when either bit moves; N1 heard it with the sleep bit alone when AC was
/// attached with the lid open, which is no lid change. The state read at the start has no
/// moment; the first change has one. A lid read where the root domain publishes none is no lid,
/// and its first reading after that is no change either.
/// NEGATIVE CONTROL: taking any clamshell message as a closed lid (`Lid::Closed` in
/// `clamshell` whatever the bits) fails the second assertion: N1's AC attach reads as a lid
/// closing at T0 + 1.
#[test]
fn a_lid_change_is_the_closed_bit_changing_not_a_message() {
    let mut lid = LidLatch::default();
    assert!(!lid.read(Some(false), T0), "the first read is no change");
    assert!(
        !lid.clamshell(CLAMSHELL_SLEEP_BIT, T0 + 1),
        "AC attached with the lid open: the sleep bit alone"
    );
    assert_eq!(
        lid.state(),
        Some(LidState {
            lid: Lid::Open,
            changed_ms: None
        })
    );
    assert!(lid.clamshell(CLAMSHELL_CLOSED_BIT | CLAMSHELL_SLEEP_BIT, T0 + 2));
    assert!(!lid.clamshell(CLAMSHELL_CLOSED_BIT, T0 + 3), "still closed");
    assert!(!lid.read(Some(true), T0 + 4), "a read of the same lid");
    assert_eq!(
        lid.state(),
        Some(LidState {
            lid: Lid::Closed,
            changed_ms: Some(T0 + 2)
        })
    );
    assert!(
        lid.read(Some(false), T0 + 5),
        "a read at a wake that finds it open"
    );
    assert_eq!(lid.state().map(|s| s.changed_ms), Some(Some(T0 + 5)));

    let mut desktop = LidLatch::default();
    assert!(!desktop.read(None, T0));
    assert_eq!(desktop.state().map(|s| s.lid), Some(Lid::None));
    assert!(
        !desktop.read(Some(false), T0 + 1),
        "no lid then a lid is no change"
    );
    assert_eq!(desktop.state().map(|s| s.changed_ms), Some(None));
}

const NONE: Posted = Posted {
    source: false,
    charge: false,
};
const SOURCE: Posted = Posted {
    source: true,
    charge: false,
};
const CHARGE: Posted = Posted {
    source: false,
    charge: true,
};

fn on(source: PowerSource, pct: u8) -> PowerReading {
    PowerReading {
        source,
        battery: Some(BatteryReading {
            pct,
            charging: source == PowerSource::Ac,
        }),
        adapter_w: None,
    }
}

/// A4 §8's law: the power sources are read only on a notification. Readings with no post read
/// nothing and hand on the last read; a post reads once; a switch of the source stamps the
/// reading that saw it; a read that failed is tried again a minute later ([`POWER_RETRY_MS`]),
/// and only until one answers.
/// NEGATIVE CONTROL: reading at every step (`if true` for `if posted.source || posted.charge ||
/// retry`) fails the first assertion: a reading with nothing posted reads the sources.
#[test]
fn the_power_sources_are_read_only_when_a_notification_fired() {
    let (ac, on_battery) = (on(PowerSource::Ac, 80), on(PowerSource::Battery, 80));
    let reads = Cell::new(0u32);
    let read = |r: Option<PowerReading>| {
        reads.set(reads.get() + 1);
        r
    };
    let mut latch = PowerLatch::default();
    let step =
        |latch: &mut PowerLatch, posted, at, r| latch.step(posted, at, || read(r), || Some(70));
    assert_eq!(
        step(&mut latch, NONE, T0, Some(ac)),
        None,
        "nothing posted, nothing read"
    );
    assert_eq!(reads.get(), 0);
    let first = step(&mut latch, SOURCE, T0 + 1, Some(ac));
    for at in 2..5 {
        assert_eq!(step(&mut latch, NONE, T0 + at, Some(on_battery)), first);
    }
    assert_eq!(reads.get(), 1, "one post, one read");
    assert_eq!(
        first,
        Some(PowerState {
            reading: PowerReading {
                adapter_w: Some(70),
                ..ac
            },
            changed_ms: None
        }),
        "the first read is no switch"
    );
    let switched = step(&mut latch, SOURCE, T0 + 5, Some(on_battery));
    assert_eq!(
        switched,
        Some(PowerState {
            reading: on_battery,
            changed_ms: Some(T0 + 5)
        })
    );
    assert_eq!(
        step(&mut latch, CHARGE, T0 + 6, None),
        None,
        "a failed read is unread"
    );
    assert_eq!(reads.get(), 3);
    assert_eq!(step(&mut latch, NONE, T0 + 7, Some(on_battery)), None);
    assert_eq!(reads.get(), 3, "no retry within the minute");
    let retry_at = T0 + 6 + i64::try_from(POWER_RETRY_MS).expect("small");
    let again = step(&mut latch, NONE, retry_at, Some(on_battery));
    assert_eq!(reads.get(), 4, "the retry, a minute on");
    assert_eq!(
        again.map(|s| s.changed_ms),
        Some(Some(T0 + 5)),
        "no new switch"
    );
    let _ = step(&mut latch, NONE, retry_at * 2, Some(ac));
    assert_eq!(reads.get(), 4, "no retry once one answered");
}

/// A power read that never answers costs a read a minute, not one a reading: ten minutes of
/// readings a second after one post read 11 times. NEGATIVE CONTROL: retrying at every reading
/// after a failure (the latch before this law) reads 601 times.
#[test]
fn a_power_read_that_never_answers_is_tried_once_a_minute() {
    let reads = Cell::new(0u32);
    let mut latch = PowerLatch::default();
    let mut step = |posted, at| {
        latch.step(
            posted,
            at,
            || {
                reads.set(reads.get() + 1);
                None
            },
            || Some(70),
        )
    };
    assert_eq!(step(SOURCE, T0), None);
    for s in 1..=600 {
        assert_eq!(step(NONE, T0 + s * 1000), None);
    }
    assert_eq!(reads.get(), 11);
}

/// The adapter's rating is read with a source post on AC, or where the source became AC, and a
/// charge post keeps the rating read before; on battery there is none. A swapped adapter is a
/// source post, so its new rating is read.
/// NEGATIVE CONTROL: reading the adapter at every read on AC (`PowerSource::Ac => adapter()`
/// for both AC arms) fails the second rating assertion: a charge post reads the adapter's 96 W
/// rather than keeping 70.
#[test]
fn the_adapter_is_read_only_when_the_source_was_posted() {
    let adapters = Cell::new(0u32);
    let mut latch = PowerLatch::default();
    let mut step = |posted, at, r, w: u32| {
        latch.step(
            posted,
            at,
            || Some(r),
            || {
                adapters.set(adapters.get() + 1);
                Some(w)
            },
        )
    };
    let rated = |s: Option<PowerState>| s.and_then(|s| s.reading.adapter_w);
    assert_eq!(
        rated(step(SOURCE, T0, on(PowerSource::Ac, 80), 70)),
        Some(70)
    );
    assert_eq!(
        rated(step(CHARGE, T0 + 1, on(PowerSource::Ac, 81), 96)),
        Some(70)
    );
    assert_eq!(
        rated(step(CHARGE, T0 + 2, on(PowerSource::Ac, 82), 96)),
        Some(70)
    );
    assert_eq!(adapters.get(), 1, "charge posts read no adapter");
    assert_eq!(
        rated(step(SOURCE, T0 + 3, on(PowerSource::Battery, 82), 96)),
        None
    );
    assert_eq!(adapters.get(), 1, "no adapter on battery");
    assert_eq!(
        rated(step(CHARGE, T0 + 4, on(PowerSource::Ac, 82), 96)),
        Some(96),
        "the source became AC though only the charge was posted"
    );
    assert_eq!(
        rated(step(SOURCE, T0 + 5, on(PowerSource::Ac, 82), 140)),
        Some(140),
        "a swap"
    );
    assert_eq!(adapters.get(), 3);
}

/// The words `IOPSGetProvidingPowerSourceType` answers, and nothing else.
#[test]
fn a_source_is_one_of_the_power_managers_words() {
    assert_eq!(PowerSource::of("AC Power"), Some(PowerSource::Ac));
    assert_eq!(PowerSource::of("Battery Power"), Some(PowerSource::Battery));
    assert_eq!(PowerSource::of("UPS Power"), Some(PowerSource::Ups));
    assert_eq!(PowerSource::of("ac"), None);
    assert_eq!(PowerSource::of(""), None);
}

/// A sleep is a span only where the wake follows it; its cause is the power manager's and its
/// wake word the kernel's.
#[test]
fn the_last_sleep_is_a_span_only_where_the_wake_follows_it() {
    let mut t = crate::timeline::Timeline {
        boot_ms: T0,
        sleep_ms: None,
        wake_ms: None,
        wake_reason: None,
    };
    assert_eq!(
        awake_of(&t, None),
        Awake {
            boot_ms: T0,
            last_sleep: None
        }
    );
    t.sleep_ms = Some(T0 + 10);
    t.wake_ms = Some(T0 + 20);
    t.wake_reason = Some("RTP.multi-touch".into());
    let a = awake_of(&t, Some("Clamshell Sleep".into()));
    assert_eq!(
        a.last_sleep,
        Some(SleepSpan {
            from_ms: T0 + 10,
            to_ms: T0 + 20,
            cause: Some("Clamshell Sleep".into()),
            wake: Some("RTP.multi-touch".into()),
        })
    );
    t.wake_ms = Some(T0 + 5);
    assert_eq!(
        awake_of(&t, None).last_sleep,
        None,
        "a wake before the sleep"
    );
}

/// Off macOS no reader runs, and each says so.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_every_situation_reader_says_it_has_none() {
    assert!(!LID_READER && !POWER_READER && !DISPLAY_READER && !SLEEP_READER);
    assert_eq!(PowerWatch::new().read(T0), None);
    assert_eq!(Watch::start().heard(), Heard::default());
    assert_eq!(awake(), None);
}

/// This Mac's power sources read at the first reading (the tokens' first check says posted),
/// in the power manager's words.
#[cfg(target_os = "macos")]
#[test]
fn this_mac_reads_its_power_source_at_the_first_reading() {
    let mut power = PowerWatch::new();
    let state = power.read(T0).expect("the power sources");
    if let Some(b) = state.reading.battery {
        assert!(b.pct <= 100, "{b:?}");
    }
    if state.reading.source != PowerSource::Ac {
        assert_eq!(state.reading.adapter_w, None, "{state:?}");
    }
}

/// The watch reads the lid, the last sleep and (on Apple silicon) the external displays as it
/// starts, and its thread lets go of what it shares once the watch is dropped.
#[cfg(target_os = "macos")]
#[test]
fn the_watch_reads_as_it_starts_and_its_thread_ends_when_dropped() {
    let watch = Watch::start();
    let heard = watch.heard();
    assert!(
        heard.lid.is_some(),
        "the root domain answers on a Mac: {heard:?}"
    );
    assert!(heard.awake.is_some(), "{heard:?}");
    assert_eq!(
        heard.external_displays.is_some(),
        DISPLAY_READER,
        "{heard:?}"
    );
    if DISPLAY_READER {
        assert_eq!(
            heard.external_displays,
            external_displays(),
            "the watch counts what one listing counts"
        );
    }
    let handle = watch.handle();
    handle.woke();
    assert_eq!(watch.heard().lid.map(|l| l.lid), heard.lid.map(|l| l.lid));
    drop(watch);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the watch's thread let go of its state",
        || (Arc::strong_count(&handle.0) == 1).then_some(()),
    );
}

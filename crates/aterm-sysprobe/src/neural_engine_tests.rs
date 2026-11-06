// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Neural Engine reader's laws (`docs/DESIGN-stats-deck-resources-2026-11-04.md` §8).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;

const FAST: Duration = Duration::from_micros(90);
const SLOW: Duration = Duration::from_millis(5);

fn power(current: i64, max: i64) -> Power {
    Power {
        current: Some(current),
        max: Some(max),
    }
}

fn started() -> SelfCheck {
    SelfCheck::start(Some(Entry { cores: Some(16) })).expect("16 cores start the reader")
}

/// The first [`COST_READINGS`] readings at `costs` (the rest at [`FAST`]), off then on: the last
/// one's answer.
fn judged(check: &mut SelfCheck, costs: &[Duration]) -> Result<Reading, Check> {
    let mut last = Err(Check::Cost);
    for i in 0..COST_READINGS {
        let cost = costs.get(i).copied().unwrap_or(FAST);
        last = check.step(power(i64::from(i % 2 == 1), 1), cost);
        if i + 1 < COST_READINGS {
            assert!(last.is_ok(), "reading {i} is judged only with the rest");
        }
    }
    last
}

/// The reader starts only where the engine's class matches and its core count reads a positive
/// integer; it reads each power state only from 0 to the dictionary's maximum; and it keeps
/// reading only while its own p95 over its first 60 readings is within 0.2 ms. A failed check
/// stands for the process, and a retired reader reads the registry no more. NEGATIVE CONTROLS:
/// one more slow reading in the first 60, a state one past its maximum, and a cost one
/// microsecond over the line each retire a reader the same steps otherwise keep.
#[test]
fn the_neural_engine_reader_starts_only_where_its_self_check_passes() {
    assert_eq!(SelfCheck::start(None), Err(Check::Matched), "an Intel Mac");
    for cores in [None, Some(0), Some(-16), Some(70_000)] {
        assert_eq!(
            SelfCheck::start(Some(Entry { cores })),
            Err(Check::Cores),
            "{cores:?}"
        );
    }

    let mut check = started();
    assert_eq!(
        check.step(power(0, 1), FAST),
        Ok(Reading {
            cores: 16,
            powered: false
        })
    );
    assert_eq!(
        check.step(power(1, 1), FAST),
        Ok(Reading {
            cores: 16,
            powered: true
        })
    );
    for bad in [
        power(2, 1),
        power(-1, 1),
        Power {
            current: None,
            max: Some(1),
        },
        Power {
            current: Some(0),
            max: None,
        },
    ] {
        let mut check = started();
        assert_eq!(check.step(bad, FAST), Err(Check::PowerState), "{bad:?}");
        assert_eq!(
            check.step(power(1, 1), FAST),
            Err(Check::PowerState),
            "a failed check stands"
        );
        assert_eq!(check.failed(), Some(Check::PowerState));
    }

    // Nearest-rank p95 over 60: three slow readings pass, four retire the reader.
    let mut kept = started();
    assert!(judged(&mut kept, &[SLOW; 3]).is_ok(), "3 of 60 slow");
    assert!(
        kept.step(power(1, 1), SLOW).is_ok(),
        "only the first 60 are judged"
    );
    let mut retired = started();
    assert_eq!(
        judged(&mut retired, &[SLOW; 4]),
        Err(Check::Cost),
        "CONTROL: 4 of 60 slow"
    );
    assert_eq!(retired.step(power(1, 1), FAST), Err(Check::Cost));
    let mut at_line = started();
    assert!(judged(&mut at_line, &[COST_LINE; COST_READINGS]).is_ok());
    let mut over = started();
    assert_eq!(
        judged(
            &mut over,
            &[COST_LINE + Duration::from_micros(1); COST_READINGS]
        ),
        Err(Check::Cost),
        "CONTROL: over the line"
    );

    // The reader matches once, on its first reading, and reads nothing after a failed check.
    static UNMATCHED_POWER: AtomicUsize = AtomicUsize::new(0);
    let mut intel = Reader::on(Source {
        entry: || None,
        power: || {
            UNMATCHED_POWER.fetch_add(1, Ordering::Relaxed);
            Power::default()
        },
    });
    assert_eq!(intel.read(), Err(Check::Matched));
    assert_eq!(intel.read(), Err(Check::Matched));
    assert_eq!(UNMATCHED_POWER.load(Ordering::Relaxed), 0, "no state read");

    static ENTRIES: AtomicUsize = AtomicUsize::new(0);
    static OUT_OF_RANGE: AtomicUsize = AtomicUsize::new(0);
    let mut odd = Reader::on(Source {
        entry: || {
            ENTRIES.fetch_add(1, Ordering::Relaxed);
            Some(Entry { cores: Some(16) })
        },
        power: || {
            OUT_OF_RANGE.fetch_add(1, Ordering::Relaxed);
            power(2, 1)
        },
    });
    assert_eq!(odd.read(), Err(Check::PowerState));
    assert_eq!(odd.read(), Err(Check::PowerState));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1, "matched once");
    assert_eq!(
        OUT_OF_RANGE.load(Ordering::Relaxed),
        1,
        "read until it failed"
    );

    // The reader times its own reads: one that takes 0.3 ms is over the line at every reading
    // (a sleep never returns early), so the 60th retires it.
    static SLOW_READS: AtomicUsize = AtomicUsize::new(0);
    let mut slow = Reader::on(Source {
        entry: || Some(Entry { cores: Some(16) }),
        power: || {
            SLOW_READS.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_micros(300));
            power(0, 1)
        },
    });
    for reading in 1..COST_READINGS {
        assert!(slow.read().is_ok(), "reading {reading}");
    }
    assert_eq!(slow.read(), Err(Check::Cost));
    assert_eq!(slow.read(), Err(Check::Cost));
    assert_eq!(SLOW_READS.load(Ordering::Relaxed), COST_READINGS);
}

/// The live reader's CoreFoundation path, on this M5's `H11ANEIn` entry as `ioreg -a -r -c
/// H11ANEIn -d 1` recorded it on 2026-11-05 under macOS 27.0.1 (Apple M5, `ane,t8132exclave`):
/// idle, and during a Vision job pinned to the engine (MR-mac's `ane_load`). Each is read back
/// into the objects a live read copies, and its `DeviceProperties` and `IOPowerManagement` go
/// through the same readers and self-check: 16 cores, off, then on. NEGATIVE CONTROLS: the same
/// record with its state one past its maximum, or with no core count, fails the check that
/// reads it.
#[cfg(target_os = "macos")]
#[test]
fn the_neural_engine_reader_parses_the_recorded_fixture() {
    use crate::iokit;
    use crate::macos::{AneKeys, Cf, ane_cores_of, ane_power_of};

    const OFF: &str = include_str!("../fixtures/ioreg/m5-27.0.1-h11anein-off.plist");
    const ON: &str = include_str!("../fixtures/ioreg/m5-27.0.1-h11anein-on.plist");

    /// The recorded entry's reading through the live readers: what the reader's start and first
    /// step answer.
    fn read(record: &str) -> Result<Reading, Check> {
        let keys = AneKeys::new().expect("the keys");
        let bytes = record.as_bytes();
        // SAFETY: `bytes` is live for the call and its length is passed; the data is +1 or
        // NULL, adopted once.
        let data = Cf::adopt(
            unsafe {
                iokit::CFDataCreate(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    iokit::CFIndex::try_from(bytes.len()).expect("a length"),
                )
            }
            .cast(),
        )
        .expect("the record's bytes");
        // SAFETY: `data` is a live CFData; the format and error out-pointers may be NULL; the
        // property list is +1 or NULL, adopted once.
        let list = Cf::adopt(unsafe {
            iokit::CFPropertyListCreateWithData(
                std::ptr::null(),
                data.0.cast(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        })
        .expect("the record parses");
        // SAFETY: `list` is live; its type is checked before it is read as an array, and its
        // first element, borrowed while it lives, before it is read as a dictionary, whose
        // values are borrowed while it lives.
        let (props, pm) = unsafe {
            assert_eq!(iokit::CFGetTypeID(list.0), iokit::CFArrayGetTypeID());
            assert_eq!(iokit::CFArrayGetCount(list.0.cast()), 1, "one engine");
            let entry = iokit::CFArrayGetValueAtIndex(list.0.cast(), 0);
            assert_eq!(iokit::CFGetTypeID(entry), iokit::CFDictionaryGetTypeID());
            (
                iokit::CFDictionaryGetValue(entry.cast(), keys.device.0),
                iokit::CFDictionaryGetValue(entry.cast(), keys.power.0),
            )
        };
        let mut check = SelfCheck::start(Some(Entry {
            cores: ane_cores_of(props, &keys),
        }))?;
        check.step(ane_power_of(pm, &keys), FAST)
    }

    assert_eq!(
        read(OFF),
        Ok(Reading {
            cores: 16,
            powered: false
        })
    );
    assert_eq!(
        read(ON),
        Ok(Reading {
            cores: 16,
            powered: true
        })
    );
    let state = "<key>CurrentPowerState</key>\n\t\t\t<integer>1</integer>";
    assert!(ON.contains(state), "CONTROL: the record holds the state");
    assert_eq!(
        read(&ON.replace(
            state,
            "<key>CurrentPowerState</key>\n\t\t\t<integer>2</integer>"
        )),
        Err(Check::PowerState)
    );
    let cores = "<key>ANEDevicePropertyNumANECores</key>";
    assert!(OFF.contains(cores), "CONTROL: the record holds the count");
    assert_eq!(
        read(&OFF.replace(cores, "<key>ANEDevicePropertyNumANECoresGone</key>")),
        Err(Check::Cores)
    );
}

/// Live: the native reader on this host, through the registry calls the injected laws stand in
/// for (the class match, and the `DeviceProperties` and `IOPowerManagement` copies). An Apple
/// silicon Mac (`hw.optional.arm64`, which reads 1 under Rosetta 2 too) that is no virtual
/// machine (`kern.hv_vmm_present`) has the engine: its first reading matches it and reads a
/// positive core count, and two more readings answer the same count. An Intel Mac has none: the
/// reader answers [`Check::Matched`] at every reading, which stands in for
/// `an_intel_mac_matches_no_neural_engine` wherever an Intel Mac runs the suite. A virtual
/// machine may expose the engine or not, so there the reader answers readings or `Matched`, and
/// never another check. NEGATIVE CONTROLS, mutations of the glue run once on this M5 (A2 §3.1):
/// a misspelt class, `ane_entry` copying `IOPowerManagement`, and `ane_power` copying
/// `DeviceProperties` each fail it, at its first reading, as `Matched`, `Cores` and `PowerState`.
#[cfg(target_os = "macos")]
#[test]
fn the_neural_engine_reads_on_this_host() {
    let apple_silicon = crate::macos::sysctl_int(c"hw.optional.arm64") == Some(1);
    let virtual_machine = crate::macos::sysctl_int(c"kern.hv_vmm_present") == Some(1);
    let mut reader = Reader::native().expect("macOS has a reader");
    let readings = [reader.read(), reader.read(), reader.read()];
    match readings[0] {
        _ if !apple_silicon => {
            assert_eq!(
                readings,
                [Err(Check::Matched); 3],
                "an Intel Mac has no engine"
            );
        }
        Err(Check::Matched) if virtual_machine => {
            assert_eq!(
                readings,
                [Err(Check::Matched); 3],
                "a VM without the engine"
            );
        }
        first => {
            let cores = first.expect("Apple silicon's engine reads").cores;
            assert!(cores > 0, "{readings:?}");
            for reading in readings {
                assert_eq!(reading.map(|r| r.cores), Ok(cores), "{readings:?}");
            }
        }
    }
}

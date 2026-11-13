// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Windows GPU and thermal decoders on instance names, registry rows and counter
//! values recorded on a Windows 11 laptop (an AMD Radeon 780M beside the Basic Render Driver),
//! and on item buffers laid out at SDK offsets.

use super::*;

/// The AMD adapter's id on the recorded boot.
const AMD: u64 = 0x0001_137F;
/// The Basic Render Driver's.
const WARP: u64 = 0x0001_2F10;

fn item(name: &str, value: u64) -> RawItem {
    RawItem {
        name: name.to_owned(),
        value,
    }
}

fn engine(pid: u32, luid: u64, eng: u32, kind: &str, running: u64) -> RawItem {
    item(
        &format!(
            "pid_{pid}_luid_0x{:08X}_0x{:08X}_phys_0_eng_{eng}_engtype_{kind}",
            luid >> 32,
            luid & 0xffff_ffff
        ),
        running,
    )
}

/// The registry's two rows as recorded.
fn recorded_adapters() -> Vec<Adapter> {
    vec![
        Adapter {
            luid: WARP,
            name: "Microsoft Basic Render Driver".to_owned(),
            vendor: 0x1414,
            dedicated: 0,
            driver: Some(2_814_751_477_605_871),
            seen: 134_364_296_290_444_339,
        },
        Adapter {
            luid: AMD,
            name: "AMD Radeon 780M Graphics".to_owned(),
            vendor: 0x1002,
            dedicated: 536_870_912,
            driver: Some(9_007_200_698_448_539),
            seen: 134_364_296_290_444_339,
        },
    ]
}

#[test]
fn an_engine_instance_name_reads_as_its_process_adapter_and_engine() {
    assert_eq!(
        engine_of("pid_1160_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D"),
        Some(Engine {
            pid: 1160,
            luid: AMD,
            phys: 0,
            eng: 0,
            kind: "3D"
        })
    );
    // Lower case, as a path the counter list prints, and a type with spaces and a digit.
    assert_eq!(
        engine_of("pid_4_luid_0x00000000_0x0001137f_phys_0_eng_9_engtype_video codec 0"),
        Some(Engine {
            pid: 4,
            luid: AMD,
            phys: 0,
            eng: 9,
            kind: "video codec 0"
        })
    );
    // The high half of the id, a second physical adapter, a two-digit engine, no type.
    assert_eq!(
        engine_of("pid_70000_luid_0x00000001_0x00012F10_phys_1_eng_16_engtype_"),
        Some(Engine {
            pid: 70000,
            luid: (1 << 32) | WARP,
            phys: 1,
            eng: 16,
            kind: ""
        })
    );
}

/// NEGATIVE CONTROL: a name of any other shape is no engine, never a guessed one.
#[test]
fn a_name_of_another_shape_is_no_engine() {
    for name in [
        "",
        "_Total",
        "luid_0x00000000_0x0001137F_phys_0",
        "pid__luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "pid_+4_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "pid_4294967296_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "pid_4_luid_00000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "pid_4_luid_0x0000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "pid_4_luid_0x00000000_0x0001137G_phys_0_eng_0_engtype_3D",
        "pid_4_luid_0x00000000_0x0001137F_phys__eng_0_engtype_3D",
        "pid_4_luid_0x00000000_0x0001137F_phys_0_eng_x_engtype_3D",
        "pid_4_luid_0x00000000_0x0001137F_phys_0_eng_0",
        "pid_4_luid_0x00000000_0x0001137F_phys_0_eng_0_type_3D",
        "pid_4_luid_0x00000000_0x0001137Fé_phys_0_eng_0_engtype_3D",
    ] {
        assert_eq!(engine_of(name), None, "{name}");
    }
}

#[test]
fn an_adapter_memory_instance_name_reads_as_its_adapter() {
    assert_eq!(
        adapter_of("luid_0x00000000_0x0001137F_phys_0"),
        Some((AMD, 0))
    );
    assert_eq!(
        adapter_of("luid_0x00000002_0x00012f10_phys_3"),
        Some(((2 << 32) | WARP, 3))
    );
    // NEGATIVE CONTROL: an engine's name, a partition's suffix and a bare id are no adapter.
    for name in [
        "pid_4_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
        "luid_0x00000000_0x0001137F_phys_0_part_0",
        "luid_0x00000000_0x0001137F",
        "luid_0x00000000_0x0001137F_phys_",
    ] {
        assert_eq!(adapter_of(name), None, "{name}");
    }
}

/// A `PdhGetRawCounterArrayW` buffer as the call fills it at `base`: the items, then their
/// names, each item's pointer the absolute address of its name.
fn raw_buffer(base: u64, items: &[(&str, u32, i64)]) -> Vec<u8> {
    let mut buf = vec![0u8; items.len() * RAW_ITEM_SIZE];
    for (i, (name, status, value)) in items.iter().enumerate() {
        let at = i * RAW_ITEM_SIZE;
        let name_at = base + buf.len() as u64;
        buf[at + RAW_ITEM_NAME..at + RAW_ITEM_NAME + 8].copy_from_slice(&name_at.to_le_bytes());
        buf[at + RAW_ITEM_STATUS..at + RAW_ITEM_STATUS + 4].copy_from_slice(&status.to_le_bytes());
        buf[at + RAW_ITEM_VALUE..at + RAW_ITEM_VALUE + 8].copy_from_slice(&value.to_le_bytes());
        for unit in name.encode_utf16().chain(Some(0)) {
            buf.extend_from_slice(&unit.to_le_bytes());
        }
    }
    buf
}

#[test]
fn a_raw_item_buffer_decodes_to_names_and_values() {
    let base = 0x0000_01F4_2A00_0000;
    let buf = raw_buffer(
        base,
        &[
            (
                "pid_1160_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
                CSTATUS_VALID_DATA,
                16_459_064_033,
            ),
            (
                "luid_0x00000000_0x0001137F_phys_0",
                CSTATUS_NEW_DATA,
                395_010_048,
            ),
        ],
    );
    assert_eq!(
        raw_items(&buf, base, 2),
        vec![
            item(
                "pid_1160_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D",
                16_459_064_033
            ),
            item("luid_0x00000000_0x0001137F_phys_0", 395_010_048),
        ]
    );
}

/// NEGATIVE CONTROL: an item that is not valid data, whose value is negative, or whose name
/// points outside the buffer or never ends is left out, and a count past the buffer stops at
/// the buffer.
#[test]
fn a_raw_item_that_is_not_data_is_left_out() {
    let base = 0x1000_0000;
    let good = ("luid_0x00000000_0x0001137F_phys_0", CSTATUS_VALID_DATA, 7);
    // `PDH_CSTATUS_NO_INSTANCE` and `PDH_CSTATUS_INVALID_DATA`.
    let buf = raw_buffer(
        base,
        &[
            ("gone", 0x8000_07D1, 1),
            ("invalid", 0xC000_0BBA, 2),
            ("negative", CSTATUS_VALID_DATA, -1),
            good,
        ],
    );
    assert_eq!(raw_items(&buf, base, 4), vec![item(good.0, 7)]);
    // The same bytes read as if filled elsewhere: every name pointer is outside the buffer.
    assert_eq!(raw_items(&buf, base + (1 << 20), 4), vec![]);
    assert_eq!(raw_items(&buf, base - (1 << 20), 4), vec![]);
    // A count larger than the buffer holds: the items stop where the bytes do. The names'
    // bytes read as items carry no valid pointer, so only the real item stands.
    let one = raw_buffer(base, &[good]);
    assert_eq!(raw_items(&one[..RAW_ITEM_SIZE - 1], base, 1), vec![]);
    assert_eq!(raw_items(&one, base, 1000), vec![item(good.0, 7)]);
    // A name with no terminator inside the buffer.
    let cut = &one[..one.len() - 2];
    assert_eq!(raw_items(cut, base, 1), vec![]);
    assert_eq!(raw_items(&[], base, 3), vec![]);
}

#[test]
fn an_instance_id_is_stable_and_tells_every_field_apart() {
    let id = entry_of(1160, AMD, 0, 0);
    assert_eq!(id, entry_of(1160, AMD, 0, 0));
    let others = [
        entry_of(1161, AMD, 0, 0),
        entry_of(1160, WARP, 0, 0),
        entry_of(1160, AMD, 1, 0),
        entry_of(1160, AMD, 0, 1),
    ];
    assert!(others.iter().all(|other| *other != id), "{others:?}");
    // The physical and the engine index are not interchangeable.
    assert_ne!(others[2], others[3]);
    let read = engine_read(
        &[
            engine(1160, AMD, 0, "3D", 5),
            item("_Total", 9),
            engine(4, AMD, 1, "Copy", 6),
        ],
        77,
    );
    assert_eq!(read.at, 77);
    assert_eq!(
        read.rows,
        vec![
            EngineRow {
                entry: id,
                pid: 1160,
                luid: AMD,
                phys: 0,
                eng: 0,
                running: 5
            },
            EngineRow {
                entry: entry_of(4, AMD, 0, 1),
                pid: 4,
                luid: AMD,
                phys: 0,
                eng: 1,
                running: 6
            },
        ]
    );
}

/// One second, in the counters' 100 ns units.
const SECOND: u64 = 10_000_000;

#[test]
fn the_busy_share_sums_an_engines_processes_and_takes_the_busiest_engine() {
    let then = engine_read(
        &[
            engine(1160, AMD, 0, "3D", 16_459_064_033),
            engine(14576, AMD, 0, "3D", 903_341_417),
            engine(4, AMD, 1, "Copy", 1_770_578_500),
            engine(1160, AMD, 9, "Video Codec 0", 40),
            engine(1160, WARP, 0, "3D", 0),
        ],
        0,
    );
    // Over one second: the 3D engine ran 0.20 s for one process and 0.15 s for another, the
    // copy engine 0.08 s, the codec nothing; the software adapter nothing.
    let now = engine_read(
        &[
            engine(1160, AMD, 0, "3D", 16_459_064_033 + SECOND / 5),
            engine(14576, AMD, 0, "3D", 903_341_417 + SECOND * 15 / 100),
            engine(4, AMD, 1, "Copy", 1_770_578_500 + SECOND * 8 / 100),
            engine(1160, AMD, 9, "Video Codec 0", 40),
            engine(1160, WARP, 0, "3D", 0),
        ],
        SECOND,
    );
    let shares = busy(&then, &now).expect("a window");
    assert_eq!(shares.get(&AMD), Some(&35), "0.20 + 0.15 on the 3D engine");
    assert_eq!(
        shares.get(&WARP),
        Some(&0),
        "an idle adapter is listed at 0"
    );
    assert_eq!(shares.len(), 2);
    // NEGATIVE CONTROL: the engines are not summed across the adapter (that would be 43), and
    // the busiest process alone is not the engine's figure (that would be 20).
    assert_ne!(shares.get(&AMD), Some(&43));
    assert_ne!(shares.get(&AMD), Some(&20));
    // Twice the window, the same running time: half the share.
    let later = EngineRead {
        at: 2 * SECOND,
        ..now.clone()
    };
    assert_eq!(busy(&then, &later).expect("a window").get(&AMD), Some(&18));
}

#[test]
fn the_busy_share_counts_a_new_instance_whole_and_a_gone_or_fallen_one_not_at_all() {
    let then = engine_read(
        &[
            engine(10, AMD, 0, "3D", 1000),
            engine(20, AMD, 0, "3D", SECOND),
            engine(30, AMD, 0, "3D", 5 * SECOND),
        ],
        SECOND,
    );
    let now = engine_read(
        &[
            // Unchanged.
            engine(10, AMD, 0, "3D", 1000),
            // Fell: the pid went to a new process whose counter starts again.
            engine(20, AMD, 0, "3D", SECOND / 100),
            // pid 30 is gone, with whatever it ran.
            // New inside the window: all it ran counts.
            engine(40, AMD, 0, "3D", SECOND / 4),
        ],
        2 * SECOND,
    );
    assert_eq!(busy(&then, &now).expect("a window").get(&AMD), Some(&25));
    // A new instance that says it ran longer than the window (it is older than it looks) counts
    // the window at most, and an engine never reads past 100.
    let old = engine_read(&[engine(50, AMD, 0, "3D", 90 * SECOND)], 2 * SECOND);
    assert_eq!(busy(&then, &old).expect("a window").get(&AMD), Some(&100));
    let both = engine_read(
        &[
            engine(10, AMD, 0, "3D", 1000 + SECOND),
            engine(40, AMD, 0, "3D", SECOND),
        ],
        2 * SECOND,
    );
    assert_eq!(busy(&then, &both).expect("a window").get(&AMD), Some(&100));
}

/// NEGATIVE CONTROL: no window, no share: two reads at one instant, or a clock read backwards,
/// answer nothing rather than a division by zero or a full device.
#[test]
fn the_busy_share_needs_time_to_pass() {
    let read = |at| engine_read(&[engine(10, AMD, 0, "3D", at)], at);
    assert_eq!(busy(&read(5), &read(5)), None);
    assert_eq!(busy(&read(9), &read(5)), None);
    assert!(busy(&read(5), &read(6)).is_some());
    // No engine at all (no process holds a device): a window with nothing in it, not a failure.
    let empty = |at| engine_read(&[], at);
    assert_eq!(busy(&empty(0), &empty(SECOND)), Some(BTreeMap::new()));
    // Rounding is to the nearest percent: 0.4 % is 0, 0.5 % is 1.
    let ran = |running| {
        let then = engine_read(&[engine(10, AMD, 0, "3D", 0)], 0);
        let now = engine_read(&[engine(10, AMD, 0, "3D", running)], SECOND);
        busy(&then, &now).and_then(|b| b.get(&AMD).copied())
    };
    assert_eq!(ran(SECOND * 4 / 1000), Some(0));
    assert_eq!(ran(SECOND * 5 / 1000), Some(1));
    assert_eq!(ran(u64::MAX), Some(100));
}

#[test]
fn adapter_memory_sums_an_adapters_physical_adapters() {
    let memory = adapter_memory(
        &[
            item("luid_0x00000000_0x0001137F_phys_0", 395_010_048),
            item("luid_0x00000000_0x0001137F_phys_1", 1000),
            item("luid_0x00000000_0x00012F10_phys_0", 0),
            item("_Total", 1 << 40),
        ],
        &[
            item("luid_0x00000000_0x0001137F_phys_0", 371_933_184),
            item("luid_0x00000000_0x00012F10_phys_0", 8192),
        ],
        &[item("luid_0x00000000_0x0001137F_phys_0", 1_004_236_800)],
    );
    assert_eq!(
        memory.get(&AMD),
        Some(&AdapterMemory {
            dedicated: 395_011_048,
            shared: 371_933_184,
            committed: 1_004_236_800
        })
    );
    assert_eq!(
        memory.get(&WARP),
        Some(&AdapterMemory {
            dedicated: 0,
            shared: 8192,
            committed: 0
        })
    );
    assert_eq!(memory.len(), 2, "the unparsed name names no adapter");
    assert!(adapter_memory(&[], &[], &[]).is_empty());
}

#[test]
fn a_driver_version_prints_as_its_four_parts() {
    assert_eq!(driver_version(9_007_200_698_448_539), "32.0.22029.15003");
    assert_eq!(driver_version(2_814_751_477_605_871), "10.0.26100.9711");
    assert_eq!(driver_version(0), "0.0.0.0");
    assert_eq!(driver_version(u64::MAX), "65535.65535.65535.65535");
}

fn live(ids: &[u64]) -> BTreeSet<u64> {
    ids.iter().copied().collect()
}

#[test]
fn the_hardware_adapter_is_chosen_over_the_software_rasteriser() {
    let adapters = recorded_adapters();
    // The recorded machine: three live ids, one with no registry row at all.
    let ids = live(&[AMD, WARP, 0x0001_2F42]);
    let chosen = choose(&adapters, &ids, None).expect("the AMD adapter");
    assert_eq!(chosen.luid, AMD);
    assert_eq!(
        chosen.adapter.map(|a| a.name.as_str()),
        Some("AMD Radeon 780M Graphics")
    );
    // The rasteriser working flat out is still not the machine's GPU.
    let busy: BTreeMap<u64, u8> = [(WARP, 100), (AMD, 0)].into();
    assert_eq!(
        choose(&adapters, &ids, Some(&busy)).map(|c| c.luid),
        Some(AMD)
    );
    // NEGATIVE CONTROL: a machine whose only live listed adapter is the rasteriser (a VM with
    // no GPU) has no GPU, and a row whose id is not live (an earlier boot's) is not chosen.
    assert_eq!(choose(&adapters, &live(&[WARP]), Some(&busy)), None);
    assert_eq!(choose(&adapters, &live(&[0x0001_2F42]), None), None);
    assert_eq!(choose(&adapters, &live(&[]), None), None);
}

#[test]
fn of_two_hardware_adapters_the_busier_is_chosen_then_the_one_with_more_memory() {
    let discrete = Adapter {
        luid: 0x2_0000,
        name: "NVIDIA GeForce RTX 4070 Laptop GPU".to_owned(),
        vendor: 0x10DE,
        dedicated: 8 << 30,
        driver: None,
        seen: 5,
    };
    let mut adapters = recorded_adapters();
    adapters.push(discrete.clone());
    let ids = live(&[AMD, WARP, discrete.luid]);
    // Both idle (or not yet rated): the one with more memory of its own.
    assert_eq!(
        choose(&adapters, &ids, None).map(|c| c.luid),
        Some(discrete.luid)
    );
    let idle: BTreeMap<u64, u8> = [(AMD, 0), (discrete.luid, 0)].into();
    assert_eq!(
        choose(&adapters, &ids, Some(&idle)).map(|c| c.luid),
        Some(discrete.luid)
    );
    // The integrated one is drawing the desktop while the discrete one sleeps: it is named.
    let working: BTreeMap<u64, u8> = [(AMD, 12), (discrete.luid, 0)].into();
    assert_eq!(
        choose(&adapters, &ids, Some(&working)).map(|c| c.luid),
        Some(AMD)
    );
    // Equal memory and equal share: the lower id, whatever the rows' order.
    let twin = Adapter {
        luid: 0x1_0000,
        ..discrete.clone()
    };
    let twins = [discrete.clone(), twin.clone()];
    let swapped = [twin, discrete];
    let ids = live(&[0x1_0000, 0x2_0000]);
    assert_eq!(choose(&twins, &ids, None).map(|c| c.luid), Some(0x1_0000));
    assert_eq!(choose(&swapped, &ids, None).map(|c| c.luid), Some(0x1_0000));
}

#[test]
fn a_reused_id_reads_the_row_seen_last_and_an_unlisted_machine_keeps_its_ids() {
    // Two rows with one id: an earlier boot's and this one's.
    let stale = Adapter {
        name: "An adapter removed since".to_owned(),
        seen: 1,
        ..recorded_adapters().remove(1)
    };
    let mut adapters = vec![stale];
    adapters.extend(recorded_adapters());
    let chosen = choose(&adapters, &live(&[AMD]), None).expect("the id is live");
    assert_eq!(
        chosen.adapter.map(|a| a.name.as_str()),
        Some("AMD Radeon 780M Graphics")
    );
    // No registry rows at all: the live ids stand unnamed, the busiest first, then the lowest.
    let ids = live(&[AMD, WARP]);
    let unnamed = choose(&[], &ids, None).expect("a live id");
    assert_eq!((unnamed.luid, unnamed.adapter), (AMD, None));
    let busy: BTreeMap<u64, u8> = [(WARP, 3)].into();
    assert_eq!(choose(&[], &ids, Some(&busy)).map(|c| c.luid), Some(WARP));
    assert_eq!(choose(&[], &live(&[]), None), None);
}

#[test]
fn the_reading_carries_the_share_the_memory_and_the_name() {
    let adapters = recorded_adapters();
    let chosen = Chosen {
        luid: AMD,
        adapter: Some(&adapters[1]),
    };
    let memory: BTreeMap<u64, AdapterMemory> = [(
        AMD,
        AdapterMemory {
            dedicated: 395_010_048,
            shared: 371_933_184,
            committed: 1_004_236_800,
        },
    )]
    .into();
    let busy: BTreeMap<u64, u8> = [(AMD, 35)].into();
    assert_eq!(
        reading(chosen, Some(&busy), &memory),
        GpuReading {
            device_pct: Some(35),
            in_use_bytes: Some(766_943_232),
            alloc_bytes: Some(1_004_236_800),
            name: Some("AMD Radeon 780M Graphics".to_owned()),
            ..GpuReading::default()
        }
    );
    // One collection only: no share yet, and the reading says why, never 0.
    let first = reading(chosen, None, &memory);
    assert_eq!((first.device_pct, first.unread), (None, Some(Why::NotRead)));
    assert_eq!(first.in_use_bytes, Some(766_943_232));
    // An adapter with no engine instance in a rated window ran nothing; one with no memory
    // instance has no memory figure (never 0); one unlisted has no name and is not called
    // unified; one listed with no memory of its own is.
    let bare = reading(
        Chosen {
            luid: 9,
            adapter: None,
        },
        Some(&busy),
        &memory,
    );
    assert_eq!(
        bare,
        GpuReading {
            device_pct: Some(0),
            ..GpuReading::default()
        }
    );
    let integrated = Adapter {
        dedicated: 0,
        name: "  ".to_owned(),
        ..adapters[1].clone()
    };
    let read = reading(
        Chosen {
            luid: AMD,
            adapter: Some(&integrated),
        },
        Some(&busy),
        &memory,
    );
    assert!(read.unified);
    assert_eq!(read.name, None, "a blank description is no name");
}

/// A read inside [`MIN_WINDOW`] of the last collection is answered by it; one at the window's end
/// or later collects. Instants are built, never waited for.
#[test]
fn a_read_inside_the_window_is_held_and_one_past_it_collects() {
    let at = Instant::now();
    let tick = Duration::from_millis(1);
    assert!(!held(None, at), "no collection yet");
    assert!(held(Some(at), at));
    assert!(held(Some(at), at + MIN_WINDOW - tick));
    // NEGATIVE CONTROL: the window's end and past it are not held.
    assert!(!held(Some(at), at + MIN_WINDOW));
    assert!(!held(Some(at), at + MIN_WINDOW * 3));
    // A clock read before the collection (two threads' instants) is inside it, never a panic.
    assert!(held(Some(at + tick), at));
}

/// The recorded compositor (pid 1160) and another long-lived process on the 3D engine, read at
/// `at`, the compositor `ran` past its recorded running time (both hold far more than any window).
fn desktop(at: u64, ran: u64) -> EngineRead {
    engine_read(
        &[
            engine(1160, AMD, 0, "3D", 16_459_064_033 + ran),
            engine(14576, AMD, 0, "3D", 903_341_417),
        ],
        at,
    )
}

/// The hazard [`step`] guards: rated against a baseline with no engine (a failed collection's),
/// every long-lived instance counts as new inside the window and the idle device reads 100.
#[test]
fn an_empty_baseline_would_read_an_idle_device_full() {
    let empty = EngineRead {
        at: 0,
        rows: Vec::new(),
    };
    let idle = desktop(SECOND, 0);
    assert_eq!(busy(&empty, &idle).expect("a window").get(&AMD), Some(&100));
    // So step never rates a collection with engines against an empty baseline.
    assert_eq!(
        step(Some(empty), true, idle.clone()),
        Step::Read {
            shares: None,
            next: idle
        }
    );
}

#[test]
fn a_failed_or_empty_collection_keeps_the_baseline_and_reads_nothing() {
    let then = desktop(0, 0);
    let failed = engine_read(&[], SECOND);
    assert_eq!(
        step(Some(then.clone()), false, failed.clone()),
        Step::Missed(Some(then.clone())),
        "a failed collection"
    );
    assert_eq!(
        step(Some(then.clone()), true, failed),
        Step::Missed(Some(then.clone())),
        "a collection that read no engine where the baseline read some"
    );
    // The next good collection is rated against the kept baseline, over the whole window since
    // it: 0.2 s of work over two seconds is 10 %, never the 100 an emptied baseline gives.
    let Step::Read { shares, next } = step(Some(then), true, desktop(2 * SECOND, SECOND / 5))
    else {
        panic!("a good collection reads");
    };
    assert_eq!(shares.and_then(|s| s.get(&AMD).copied()), Some(10));
    assert_eq!(next, desktop(2 * SECOND, SECOND / 5), "the next baseline");
    // An ordinary collection after an ordinary baseline rates over its own window.
    let Step::Read { shares, .. } = step(Some(desktop(0, 0)), true, desktop(SECOND, SECOND / 10))
    else {
        panic!("a good collection reads");
    };
    assert_eq!(shares.and_then(|s| s.get(&AMD).copied()), Some(10));
}

#[test]
fn with_no_baseline_a_collection_reads_unrated_and_becomes_one() {
    // The process's first collection failed: the second is the baseline, its share unread.
    let first = desktop(SECOND, 0);
    assert_eq!(
        step(None, true, first.clone()),
        Step::Read {
            shares: None,
            next: first
        }
    );
    // Both failed: nothing read, nothing kept.
    assert_eq!(
        step(None, false, engine_read(&[], SECOND)),
        Step::Missed(None)
    );
    // A device no process holds, read twice: an idle device, 0 % on every adapter it lists
    // (none here), never unread.
    let none = |at| engine_read(&[], at);
    assert_eq!(
        step(Some(none(0)), true, none(SECOND)),
        Step::Read {
            shares: Some(BTreeMap::new()),
            next: none(SECOND)
        }
    );
}

/// The ids the recorded machine's counters carry on a later boot: every adapter started again
/// under a new id, the compute-only one with no registry row among them.
const AMD_NOW: u64 = 0x0002_0A11;
const WARP_NOW: u64 = 0x0002_0B22;
const COMPUTE_NOW: u64 = 0x0002_0C33;

/// The recorded rows rewritten for the later boot, as `DXGIAdapterCache` writes them once it runs.
fn rewritten_adapters() -> Vec<Adapter> {
    let mut rows = recorded_adapters();
    rows[0].luid = WARP_NOW;
    rows[1].luid = AMD_NOW;
    rows
}

/// THE STALE-CACHE CASE: read before DirectX's cache task runs after a boot or a driver restart,
/// the rows carry the adapters' previous ids, so no live hardware adapter is named; the rows are
/// read again a second later, and once rewritten the GPU is named, after which they are not read
/// again while the live ids hold. Instants are built, and the registry is a counting stand-in.
#[test]
fn stale_adapter_rows_are_read_again_until_they_name_a_live_adapter() {
    let ids = live(&[AMD_NOW, WARP_NOW, COMPUTE_NOW]);
    let reads = std::cell::Cell::new(0u32);
    let cache = std::cell::RefCell::new(recorded_adapters());
    let registry = || {
        reads.set(reads.get() + 1);
        cache.borrow().clone()
    };
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut listing = Listing::default();
    assert_eq!(
        choose(listing.rows(&ids, at(0), registry), &ids, None),
        None
    );
    assert_eq!(reads.get(), 1);
    let _ = listing.rows(&ids, at(999), registry);
    assert_eq!(reads.get(), 1, "inside the wait, the held rows");
    let _ = listing.rows(&ids, at(1000), registry);
    assert_eq!(
        reads.get(),
        2,
        "a second on, read again: still the cache's old rows"
    );
    // The task rewrote the rows.
    *cache.borrow_mut() = rewritten_adapters();
    let _ = listing.rows(&ids, at(2999), registry);
    assert_eq!(reads.get(), 2, "the wait doubled after a second miss");
    let chosen = choose(listing.rows(&ids, at(3000), registry), &ids, None);
    assert_eq!(chosen.map(|c| c.luid), Some(AMD_NOW), "the GPU is named");
    assert_eq!(reads.get(), 3);
    // NEGATIVE CONTROL: rows that name the GPU are not read again while the live ids hold, the
    // compute-only adapter with no row of its own notwithstanding.
    for later in [3001, 10_000, 3_600_000] {
        let _ = listing.rows(&ids, at(later), registry);
    }
    assert_eq!(reads.get(), 3);
    // A driver restart (new ids) reads them at once, whatever the wait.
    let restarted = live(&[0x0003_0001, WARP_NOW]);
    let _ = listing.rows(&restarted, at(3_600_001), registry);
    assert_eq!(reads.get(), 4);
    assert_eq!(
        listing.misses, 0,
        "a first read for new ids starts the wait again"
    );
}

/// A machine whose only adapter is the software rasteriser never matches: its rows are read again
/// at a wait that doubles to [`RELIST_MOST`], never more often. A machine whose registry lists no
/// adapter at all, and one with no live id, have nothing a second read could name.
#[test]
fn rows_that_never_name_a_gpu_are_read_at_a_backed_off_pace() {
    let reads = std::cell::Cell::new(0u32);
    let rasteriser = || {
        reads.set(reads.get() + 1);
        vec![recorded_adapters().remove(0)]
    };
    let warp = live(&[WARP]);
    let t0 = Instant::now();
    let mut listing = Listing::default();
    let _ = listing.rows(&warp, t0, rasteriser);
    let mut now = t0;
    let mut waits = Vec::new();
    for _ in 0..10 {
        let wait = listing.wait();
        assert!(!listing.due(&warp, now + wait - Duration::from_millis(1)));
        now += wait;
        let _ = listing.rows(&warp, now, rasteriser);
        waits.push(wait.as_secs());
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);
    assert_eq!(reads.get(), 11);
    assert_eq!(listing.wait(), RELIST_MOST);

    for (rows, ids) in [(Vec::new(), warp), (recorded_adapters(), BTreeSet::new())] {
        let mut listing = Listing::default();
        let _ = listing.rows(&ids, t0, || rows.clone());
        assert!(
            !listing.due(&ids, t0 + RELIST_MOST * 10),
            "{rows:?} for {ids:?}"
        );
    }
}

#[test]
fn every_engine_instance_is_a_client_with_its_process_name_and_time_in_ns() {
    let read = engine_read(
        &[
            engine(1160, AMD, 0, "3D", 16_459_064_033),
            engine(1160, AMD, 1, "Copy", 7),
            engine(0, AMD, 0, "3D", 99),
            engine(555, AMD, 0, "3D", 0),
        ],
        1,
    );
    let asked = std::cell::RefCell::new(Vec::new());
    let got = clients(&read, |pid| {
        asked.borrow_mut().push(pid);
        (pid == 1160).then(|| "dwm.exe".to_owned())
    });
    assert_eq!(
        got.rows,
        vec![
            GpuClient {
                entry: entry_of(1160, AMD, 0, 0),
                pid: 1160,
                name: "dwm.exe".to_owned(),
                gpu_ns: 1_645_906_403_300
            },
            GpuClient {
                entry: entry_of(1160, AMD, 0, 1),
                pid: 1160,
                name: "dwm.exe".to_owned(),
                gpu_ns: 700
            },
            // A process the sweep does not list (gone since) keeps its time, unnamed.
            GpuClient {
                entry: entry_of(555, AMD, 0, 0),
                pid: 555,
                name: String::new(),
                gpu_ns: 0
            },
        ]
    );
    // Every instance's id is listed, pid 0's too, so a later read knows none of them as new.
    assert_eq!(got.entries.len(), 4);
    assert!(got.entries.contains(&entry_of(0, AMD, 0, 0)));
    assert_eq!(
        (got.children, got.truncated, got.complete),
        (4, false, true)
    );
    assert_eq!(*asked.borrow(), vec![1160, 555], "each process asked once");
    // No instance: an empty, complete read.
    let none = clients(&engine_read(&[], 2), |_| None);
    assert_eq!(
        (none.rows.len(), none.entries.len(), none.complete),
        (0, 0, true)
    );
}

/// NEGATIVE CONTROL: a read past the cap says so and is not complete, so the next read counts
/// no unseen instance as born inside its window.
#[test]
fn a_client_read_past_the_cap_is_truncated() {
    let items: Vec<RawItem> = (0..=MAX_GPU_CLIENTS)
        .map(|i| engine(u32::try_from(i).expect("a small pid") + 1, AMD, 0, "3D", 1))
        .collect();
    let got = clients(&engine_read(&items, 1), |_| None);
    assert_eq!(got.rows.len(), MAX_GPU_CLIENTS);
    assert_eq!(got.entries.len(), MAX_GPU_CLIENTS);
    assert_eq!(
        (got.children as usize, got.truncated, got.complete),
        (MAX_GPU_CLIENTS, true, false)
    );
    // A time past what ns hold saturates.
    let huge = clients(
        &engine_read(&[engine(7, AMD, 0, "3D", u64::MAX)], 1),
        |_| None,
    );
    assert_eq!(huge.rows[0].gpu_ns, u64::MAX);
}

#[test]
fn the_throttle_level_is_the_most_constrained_zones_band() {
    let level = |limits: &[u64]| throttle_of(limits).map(|t| t.level);
    // The recorded machine: two zones, both unconstrained.
    assert_eq!(level(&[100, 100]), Ok(ThrottleLevel::Nominal));
    assert_eq!(level(&[100, 99]), Ok(ThrottleLevel::Light));
    assert_eq!(level(&[80]), Ok(ThrottleLevel::Light));
    assert_eq!(level(&[79, 100]), Ok(ThrottleLevel::Heavy));
    assert_eq!(level(&[50]), Ok(ThrottleLevel::Heavy));
    assert_eq!(level(&[100, 49, 90]), Ok(ThrottleLevel::Severe));
    assert_eq!(level(&[0]), Ok(ThrottleLevel::Severe));
    // A value past 100 is no constraint.
    assert_eq!(level(&[250]), Ok(ThrottleLevel::Nominal));
    // The banding is this reader's, so the level is never called exact.
    assert_eq!(throttle_of(&[100]).map(|t| t.exact), Ok(false));
    // NEGATIVE CONTROL: no zone is no reading, never "none".
    assert_eq!(throttle_of(&[]), Err(Why::Absent));
}

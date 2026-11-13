// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GIB: u64 = 1 << 30;
/// The Folded cell, pt.
const CELL: f64 = 8.5;

/// This Mac (A1 §1.3, A4 §4): a 10-core GPU with a Neural Engine, 32 GiB, a disk reader, the
/// whole heat chain with its Super and Efficiency clocks, and the network and processes rows.
fn this_mac() -> UnitFacts {
    UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: true,
        }),
        memory: Some(32 * GIB),
        disk: true,
        heat: Some(Heat {
            ladder: true,
            clocks: 2,
            watts: 3,
            work: true,
        }),
        network: true,
        processes: true,
    }
}

fn at(facts: &UnitFacts, row: UnitRow, form: Form) -> [Option<f64>; 2] {
    [1.0, 2.0].map(|s| facts.width(row, form, s, CELL))
}

/// The forms measure what A1 §2.4, §4.2–§4.4, A2 §3.1 and A4 §4 and §5 say: this Mac at 1×
/// (GPU 31 and 36 with the Neural Engine, Memory 46, Disk 19 for two devices' place, Heat 30,
/// Network 15 for four links' place, Processes 15) and at 2× (a hairline is half a point
/// there, and a half-point tile is 5 px, not 3), the tank's rows at 16 and 40 cores, the
/// undivided tank over 40 or with no count, and the heat chain without each reader.
#[test]
fn the_forms_measure_what_the_design_says() {
    use Form::{Column, Half, Undivided};
    use UnitRow::{Disk, Heat as HeatRow, Memory, Network, Processes};
    let mac = this_mac();
    assert_eq!(at(&mac, UnitRow::Gpu, Column), [Some(36.0), Some(30.5)]);
    assert_eq!(at(&mac, UnitRow::Gpu, Undivided), [Some(13.0), Some(13.0)]);
    assert_eq!(at(&mac, Memory, Column), [Some(46.0), Some(45.5)]);
    assert_eq!(at(&mac, Memory, Half), [Some(26.0), Some(25.5)]);
    // Two square tiles a point apart: 9 + 1 + 9 px at 1×, 17 + 2 + 17 at 2× (A1 §4.4).
    assert_eq!(at(&mac, Disk, Column), [Some(19.0), Some(18.0)]);
    // The ladder 3, 2, the Super clock 3, 1, the Efficiency clock 2, the wall, three watt
    // tiles of 3 a point apart, 2, the work per joule 3 (A4 §4).
    assert_eq!(at(&mac, HeatRow, Column), [Some(30.0), Some(29.5)]);
    // Four 3 pt tiles a point apart, whichever links are up (A4 §5.1).
    assert_eq!(at(&mac, Network, Column), [Some(15.0), Some(15.0)]);
    // The flow tile 5, the wall, the pile and the stuck column a point apart (A4 §6.3).
    assert_eq!(at(&mac, Processes, Column), [Some(15.0), Some(13.5)]);

    let tank = |cores: Option<u32>| UnitFacts {
        gpu: Some(Gpu {
            cores,
            neural_engine: false,
        }),
        ..mac
    };
    // 10 cores: 10 × 4 px + 9 seams + the rim = 51 px at 2×; 31 px at 1×.
    assert_eq!(
        at(&tank(Some(10)), UnitRow::Gpu, Column),
        [Some(31.0), Some(25.5)]
    );
    assert_eq!(
        at(&tank(Some(16)), UnitRow::Gpu, Column),
        [Some(25.0), Some(20.5)]
    );
    // 40 cores: 4 × 10 at 2×, 2 × 20 of a point at 1×.
    assert_eq!(
        at(&tank(Some(40)), UnitRow::Gpu, Column),
        [Some(41.0), Some(25.5)]
    );
    for cores in [Some(41), Some(80), Some(0), None] {
        let gpu = tank(cores);
        assert_eq!(at(&gpu, UnitRow::Gpu, Column), [None; 2], "{cores:?}");
        assert_eq!(
            at(&gpu, UnitRow::Gpu, Undivided),
            [Some(8.0); 2],
            "{cores:?}"
        );
    }

    // The heat chain keeps no width for a unit with no reader: an Intel Mac's one clock, a
    // Mac with no Neural Engine, Linux's ladder alone, the watts alone (no wall), and none.
    let heat = |ladder, clocks, watts, work| UnitFacts {
        heat: Some(Heat {
            ladder,
            clocks,
            watts,
            work,
        }),
        ..mac
    };
    for (facts, want) in [
        (heat(true, 1, 2, true), [Some(23.0), Some(22.5)]),
        (heat(true, 2, 2, true), [Some(26.0), Some(25.5)]),
        (heat(true, 0, 0, false), [Some(3.0), Some(3.0)]),
        (heat(false, 0, 3, false), [Some(11.0), Some(11.0)]),
        (heat(false, 2, 0, false), [Some(6.0), Some(6.0)]),
        (heat(true, 7, 9, true), [Some(30.0), Some(29.5)]),
        (heat(false, 0, 0, false), [None; 2]),
    ] {
        assert_eq!(at(&facts, HeatRow, Column), want, "{:?}", facts.heat);
    }

    // Where no reader runs, a row draws no units.
    let diskless = UnitFacts { disk: false, ..mac };
    assert_eq!(at(&diskless, Disk, Column), [None; 2]);
    let bare = UnitFacts {
        heat: None,
        network: false,
        processes: false,
        ..mac
    };
    for row in [HeatRow, Network, Processes] {
        assert_eq!(at(&bare, row, Column), [None; 2], "{row:?}");
    }

    // Unread, a row draws nothing, and no form off its ladder draws.
    let none = UnitFacts::NONE;
    for row in UnitRow::ALL {
        for form in [Column, Undivided, Half] {
            assert_eq!(none.width(row, form, 1.0, CELL), None, "{row:?} {form:?}");
            if !row.ladder().contains(&form) {
                assert_eq!(mac.width(row, form, 1.0, CELL), None, "{row:?} {form:?}");
            }
        }
    }
    for rows in [&UnitRow::BLOCK_A[..], &UnitRow::BLOCK_B, &UnitRow::ALL] {
        assert!(!none.draw_any(CELL, rows) && mac.draw_any(CELL, rows));
    }
    let gpuless = UnitFacts { gpu: None, ..mac };
    assert_eq!(at(&gpuless, UnitRow::Gpu, Undivided), [None; 2]);
    // A table's forms name its own rows alone.
    let a = mac.forms_within(f64::INFINITY, CELL, &UnitRow::BLOCK_A);
    let b = mac.forms_within(f64::INFINITY, CELL, &UnitRow::BLOCK_B);
    for row in UnitRow::ALL {
        let ours = UnitRow::BLOCK_A.contains(&row);
        assert_eq!(a[row.index()].is_some(), ours, "{row:?}");
        assert_eq!(b[row.index()].is_some(), !ours, "{row:?}");
    }
    assert_eq!((mac.widest(&a, CELL), mac.widest(&b, CELL)), (46.0, 15.0));
}

/// A1 §2.4's choice, before the planner: for every machine shape (GPUs of no count and 0 to
/// 80 cores with and without the Neural Engine, 1 to 512 GiB, a disk reader or none, every
/// heat chain, and the network and processes rows read or not), as the cap grows no row's
/// form steps down its ladder, every form taken is within the cap at 1× and no wider at 2×,
/// and a row takes nothing only where no form of its ladder fits. CONTROL: each form of each
/// ladder is taken somewhere in the sweep.
#[test]
fn a_wider_cap_never_takes_a_smaller_form() {
    let rank = |row: UnitRow, form: Option<Form>| {
        form.and_then(|f| row.ladder().iter().position(|&g| g == f))
            .unwrap_or(row.ladder().len())
    };
    let gpus = (0..=80)
        .map(Some)
        .chain([None])
        .flat_map(|cores| {
            [false, true].map(|neural_engine| Gpu {
                cores,
                neural_engine,
            })
        })
        .map(Some)
        .chain([None]);
    let mut taken = std::collections::HashSet::new();
    let mut check = |facts: UnitFacts| {
        let mut last = [usize::MAX; UnitRow::ALL.len()];
        for cap in 0..=140 {
            let cap = f64::from(cap);
            let forms = facts.forms_within(cap, CELL, &UnitRow::ALL);
            for (i, (row, form)) in UnitRow::ALL.into_iter().zip(forms).enumerate() {
                let r = rank(row, form);
                assert!(
                    r <= last[i],
                    "{facts:?}: {row:?} steps down to {form:?} at {cap}"
                );
                last[i] = r;
                let Some(form) = form else {
                    assert!(
                        row.ladder().iter().all(|&f| facts
                            .width(row, f, PLAN_SCALE, CELL)
                            .is_none_or(|w| w > cap)),
                        "{facts:?}: {row:?} takes nothing in {cap}"
                    );
                    continue;
                };
                taken.insert((row, form));
                let [w1, w2] = at(&facts, row, form).map(|w| w.expect("taken, so drawn"));
                assert!(
                    w1 <= cap && w2 <= w1,
                    "{facts:?}: {row:?} {form:?} in {cap}"
                );
            }
        }
    };
    let mac = this_mac();
    for gpu in gpus {
        check(UnitFacts {
            gpu,
            ..UnitFacts::NONE
        });
    }
    for gib in 1..=512 {
        check(UnitFacts {
            memory: Some(gib * GIB),
            ..UnitFacts::NONE
        });
    }
    for ladder in [false, true] {
        for clocks in 0..=3 {
            for watts in 0..=4 {
                for work in [false, true] {
                    check(UnitFacts {
                        heat: Some(Heat {
                            ladder,
                            clocks,
                            watts,
                            work,
                        }),
                        ..UnitFacts::NONE
                    });
                }
            }
        }
    }
    for disk in [false, true] {
        for network in [false, true] {
            for processes in [false, true] {
                check(UnitFacts {
                    disk,
                    network,
                    processes,
                    ..mac
                });
            }
        }
    }
    for row in UnitRow::ALL {
        for &form in row.ladder() {
            assert!(
                taken.contains(&(row, form)),
                "CONTROL: {row:?} {form:?} never taken"
            );
        }
    }
}

/// A1 §4.3's table: the tile is a power of two GiB, the least whose 16 tiles hold the total,
/// so there are never more than 16 and the tile is never needlessly large; Half doubles it.
#[test]
fn the_memory_unit_is_a_power_of_two_gib_and_tiles_never_exceed_sixteen() {
    let table = [
        (8, (GIB / 2, 16)),
        (16, (GIB, 16)),
        (18, (2 * GIB, 9)),
        (24, (2 * GIB, 12)),
        (32, (2 * GIB, 16)),
        (64, (4 * GIB, 16)),
        (128, (8 * GIB, 16)),
    ];
    for (gib, want) in table {
        assert_eq!(memory_tiles(gib * GIB), Some(want), "{gib} GiB");
    }
    assert_eq!(memory_tiles(0), None);
    let totals = (1..=4096_u64)
        .map(|mib| mib * (1 << 20) * 37)
        .chain([1, 15, 16, 17, u64::MAX]);
    for total in totals {
        let (unit, tiles) = memory_tiles(total).expect("some memory");
        assert!(unit.is_power_of_two(), "{total}: {unit}");
        assert!((1..=16).contains(&tiles), "{total}: {tiles} tiles");
        assert_eq!(tiles, total.div_ceil(unit), "{total}");
        if unit > 1 {
            assert!(
                total.div_ceil(unit / 2) > 16,
                "{total}: {unit} is not the least"
            );
        }
    }
}

/// The host's reading supplies every fact the units plan reads (A1 step R5, A4 step N4): the
/// deck's three-reading snapshot of this Mac reads its 10-core GPU and 32 GiB, the Neural
/// Engine tile only once its reader reads, the disk, network and processes rows on macOS
/// and Linux, and on macOS the heat chain with a clock for
/// each of its two CPU kinds and a watt tile for the CPU, the GPU and, once it reads, the
/// Neural Engine; planned from them, both deck forms lay the units columns the laws plan for
/// this Mac at 1039 pt. A CPU of one kind derives one clock, and an empty snapshot gives the
/// planner only the platform's readers.
#[test]
fn the_hosts_reading_supplies_the_facts_the_units_plan_reads() {
    use crate::stats_plan::{plan_folded_with, plan_full_with};
    let mut snap = crate::stats_deck::tests::snapshot();
    let disk = cfg!(any(target_os = "macos", target_os = "linux"));
    let heat = |clocks, watts| {
        if cfg!(target_os = "macos") {
            Some(Heat {
                ladder: true,
                clocks,
                watts,
                work: true,
            })
        } else if cfg!(target_os = "linux") {
            Some(Heat {
                ladder: true,
                clocks: 0,
                watts: 0,
                work: false,
            })
        } else {
            None
        }
    };
    let want = UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: false,
        }),
        disk,
        heat: heat(2, 2),
        network: disk,
        processes: disk,
        ..this_mac()
    };
    assert_eq!(UnitFacts::of(&snap), want);
    snap.ane = Some(Ok(aterm_vitals::AneSample {
        cores: 16,
        powered: false,
    }));
    let read = UnitFacts::of(&snap);
    assert_eq!(
        read,
        UnitFacts {
            disk,
            heat: heat(2, 3),
            network: disk,
            processes: disk,
            ..this_mac()
        }
    );

    let m = crate::deck_face::FaceMetrics::helvetica_neue();
    let folded = plan_folded_with(1039.0, &m, &read).blocks;
    let full = plan_full_with(1039.0, &m, &read).units;
    assert_eq!(
        folded.map(|block| block.units.map(|u| u.at.w)),
        [Some(46.0), disk.then_some(15.0)]
    );
    let column = Some(Form::Column);
    let mut forms = [column; UnitRow::ALL.len()];
    for row in [UnitRow::Disk, UnitRow::Network, UnitRow::Processes] {
        forms[row.index()] = disk.then_some(Form::Column);
    }
    if read.heat.is_none() {
        forms[UnitRow::Heat.index()] = None;
    }
    assert_eq!(full.map(|u| u.forms), Some(forms), "{full:?}");

    if let Some(cpu) = snap.cpu.as_mut() {
        cpu.clusters.truncate(1);
    }
    assert_eq!(UnitFacts::of(&snap).heat, heat(1, 3));
    let empty = UnitFacts::of(&Snapshot::default());
    assert_eq!(
        empty,
        UnitFacts {
            disk,
            heat: heat(0, 1),
            network: disk,
            processes: disk,
            ..UnitFacts::NONE
        }
    );
    let alone = empty.draw_any(CELL, &UnitRow::ALL);
    assert_eq!(alone, disk || empty.heat.is_some());
    assert!(!UnitFacts::NONE.draw_any(CELL, &UnitRow::ALL));
}

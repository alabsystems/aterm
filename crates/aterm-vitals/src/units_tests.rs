// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The units' laws (A1 §12, step R4): what the tiles draw against the rows they sit beside.

use super::*;
use crate::model::{ClusterSpec, MemCounters, MemSample, Resource, Sample, Ticks, Vitals};
use crate::peaks::DiskPeaks;
use crate::ring::BUCKET_S;

/// A wall clock on a ten-second bucket's edge.
const T0: i64 = 1_792_000_800_000;
const PAGE: u64 = 16_384;
const DEVICE: &str = "00000000000000a1";

/// Reading `t` (seconds), at the 1 s cadence.
fn reading(t: u64) -> Sample {
    Sample {
        at_ns: t * 1_000_000_000,
        mono_ms: t * 1000,
        at_unix_ms: T0 + i64::try_from(t).expect("small") * 1000,
        cadence_ms: 1000,
        ..Sample::default()
    }
}

/// Four cores, efficiency cores 0 and 1, performance cores 2 and 3, with cumulative busy and
/// total ticks.
fn with_cores(mut s: Sample, busy: &[u64; 4], total: &[u64; 4]) -> Sample {
    s.cores = Some(
        busy.iter()
            .zip(total)
            .map(|(&busy, &total)| Ticks { busy, total })
            .collect(),
    );
    s.clusters = vec![
        ClusterSpec {
            name: "Performance".into(),
            logical: vec![2, 3],
        },
        ClusterSpec {
            name: "Efficiency".into(),
            logical: vec![0, 1],
        },
    ];
    s
}

/// A busy fraction that swings from second to second and core to core, in hundredths.
fn swing(t: u64, core: u64) -> u64 {
    (t * 37 + core * 53) % 101
}

#[test]
fn the_mean_of_the_cpu_tiles_is_the_cpu_percent() {
    let mut v = Vitals::default();
    let (mut busy, mut total) = ([0u64; 4], [0u64; 4]);
    let (mut checked, mut trend_apart) = (0, false);
    for t in 1..=30u64 {
        for core in 0..4 {
            // At second 12 core 3's counters do not advance: no core reads, nor does the row.
            if t == 12 && core == 3 {
                continue;
            }
            busy[core] += swing(t, core as u64);
            total[core] += 100;
        }
        let snap = v.absorb(&with_cores(reading(t), &busy, &total));
        let cpu = &snap.resources[Resource::Cpu.index()];
        let (Some(now), Some(cores)) = (cpu.now, cpu.capacity) else {
            continue;
        };
        let pct = now / cores;
        let tiles: Vec<Level> = snap
            .units
            .cores
            .iter()
            .map(|l| l.expect("every core read in the window"))
            .collect();
        assert_eq!(tiles.len(), 4);
        let mean = tiles.iter().map(|l| l.mean).sum::<f64>() / 4.0;
        assert!(
            (mean - pct).abs() < 1e-12,
            "second {t}: tiles {mean} against the row's {pct}"
        );
        assert!(tiles.iter().all(|l| l.max >= l.mean && l.max <= 1.0));
        // Each cluster is its cores' mean, its tick its busiest core's max.
        let p = snap.units.clusters[0].expect("the performance cluster");
        assert!((p.mean - (tiles[2].mean + tiles[3].mean) / 2.0).abs() < 1e-12);
        assert_eq!(p.max, tiles[2].max.max(tiles[3].max));
        let e = snap.units.clusters[1].expect("the efficiency cluster");
        assert!((e.mean - (tiles[0].mean + tiles[1].mean) / 2.0).abs() < 1e-12);

        // Negative control: the per-core trend (τ 2 s) the deck's Cores row drew is not the
        // row's 10 s mean on a swinging load; the units do not take it.
        let cpu_view = snap.cpu.as_ref().expect("the cpu");
        let trend = cpu_view
            .cores
            .iter()
            .map(|c| f64::from(c.load.level))
            .sum::<f64>()
            / 4.0;
        trend_apart |= (trend - pct).abs() > 0.01;
        checked += 1;
    }
    assert!(checked >= 25, "{checked} readings checked");
    assert!(trend_apart, "the trend never left the 10 s mean");
}

#[test]
fn the_queue_tile_is_load_beyond_the_cores_and_turns_over_with_the_cpu() {
    assert_eq!(queue_level(6.0, 4), 0.5);
    assert_eq!(queue_level(12.0, 4), 1.0, "clamped at the cores again");
    assert!(cpu_over(Some(3.9), Some(4), Some(6.0)));
    assert!(
        !cpu_over(Some(3.7), Some(4), Some(6.0)),
        "a queue under 95 % busy is not over"
    );
    assert!(!cpu_over(Some(4.0), Some(4), Some(4.0)));
    assert!(!cpu_over(None, Some(4), Some(6.0)));

    let mut v = Vitals::default();
    let load = |mut s: Sample, load1: f64| {
        s.load1 = Some(load1);
        s
    };
    let full = |t: u64| [t * 100; 4];
    let _ = v.absorb(&load(with_cores(reading(1), &full(1), &full(1)), 6.0));
    let snap = v.absorb(&load(with_cores(reading(2), &full(2), &full(2)), 6.0));
    assert_eq!(
        snap.units.queue,
        Some(Queue {
            level: 0.5,
            over: true
        })
    );
    assert!(snap.resources[Resource::Cpu.index()].over, "the row agrees");
    // Half busy under the same queue: the tile stands, not over.
    let snap = v.absorb(&load(with_cores(reading(3), &[250; 4], &full(3)), 6.0));
    assert_eq!(
        snap.units.queue.map(|q| (q.level, q.over)),
        Some((0.5, false))
    );
}

#[test]
fn no_queue_tile_draws_at_or_under_capacity() {
    assert_eq!(queue_level(4.0, 4), 0.0);
    assert_eq!(queue_level(0.5, 4), 0.0);
    assert_eq!(queue_level(f64::NAN, 4), 0.0);
    assert_eq!(queue_level(9.0, 0), 0.0);
    let mut v = Vitals::default();
    let mut s = with_cores(reading(1), &[50; 4], &[100; 4]);
    s.load1 = Some(3.5);
    let snap = v.absorb(&s);
    assert_eq!(snap.units.queue.map(|q| q.level), Some(0.0));
    // Negative control: no load read, no queue tile at all.
    s.load1 = None;
    assert_eq!(v.absorb(&s).units.queue, None);
}

/// Memory with the VM's counters: `compress` and `decompress` pages a second since `t` 0.
fn with_mem(mut s: Sample, t: u64, compress: u64, decompress: u64) -> Sample {
    s.mem = Some(MemSample {
        total: 32 << 30,
        app: 10 << 30,
        wired: 3 << 30,
        compressed: Some(1 << 30),
        stored: Some(3 << 30),
        cached: 8 << 30,
        free: 9 << 30,
        swap_used: 1 << 30,
        swap_total: 2 << 30,
        counters: Some(MemCounters {
            compressions: compress * t,
            decompressions: decompress * t,
            page_bytes: PAGE,
            ..MemCounters::default()
        }),
        exact: true,
        ..MemSample::default()
    });
    s
}

#[test]
fn compressor_churn_is_its_pages_times_the_page_size_against_its_own_peak() {
    let churn = 150.0 * PAGE as f64;
    let mut v = Vitals::default();
    let mut last = None;
    for t in 1..=5 {
        last = Some(v.absorb(&with_mem(reading(t), t, 100, 50)).units);
    }
    let units = last.expect("readings");
    let rate = units.churn.expect("the VM reads its compressor");
    let level = rate.level.expect("a rate");
    assert!((level.mean - churn).abs() < 1e-6 && (level.max - churn).abs() < 1e-6);
    assert_eq!(rate.basis, CapBasis::Learning);
    assert_eq!(rate.share(), None, "hollow while the peak learns");

    // A peak learned all but one bucket: 30 s of churn close buckets, and it draws against it.
    let mut v = Vitals::default();
    let today = u32::try_from(T0.div_euclid(86_400_000)).expect("a day");
    v.restore_peaks(Peaks {
        mem_churn: DailyMax {
            days: vec![(today, 2.0 * churn)],
            read_s: IO_LEARN_S - BUCKET_S as u32,
        },
        ..Peaks::default()
    });
    let mut units = None;
    for t in 1..=41 {
        units = Some(v.absorb(&with_mem(reading(t), t, 100, 50)).units);
    }
    let rate = units.and_then(|u| u.churn).expect("churn");
    assert_eq!(rate.basis, CapBasis::Peak);
    assert_eq!(rate.peak, Some(2.0 * churn));
    let share = rate.share().expect("a share");
    assert!((share.mean - 0.5).abs() < 1e-9, "{share:?}");
    assert!(v.peaks().mem_churn.read_s >= IO_LEARN_S);

    // Negative control: memory read without the VM's counters (Linux) draws no churn column.
    let mut s = with_mem(reading(42), 42, 100, 50);
    if let Some(m) = s.mem.as_mut() {
        m.counters = None;
    }
    assert_eq!(v.absorb(&s).units.churn, None);
}

#[test]
fn swap_traffic_turns_over_at_the_memory_over_rate() {
    let at = |mean: f64| Swap {
        used: 1 << 30,
        traffic: Some(Level { mean, max: mean }),
    };
    assert!(at(SWAP_OVER_BPS).over());
    let half = at(SWAP_OVER_BPS / 2.0);
    assert_eq!(half.share().map(|s| s.mean), Some(0.5));
    assert!(!half.over());
    // Through the model: 1 MiB a second swapped (the sample's KiB counter).
    let mut v = Vitals::default();
    let mut units = None;
    for t in 1..=4u64 {
        let mut s = with_mem(reading(t), t, 0, 0);
        s.swapped_kib = Some(1024 * t);
        units = Some(v.absorb(&s).units);
    }
    let swap = units.and_then(|u| u.swap).expect("swap");
    assert_eq!(swap.used, 1 << 30);
    let traffic = swap.traffic.expect("a rate");
    assert!((traffic.mean - 1_048_576.0).abs() < 1e-6, "{traffic:?}");
    assert!(!swap.over());
}

fn block(write_bytes: u64, read_bytes: u64) -> BlockSample {
    BlockSample {
        id: DEVICE.into(),
        name: "APPLE SSD AP4096Z".into(),
        location: DiskLocation::Internal,
        write_bytes,
        read_bytes,
        ..BlockSample::default()
    }
}

#[test]
fn a_disk_half_is_hollow_while_its_peak_learns() {
    let mut v = Vitals::default();
    let today = u32::try_from(T0.div_euclid(86_400_000)).expect("a day");
    let mut units = None;
    for t in 1..=5u64 {
        let mut s = reading(t);
        s.block = vec![block(10_000_000 * t, 2_000_000 * t)];
        units = Some(v.absorb(&s).units);
    }
    let disk = units.expect("units").disks.remove(0);
    assert_eq!(disk.id, DEVICE);
    assert_eq!(disk.location, DiskLocation::Internal);
    assert_eq!(disk.write.level.map(|l| l.mean), Some(10_000_000.0));
    assert_eq!(disk.read.level.map(|l| l.mean), Some(2_000_000.0));
    assert_eq!(
        (disk.write.basis, disk.read.basis),
        (CapBasis::Learning, CapBasis::Learning)
    );
    assert_eq!((disk.write.share(), disk.read.share()), (None, None));

    // The write direction learned, the read one not: one half draws, the other stays hollow.
    v.restore_peaks(Peaks {
        disks: vec![DiskPeaks {
            id: DEVICE.into(),
            write: DailyMax {
                days: vec![(today, 20_000_000.0)],
                read_s: IO_LEARN_S,
            },
            read: DailyMax {
                days: vec![(today, 4_000_000.0)],
                read_s: IO_LEARN_S - 3_600,
            },
        }],
        ..Peaks::default()
    });
    let mut s = reading(6);
    s.block = vec![block(60_000_000, 12_000_000)];
    let disk = v.absorb(&s).units.disks.remove(0);
    assert_eq!(disk.write.basis, CapBasis::Peak);
    assert_eq!(disk.write.share().map(|l| l.mean), Some(0.5));
    assert_eq!(disk.read.basis, CapBasis::Learning);
    assert_eq!(disk.read.share(), None);
}

#[test]
fn each_disk_direction_is_drawn_against_its_own_peak() {
    let mut v = Vitals::default();
    let today = u32::try_from(T0.div_euclid(86_400_000)).expect("a day");
    let learned = |peak: f64| DailyMax {
        days: vec![(today, peak)],
        read_s: IO_LEARN_S,
    };
    v.restore_peaks(Peaks {
        disks: vec![DiskPeaks {
            id: DEVICE.into(),
            write: learned(40_000_000.0),
            read: learned(4_000_000.0),
        }],
        ..Peaks::default()
    });
    let mut units = None;
    for t in 1..=3u64 {
        let mut s = reading(t);
        s.block = vec![block(10_000_000 * t, 2_000_000 * t)];
        units = Some(v.absorb(&s).units);
    }
    let disk = units.expect("units").disks.remove(0);
    assert_eq!(disk.write.share().map(|l| l.mean), Some(0.25));
    // Negative control: the read rate against the write peak would draw a twentieth.
    assert_eq!(disk.read.share().map(|l| l.mean), Some(0.5));
    assert_ne!(
        disk.read.share(),
        disk.read.level.and_then(|l| l.over(40_000_000.0))
    );
}

#[test]
fn a_device_whose_counters_went_back_reads_no_rate_that_reading() {
    let mut rings = UnitRings::default();
    let mut peaks = Peaks::default();
    let mut at = |t: u64, w: u64| {
        let block = [block(w, 0)];
        let r = UnitReading {
            now: T0 + i64::try_from(t).expect("small") * 1000,
            at_ns: t * 1_000_000_000,
            block: &block,
            ..UnitReading::default()
        };
        let mut units = UnitRings::absorb(&mut rings, &r, &mut peaks);
        units.disks.remove(0).write.level
    };
    let level = |mean: f64, max: f64| Some(Level { mean, max });
    assert_eq!(at(1, 1_000), None, "one reading is no rate");
    assert_eq!(at(2, 3_000), level(2_000.0, 2_000.0));
    // The device came back with its counters from zero: that reading adds no rate, so the
    // window holds the last real one alone (a zero pushed for it would halve the mean), and the
    // next rate runs from the new count.
    assert_eq!(at(3, 500), level(2_000.0, 2_000.0));
    assert_eq!(at(4, 9_500), level(5_500.0, 9_000.0));
}

#[test]
fn no_unit_ring_keeps_a_six_hour_history() {
    // Twelve minutes of readings, past a ten-minute bucket's edge, through every unit.
    let mut rings = UnitRings::default();
    let mut peaks = Peaks::default();
    let cores = [0.25, 0.5, 0.75, 1.0];
    for t in 1..=720u64 {
        let block = [block(1_000 * t, 500 * t)];
        let r = UnitReading {
            now: T0 + i64::try_from(t).expect("small") * 1000,
            at_ns: t * 1_000_000_000,
            cores: Some(&cores),
            n_cores: Some(cores.len()),
            churn_bps: Some(1e6),
            churn_read: true,
            swap_bps: Some(1e5),
            swap_used: Some(1 << 30),
            block: &block,
            ..UnitReading::default()
        };
        let _ = rings.absorb(&r, &mut peaks);
    }
    let all: Vec<(&str, &BucketRing)> = rings
        .cores
        .iter()
        .map(|r| ("core", r))
        .chain([("churn", &rings.churn), ("swap", &rings.swap)])
        .chain(
            rings
                .disks
                .iter()
                .flat_map(|d| [("disk write", &d.write), ("disk read", &d.read)]),
        )
        .collect();
    assert_eq!(all.len(), 4 + 2 + 2);
    for (name, ring) in all {
        assert!(
            ring.long_cells().is_empty() && ring.long_k().is_none(),
            "a {name} ring keeps ten-minute buckets"
        );
        assert!(
            ring.now_mean(T0 + 720_000, NOW_WINDOW_MS).is_some(),
            "{name}"
        );
    }

    // Negative control: a row's ring pushed the same readings keeps them.
    let mut row = BucketRing::with_capacity(UNIT_RING_BUCKETS);
    for t in 1..=720i64 {
        let _ = row.push(
            crate::ring::Taken::at(T0 + t * 1000, 1_000),
            Some(1.0),
            false,
        );
    }
    assert_eq!(row.long_cells().len(), 2, "two ten-minute buckets");
}

#[test]
fn the_units_keep_two_devices_and_peaks_four_the_least_recently_read_dropped() {
    let id = |n: u8| format!("{n:016x}");
    let mut rings = UnitRings::default();
    let mut peaks = Peaks::default();
    for n in 1..=5u8 {
        let block = [
            BlockSample {
                id: id(n),
                ..BlockSample::default()
            },
            BlockSample {
                id: id(n),
                ..BlockSample::default()
            },
            BlockSample {
                id: "not-a-device-id".into(),
                ..BlockSample::default()
            },
            BlockSample {
                id: id(n + 100),
                ..BlockSample::default()
            },
        ];
        let r = UnitReading {
            now: T0 + i64::from(n) * 1000,
            at_ns: u64::from(n) * 1_000_000_000,
            block: &block,
            ..UnitReading::default()
        };
        let units = rings.absorb(&r, &mut peaks);
        let ids: Vec<String> = units.disks.into_iter().map(|d| d.id).collect();
        assert_eq!(ids, [id(n), id(n + 100)], "distinct, valid, at most two");
    }
    let kept: Vec<&str> = peaks.disks.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(kept, [id(105), id(5), id(104), id(4)]);
    assert_eq!(rings.disks.len(), DISK_DEVICES_KEPT);

    // The rows round-trip in recency order; a row of a fifth device, of a bad id or of an unknown
    // name is skipped.
    let mut churned = peaks.clone();
    churned.mem_churn = DailyMax {
        days: vec![(20_740, 1.5)],
        read_s: 30,
    };
    churned.disks[1].write = DailyMax {
        days: vec![(20_740, 9.0)],
        read_s: 10,
    };
    let mut rows = churned.to_pairs();
    assert_eq!(rows.len(), Peaks::NAMES.len() + 2 * DISK_DEVICES_KEPT);
    assert_eq!(
        rows[Peaks::NAMES.len()].0,
        format!("disk_write:{}", id(105))
    );
    rows.push((format!("disk_read:{}", id(9)), 10, vec![(20_740, 1.0)]));
    rows.push(("disk_read:XYZ".into(), 10, vec![(20_740, 1.0)]));
    rows.push(("gpu_clock".into(), 10, vec![(20_740, 1.0)]));
    let back = Peaks::from_pairs(rows.iter().map(|(n, r, d)| (n.as_str(), *r, d.as_slice())));
    assert_eq!(back, churned);
}

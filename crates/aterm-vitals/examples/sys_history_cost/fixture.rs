// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure six-hour filled rings shared by the component and real-host query measurements.

use std::sync::Arc;

use aterm_vitals::model::{
    ClusterSpec, GpuSample, LinkKind, LinkSample, MemSample, Pressure, Resource, Sample, Snapshot,
    TabTokens, TaskRoom, Ticks, Vitals, VolumeSample,
};
use aterm_vitals::net_counter::ByteCounter;
use aterm_vitals::ring::MultiScaleSpec;
use aterm_vitals::starts::StartCount;

const SECOND: u64 = 1_000;
const SECONDS: u64 = 6 * 60 * 60;
const START_MS: i64 = 1_800_000_000_000;
const GIB: u64 = 1 << 30;

pub fn filled_rings() -> Arc<Snapshot> {
    let mut model = Vitals::new();
    let mut busy = [0_u64; 4];
    let (mut rx, mut tx, mut output, mut input) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut last = None;
    for second in 0..=SECONDS {
        // One genuine missing-observation gap, without sleeping or generating host load.
        if (SECONDS - 60..SECONDS - 30).contains(&second) {
            continue;
        }
        for (core, total) in busy.iter_mut().enumerate() {
            let increment = if second % 37 == 0 || second >= SECONDS - 20 {
                0
            } else {
                (100 + (second % 10) * 100 + core as u64 * 25).min(1000)
            };
            *total += increment;
        }
        rx += if second % 41 == 0 {
            0
        } else {
            100_000 + second % 11 * 1_000_000
        };
        tx += 50_000 + second % 17 * 250_000;
        output += 1 + second % 29 * 11;
        input += 4 + second % 13 * 29;
        let app = (16 + second % 40) * GIB;
        let free = 64 * GIB - app - 7 * GIB;
        let sample = Sample {
            at_ns: second * 1_000_000_000,
            at_unix_ms: START_MS + (second * SECOND) as i64,
            mono_ms: second * SECOND,
            cadence_ms: SECOND as u32,
            cores: Some(
                busy.iter()
                    .map(|&busy| Ticks {
                        busy,
                        total: second * 1000,
                    })
                    .collect(),
            ),
            clusters: vec![ClusterSpec {
                name: "Synthetic".into(),
                logical: vec![0, 1, 2, 3],
            }],
            mem: Some(MemSample {
                total: 64 * GIB,
                app,
                wired: 4 * GIB,
                compressed: Some(GIB),
                stored: Some(3 * GIB),
                cached: 2 * GIB,
                free,
                available: (free + 3 * GIB).min(64 * GIB),
                pressure: Pressure::Normal,
                swap_used: Some(GIB),
                swap_total: Some(2 * GIB),
                exact: true,
                ..MemSample::default()
            }),
            gpu: Some(GpuSample {
                device_pct: Some((second % 101) as u8),
                ..GpuSample::default()
            }),
            volume: Some(VolumeSample {
                total: 1024 * GIB,
                free: (128 + second % 256) * GIB,
                mount: "/synthetic".into(),
                reserved: 0,
            }),
            links: Some(vec![LinkSample {
                name: "synthetic0".into(),
                index: 1,
                byte_counter: ByteCounter::Cumulative64,
                kind: LinkKind::Ethernet,
                up: true,
                counts: true,
                rx,
                tx,
                rx_packets: second * 1000,
                tx_packets: second * 500,
                rate_bps: Some(1_000_000_000),
                ..LinkSample::default()
            }]),
            procs: Some(Vec::new()),
            room: TaskRoom {
                procs: Some(100 + second % 200),
                procs_max: Some(4096),
                ..TaskRoom::default()
            },
            starts: Some(StartCount::Forks(second * 4)),
            tokens: vec![TabTokens {
                key: 1,
                session: "synthetic-session".into(),
                out_total: output,
                in_total: input,
                complete: true,
                ..TabTokens::default()
            }],
            ..Sample::default()
        };
        last = Some(model.absorb(&sample));
    }
    let snapshot = last.expect("six hours of model readings");
    assert_eq!(snapshot.at_unix_ms - START_MS, (SECONDS * SECOND) as i64);
    let expected = MultiScaleSpec::FULL_6H.iter_slots().count();
    let mut cells = 0;
    for row in Resource::ALL {
        let projection = snapshot.resource(row, &[]).history_slots(
            MultiScaleSpec::FULL_6H,
            snapshot.at_unix_ms,
            &snapshot.time_gaps,
        );
        assert_eq!(projection.cells.len(), expected);
        assert!(
            projection
                .cells
                .iter()
                .any(|cell| cell.primary.cell.mean.is_some()),
            "every measured row needs real retained data: {row:?}"
        );
        assert!(
            projection.cells.last().unwrap().primary.cell.mean.is_some(),
            "every measured row needs a populated oldest box: {row:?}"
        );
        for period_ms in [1_000, 10_000, 60_000, 600_000, 3_600_000] {
            assert!(
                projection.cells.iter().any(|cell| {
                    MultiScaleSpec::FULL_6H.bands[usize::from(cell.primary.slot.band)].period_ms
                        == period_ms
                        && cell.primary.cell.mean.is_some()
                }),
                "every measured row needs a populated tier: {row:?}, {period_ms}"
            );
        }
        if row == Resource::Network {
            assert!(
                projection
                    .cells
                    .last()
                    .unwrap()
                    .secondary
                    .as_ref()
                    .is_some_and(|cell| cell.cell.mean.is_some()),
                "the oldest transmit box must be populated"
            );
        }
        if matches!(row, Resource::Cpu | Resource::Memory) {
            assert!(
                projection
                    .cells
                    .iter()
                    .any(|cell| cell.primary.cell.max != cell.primary.cell.mean),
                "a nonuniform source must exercise both statistics: {row:?}"
            );
        }
        assert!(
            projection
                .cells
                .iter()
                .any(|cell| { cell.primary.gap.is_some() && cell.primary.cell.mean.is_none() }),
            "every measured row must retain a hollow missing-observation gap: {row:?}"
        );
        if row == Resource::Cpu {
            assert!(
                projection
                    .cells
                    .iter()
                    .any(|cell| cell.primary.cell.mean == Some(0.0)),
                "a measured zero must survive projection beside the hollow gap"
            );
        }
        cells += projection.cells.len();
    }
    println!(
        "fixture synthetic=1 span_ms={} cadence_ms={SECOND} rows={} boxes={cells} gaps={} oldest_all_rows_populated=1 all_tiers_populated=1 oldest_network_secondary_populated=1 projected_cpu_zero=1 hollow_gap_each_row=1",
        SECONDS * SECOND,
        Resource::ALL.len(),
        snapshot.time_gaps.len()
    );
    Arc::new(snapshot)
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::model::{MemSample, Pressure, Sample, Ticks, Vitals};
use aterm_vitals::ring::{Tiers, multi_scale_cells};
use aterm_vitals::sys::json::Jv;
use aterm_vitals::sys::{Ctx, Freshness, Machine, SysQuery, Target, render_json};

const HISTORY_AT: i64 = 1_800_000_000_000;
const GIB: u64 = 1 << 30;

fn history_reading(second: u64, busy: u64, used_gib: u64) -> Sample {
    Sample {
        at_ns: (second + 1) * 1_000_000_000,
        at_unix_ms: HISTORY_AT + second as i64 * 1000,
        mono_ms: second * 1000,
        cadence_ms: 1000,
        cores: Some(vec![
            Ticks {
                busy,
                total: (second + 1) * 100
            };
            2
        ]),
        mem: Some(MemSample {
            total: 16 * GIB,
            app: used_gib * GIB,
            free: (16 - used_gib) * GIB,
            pressure: Pressure::Normal,
            ..MemSample::default()
        }),
        ..Sample::default()
    }
}

fn history_field<'a>(value: &'a Jv, key: &str) -> &'a Jv {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing {key}: {value:?}"))
}

fn history_array(value: &Jv) -> &[Jv] {
    let Jv::Arr(values) = value else {
        panic!("expected array: {value:?}")
    };
    values
}

fn history_number(value: Option<f64>) -> Jv {
    value
        .and_then(Jv::number)
        .unwrap_or_else(|| Jv::why("not_sampled"))
}

fn compare_history_with_deck(snapshot: &Snapshot, resolution: Option<&str>) {
    let machine = Machine::default();
    let target = Target::default();
    let context = Ctx {
        snap: Some(snapshot),
        epoch: 7,
        as_of_unix_ms: snapshot.at_unix_ms as u64,
        age_ms: Some(9000),
        state: Freshness::Stale,
        machine: &machine,
        target: &target,
        tabs: &[],
        caller_panes: None,
        event_caller: None,
        owner_class: true,
        volume: None,
        roots: &[],
        disk: None,
        stores: &[],
        home: None,
        events: &[],
        gap: None,
        head_seq: snapshot.seq,
        held: None,
    };
    let option = resolution.map_or_else(String::new, |word| format!(" res={word}"));
    let query = SysQuery::parse(&format!("history cpu mem{option} --json")).unwrap();
    let reply = render_json(&query, &context);
    let rows = history_array(history_field(history_field(&reply, "history"), "rows"));
    assert_eq!(
        rows.len(),
        2,
        "the requested CPU and memory rows must exist"
    );
    let metrics = FaceMetrics::helvetica_neue();
    let locale = Locale::default();
    for (resource, word) in [(Resource::Cpu, "cpu"), (Resource::Memory, "mem")] {
        let exported = rows
            .iter()
            .find(|row| row.get("row") == Some(&Jv::s(word)))
            .unwrap();
        let view = snapshot.resource(resource, &[]);
        let deck = RowWords::new(resource, snapshot, &[], &locale, &metrics);
        let painted = &deck.multi_scale[0];
        assert_eq!(painted.len(), 34);
        let series = view.first();
        let raw = multi_scale_cells(
            Tiers {
                ring: &series.ring,
                ring_k: view.ring_k,
                long: &series.long,
                long_k: view.long_k,
                recent: &series.recent,
            },
            snapshot.at_unix_ms,
            STRIP_SPECS[0],
            SLOT_COVER_MS,
            &snapshot.time_gaps,
        );
        let selected: Vec<_> = painted
            .iter()
            .enumerate()
            .filter(|(_, cell)| {
                query.history_res.is_none_or(|resolution| {
                    STRIP_SPECS[0].bands[usize::from(cell.slot.band)].period_ms
                        == resolution.period_ms()
                })
            })
            .collect();
        let boxes = history_array(history_field(exported, "boxes"));
        assert_eq!(boxes.len(), selected.len());
        assert_eq!(
            history_field(exported, "capacity"),
            &history_number(view.capacity)
        );
        assert_eq!(
            history_field(exported, "secondary_capacity"),
            &Jv::why("not_sampled")
        );
        for (exported, (index, drawn)) in boxes.iter().zip(selected) {
            let slot = drawn.slot;
            assert_eq!(
                history_field(exported, "band"),
                &Jv::u(u64::from(slot.band))
            );
            assert_eq!(
                history_field(exported, "start_unix_ms"),
                &Jv::Int(snapshot.at_unix_ms.saturating_sub(slot.age_hi_ms))
            );
            assert_eq!(
                history_field(exported, "end_unix_ms"),
                &Jv::Int(snapshot.at_unix_ms.saturating_sub(slot.age_lo_ms))
            );
            assert_eq!(
                history_field(exported, "age_hi_ms"),
                &Jv::Int(slot.age_hi_ms)
            );
            assert_eq!(
                history_field(exported, "age_lo_ms"),
                &Jv::Int(slot.age_lo_ms)
            );
            assert_eq!(history_field(exported, "live"), &Jv::Bool(slot.is_live_now));
            let source = history_field(exported, "primary");
            let source_cell = raw[index].cell;
            let source_number = |value: Option<f64>| {
                if value.is_none() && raw[index].gap == Some(GapKind::Sleep) {
                    Jv::why("asleep")
                } else {
                    history_number(value)
                }
            };
            assert_eq!(
                history_field(source, "mean"),
                &source_number(source_cell.mean)
            );
            assert_eq!(
                history_field(source, "max"),
                &source_number(source_cell.max)
            );
            assert_eq!(
                history_field(source, "weighted_source_ms"),
                &history_number(Some(source_cell.tau_ms))
            );
            assert_eq!(
                history_field(source, "over_ms"),
                &history_number(Some(source_cell.over_ms))
            );
            assert_eq!(
                history_field(exported, "secondary"),
                &Jv::why("not_sampled")
            );
            let paint = history_field(exported, "painted");
            assert_eq!(
                history_field(paint, "mean"),
                &history_number(drawn.cell.heat.map(f64::from))
            );
            assert_eq!(
                history_field(paint, "max"),
                &history_number(drawn.cell.max.map(f64::from))
            );
            assert_eq!(
                history_field(paint, "over_share"),
                &history_number(Some(f64::from(drawn.cell.over)))
            );
            let gap = match drawn.cell.gap {
                Some(GapKind::Paused) => Jv::s("paused"),
                Some(GapKind::Sleep) => Jv::s("sleep"),
                Some(GapKind::Crash) => Jv::s("crash"),
                None => Jv::s("none"),
            };
            assert_eq!(history_field(paint, "gap"), &gap);
            assert_eq!(history_field(source, "gap"), &gap);
        }
    }
    let mut newer_context = context.clone();
    newer_context.as_of_unix_ms += 300_000;
    newer_context.age_ms = Some(newer_context.age_ms.unwrap() + 300_000);
    let later_reply = render_json(&query, &newer_context);
    assert_eq!(
        history_field(&reply, "history"),
        history_field(&later_reply, "history"),
        "reply age must not move boxes away from the source snapshot's wall axis"
    );
}

#[test]
fn sys_history_boxes_are_the_strips_boxes() {
    let mut engine = Vitals::new();
    let mut snapshot = engine.absorb(&history_reading(0, 0, 0));
    for (second, busy, memory) in [
        (1, 25, 8),
        (2, 125, 16),
        (3, 125, 0),
        (4, 175, 4),
        (5, 175, 0),
    ] {
        snapshot = engine.absorb(&history_reading(second, busy, memory));
    }
    let metrics = FaceMetrics::helvetica_neue();
    let locale = Locale::default();
    for resource in [Resource::Cpu, Resource::Memory] {
        let deck = RowWords::new(resource, &snapshot, &[], &locale, &metrics);
        assert!(
            deck.multi_scale[0]
                .iter()
                .any(|box_| box_.cell.heat == Some(0.0)),
            "known zero must be painted, not made hollow: {resource:?}"
        );
        assert!(
            deck.multi_scale[0]
                .iter()
                .any(|box_| box_.cell.heat == Some(1.0)),
            "the raw maximum must be normalized against its actual capacity: {resource:?}"
        );
        assert!(
            deck.multi_scale[0]
                .iter()
                .any(|box_| box_.cell.heat.is_none()),
            "history before the first reading must remain unread: {resource:?}"
        );
    }
    assert_eq!(snapshot.resource(Resource::Cpu, &[]).capacity, Some(2.0));
    assert_eq!(
        snapshot.resource(Resource::Memory, &[]).capacity,
        Some((16 * GIB) as f64)
    );
    compare_history_with_deck(&snapshot, None);
    for resolution in ["1s", "10s", "1m", "10m", "1h"] {
        compare_history_with_deck(&snapshot, Some(resolution));
    }

    let mut asleep = snapshot.clone();
    asleep.time_gaps.push(TimeGap {
        start_ms: HISTORY_AT + 1000,
        end_ms: HISTORY_AT + 4000,
        kind: GapKind::Sleep,
    });
    for resource in [Resource::Cpu, Resource::Memory] {
        let deck = RowWords::new(resource, &asleep, &[], &locale, &metrics);
        assert!(
            deck.multi_scale[0]
                .iter()
                .any(|box_| box_.cell.gap == Some(GapKind::Sleep))
        );
    }
    compare_history_with_deck(&asleep, None);

    let mut missing = history_reading(10, 0, 0);
    missing.cores = None;
    missing.mem = None;
    let unread = Vitals::new().absorb(&missing);
    for resource in [Resource::Cpu, Resource::Memory] {
        let deck = RowWords::new(resource, &unread, &[], &locale, &metrics);
        assert!(
            deck.multi_scale[0]
                .iter()
                .all(|box_| box_.cell.heat.is_none())
        );
    }
    compare_history_with_deck(&unread, None);

    let mut stepped = history_reading(6, 175, 0);
    stepped.at_unix_ms = HISTORY_AT + 1000;
    stepped.cores = None;
    stepped.mem = None;
    let backward = engine.absorb(&stepped);
    assert!(backward.at_ns > snapshot.at_ns && backward.at_unix_ms < snapshot.at_unix_ms);
    let memory = backward.resource(Resource::Memory, &[]);
    let fine = multi_scale_cells(
        Tiers {
            ring: &memory.ring,
            ring_k: memory.ring_k,
            long: &memory.long,
            long_k: memory.long_k,
            recent: &memory.recent,
        },
        backward.at_unix_ms,
        STRIP_SPECS[0],
        SLOT_COVER_MS,
        &backward.time_gaps,
    );
    let fine_max: Vec<_> = fine
        .iter()
        .take(10)
        .filter_map(|box_| box_.cell.max)
        .collect();
    assert!(
        !fine_max.is_empty(),
        "the backward-time control must retain known past readings"
    );
    assert!(
        fine_max.iter().all(|value| *value <= (8 * GIB) as f64),
        "the later 16GiB reading cannot enter boxes before it: {fine_max:?}"
    );
    compare_history_with_deck(&backward, None);
}

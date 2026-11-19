// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The append-only strip checkpoint's laws (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §9).

use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn strip_event_codec_preserves_original_private_scope_and_withholds_legacy_authority() {
    use aterm_vitals::events::{EventScope, EventSpan, SessionScope};
    let marker = StripEvent {
        at_unix_ms: T0 + 1_000,
        kind: StripEventKind::SleepWake,
        label: "original private marker".into(),
        scope: EventScope::Session(SessionScope {
            local: 7,
            sid: "t0000000001".into(),
            nonce: "11".repeat(16),
        }),
        span: Some(EventSpan {
            onset_unix_ms: T0,
            end_unix_ms: T0 + 1_000,
        }),
    };
    let saved = SavedStrip {
        last_wall_ms: T0 + 2_000,
        strip_events: vec![marker.clone()],
        ..SavedStrip::default()
    };
    let encoded = super::base_json(&saved, 1).unwrap();
    let (_, restored) = super::base_of(&encoded).unwrap();
    assert_eq!(restored.strip_events, vec![marker]);
    let mut old_document: super::BaseDoc = aterm_json::from_str(&encoded).unwrap();
    old_document.schema = 4;
    let (_, old) = super::base_of(&aterm_json::to_string(&old_document).unwrap()).unwrap();
    assert_eq!(old.strip_events[0].scope, EventScope::Unknown);
    assert_eq!(old.strip_events[0].span, None);
    let legacy: super::EventDoc =
        aterm_json::from_str(r#"[1234,"crash","legacy private label"]"#).unwrap();
    let legacy = super::event_of(legacy).unwrap();
    assert_eq!(legacy.scope, EventScope::Unknown);
    assert_eq!(legacy.span, None);
    assert_eq!(legacy.at_unix_ms, 1234);
    let malformed: super::EventDoc = aterm_json::from_str(
        r#"[1234,"crash","unbound label",["session",7,"not-a-session","bad-nonce"],[2000,1000]]"#,
    )
    .unwrap();
    let malformed = super::event_of(malformed).unwrap();
    assert_eq!(malformed.scope, EventScope::Unknown);
    assert_eq!(malformed.span, None);
}

use aterm_vitals::blockage::{Blockage, BlockageBox};
use aterm_vitals::heat::{
    ClockRange, Counters, HeatBook, PressureBox, ThermalPressure, UsualClocks,
};
use aterm_vitals::model::{
    ClusterSpec, GpuSample, LinkKind, LinkSample, MemSample, PlaceKind, Pressure, ProcSample,
    ProcState, STRIP_HISTORY_CAP, STRIP_RINGS, Sample, SavedStrip, StripEvent, StripEventKind,
    StripLine, TabRoot, TaskRoom, Ticks, VolumeSample,
};
use aterm_vitals::ring::{
    Agg, BUCKET_MS, GapKind, LONG_BUCKET_MS, LONG_BUCKETS, RECENT_CAP, RING_BUCKETS, SavedRing,
    TimeGap,
};
use aterm_vitals::situation::{
    Lid, LidReading, Power, PowerSource, Reading, Situation, Span, Stay,
};
use aterm_vitals::starts::{CoalTaskRead, CoalTasks};

#[test]
fn reported_cpu_history_codec_preserves_its_basis_without_guessing_legacy_accounts() {
    let accounting = aterm_vitals::published_cpu::CpuAccounting::PUBLISHED_COUNTERS;
    let saved = SavedStrip {
        last_wall_ms: 1000,
        scope_cpu_accounting: Some(accounting),
        ..SavedStrip::default()
    };
    let text = base_json(&saved, 1).expect("a base");
    let (_, decoded) = base_of(&text).expect("current base");
    assert_eq!(decoded.scope_cpu_accounting, Some(accounting));
    let mut doc: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    doc.as_object_mut().unwrap().remove("scope_cpu_accounting");
    assert_eq!(
        base_of(&aterm_json::to_string(&doc).unwrap())
            .unwrap()
            .1
            .scope_cpu_accounting,
        None
    );
    doc.as_object_mut().unwrap().insert(
        "scope_cpu_accounting".into(),
        aterm_json::Value::String("unknown_basis".into()),
    );
    assert_eq!(
        base_of(&aterm_json::to_string(&doc).unwrap())
            .unwrap()
            .1
            .scope_cpu_accounting,
        None
    );
    let line = StripLine {
        k: 1,
        wall_ms: 1000,
        scope_cpu_accounting: Some(accounting),
        ..StripLine::default()
    };
    let text = line_json(&line, 1).expect("a tail line");
    assert_eq!(
        line_of(&text).unwrap().1.scope_cpu_accounting,
        Some(accounting)
    );
    let mut doc: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    doc.as_object_mut().unwrap().remove("scope_cpu_accounting");
    assert_eq!(
        line_of(&aterm_json::to_string(&doc).unwrap())
            .unwrap()
            .1
            .scope_cpu_accounting,
        None
    );
}

use super::{
    BASE_FILE, LEGACY_FILE, STRIP_MAX_BYTES, STRIP_SCHEMA, StripStore, TAIL_FILE, TAIL_MAX_BYTES,
    base_json, base_of, line_json, line_of, strip_dir,
};
use crate::vitals_host::StripLock;

/// The wall clock the tests' readings start at: an epoch's start.
const T0: i64 = 1_793_000_000_000;

/// Reading `i` of a machine one second apart from [`T0`]'s half-slot, the rows' strip rings fed:
/// the rows, Network's two directions, memory pressure, aterm's own processes and the process
/// count; and of the instruments', the thermal pressure level and blockage, with a stay for the
/// situation's span. [`instrumented`] feeds the heat chain's and the exits ring too.
fn reading(i: u64) -> Sample {
    let mono_ms = (i + 1) * 1_000;
    let wobble = (i * 7_919) % 1_009;
    let ticks = mono_ms / 10 + wobble;
    let proc = |pid, ppid, cpu_ms: u64, foot: u64, name: &str| ProcSample {
        pid,
        start: Some(1),
        ppid,
        cpu_ns: Some(cpu_ms * 1_000_000 + wobble * 1_013),
        footprint: Some((foot << 20) + wobble * 4_099),
        disk_written: Some(2_000 * mono_ms + wobble * 37),
        uid: Some(501),
        name: name.into(),
        ..ProcSample::default()
    };
    Sample {
        at_ns: mono_ms * 1_000_000,
        at_unix_ms: T0 + 500 + 1_000 * i as i64,
        mono_ms,
        cadence_ms: 1_000,
        cores: Some(vec![
            Ticks {
                busy: ticks,
                total: mono_ms / 10 + 1_009,
            },
            Ticks {
                busy: ticks / 3,
                total: mono_ms / 10 + 1_009,
            },
        ]),
        mem: Some(MemSample {
            total: 32 << 30,
            app: (8 << 30) + wobble * 65_537,
            wired: (3 << 30) + wobble * 4_093,
            free: 12 << 30,
            pressure: if wobble.is_multiple_of(3) {
                Pressure::Warn
            } else {
                Pressure::Normal
            },
            ..MemSample::default()
        }),
        gpu: Some(GpuSample {
            device_pct: Some(u8::try_from(wobble % 100).unwrap_or(0)),
            ..GpuSample::default()
        }),
        volume: Some(VolumeSample {
            total: 2_000_000_000_000,
            free: 1_000_000_000_000 - wobble * 1_000_003,
            reserved: 0,
            mount: "/".into(),
        }),
        procs: Some(vec![
            proc(100, 1, mono_ms / 10, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, mono_ms / 2, 300, "cargo"),
        ]),
        self_pid: 100,
        tabs: vec![TabRoot {
            key: 7,
            tab: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "claude".into(),
            tab_id: Some(7),
            agent_leader: None,
        }],
        links: Some(vec![LinkSample {
            byte_counter: aterm_vitals::net_counter::ByteCounter::Cumulative64,
            name: "en0".into(),
            kind: LinkKind::Wifi,
            up: true,
            counts: true,
            rx: 1_000 * mono_ms + wobble * 977,
            tx: 250 * mono_ms + wobble * 331,
            rate_bps: Some(100_000_000),
            ..LinkSample::default()
        }]),
        room: TaskRoom {
            procs: Some(900 + wobble % 50),
            procs_max: Some(16_000),
            ..TaskRoom::default()
        },
        starts: Some(aterm_vitals::StartCount::Forks(40 * i + wobble % 7)),
        self_uid: Some(501),
        thermal_pressure: Some(Ok(if wobble.is_multiple_of(5) {
            ThermalPressure::Moderate
        } else {
            ThermalPressure::Nominal
        })),
        place: Some(PlaceKind::Lan),
        situation: at_a_desk(),
        ..Sample::default()
    }
}

fn situation(source: PowerSource, lid: Lid) -> Situation {
    Situation {
        lid: Reading::Read(LidReading {
            lid,
            changed_ms: None,
        }),
        power: Reading::Read(Power {
            source,
            battery: None,
            adapter_w: None,
            changed_ms: None,
        }),
        ..Situation::default()
    }
}

/// On AC with the lid open.
fn at_a_desk() -> Situation {
    situation(PowerSource::Ac, Lid::Open)
}

/// Reading `i`, a second after the one before from [`T0`]'s half-slot, with every instrument's
/// reader answering (A4): one coalition that has done a second's work each second (four Super
/// cores at 4000 MHz, three Efficiency cores' worth at 2500 MHz, a watt a busy core and a GPU
/// drawing 1.6 W four readings in five and 1.1 W the fifth), started 40 tasks and ended 30;
/// thermal pressure Moderate one reading in seven; a zombie under the tab's shell; on the LAN on AC for the first 40 readings,
/// then on a hotspot on battery.
fn instrumented(i: u64) -> Sample {
    let k = i + 1;
    let mono_ms = k * 1_000;
    let (p_ns, e_ns) = (4_000_000_000_u64, 3_000_000_000_u64);
    let (p_cycles, e_cycles) = (16_000_000_000_u64, 7_500_000_000_u64);
    let ticks = |ns: u64| ns * 3 / 125;
    let so_far = Counters {
        cpu_ticks: ticks(p_ns + e_ns) * k,
        qos_ticks: ticks(p_ns + e_ns) * k,
        p_ticks: ticks(p_ns) * k,
        energy_nj: (p_ns + e_ns) * k,
        instructions: 2 * (p_cycles + e_cycles) * k,
        cycles: (p_cycles + e_cycles) * k,
        p_instructions: 2 * p_cycles * k,
        p_cycles: p_cycles * k,
        gpu_energy_nj: 1_500_000_000 * k + (k % 5) * 100_000_000,
        ..Counters::default()
    }
    .words();
    let read = CoalTaskRead::of(
        Some(1),
        false,
        [CoalTasks {
            id: 1,
            started: 40 * k,
            exited: 30 * k,
        }],
    )
    .with_heat(HeatBook {
        at_ns: mono_ms * 1_000_000,
        timebase: (125, 3),
        by_id: vec![(1, so_far)],
        total: Ok(so_far),
    });
    let proc = |pid, ppid, name: &str, state| ProcSample {
        pid,
        start: Some(1),
        ppid,
        uid: Some(501),
        name: name.into(),
        state,
        ..ProcSample::default()
    };
    let away = i >= 40;
    Sample {
        at_ns: mono_ms * 1_000_000,
        at_unix_ms: T0 + 500 + 1_000 * i as i64,
        mono_ms,
        cadence_ms: 1_000,
        cores: Some(
            (0..10)
                .map(|c| Ticks {
                    busy: k * if c < 4 { 100 } else { 50 },
                    total: k * 100,
                })
                .collect(),
        ),
        clusters: vec![
            ClusterSpec {
                name: "Super".into(),
                logical: vec![0, 1, 2, 3],
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: (4..10).collect(),
            },
        ],
        procs: Some(vec![
            proc(100, 1, "aterm", ProcState::Run),
            proc(200, 100, "zsh", ProcState::Run),
            proc(300, 200, "cc", ProcState::Zombie),
        ]),
        self_pid: 100,
        self_uid: Some(501),
        tabs: vec![TabRoot {
            key: 7,
            tab: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "make".into(),
            tab_id: Some(7),
            agent_leader: None,
        }],
        starts: Some(aterm_vitals::StartCount::Coalitions { read, own: None }),
        coal_lane: aterm_vitals::explain::CoalLane::On,
        thermal_pressure: Some(Ok(if i % 7 == 3 {
            ThermalPressure::Moderate
        } else {
            ThermalPressure::Nominal
        })),
        clock_range: Some(Ok(ClockRange {
            p_mhz: (1308.0, 4464.0),
            e_mhz: (972.0, 3048.0),
        })),
        cpu_brand: Some("Apple M5".into()),
        place: Some(if away {
            PlaceKind::Hotspot
        } else {
            PlaceKind::Lan
        }),
        situation: if away {
            situation(PowerSource::Battery, Lid::Open)
        } else {
            at_a_desk()
        },
        ..Sample::default()
    }
}

/// A model that read readings `0..n`, kept by `store` at each.
fn read_kept(
    model: &mut aterm_vitals::Vitals,
    store: &mut StripStore,
    range: std::ops::Range<u64>,
) {
    let t0 = Instant::now();
    for i in range {
        let _ = model.absorb(&reading(i));
        store.keep(model, t0 + Duration::from_secs(i), &mut StripLock::Unlocked);
    }
}

/// The checkpoint in the state folder `home`, opened as a start opens it.
fn store_in(home: &Path) -> (StripStore, SavedStrip) {
    StripStore::open(Some(strip_dir(home)), Some(home.join(LEGACY_FILE)))
}

fn tail_in(home: &Path) -> String {
    std::fs::read_to_string(strip_dir(home).join(TAIL_FILE)).unwrap_or_default()
}

/// Each slot's 10 s buckets at or before `through`, oldest first.
fn closed(strip: &SavedStrip, through: i64) -> Vec<Vec<(i64, Agg)>> {
    (0..STRIP_RINGS)
        .map(|slot| {
            strip
                .ring(slot)
                .map(|r| {
                    r.buckets
                        .iter()
                        .copied()
                        .filter(|&(k, _)| k <= through)
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// Each slot's ten-minute buckets.
fn longs(strip: &SavedStrip) -> Vec<Vec<(i64, Agg)>> {
    (0..STRIP_RINGS)
        .map(|slot| strip.ring(slot).map(|r| r.long.clone()).unwrap_or_default())
        .collect()
}

/// Record §9: the schema-1 file a build before the append-only checkpoint wrote restores
/// nothing, so no bucket's read time, minimum or start meaning is invented, and goes at the first
/// base written; a base of schema 1 restores nothing either, in the old layout or in the
/// current one, where only the schema guard refuses it; nor does a base labelled with an
/// earlier schema this reads (2, 3) that does not hold that schema's rings, nor one of the
/// schema after this. None panics.
/// CONTROL: the same strip written at the current schema restores.
#[test]
fn a_schema_one_strip_file_restores_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let home = dir.path();
    let legacy = home.join(LEGACY_FILE);
    let schema_one = r#"{"schema":1,"last_wall_ms":1793000060000,"clean_stop":false,"caps":[8.0,null,null,null,null,null,null],"rings":[{"buckets":[[179300000,0.5,10,0.9,0],[179300001,0.25,10,0.3,0]],"recent":[[1793000060000,0.25,false]],"long":[[2988333,7.5,20,0.9,0]]}],"net_tx":{},"yours":{"buckets":[[179300000,400.0,10,40.0,0]]},"pressure":{},"aterm":[],"gaps":[[1793000000000,1793000005000,"sleep"]],"events":[[1793000001000,"crash","aterm crash"]]}"#;
    std::fs::write(&legacy, schema_one).expect("the schema-1 file");
    let (mut store, kept) = store_in(home);
    assert_eq!(
        kept,
        SavedStrip::default(),
        "a schema-1 file restores nothing"
    );
    assert!(legacy.exists(), "opening removes nothing");

    std::fs::create_dir_all(strip_dir(home)).expect("the folder");
    std::fs::write(strip_dir(home).join(BASE_FILE), schema_one).expect("a schema-1 base");
    assert_eq!(
        store_in(home).1,
        SavedStrip::default(),
        "nor does a schema-1 base"
    );

    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..3);
    assert!(!legacy.exists(), "the first base written removes it");
    let (_, back) = store_in(home);
    assert_eq!(
        (back.last_wall_ms, back.proc_starts),
        (T0 + 500, Some(true)),
        "CONTROL: the current schema restores the base the first reading wrote"
    );
    let base = strip_dir(home).join(BASE_FILE);
    let current = std::fs::read_to_string(&base).expect("the base");
    let schema = format!("{{\"schema\":{STRIP_SCHEMA},");
    assert!(current.starts_with(&schema), "{current}");
    for other in [1, 2, 3, STRIP_SCHEMA + 1] {
        let relabelled = current.replacen(&schema, &format!("{{\"schema\":{other},"), 1);
        std::fs::write(&base, relabelled).expect("a base of another schema");
        assert_eq!(
            store_in(home).1,
            SavedStrip::default(),
            "a schema-{other} base in the current layout"
        );
    }
    // A base labelled 2 or 3 that holds one ring: neither schema's count, so neither reads it.
    let one_ring = r#"{"schema":2,"generation":4,"last_wall_ms":1793000060000,"through_k":179300005,"clean_stop":true,"caps":[8.0,null,null,null,null,null,null],"rings":[{"b":[[179300004,10000,10,0,0.5,0.25,0.75],[179300005,10000,10,0,0.25,0.125,0.5]],"r":[[1793000059500,0.25,false,1000]],"l":[[2988333,20000,20,0,0.375,0.125,0.75]],"f":179300006}],"proc_starts":true,"gaps":[],"events":[]}"#;
    for text in [
        one_ring.to_owned(),
        one_ring.replacen("{\"schema\":2,", "{\"schema\":3,", 1),
    ] {
        std::fs::write(&base, &text).expect("a one-ring base");
        assert_eq!(store_in(home).1, SavedStrip::default(), "{text}");
    }
}

#[test]
fn prior_coverage_schema_preserves_measurements_without_claiming_they_were_complete() {
    let saved = SavedStrip {
        last_wall_ms: T0 + 7 * 3_600_000,
        aterm: std::array::from_fn(|i| fullest_ring(i as f64)),
        rings: std::array::from_fn(|i| fullest_ring(i as f64 + 10.0)),
        ..SavedStrip::default()
    };
    let mut document: super::BaseDoc =
        aterm_json::from_str(&base_json(&saved, 3).unwrap()).unwrap();
    document.schema = 2;
    document.rings.truncate(17);
    let (_, migrated) = base_of(&aterm_json::to_string(&document).unwrap()).unwrap();
    assert_eq!(
        migrated.rings, saved.rings,
        "the machine's measured history survives"
    );
    assert_eq!(
        migrated.aterm, saved.aterm,
        "historical scope figures survive too"
    );
    for (figure, coverage) in [(0, 1), (2, 0)] {
        let old = &saved.aterm[figure];
        let mark = &migrated.aterm_unread[coverage];
        assert_eq!(mark.buckets.len(), old.buckets.len());
        assert_eq!(mark.recent.len(), old.recent.len());
        assert!(
            mark.buckets
                .iter()
                .all(|(_, agg)| agg.mean == 1.0 && agg.max == 1.0)
        );
        assert!(mark.recent.iter().all(|(_, value, _, _)| *value == 1.0));
        assert_eq!(mark.buckets[0].1.tau_ms, old.buckets[0].1.tau_ms);
    }
    assert!(
        migrated.aterm_cpu_charge.is_empty(),
        "the old format never measured a child charge"
    );
    assert_eq!(
        base_of(&base_json(&migrated, 4).unwrap()),
        Some((4, migrated))
    );
}

/// A ring at its fullest: an hour of 10 s buckets, [`LONG_BUCKETS`] ten-minute ones and
/// [`RECENT_CAP`] raw readings, each number at seventeen significant digits.
fn fullest_ring(seed: f64) -> SavedRing {
    let open_k = (T0 + 7 * 3_600_000).div_euclid(BUCKET_MS);
    let v = |j: usize| -(123_456_789.012_345_67 + seed + j as f64 * 0.001_234_567_890_123_4);
    let agg = |j: usize, tau_ms: u32, n: u16| Agg {
        tau_ms,
        n,
        over_ms: tau_ms,
        mean: v(j),
        min: v(j) - 1.000_000_000_000_1,
        max: v(j) + 1.000_000_000_000_1,
    };
    SavedRing {
        buckets: (0..RING_BUCKETS)
            .map(|j| (open_k - (RING_BUCKETS - 1 - j) as i64, agg(j, 10_000, 40)))
            .collect(),
        recent: (0..RECENT_CAP)
            .map(|j| {
                (
                    T0 + 7 * 3_600_000 - 59_999 + j as i64 * 234,
                    v(j),
                    true,
                    65_535,
                )
            })
            .collect(),
        long: (0..LONG_BUCKETS)
            .map(|j| {
                let lk =
                    (T0 + 7 * 3_600_000).div_euclid(LONG_BUCKET_MS) - (LONG_BUCKETS - 1 - j) as i64;
                (lk, agg(j, 600_000, 2_400))
            })
            .collect(),
        folded_below: open_k,
    }
}

/// Record §9, with A4's rings (N3): a base with each of its [`STRIP_RINGS`] rings at its
/// fullest (the instruments' four among them), its numbers at their widest, the most gaps,
/// events and situation spans a strip keeps, each span's words the longest, and every usual
/// clock taught, is under 85 % of [`STRIP_MAX_BYTES`], and reads back whole.
/// CONTROL: the bound is what a read refuses past.
#[test]
fn a_full_strip_base_fits_within_85_percent_of_its_cap() {
    let mut full = SavedStrip {
        last_wall_ms: T0 + 7 * 3_600_000,
        through_k: (T0 + 7 * 3_600_000).div_euclid(BUCKET_MS) - 1,
        clean_stop: true,
        caps: [Some(1.234_567_890_123_456_7e15); 7],
        proc_starts: Some(true),
        time_gaps: (0..STRIP_HISTORY_CAP as i64)
            .map(|j| TimeGap {
                start_ms: T0 + j * 100_000,
                end_ms: T0 + j * 100_000 + 99_999,
                kind: GapKind::Paused,
            })
            .collect(),
        strip_events: (0..STRIP_HISTORY_CAP as i64)
            .map(|j| StripEvent {
                scope: aterm_vitals::events::EventScope::Session(
                    aterm_vitals::events::SessionScope {
                        local: u64::MAX,
                        sid: "s-00000000000000000000".into(),
                        nonce: "ff".repeat(16),
                    },
                ),
                span: Some(aterm_vitals::events::EventSpan {
                    onset_unix_ms: i64::MIN,
                    end_unix_ms: i64::MAX,
                }),
                at_unix_ms: T0 + j * 100_000,
                kind: StripEventKind::PressureSpike,
                label: "memory pressure critical".into(),
            })
            .collect(),
        situation: (0..STRIP_HISTORY_CAP as i64)
            .map(|j| Span {
                start_ms: T0 + j * 100_000,
                end_ms: T0 + j * 100_000 + 99_999,
                stay: Stay {
                    place: Some(PlaceKind::Cellular),
                    power: Some(PowerSource::Battery),
                    lid: Some(Lid::Closed),
                },
            })
            .collect(),
        usual: UsualClocks::restored(
            &(0..10_u8)
                .map(|d| {
                    (
                        d,
                        123_456.789_012_345_67 + f64::from(d),
                        4_464.123_456_789_012 - f64::from(d),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        ..SavedStrip::default()
    };
    assert_eq!(
        STRIP_RINGS, 24,
        "the rows' 17, the scope's 3 and the instruments' 4"
    );
    for slot in 0..STRIP_RINGS {
        *full.ring_mut(slot).expect("a slot") = fullest_ring(slot as f64);
    }
    assert!(full.ring_mut(STRIP_RINGS).is_none());
    assert_eq!(full.usual.saved().len(), 10);
    let text = base_json(&full, u64::MAX).expect("text");
    eprintln!(
        "a full base is {} bytes, {:.1} % of its bound",
        text.len(),
        text.len() as f64 * 100.0 / STRIP_MAX_BYTES as f64
    );
    assert!(
        text.len() as u64 * 100 <= STRIP_MAX_BYTES * 85,
        "{} bytes is over 85 % of the {STRIP_MAX_BYTES}-byte bound",
        text.len()
    );
    assert_eq!(
        base_of(&text),
        Some((u64::MAX, full.clone())),
        "it reads back whole"
    );

    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let folder = strip_dir(dir.path());
    std::fs::create_dir_all(&folder).expect("the folder");
    std::fs::write(folder.join(BASE_FILE), &text).expect("the base");
    assert_eq!(store_in(dir.path()).1, full);
    let over = format!(
        "{text}{}",
        " ".repeat(STRIP_MAX_BYTES as usize - text.len() + 1)
    );
    std::fs::write(folder.join(BASE_FILE), over).expect("the base past its bound");
    assert_eq!(
        store_in(dir.path()).1,
        SavedStrip::default(),
        "CONTROL: past the bound a base restores nothing"
    );
}

/// Record §9: between bases each reading past an epoch appends that epoch's line, and a restore
/// gives back the base with its tail replayed: every ring's closed 10 s buckets and its
/// ten-minute ones as the live model holds them. CONTROL: the base alone holds only what closed
/// before it.
#[test]
fn a_strip_restores_its_base_and_replays_its_tail() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..95);
    let tail = tail_in(dir.path());
    assert_eq!(
        tail.lines().count(),
        9,
        "a line per epoch closed since the base"
    );
    let through = model.strip_open_k().expect("read") - 1;
    let (_, kept) = store_in(dir.path());
    let live = model.saved_strip(false);
    assert_eq!(kept.through_k, through);
    assert_eq!(closed(&kept, through), closed(&live, through));
    assert_eq!(longs(&kept), longs(&live), "the ten-minute buckets too");
    assert_eq!(kept.proc_starts, Some(true));
    assert!(
        closed(&kept, through)
            .iter()
            .filter(|r| !r.is_empty())
            .count()
            >= 12,
        "most rings read: {:?}",
        closed(&kept, through)
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>()
    );

    std::fs::remove_file(strip_dir(dir.path()).join(TAIL_FILE)).expect("the tail");
    let (_, base) = store_in(dir.path());
    assert_eq!(
        base.through_k,
        T0.div_euclid(BUCKET_MS) - 1,
        "CONTROL: the base holds no closed epoch"
    );
    assert_ne!(closed(&base, through), closed(&live, through));
}

/// Record §9: a restore replays the tail up to its first line that does not parse, and a last
/// line with no newline was cut short and is not replayed. CONTROL: the whole tail replays.
#[test]
fn a_tail_replay_stops_at_its_first_unparsable_line() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..45);
    let path = strip_dir(dir.path()).join(TAIL_FILE);
    let tail = tail_in(dir.path());
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 4);
    let first_k = T0.div_euclid(BUCKET_MS);
    let through = |text: String| {
        std::fs::write(&path, text).expect("the tail");
        store_in(dir.path()).1.through_k
    };
    assert_eq!(through(tail.clone()), first_k + 3, "CONTROL: every line");
    let damaged = format!("{}\n{}\n{{\"g\":\n{}\n", lines[0], lines[1], lines[3]);
    assert_eq!(through(damaged), first_k + 1, "up to the damaged line");
    let short = format!("{}\n{}", lines[0], &lines[1][..lines[1].len() / 2]);
    assert_eq!(through(short), first_k, "a line cut short is not replayed");
    let whole_but_last = format!("{}\n{}\n{}", lines[0], lines[1], lines[2]);
    assert_eq!(
        through(whole_but_last),
        first_k + 1,
        "a whole line with no newline after it may have been cut at its end"
    );
}

/// Record §9: a base written empties the tail, and the lines after it are of the base's
/// generation; an append after a short write cuts the tail back to its last whole line first.
/// CONTROL: without a base written the tail keeps growing.
#[test]
fn a_base_rewrite_truncates_the_tail() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..35);
    let grown = tail_in(dir.path()).len();
    assert!(grown > 0, "CONTROL: lines appended");
    assert!(store.put_base(
        &model.saved_strip(false),
        model.strip_rewrites(),
        Instant::now()
    ));
    assert_eq!(tail_in(dir.path()), "", "the base empties the tail");
    read_kept(&mut model, &mut store, 35..45);
    let after: Vec<String> = tail_in(dir.path()).lines().map(str::to_owned).collect();
    assert_eq!(after.len(), 1);
    assert!(
        after[0].starts_with("{\"g\":2,"),
        "the second base's: {}",
        after[0]
    );

    let path = strip_dir(dir.path()).join(TAIL_FILE);
    let mut text = tail_in(dir.path());
    text.push_str("{\"g\":2,\"k\":");
    std::fs::write(&path, &text).expect("a short write's leftover");
    store.tail_dirty = true;
    read_kept(&mut model, &mut store, 45..55);
    let healed = tail_in(dir.path());
    assert_eq!(healed.lines().count(), 2, "{healed}");
    assert!(healed.ends_with('\n'));
    let (_, kept) = store_in(dir.path());
    assert_eq!(kept.through_k, model.strip_open_k().expect("read") - 1);
}

/// Record §9, as amended: a crash between a base's rename and the tail's truncation leaves the
/// old generation's lines after the new base, and a restore replays none of them, even those past
/// the new base's last epoch (a base rewritten after a backward clock step re-keyed the rings).
/// CONTROL: the same lines carrying the new base's generation would be counted again.
#[test]
fn a_crash_between_base_rename_and_tail_truncate_counts_nothing_twice() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..65);
    let stale = tail_in(dir.path());
    assert_eq!(stale.lines().count(), 6);
    let mut rekeyed = model.saved_strip(false);
    let back_to = rekeyed.through_k - 3;
    for slot in 0..STRIP_RINGS {
        let ring = rekeyed.ring_mut(slot).expect("a slot");
        ring.buckets.retain(|&(k, _)| k <= back_to);
    }
    rekeyed.through_k = back_to;
    assert!(store.put_base(&rekeyed, model.strip_rewrites(), Instant::now()));
    let path = strip_dir(dir.path()).join(TAIL_FILE);
    std::fs::write(&path, &stale).expect("the tail the crash left");
    let (_, kept) = store_in(dir.path());
    assert_eq!(kept, rekeyed, "the old generation's lines replay nothing");

    std::fs::write(&path, stale.replace("{\"g\":1,", "{\"g\":2,")).expect("relabelled");
    let (_, counted) = store_in(dir.path());
    assert_eq!(
        counted.through_k,
        back_to + 3,
        "CONTROL: lines of the base's generation past it replay"
    );
}

/// The generation of the base on disk in the state folder `home`, where one parses.
fn base_generation(home: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(strip_dir(home).join(BASE_FILE)).ok()?;
    base_of(&text).map(|(generation, _)| generation)
}

/// The tail's lines in the state folder `home`, parsed.
fn tail_lines(home: &Path) -> Vec<(u64, StripLine)> {
    tail_in(home).lines().filter_map(super::line_of).collect()
}

/// `model` after reading `s`, kept by `store` at `at`.
fn keep_at(model: &mut aterm_vitals::Vitals, store: &mut StripStore, s: &Sample, at: Instant) {
    let _ = model.absorb(s);
    store.keep(model, at, &mut StripLock::Unlocked);
}

/// Reading `i` taken after the wall clock stepped `back_ms` back.
fn stepped(i: u64, back_ms: i64) -> Sample {
    let s = reading(i);
    Sample {
        at_unix_ms: s.at_unix_ms - back_ms,
        ..s
    }
}

/// Record §9, as amended: a reading more than `CLAMP_BACK_S` behind the newest re-keys the
/// rings, which moves `Vitals::strip_rewrites`, so the store writes a whole base at it, and again
/// at the reading after, where aterm's rings, which read no rate across the step, re-key; the
/// tail resumes from the newest base, and a restore gives back what the live model holds, slot by
/// slot. CONTROL: the lines after the step replayed onto the base before it replay nothing, since
/// their epochs stand at or before its last, which is why the base is written.
#[test]
fn a_backward_clock_step_writes_a_whole_base_and_the_tail_resumes_from_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let home = dir.path();
    let (mut store, _) = store_in(home);
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..35);
    assert_eq!(base_generation(home), Some(1));
    assert_eq!(tail_lines(home).len(), 3);
    let (_, before) = store_in(home);
    let rewrites = model.strip_rewrites();
    let t0 = Instant::now();
    let _ = model.absorb(&stepped(35, 120_000));
    assert_eq!(model.strip_rewrites(), rewrites + 1, "the re-key moved it");
    store.keep(&model, t0, &mut StripLock::Unlocked);
    assert_eq!(base_generation(home), Some(2), "a whole base at the step");
    assert_eq!(tail_in(home), "", "its tail emptied");
    for i in 36..60 {
        let _ = model.absorb(&stepped(i, 120_000));
        store.keep(
            &model,
            t0 + Duration::from_secs(i - 35),
            &mut StripLock::Unlocked,
        );
    }
    // aterm's rings, which read no rate across the step, re-key at the reading after it, and
    // so write the base again.
    assert_eq!(
        base_generation(home),
        Some(3),
        "a base at each ring's re-key"
    );
    let lines = tail_lines(home);
    assert!(
        lines.len() >= 2 && lines.iter().all(|(g, _)| *g == 3),
        "the tail resumes from the newest base: {lines:?}"
    );
    let through = model.strip_open_k().expect("read") - 1;
    let (_, kept) = store_in(home);
    let live = model.saved_strip(false);
    assert_eq!(kept.through_k, through);
    assert_eq!(closed(&kept, through), closed(&live, through));
    assert_eq!(longs(&kept), longs(&live));

    let mut onto_old = before.clone();
    for (_, line) in lines {
        assert!(
            !onto_old.replay(line),
            "CONTROL: a line behind the old base"
        );
    }
    assert_eq!(onto_old, before);
}

/// Record §8.1 and §9, as amended: a Processes row whose meaning moves (the start counter first
/// reads, so the row turns from a count to starts a second) clears its rings and moves
/// `Vitals::strip_rewrites`, so the store writes a whole base at it, and a restore's Processes
/// ring holds starts alone. CONTROL: the lines after it replayed onto the base before it mix the
/// count's buckets with the starts'.
#[test]
fn a_processes_meaning_change_writes_a_whole_base() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let home = dir.path();
    let (mut store, _) = store_in(home);
    let mut model = aterm_vitals::Vitals::new();
    let t0 = Instant::now();
    let counted = |i: u64| Sample {
        starts: None,
        ..reading(i)
    };
    for i in 0..25 {
        let _ = model.absorb(&counted(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
    }
    assert_eq!(base_generation(home), Some(1));
    let (_, before) = store_in(home);
    assert_eq!(before.proc_starts, Some(false), "a count until now");
    let processes = aterm_vitals::model::Resource::Processes.index();
    let rewrites = model.strip_rewrites();
    let _ = model.absorb(&reading(25));
    assert_eq!(model.strip_rewrites(), rewrites + 1, "the meaning moved it");
    store.keep(
        &model,
        t0 + Duration::from_secs(25),
        &mut StripLock::Unlocked,
    );
    assert_eq!(base_generation(home), Some(2), "a whole base at the change");
    read_kept(&mut model, &mut store, 26..45);
    let lines: Vec<StripLine> = tail_lines(home).into_iter().map(|(_, l)| l).collect();
    assert!(lines.len() >= 2, "{lines:?}");
    let (_, kept) = store_in(home);
    assert_eq!(kept.proc_starts, Some(true));
    let held: Vec<f64> = kept.rings[processes]
        .buckets
        .iter()
        .map(|(_, a)| a.max)
        .collect();
    assert!(
        !held.is_empty() && held.iter().all(|&m| m < 100.0),
        "starts a second alone, no count of ~900: {held:?}"
    );
    let through = model.strip_open_k().expect("read") - 1;
    assert_eq!(
        closed(&kept, through),
        closed(&model.saved_strip(false), through)
    );

    let mut mixed = before;
    for line in lines {
        mixed.replay(line);
    }
    let maxima: Vec<f64> = mixed.rings[processes]
        .buckets
        .iter()
        .map(|(_, a)| a.max)
        .collect();
    assert!(
        maxima.iter().any(|&m| m > 800.0) && maxima.iter().any(|&m| m < 100.0),
        "CONTROL: lines on the count's base mix counts and starts: {maxima:?}"
    );
}

/// Record §9, as amended: while an hourly base cannot be written (here its path is a folder, as
/// a full disk refuses one), each closed epoch still appends its line onto the base on disk, so a
/// restore loses none of them, and no append takes the tail past `TAIL_MAX_BYTES`. A base due
/// because the held buckets changed (here the Processes row's meaning moved) appends nothing
/// while it cannot be written, since its lines would land on a base the model no longer holds:
/// each half is the other's control.
#[test]
fn a_failing_hourly_base_keeps_the_tail_and_a_rewrite_stops_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let home = dir.path();
    let (mut store, _) = store_in(home);
    let mut model = aterm_vitals::Vitals::new();
    let t0 = Instant::now();
    let counted = |i: u64| Sample {
        starts: None,
        ..reading(i)
    };
    for i in 0..15 {
        keep_at(
            &mut model,
            &mut store,
            &counted(i),
            t0 + Duration::from_secs(i),
        );
    }
    let base = strip_dir(home).join(BASE_FILE);
    let good = std::fs::read_to_string(&base).expect("the first base");
    std::fs::remove_file(&base).expect("the base");
    std::fs::create_dir(&base).expect("a folder where the base goes");
    std::fs::write(base.join("held"), "x").expect("so no rename replaces it");
    let hour = t0 + super::BASE_EVERY;
    for i in 15..45 {
        keep_at(
            &mut model,
            &mut store,
            &counted(i),
            hour + Duration::from_secs(i),
        );
    }
    let lines = tail_lines(home);
    assert_eq!(lines.len(), 4, "epochs 0 to 3, each a line: {lines:?}");
    assert!(lines.iter().all(|(g, _)| *g == 1));
    let through = model.strip_open_k().expect("read") - 1;
    std::fs::remove_dir_all(&base).expect("the folder");
    std::fs::write(&base, &good).expect("the base back");
    let (_, kept) = store_in(home);
    assert_eq!(kept.through_k, through, "every closed epoch restores");
    assert_eq!(
        closed(&kept, through),
        closed(&model.saved_strip(false), through)
    );
    std::fs::remove_file(&base).expect("the base");
    std::fs::create_dir(&base).expect("a folder where the base goes");
    std::fs::write(base.join("held"), "x").expect("so no rename replaces it");

    let tail_before = tail_in(home);
    let ok = store.tail_ok;
    store.tail_ok = super::TAIL_MAX_BYTES - 1;
    for i in 45..55 {
        keep_at(
            &mut model,
            &mut store,
            &counted(i),
            hour + Duration::from_secs(i),
        );
    }
    assert_eq!(
        tail_in(home),
        tail_before,
        "no append past the tail's bound"
    );
    store.tail_ok = ok;

    let rewrites = model.strip_rewrites();
    for i in 55..85 {
        keep_at(
            &mut model,
            &mut store,
            &reading(i),
            hour + Duration::from_secs(i),
        );
    }
    assert!(model.strip_rewrites() > rewrites, "the meaning moved");
    assert_eq!(
        tail_in(home),
        tail_before,
        "a rewrite's base, unwritten, stops the tail"
    );
}

/// Record §9: each line carries the gaps and events opened since the line before, so a restore
/// keeps those its base never held. Network places come with the Network row's own step.
/// CONTROL: without the tail they are gone.
#[test]
fn a_restored_strip_keeps_the_gaps_events_and_places_of_its_tail() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    read_kept(&mut model, &mut store, 0..12);
    let gap = TimeGap {
        start_ms: T0 + 12_000,
        end_ms: T0 + 12_400,
        kind: GapKind::Sleep,
    };
    let event = StripEvent {
        scope: aterm_vitals::events::EventScope::Machine,
        span: None,
        at_unix_ms: T0 + 12_400,
        kind: StripEventKind::SleepWake,
        label: "wake".into(),
    };
    let t0 = Instant::now();
    let _ = model.absorb(&Sample {
        note_gaps: vec![gap],
        note_events: vec![event.clone()],
        ..reading(12)
    });
    store.keep(&model, t0, &mut StripLock::Unlocked);
    read_kept(&mut model, &mut store, 13..25);
    let tail = tail_in(dir.path());
    assert_eq!(
        tail.lines().filter(|l| l.contains("\"wake\"")).count(),
        1,
        "carried once: {tail}"
    );
    let (_, kept) = store_in(dir.path());
    assert!(kept.time_gaps.contains(&gap), "{:?}", kept.time_gaps);
    assert!(
        kept.strip_events.contains(&event),
        "{:?}",
        kept.strip_events
    );

    std::fs::remove_file(strip_dir(dir.path()).join(TAIL_FILE)).expect("the tail");
    let (_, base) = store_in(dir.path());
    assert!(!base.time_gaps.contains(&gap), "CONTROL");
    assert!(!base.strip_events.contains(&event), "CONTROL");
}

/// Record §9, with A4's rings (N3): an hour of readings a second, the rows' rings and the
/// instruments' four fed, each line with the stay's span and the usual clock the reading
/// taught, writes under 2 MB to the strip's files: a base at the start and at the hour, and a
/// line per epoch between. CONTROL: a whole base every 10 s, the schema-1 checkpoint's rule,
/// would write far more.
#[test]
fn the_strip_writes_under_two_megabytes_an_hour() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    let t0 = Instant::now();
    let mut whole_every_10s = 0_u64;
    // The rows' readings with the heat chain's and the exit counter's beside them.
    let every_ring = |i: u64| {
        let heat = instrumented(i);
        Sample {
            cores: heat.cores,
            clusters: heat.clusters,
            starts: heat.starts,
            coal_lane: heat.coal_lane,
            clock_range: heat.clock_range,
            cpu_brand: heat.cpu_brand,
            ..reading(i)
        }
    };
    let mut a_line = None;
    for i in 0..=3_600 {
        let _ = model.absorb(&every_ring(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
        if i == 3_595 {
            a_line = tail_in(dir.path()).lines().last().map(str::to_owned);
        }
        if i % 10 == 0 {
            whole_every_10s +=
                base_json(&model.saved_strip(false), 1).map_or(0, |t| t.len() as u64);
        }
    }
    let written = store.bytes_written();
    eprintln!(
        "an hour wrote {written} bytes; a whole base every 10 s would write {whole_every_10s}"
    );
    let held = model.saved_strip(false);
    for slot in 20..STRIP_RINGS {
        assert!(
            held.ring(slot).is_some_and(|r| r.buckets.len() >= 359),
            "slot {slot} read the hour"
        );
    }
    assert!(!held.situation.is_empty() && !held.usual.saved().is_empty());
    let line = a_line.expect("the hour's last line, before the hourly base emptied the tail");
    assert!(
        line.contains("\"s\":[[") && line.contains("\"u\":[["),
        "{line}"
    );
    assert!(written < 2_000_000, "{written} bytes in an hour");
    assert!(
        whole_every_10s > 10 * written,
        "CONTROL: {whole_every_10s} bytes rewriting it whole"
    );
}

/// A tail line round-trips with its generation, and one with an unknown slot is kept parsed
/// but changes nothing on replay.
#[test]
fn a_tail_line_reads_back_as_written() {
    let line = StripLine {
        k: 179_300_000,
        wall_ms: T0 + 9_500,
        caps: [Some(8.0), None, None, None, None, None, None],
        rings: vec![(
            0,
            Agg {
                tau_ms: 10_000,
                n: 10,
                over_ms: 0,
                mean: 0.25,
                min: 0.125,
                max: 0.5,
            },
        )],
        proc_starts: Some(true),
        scope_cpu_accounting: Some(aterm_vitals::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
        time_gaps: Vec::new(),
        strip_events: Vec::new(),
        spans: vec![
            Span {
                start_ms: T0,
                end_ms: T0 + 4_500,
                stay: Stay {
                    place: Some(PlaceKind::Vpn),
                    power: Some(PowerSource::Ac),
                    lid: Some(Lid::None),
                },
            },
            Span {
                start_ms: T0 + 4_500,
                end_ms: T0 + 9_500,
                stay: Stay {
                    place: None,
                    power: Some(PowerSource::Ups),
                    lid: None,
                },
            },
        ],
        usual: vec![(3, 12.5, 2_812.25), (9, 61.0, 4_011.5)],
    };
    let text = line_json(&line, 7).expect("text");
    assert_eq!(line_of(&text), Some((7, line.clone())));
    let mut strip = SavedStrip {
        last_wall_ms: T0,
        through_k: 179_299_999,
        ..SavedStrip::default()
    };
    let mut odd = line;
    odd.rings.push((u8::MAX, odd.rings[0].1));
    assert!(strip.replay(odd));
    assert_eq!(strip.rings[0].buckets.len(), 1);
}

/// The tail's bound holds what its constant says (N3): four hours of lines, each with every
/// ring's bucket at its widest, the stay's span moved and every usual clock taught, fit
/// [`TAIL_MAX_BYTES`], and such a line reads back as written.
/// CONTROL: five hours of them do not, so the bound is no looser than it says.
#[test]
fn four_hours_of_the_fullest_lines_fit_the_tails_bound() {
    let v = |slot: usize| -(123_456_789.012_345_67 + slot as f64);
    let line = StripLine {
        k: (T0 + 7 * 3_600_000).div_euclid(BUCKET_MS),
        wall_ms: T0 + 7 * 3_600_000,
        caps: [Some(1.234_567_890_123_456_7e15); 7],
        rings: (0..STRIP_RINGS)
            .map(|slot| {
                (
                    slot as u8,
                    Agg {
                        tau_ms: 10_000,
                        n: u16::MAX,
                        over_ms: 10_000,
                        mean: v(slot),
                        min: v(slot) - 1.000_000_000_000_1,
                        max: v(slot) + 1.000_000_000_000_1,
                    },
                )
            })
            .collect(),
        proc_starts: Some(true),
        scope_cpu_accounting: Some(aterm_vitals::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
        time_gaps: Vec::new(),
        strip_events: Vec::new(),
        spans: vec![Span {
            start_ms: T0 + 6 * 3_600_000,
            end_ms: T0 + 7 * 3_600_000,
            stay: Stay {
                place: Some(PlaceKind::Cellular),
                power: Some(PowerSource::Battery),
                lid: Some(Lid::Closed),
            },
        }],
        usual: (0..10_u8)
            .map(|d| {
                (
                    d,
                    123_456.789_012_345_67 + f64::from(d),
                    4_464.123_456_789_012 - f64::from(d),
                )
            })
            .collect(),
    };
    let text = line_json(&line, u64::MAX).expect("text");
    assert_eq!(line_of(&text), Some((u64::MAX, line)));
    let bytes = text.len() as u64 + 1;
    let lines_an_hour = 3_600_000 / BUCKET_MS as u64;
    eprintln!("the fullest line is {bytes} bytes");
    assert!(
        4 * lines_an_hour * bytes <= TAIL_MAX_BYTES,
        "four hours of {bytes}-byte lines pass the {TAIL_MAX_BYTES}-byte bound"
    );
    assert!(5 * lines_an_hour * bytes > TAIL_MAX_BYTES, "CONTROL");
}

/// A base as schema 3 wrote it (20 rings: the rows' 17, the scope's coverage and its raw CPU
/// charge) reads with each of those rings as written, and with the instruments' rings, the
/// situation spans and the usual clocks empty: nothing is made up for what it never held.
/// CONTROL: the same 20 rings labelled with the current schema are too few, and restore nothing.
#[test]
fn a_schema_three_base_keeps_its_rings_and_starts_the_instruments_empty() {
    const SCHEMA_3_RINGS: usize = 20;
    let mut saved = SavedStrip {
        last_wall_ms: T0 + 7 * 3_600_000,
        through_k: (T0 + 7 * 3_600_000).div_euclid(BUCKET_MS) - 1,
        ..SavedStrip::default()
    };
    for slot in 0..SCHEMA_3_RINGS {
        *saved.ring_mut(slot).expect("a slot") = fullest_ring(slot as f64);
    }
    let mut document: super::BaseDoc =
        aterm_json::from_str(&base_json(&saved, 5).expect("text")).expect("a base");
    assert_eq!(document.rings.len(), STRIP_RINGS);
    document.schema = 3;
    document.rings.truncate(SCHEMA_3_RINGS);
    let (generation, back) =
        base_of(&aterm_json::to_string(&document).expect("text")).expect("schema 3 reads");
    assert_eq!(generation, 5);
    for slot in 0..SCHEMA_3_RINGS {
        assert_eq!(back.ring(slot), saved.ring(slot), "slot {slot}");
        assert!(back.ring(slot).is_some_and(|r| !r.is_empty()));
    }
    for slot in SCHEMA_3_RINGS..STRIP_RINGS {
        assert!(
            back.ring(slot).is_some_and(SavedRing::is_empty),
            "instrument slot {slot} starts empty"
        );
    }
    assert!(back.situation.is_empty());
    assert_eq!(back.usual, UsualClocks::default());

    document.schema = STRIP_SCHEMA;
    assert_eq!(
        base_of(&aterm_json::to_string(&document).expect("text")),
        None,
        "CONTROL: the current schema holds {STRIP_RINGS} rings"
    );
}

/// A situation span reads back with its words (netplace's kind, the power source, the lid), a
/// value no reader gave as an empty word.
/// CONTROL: a span that names a word nobody says (`checking` is no kind a span keeps), ends
/// before it starts, or holds no value at all is dropped, and the line's other spans stand.
#[test]
fn a_span_reads_back_by_its_words_and_a_damaged_one_is_dropped() {
    let spans: Vec<Span> = [
        (Some(PlaceKind::Lan), Some(PowerSource::Ac), Some(Lid::Open)),
        (
            Some(PlaceKind::Hotspot),
            Some(PowerSource::Battery),
            Some(Lid::Closed),
        ),
        (
            Some(PlaceKind::Cellular),
            Some(PowerSource::Ups),
            Some(Lid::None),
        ),
        (Some(PlaceKind::Vpn), None, None),
        (Some(PlaceKind::Offline), None, Some(Lid::Open)),
        (None, Some(PowerSource::Ac), None),
    ]
    .into_iter()
    .zip(0_i64..)
    .map(|((place, power, lid), j)| Span {
        start_ms: T0 + j * 1_000,
        end_ms: T0 + j * 1_000 + 1_000,
        stay: Stay { place, power, lid },
    })
    .collect();
    let line = StripLine {
        k: 179_300_000,
        wall_ms: T0 + 9_500,
        spans: spans.clone(),
        ..StripLine::default()
    };
    let text = line_json(&line, 2).expect("text");
    for word in [
        "lan", "hotspot", "cellular", "vpn", "offline", "battery", "ups", "closed",
    ] {
        assert!(text.contains(&format!("\"{word}\"")), "{word}: {text}");
    }
    assert_eq!(line_of(&text).map(|(_, l)| l.spans), Some(spans));

    let damaged = format!(
        r#"{{"g":2,"k":179300000,"w":{w},"c":[],"r":[],"s":[[{t},{t},"checking","ac","open"],[{t},{t},"lan","mains","open"],[{t},{t},"lan","ac","ajar"],[{t},{before},"lan","ac","open"],[{t},{t},"","",""],[{t},{t},"vpn","",""]]}}"#,
        w = T0 + 9_500,
        t = T0,
        before = T0 - 1,
    );
    assert_eq!(
        line_of(&damaged).map(|(_, l)| l.spans),
        Some(vec![Span {
            start_ms: T0,
            end_ms: T0,
            stay: Stay {
                place: Some(PlaceKind::Vpn),
                power: None,
                lid: None,
            },
        }])
    );
}

/// Each slot's 10 s bucket of epoch `k`.
fn bucket_at(strip: &SavedStrip, slot: usize, k: i64) -> Option<Agg> {
    strip
        .ring(slot)
        .and_then(|r| r.buckets.iter().find(|(bk, _)| *bk == k))
        .map(|(_, a)| *a)
}

/// A4's rings across a relaunch (N3): compute watts, the thermal pressure level with each box's
/// share at Moderate or more and its worst level, process exits a second and the blockage share
/// with its worst kind come back from the base and its tail as the live model holds them, the
/// situation spans with them, and the model that restores them draws the same boxes. The run
/// ends with no stop's write, as a crash ends it: the tail alone carries what closed since the
/// first base.
/// CONTROL: without the tail the base written at the first reading holds none of the four
/// rings' closed buckets and no span past its own reading; and a model that restores nothing
/// holds none.
#[test]
fn the_instruments_rings_restore_after_a_relaunch() {
    const INSTRUMENTS: std::ops::Range<usize> = 20..STRIP_RINGS;
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    let t0 = Instant::now();
    // Reading 90 is the first past epoch 8's end: the last line is written at it.
    let mut at_last_line = None;
    for i in 0..95 {
        let _ = model.absorb(&instrumented(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
        if i == 90 {
            at_last_line = Some((model.strip_spans().to_vec(), *model.usual_clocks()));
        }
    }
    let (spans, usual) = at_last_line.expect("reading 90");
    let live = model.saved_strip(false);
    let through = model.strip_open_k().expect("read") - 1;
    let k0 = T0.div_euclid(BUCKET_MS);
    assert_eq!(through, k0 + 8);

    // What the live model holds, so the restore below is of something: 8.5 W of compute (7 W of
    // CPU, and the GPU's 1.6 W eight readings in ten and 1.1 W two); the pressure box of readings 20 to 29 (reading 24 at Moderate: a second of
    // ten); 30 exits a second; the zombie counted from its tenth second, a steady pile.
    let watts = bucket_at(&live, 20, k0 + 2).expect("compute watts");
    assert!((watts.mean - 8.5).abs() < 1e-9, "{watts:?}");
    let pressed = PressureBox::of(&bucket_at(&live, 21, k0 + 2).expect("pressure")).expect("read");
    assert!((pressed.share - 0.1).abs() < 1e-9, "{pressed:?}");
    assert_eq!(pressed.worst, ThermalPressure::Moderate);
    assert_eq!(bucket_at(&live, 22, k0 + 2).map(|a| a.mean), Some(30.0));
    assert_eq!(
        bucket_at(&live, 23, k0 + 2)
            .as_ref()
            .and_then(BlockageBox::of),
        Some(BlockageBox {
            share: 1.0,
            worst: Some(Blockage::SteadyPile)
        })
    );
    assert_eq!(
        bucket_at(&live, 23, k0).as_ref().and_then(BlockageBox::of),
        Some(BlockageBox {
            share: 0.0,
            worst: None
        }),
        "the zombie's first ten seconds read clear"
    );
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.stay.place, s.stay.power, s.start_ms - T0, s.end_ms - T0))
            .collect::<Vec<_>>(),
        [
            (Some(PlaceKind::Lan), Some(PowerSource::Ac), 500, 39_500),
            (
                Some(PlaceKind::Hotspot),
                Some(PowerSource::Battery),
                39_500,
                90_500
            ),
        ]
    );

    // The files: the first reading's base with nine lines replayed.
    assert_eq!(tail_in(dir.path()).lines().count(), 9);
    let (_, kept) = store_in(dir.path());
    assert_eq!(kept.through_k, through);
    for slot in INSTRUMENTS {
        let (file, held) = (&closed(&kept, through)[slot], &closed(&live, through)[slot]);
        assert_eq!(file, held, "slot {slot}");
        assert!(file.len() >= 8, "slot {slot} read {} epochs", file.len());
    }
    assert_eq!(longs(&kept)[INSTRUMENTS], longs(&live)[INSTRUMENTS]);
    assert_eq!(
        kept.situation, spans,
        "the spans as the last line left them"
    );
    assert_eq!(kept.usual, usual);

    // The relaunch.
    let mut relaunched = aterm_vitals::Vitals::new();
    relaunched.restore_strip_history(kept, T0 + 120_000);
    let back = relaunched.saved_strip(false);
    for slot in INSTRUMENTS {
        assert_eq!(
            closed(&back, through)[slot],
            closed(&live, through)[slot],
            "slot {slot}"
        );
    }
    assert_eq!(
        bucket_at(&back, 21, k0 + 2)
            .as_ref()
            .and_then(PressureBox::of),
        Some(pressed)
    );
    assert_eq!(relaunched.strip_spans(), spans);
    assert_eq!(*relaunched.usual_clocks(), usual);

    // A base written whole holds them with no tail, the open epoch and the raw readings too.
    store.stop(
        &model,
        true,
        t0 + Duration::from_secs(95),
        &mut StripLock::Unlocked,
    );
    assert_eq!(tail_in(dir.path()), "");
    let (_, whole) = store_in(dir.path());
    assert_eq!(whole, model.saved_strip(true));
    assert_eq!(whole.situation, model.strip_spans());

    // CONTROL: the first reading's base alone.
    let alone = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(alone.path());
    let mut model = aterm_vitals::Vitals::new();
    for i in 0..95 {
        let _ = model.absorb(&instrumented(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
    }
    std::fs::remove_file(strip_dir(alone.path()).join(TAIL_FILE)).expect("the tail");
    let (_, base) = store_in(alone.path());
    assert_eq!(base.through_k, k0 - 1, "the base holds no closed epoch");
    for slot in INSTRUMENTS {
        let held = &closed(&base, through)[slot];
        assert!(
            held.iter().all(|&(k, _)| k == k0),
            "slot {slot} holds its open epoch at most: {held:?}"
        );
        assert_ne!(*held, closed(&live, through)[slot], "slot {slot}");
    }
    assert_eq!(
        base.situation
            .iter()
            .map(|s| (s.start_ms - T0, s.end_ms - T0))
            .collect::<Vec<_>>(),
        [(500, 500)]
    );
    assert_eq!(base.usual, UsualClocks::default());
    // CONTROL: nothing restored.
    let fresh = aterm_vitals::Vitals::new().saved_strip(false);
    for slot in INSTRUMENTS {
        assert_eq!(closed(&fresh, through)[slot], Vec::new());
    }
}

/// The heat chain's peaks and usual clocks across a relaunch (N3): the compute watts peak and
/// the work-per-joule peak come back from the peaks file, the usual Super clock of each decile
/// from the strip's checkpoint (here its tail: the run ends with no stop's write of the strip),
/// and the relaunched model holds each as the run before learned it.
/// CONTROL: a relaunch with neither file has learned none; a peaks file written before these
/// two rows reads with the peaks it holds and learns these from zero; and usual clocks the
/// first base never held are gone without the tail.
#[test]
fn the_heat_peaks_and_usual_clocks_restore_after_a_relaunch() {
    use crate::vitals_host::{PEAKS_FILE, PeakFile, peaks_of};
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let peaks_path = dir.path().join(PEAKS_FILE);
    let t0 = Instant::now();
    let (mut peaks_file, none) = PeakFile::open(Some(peaks_path.clone()), t0);
    assert_eq!(none, aterm_vitals::Peaks::default());
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    for i in 0..95 {
        let _ = model.absorb(&instrumented(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
    }
    let today = u32::try_from(T0.div_euclid(86_400_000)).expect("a day");
    let learned = model.peaks().clone();
    let watts = learned.compute_w.peak(today).expect("a watts peak");
    assert!((watts - 8.5).abs() < 1e-9, "{watts}");
    // 47e9 instructions a second on the CPU's 7 W.
    let work = learned.work_per_joule.peak(today).expect("a work peak");
    assert!((work - 47e9 / 7.0).abs() < 1.0, "{work}");
    assert!(learned.compute_w.read_s >= 30 && learned.work_per_joule.read_s >= 30);
    let usual = *model.usual_clocks();
    assert!(
        usual
            .usual(9)
            .is_some_and(|mhz| (mhz - 4000.0).abs() < 0.01),
        "four Super cores busy throughout teach decile 9: {usual:?}"
    );
    // The peaks file is written at the thread's stop; the strip is left as a crash leaves it.
    peaks_file.keep(&learned, t0 + Duration::from_secs(95), true);

    let (_, peaks) = PeakFile::open(Some(peaks_path.clone()), Instant::now());
    assert_eq!(peaks, learned);
    let (_, kept) = store_in(dir.path());
    let mut relaunched = aterm_vitals::Vitals::new();
    relaunched.restore_peaks(peaks);
    relaunched.restore_strip_history(kept, T0 + 120_000);
    assert_eq!(relaunched.peaks().compute_w.peak(today), Some(watts));
    assert_eq!(relaunched.peaks().work_per_joule.peak(today), Some(work));
    assert_eq!(relaunched.peaks().compute_w, learned.compute_w);
    assert_eq!(relaunched.peaks().work_per_joule, learned.work_per_joule);
    // The last line was written at reading 90: the clocks as they stood then, already learned.
    let restored = *relaunched.usual_clocks();
    assert!(
        restored
            .usual(9)
            .is_some_and(|mhz| (mhz - 4000.0).abs() < 0.01),
        "{restored:?}"
    );
    assert!(
        (restored.deciles[9].read_s - 90.0).abs() < 1.5,
        "ninety seconds of the ninety-four: {restored:?}"
    );

    // CONTROL: neither file.
    let fresh = aterm_vitals::Vitals::new();
    assert_eq!(fresh.peaks().compute_w, aterm_vitals::DailyMax::default());
    assert_eq!(*fresh.usual_clocks(), UsualClocks::default());
    // CONTROL: a file from before the two rows.
    let text = std::fs::read_to_string(&peaks_path).expect("the peaks file");
    assert!(
        text.contains("\"compute_w\"") && text.contains("\"work_per_joule\""),
        "{text}"
    );
    let before = r#"{"schema":1,"peaks":[{"name":"net_rx","read_s":86400,"days":[[20380,2500000.0]]},{"name":"proc_starts","read_s":600,"days":[[20380,41.5]]}]}"#;
    let old = peaks_of(before).expect("a file from before");
    assert_eq!(old.proc_starts.peak(20_380), Some(41.5));
    assert_eq!(old.compute_w, aterm_vitals::DailyMax::default());
    assert_eq!(old.work_per_joule, aterm_vitals::DailyMax::default());
    // CONTROL: without the tail.
    std::fs::remove_file(strip_dir(dir.path()).join(TAIL_FILE)).expect("the tail");
    assert_eq!(store_in(dir.path()).1.usual, UsualClocks::default());
}

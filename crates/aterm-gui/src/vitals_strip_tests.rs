// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The append-only strip checkpoint's laws (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §9).

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_vitals::model::{
    GpuSample, LinkKind, LinkSample, MemSample, Pressure, ProcSample, STRIP_HISTORY_CAP,
    STRIP_RINGS, Sample, SavedStrip, StripEvent, StripEventKind, StripLine, TabRoot, TaskRoom,
    Ticks, VolumeSample,
};
use aterm_vitals::ring::{
    Agg, BUCKET_MS, GapKind, LONG_BUCKET_MS, LONG_BUCKETS, RECENT_CAP, RING_BUCKETS, SavedRing,
    TimeGap,
};

use super::{
    BASE_FILE, LEGACY_FILE, STRIP_MAX_BYTES, StripStore, TAIL_FILE, base_json, base_of, line_json,
    strip_dir,
};
use crate::vitals_host::StripLock;

/// The wall clock the tests' readings start at: an epoch's start.
const T0: i64 = 1_793_000_000_000;

/// Reading `i` of a machine one second apart from [`T0`]'s half-slot, every strip ring fed: the
/// rows, Network's two directions, memory pressure, aterm's own processes and the process count.
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
            pressure: if wobble % 3 == 0 {
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
            available: 1_000_000_000_000 - wobble * 1_000_003,
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
/// base written; a base of schema 1 restores nothing either, in the old layout or in schema 2's
/// own, where only the schema guard refuses it, and nor does one of schema 3. None panics.
/// CONTROL: the same strip written at schema 2 restores.
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
        "CONTROL: schema 2 restores the base the first reading wrote"
    );
    let base = strip_dir(home).join(BASE_FILE);
    let two = std::fs::read_to_string(&base).expect("the base");
    assert!(two.starts_with("{\"schema\":2,"), "{two}");
    for other in [1, 3] {
        let relabelled = two.replacen("{\"schema\":2,", &format!("{{\"schema\":{other},"), 1);
        std::fs::write(&base, relabelled).expect("a base of another schema");
        assert_eq!(
            store_in(home).1,
            SavedStrip::default(),
            "a schema-{other} base in schema 2's layout"
        );
    }
}

/// A ring at its fullest: an hour of 10 s buckets, [`LONG_BUCKETS`] ten-minute ones and
/// [`RECENT_CAP`] raw readings, each number at seventeen significant digits.
fn fullest_ring(seed: f64) -> SavedRing {
    let open_k = (T0 + 7 * 3_600_000).div_euclid(BUCKET_MS);
    let v = |j: usize| -(123_456_789.012_345_67 + seed + j as f64 * 0.001_234_567_890_123_4);
    let agg = |j: usize, tau_ms: u32, n: u16| Agg {
        tau_ms,
        n,
        over_ms: tau_ms - 1,
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

/// Record §9: a base with every ring at its fullest, its numbers at their widest, and the most
/// gaps and events a strip keeps, is under 85 % of [`STRIP_MAX_BYTES`], and reads back whole.
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
                at_unix_ms: T0 + j * 100_000,
                kind: StripEventKind::PressureSpike,
                label: "memory pressure critical".into(),
            })
            .collect(),
        ..SavedStrip::default()
    };
    for slot in 0..STRIP_RINGS {
        *full.ring_mut(slot).expect("a slot") = fullest_ring(slot as f64);
    }
    let text = base_json(&full, u64::MAX).expect("text");
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

/// Record §9: an hour of readings a second, every ring fed, writes under 2 MB to the strip's
/// files: a base at the start and at the hour, and a line per epoch between. CONTROL: a whole
/// base every 10 s, the schema-1 checkpoint's rule, would write far more.
#[test]
fn the_strip_writes_under_two_megabytes_an_hour() {
    let dir = aterm_tempfile::tempdir().expect("scratch state");
    let (mut store, _) = store_in(dir.path());
    let mut model = aterm_vitals::Vitals::new();
    let t0 = Instant::now();
    let mut whole_every_10s = 0_u64;
    for i in 0..=3_600 {
        let _ = model.absorb(&reading(i));
        store.keep(
            &model,
            t0 + Duration::from_secs(i),
            &mut StripLock::Unlocked,
        );
        if i % 10 == 0 {
            whole_every_10s +=
                base_json(&model.saved_strip(false), 1).map_or(0, |t| t.len() as u64);
        }
    }
    let written = store.bytes_written();
    eprintln!(
        "an hour wrote {written} bytes; a whole base every 10 s would write {whole_every_10s}"
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
        time_gaps: Vec::new(),
        strip_events: Vec::new(),
    };
    let text = line_json(&line, 7).expect("text");
    assert_eq!(super::line_of(&text), Some((7, line.clone())));
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

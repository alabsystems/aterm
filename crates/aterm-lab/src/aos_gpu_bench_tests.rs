// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_sysprobe::Why;

const COMMIT: &str = "0123456789ab";
const WORKLOAD: &str = "a1e0780d3c01fde2afcfcdc70909f0c76f265115f1569ab2b53f4cda30410ac3";
const TONE: &str = "f2299a7c94d530521d5a04bb7dd137c6c36a72d4c86e72d80e3b5faa77919823";
const B371: &str = "2b7e3012199b49ac429ec6d4137a4ab189810a6dcf862373a5bd8326e47849c5";
const ARGMAX: &str = "0001112223334445";
const TB: (u32, u32) = (125, 3);

/// Each arm's end-to-end p50 per series (b1, b1iso, b371, b16384), ns.
fn e2e(arm: Arm) -> [u64; 4] {
    match arm {
        Arm::F => [20_000, 25_000, 4_000_000, 180_000_000],
        Arm::I => [5_000, 6_000, 600_000, 26_000_000],
        Arm::T => [30_000, 40_000, 300_000, 5_000_000],
        Arm::G => [300_000, 1_000_000, 900_000, 4_000_000],
    }
}

/// A complete, valid summary of `arm` for run `id`, as the harness writes it.
fn fixture(arm: Arm, id: &str) -> Vec<(String, String)> {
    let g = arm == Arm::G;
    let gv = |v: &str| if g { v.to_owned() } else { "-".to_owned() };
    let mut p: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: String| p.push((k.to_owned(), v));
    put("entry_ticks", "1000".into());
    put("format", "aos-gpu-v1".into());
    put("arm", arm.name().into());
    put("run_id", id.into());
    put("build_commit", COMMIT.into());
    put("build_profile", "release opt-level=3".into());
    put("build_toolchain", "rustc 1.99.0-dev (trustc 0.11.0)".into());
    put("debug_assertions", "false".into());
    put("hw_model", "Mac17,4".into());
    put("kern_osversion", "26A434".into());
    put("timebase", "125/3".into());
    put("parallelism", "10".into());
    put("qos", "0x21 0".into());
    for key in ["pool_qos_set", "pool_qos_wanted"] {
        put(
            key,
            if arm == Arm::T {
                "10".into()
            } else {
                "-".into()
            },
        );
    }
    put("reps", "1000 100 500 200".into());
    put("iso_ms", "100".into());
    put("thermal_start", "0".into());
    put("cold.model_load_ns", "900000".into());
    put("cold.pipeline_salted_ns", gv("17000000"));
    put("cold.start_cold_ns", gv("60000000"));
    put("cold.pipeline_ns", gv("130416"));
    put("cold.start_warm_ns", gv("9000000"));
    put("cold.cache_hit", gv("0"));
    put("cold.first_b1_ns", "300000".into());
    put("device_name", gv("Apple M5"));
    put("device_registry_id", gv("4294968354"));
    put(
        "limits",
        gv("16777216 67108864 268435456 20100448256 26800603136"),
    );
    put("params_bytes", gv("70340"));
    put("job_bytes", gv("2697988"));
    put("qualified", gv("1 1"));
    for (i, (s, rows)) in SERIES.iter().enumerate() {
        let n = REPS[i];
        let e = e2e(arm)[i];
        put(&format!("{s}.reps"), n.to_string());
        put(&format!("{s}.rows"), rows.to_string());
        put(&format!("{s}.e2e_ns"), format!("{e} {} {}", e + 10, e + 20));
        put(&format!("{s}.e2e_sum_ns"), (e * n).to_string());
        if arm == Arm::F {
            put(&format!("{s}.packed_ns"), "-".into());
            put(&format!("{s}.packed_sum_ns"), "-".into());
        } else {
            let pk = e / 2;
            put(
                &format!("{s}.packed_ns"),
                format!("{pk} {} {}", pk + 5, pk + 9),
            );
            put(&format!("{s}.packed_sum_ns"), (pk * n).to_string());
        }
        put(&format!("{s}.cpu_ns"), format!("{} 1000", e * n));
        put(&format!("{s}.compare_ns"), "100 5".into());
        put(&format!("{s}.wall_ns"), (e * n + 1000).to_string());
        put(&format!("{s}.split_valid"), gv(&n.to_string()));
        put(&format!("{s}.part_invalid"), gv("0 0 0 0 0 0 0 0"));
        for part in PARTS {
            put(&format!("{s}.part.{part}_ns"), gv("1000 2000 3000"));
        }
        put(&format!("{s}.gpu_busy_ns"), gv("5000"));
    }
    put("jobs", gv("1802 1802"));
    put("declared_max", "2768328".into());
    for (i, pt) in POINTS.iter().enumerate() {
        let metal = if g && (2..=7).contains(&i) {
            "458752"
        } else {
            "-"
        };
        put(
            &format!("mem.{pt}"),
            format!("{} {metal} 0 280400 280400", 2_000_000 + i as u64 * 1000),
        );
    }
    let footprint: u64 = if g { 64 << 20 } else { 8 << 20 };
    put("lifetime_max_footprint", footprint.to_string());
    put("workload_digest", WORKLOAD.into());
    put("workload_digest_pin", WORKLOAD.into());
    let cpu_pin = |v: &str| {
        if arm == Arm::F {
            "-".to_owned()
        } else {
            v.to_owned()
        }
    };
    put("tone_int_id", cpu_pin(TONE));
    put("tone_int_id_pin", cpu_pin(TONE));
    put("b371_out_digest", cpu_pin(B371));
    put("b371_out_digest_pin", cpu_pin(B371));
    let f_pin = |v: &str| {
        if arm == Arm::F {
            v.to_owned()
        } else {
            "-".to_owned()
        }
    };
    put("f_argmax", f_pin(ARGMAX));
    put("f_argmax_pin", f_pin(ARGMAX));
    put("thermal_end", "0".into());
    put("gpu_failure", "none".into());
    put("last_ticks", "5000".into());
    p
}

fn text(p: &[(String, String)]) -> String {
    p.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

fn set(p: &mut [(String, String)], key: &str, value: &str) {
    let slot = p
        .iter_mut()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no {key} in the fixture"));
    slot.1 = value.to_owned();
}

/// `pmset -g` as this host prints it, Low Power Mode off.
const PMSET: &str = "System-wide power settings:\nCurrently in use:\n standby              1\n lowpowermode         0\n sleep                1\n";

/// `pmset -g ps` on AC power.
const PMSET_AC: &str = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n";

/// `pmset -g ps` on battery.
const PMSET_BATTERY: &str = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%; discharging; 4:00 remaining present: true\n";

fn on_battery(r: &mut Run) {
    r.cond.pmset_ps = Some(PMSET_BATTERY.into());
}

fn conditions_ok() -> Conditions {
    let gpu = GpuReading {
        device_pct: Some(3),
        recoveries: Some(0),
        ..GpuReading::default()
    };
    Conditions {
        load1: Some(1.2),
        swap: Some("total = 2048.00M  used = 477.25M  free = 1570.75M  (encrypted)\n".into()),
        pmset: Some(PMSET.into()),
        pmset_ps: Some(PMSET_AC.into()),
        gpu_before: Some(gpu.clone()),
        gpu_after: Some(gpu),
        driver: Some(GpuDriver {
            bundle: Some("com.apple.AGXG17G".into()),
            version: Some("360.34.5".into()),
        }),
        notes: Vec::new(),
    }
}

fn measured_ok() -> Measured {
    Measured {
        spawn_ticks: 100,
        exit_ticks: Some(6000),
        exit: Some(0),
        maxrss: Some(30 << 20),
        cpu_ns: Some(2_000_000_000),
        ..Measured::default()
    }
}

/// A judged run of `arm` from summary `p`.
fn run_of(arm: Arm, round: usize, pos: usize, p: &[(String, String)]) -> Run {
    let mut r = Run {
        id: format!("s.r{round:02}.{pos}"),
        round,
        pos,
        arm,
        cond: conditions_ok(),
        m: measured_ok(),
        summary: parse_summary(&text(p)),
        verdict: Verdict::Valid,
    };
    r.verdict = judge(&r);
    r
}

fn valid_run(arm: Arm, round: usize, pos: usize) -> Run {
    run_of(
        arm,
        round,
        pos,
        &fixture(arm, &format!("s.r{round:02}.{pos}")),
    )
}

/// Rejudge `r` after `edit`.
fn rejudged(mut r: Run, edit: impl FnOnce(&mut Run)) -> Run {
    edit(&mut r);
    r.verdict = judge(&r);
    r
}

fn header(rounds: usize) -> Header {
    Header {
        rounds,
        session: "s".into(),
        runner_commit: COMMIT.into(),
        runner_profile: "debug opt-level=0".into(),
        harness: "/x/aos_gpu".into(),
        harness_digest: "abcdef012345".into(),
        harness_unchanged: Some(true),
        timebase: TB,
    }
}

fn session(rounds: usize) -> Vec<Run> {
    schedule(rounds)
        .iter()
        .enumerate()
        .flat_map(|(n, order)| {
            order
                .iter()
                .enumerate()
                .map(move |(p, arm)| valid_run(*arm, n + 1, p + 1))
        })
        .collect()
}

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

fn gates_of(v: &Verdict) -> Vec<Gate> {
    match v {
        Verdict::Invalid(w) => w.iter().map(|(g, _)| *g).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn the_schedule_is_the_registered_williams_order_three_times_over() {
    let s = schedule(12);
    let names: Vec<String> = s
        .iter()
        .map(|o| o.iter().map(|a| a.name()).collect())
        .collect();
    assert_eq!(
        names,
        ["FIGT", "ITFG", "TGIF", "GFTI"].repeat(3),
        "§12.9's order"
    );
    for pos in 0..4 {
        for arm in Arm::ALL {
            assert_eq!(s.iter().filter(|o| o[pos] == arm).count(), 3);
        }
    }
}

#[test]
fn the_arguments_parse_and_rounds_stay_within_the_registered_twelve() {
    let a = parse_args(&args("--harness h --out o")).expect("args");
    assert_eq!(
        (a.harness.as_str(), a.out.as_str(), a.rounds),
        ("h", "o", 12)
    );
    assert_eq!(
        parse_args(&args("--harness h --out o --rounds 2"))
            .expect("smoke")
            .rounds,
        2
    );
    for bad in [
        "--out o",
        "--harness h",
        "--harness h --out o --rounds 0",
        "--harness h --out o --rounds 13",
        "--harness h --out o --rounds x",
        "--harness h --out o --faults",
    ] {
        assert!(parse_args(&args(bad)).is_err(), "{bad}");
    }
}

#[test]
fn a_summary_parses_by_key_and_refuses_what_is_not_one() {
    let p = fixture(Arm::G, "s.r01.1");
    let s = parse_summary(&text(&p)).expect("the fixture");
    assert_eq!(s.int("entry_ticks"), Some(1000));
    assert_eq!(s.get("cold.pool_ns"), None);
    assert_eq!(s.nth("b371.e2e_ns", 2), Some(900_020));
    assert_eq!(s.ints("qualified"), Some(vec![1, 1]));
    assert_eq!(
        s.get("build_toolchain"),
        Some("rustc 1.99.0-dev (trustc 0.11.0)")
    );
    let mut twice = text(&p);
    twice.push_str("arm=G\n");
    assert!(parse_summary(&twice).unwrap_err().contains("twice"));
    assert!(parse_summary("entry_ticks=1\nnot a pair\n").is_err());
    let mut other = p.clone();
    set(&mut other, "format", "aos-gpu-v0");
    assert!(parse_summary(&text(&other)).unwrap_err().contains("format"));
    let mut armless = p.clone();
    set(&mut armless, "arm", "X");
    assert!(parse_summary(&text(&armless)).is_err());
    let truncated: Vec<_> = p.iter().take(20).cloned().collect();
    assert!(
        parse_summary(&text(&truncated))
            .unwrap_err()
            .contains("last_ticks")
    );
}

#[test]
fn pmset_and_swapusage_read_as_this_host_prints_them() {
    assert_eq!(parse_lowpower(PMSET), Some(false));
    assert_eq!(
        parse_lowpower(&PMSET.replace("lowpowermode         0", "lowpowermode 1")),
        Some(true)
    );
    assert_eq!(parse_lowpower("Currently in use:\n sleep 1\n"), None);
    assert_eq!(parse_ac(PMSET_AC), Some(true));
    assert_eq!(parse_ac("Now drawing from 'Battery Power'\n"), Some(false));
    assert_eq!(parse_ac(""), None);
    let swap = "total = 2048.00M  used = 477.25M  free = 1570.75M  (encrypted)";
    assert_eq!(parse_swap_used(swap), Some(477.25));
    assert_eq!(
        parse_swap_used("total = 0.00M  used = 0.00M  free = 0.00M"),
        Some(0.0)
    );
}

#[test]
fn a_complete_run_of_each_arm_is_valid() {
    for arm in Arm::ALL {
        let r = valid_run(arm, 1, 1);
        assert_eq!(r.verdict, Verdict::Valid, "{arm:?}");
        assert_eq!(r.creation_ns(TB), Some(900 * 125 / 3));
        assert_eq!(r.exit_ns(TB), Some(1000 * 125 / 3));
        assert_eq!(r.wall_ns(TB), Some(5900 * 125 / 3));
    }
}

#[test]
fn every_defect_named_in_g6_stops_the_session() {
    let g = valid_run(Arm::G, 1, 1);
    let defect = |r: &Run| matches!(r.verdict, Verdict::Defect(_));
    assert!(
        defect(&rejudged(g.clone(), |r| r.m.exit = Some(1))),
        "a nonzero exit"
    );
    assert!(
        defect(&rejudged(g.clone(), |r| r.m.exit = Some(101))),
        "a forward panic"
    );
    assert!(defect(&rejudged(g.clone(), |r| {
        r.m.exit = None;
        r.m.signal = Some(6);
    })));
    assert!(defect(
        &rejudged(g.clone(), |r| r.m.error = Some("cannot spawn".into()))
    ));
    assert!(defect(
        &rejudged(g.clone(), |r| r.summary = Err("absent".into()))
    ));
    assert!(
        defect(&rejudged(g.clone(), |r| r.arm = Arm::T)),
        "an arm mismatch"
    );
    assert!(defect(&rejudged(g.clone(), |r| r.id = "s.r01.2".into())));
    let mut p = fixture(Arm::G, "s.r01.1");
    set(&mut p, "b371_out_digest", TONE);
    let r = run_of(Arm::G, 1, 1, &p);
    assert!(
        matches!(&r.verdict, Verdict::Defect(d) if d.contains("C0")),
        "{:?}",
        r.verdict
    );
    let mut p = fixture(Arm::I, "s.r01.1");
    set(&mut p, "reps", "3 2 1 1");
    assert!(defect(&run_of(Arm::I, 1, 1, &p)), "a smoke harness run");
    let mut p = fixture(Arm::I, "s.r01.1");
    set(&mut p, "b16384.e2e_ns", "-");
    assert!(defect(&run_of(Arm::I, 1, 1, &p)), "a figure missing");
    let mut p = fixture(Arm::T, "s.r01.1");
    set(&mut p, "b371.reps", "499");
    assert!(defect(&run_of(Arm::T, 1, 1, &p)));
    let f = valid_run(Arm::F, 1, 1);
    assert!(
        defect(&rejudged(f, |r| r.m.exit = Some(EXIT_GPU))),
        "exit 3 from a CPU arm"
    );
    let mut p = fixture(Arm::G, "s.r01.1");
    set(&mut p, "gpu_failure", "a job ended Hung");
    assert!(
        defect(&run_of(Arm::G, 1, 1, &p)),
        "exit 0 with a GPU failure"
    );
}

#[test]
fn a_gpu_that_misbehaves_voids_its_run_under_g3() {
    let mut p = fixture(Arm::G, "s.r01.1");
    set(
        &mut p,
        "gpu_failure",
        "a job ended Lost(\"device removed\")",
    );
    let r = rejudged(run_of(Arm::G, 1, 1, &p), |r| r.m.exit = Some(EXIT_GPU));
    assert_eq!(gates_of(&r.verdict), [Gate::G3], "{:?}", r.verdict);
    let mut p = fixture(Arm::G, "s.r01.1");
    set(&mut p, "cold.pipeline_ns", "1999999");
    assert_eq!(
        run_of(Arm::G, 1, 1, &p).verdict,
        Verdict::Valid,
        "a canonical pipeline under 2 ms is a warm-cache hit"
    );
    for (key, value) in [
        ("cold.cache_hit", "1"),
        ("cold.pipeline_salted_ns", "1999999"),
        ("cold.pipeline_ns", "2000000"),
        ("qualified", "1 0"),
    ] {
        let mut p = fixture(Arm::G, "s.r01.1");
        set(&mut p, key, value);
        let r = run_of(Arm::G, 1, 1, &p);
        assert!(
            gates_of(&r.verdict).contains(&Gate::G3),
            "{key}: {:?}",
            r.verdict
        );
    }
    let moved = rejudged(valid_run(Arm::G, 1, 1), |r| {
        r.cond.gpu_after = Some(GpuReading {
            recoveries: Some(1),
            ..GpuReading::default()
        });
    });
    assert_eq!(gates_of(&moved.verdict), [Gate::G3]);
    let unread = rejudged(valid_run(Arm::G, 1, 1), |r| r.cond.gpu_before = None);
    assert_eq!(gates_of(&unread.verdict), [Gate::G3]);
    let cpu = rejudged(valid_run(Arm::I, 1, 1), |r| r.cond.gpu_before = None);
    assert_eq!(cpu.verdict, Verdict::Valid, "G3 reads G runs only");
}

#[test]
fn a_budget_missed_voids_its_run_under_g5() {
    let g = valid_run(Arm::G, 1, 1);
    let r = rejudged(g.clone(), |r| {
        r.m.deadline = true;
        r.m.exit = None;
        r.m.signal = Some(9);
    });
    assert_eq!(gates_of(&r.verdict), [Gate::G5]);
    assert_eq!(
        gates_of(
            &rejudged(g.clone(), |r| {
                r.cond.pmset = Some(PMSET.replace("lowpowermode         0", "lowpowermode 1"));
            })
            .verdict
        ),
        [Gate::G5]
    );
    assert_eq!(
        gates_of(&rejudged(g.clone(), |r| r.cond.pmset = None).verdict),
        [Gate::G5]
    );
    assert_eq!(
        gates_of(&rejudged(g.clone(), on_battery).verdict),
        [Gate::G5]
    );
    assert_eq!(
        gates_of(&rejudged(g.clone(), |r| r.cond.pmset_ps = None).verdict),
        [Gate::G5]
    );
    let mut p = fixture(Arm::T, "s.r01.1");
    set(&mut p, "pool_qos_set", "9");
    assert_eq!(
        gates_of(&run_of(Arm::T, 1, 1, &p).verdict),
        [Gate::G5],
        "a pool thread that missed a settable QoS"
    );
    set(&mut p, "qos", "0x0 0");
    set(&mut p, "pool_qos_set", "0");
    set(&mut p, "pool_qos_wanted", "0");
    assert_eq!(
        run_of(Arm::T, 1, 1, &p).verdict,
        Verdict::Valid,
        "an unspecified class leaves the pool at the main thread's, the registered arm"
    );
    set(&mut p, "qos", "0x19 0");
    set(&mut p, "pool_qos_set", "-");
    set(&mut p, "pool_qos_wanted", "10");
    assert_eq!(gates_of(&run_of(Arm::T, 1, 1, &p).verdict), [Gate::G5]);
    for (key, value) in [
        ("lifetime_max_footprint", "2147483649"),
        ("job_bytes", "67108865"),
        ("declared_max", "268435457"),
    ] {
        let mut p = fixture(Arm::G, "s.r01.1");
        set(&mut p, key, value);
        assert_eq!(
            gates_of(&run_of(Arm::G, 1, 1, &p).verdict),
            [Gate::G5],
            "{key}"
        );
    }
    let mut p = fixture(Arm::F, "s.r01.1");
    set(&mut p, "lifetime_max_footprint", "2147483648");
    assert_eq!(
        run_of(Arm::F, 1, 1, &p).verdict,
        Verdict::Valid,
        "2 GiB itself is within"
    );
}

#[test]
fn a_base_that_does_not_reproduce_fails_g1_and_voids_the_session() {
    let mut p = fixture(Arm::F, "s.r01.1");
    set(&mut p, "f_argmax", "0001112223334444");
    assert_eq!(gates_of(&run_of(Arm::F, 1, 1, &p).verdict), [Gate::G1]);
    let mut p = fixture(Arm::T, "s.r01.1");
    set(&mut p, "workload_digest", TONE);
    assert_eq!(gates_of(&run_of(Arm::T, 1, 1, &p).verdict), [Gate::G1]);
    let mut p = fixture(Arm::I, "s.r01.1");
    set(&mut p, "tone_int_id", WORKLOAD);
    assert_eq!(gates_of(&run_of(Arm::I, 1, 1, &p).verdict), [Gate::G1]);
    let mut runs = session(12);
    let i = runs.iter().position(|r| r.arm == Arm::F).expect("an F run");
    let id = runs[i].id.clone();
    let mut p = fixture(Arm::F, &id);
    set(&mut p, "f_argmax", "0001112223334444");
    runs[i] = run_of(Arm::F, runs[i].round, runs[i].pos, &p);
    let ev = evaluate(&header(12), &runs, true);
    assert_eq!(ev.gate1.len(), 1);
    assert!(matches!(&ev.outcome, Outcome::Void(w) if w.iter().any(|w| w.starts_with("G1"))));
}

#[test]
fn a_clean_registered_session_is_valid_and_its_criteria_read_per_round() {
    let runs = session(12);
    let ev = evaluate(&header(12), &runs, true);
    assert_eq!(ev.outcome, Outcome::Valid, "{ev:?}");
    assert_eq!(ev.valid_rounds, (1..=12).collect::<Vec<_>>());
    assert_eq!(ev.f_span, Some(1.0));
    let cs = criteria(&runs, &ev.valid_rounds);
    let shown = |name: &str, figure: Figure| {
        cs.iter()
            .find(|c| c.name == name && c.figure == figure)
            .expect(name)
            .shown()
    };
    for f in [Figure::E2e, Figure::Packed] {
        assert!(shown("C1", f) && shown("C2", f), "{f:?}");
        assert!(!shown("C5", f) && !shown("C6", f), "{f:?}");
    }
    assert!(shown("C3", Figure::E2e) && shown("C4", Figure::E2e));
    assert_eq!(cs.len(), 10);
    let mut slow = runs.clone();
    let g = slow
        .iter()
        .position(|r| r.arm == Arm::G && r.round == 7)
        .expect("round 7's G");
    let mut p = fixture(Arm::G, &slow[g].id);
    set(&mut p, "b16384.e2e_ns", "30000000 30000010 30000020");
    slow[g] = run_of(Arm::G, 7, slow[g].pos, &p);
    let cs = criteria(&slow, &(1..=12).collect::<Vec<_>>());
    let c1 = cs
        .iter()
        .find(|c| c.name == "C1" && c.figure == Figure::E2e)
        .expect("C1");
    assert!(!c1.shown(), "one valid round against it");
    assert_eq!(c1.holds_in(6), Some(false));
    assert!(
        cs.iter()
            .any(|c| c.name == "C1" && c.figure == Figure::Packed && c.shown())
    );
}

#[test]
fn g0_needs_one_clean_commit_and_one_unchanged_release_harness() {
    let runs = session(1);
    let mut h = header(1);
    assert!(gate0(&h, &runs, true).is_empty());
    h.runner_commit = format!("{COMMIT}-dirty");
    assert!(!gate0(&h, &runs, true).is_empty());
    let mut h = header(1);
    h.harness_unchanged = Some(false);
    assert_eq!(gate0(&h, &runs, true).len(), 1);
    h.harness_unchanged = None;
    assert!(
        gate0(&h, &runs, false).is_empty(),
        "read again only at the end"
    );
    assert_eq!(gate0(&h, &runs, true).len(), 1);
    for (key, value) in [
        ("build_commit", "fedcba987654"),
        ("build_profile", "release opt-level=2"),
        ("debug_assertions", "true"),
    ] {
        let mut runs = session(1);
        let mut p = fixture(Arm::I, &runs[1].id);
        set(&mut p, key, value);
        runs[1] = run_of(Arm::I, 1, 2, &p);
        let w = gate0(&header(1), &runs, true);
        assert_eq!(w.len(), 1, "{key}: {w:?}");
        assert!(matches!(
            evaluate(&header(1), &runs, true).outcome,
            Outcome::Void(_)
        ));
    }
    assert!(clean_commit(COMMIT) && !clean_commit("unknown") && !clean_commit("0123456789AB"));
}

#[test]
fn g3_holds_the_machine_and_driver_still_across_the_session() {
    let mut runs = session(2);
    assert!(gate3_session(&runs).is_empty());
    let mut p = fixture(Arm::T, &runs[5].id);
    set(&mut p, "kern_osversion", "26B12");
    runs[5] = run_of(Arm::T, runs[5].round, runs[5].pos, &p);
    let w = gate3_session(&runs);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("kern.osversion"));
    runs[6].cond.driver = Some(GpuDriver {
        bundle: Some("com.apple.AGXG17G".into()),
        version: Some("361.0".into()),
    });
    assert_eq!(gate3_session(&runs).len(), 2);
    for r in &mut runs {
        r.cond.driver = None;
    }
    assert!(
        gate3_session(&runs)
            .iter()
            .any(|w| w.contains("never read"))
    );
    let mut runs = session(2);
    assert_eq!(runs[1].arm, Arm::I);
    let mut p = fixture(Arm::I, &runs[1].id);
    set(&mut p, "qos", "0x11 0");
    runs[1] = run_of(Arm::I, runs[1].round, runs[1].pos, &p);
    assert_eq!(runs[1].verdict, Verdict::Valid, "a run alone holds no QoS");
    let w = gate3_session(&runs);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("QoS") && w[0].contains("0x11 0") && w[0].contains("0x21 0"),
        "{w:?}"
    );
    assert!(matches!(
        evaluate(&header(2), &runs, true).outcome,
        Outcome::Void(w) if w.iter().any(|w| w.starts_with("G3") && w.contains("QoS"))
    ));
}

#[test]
fn g4_and_g6_void_an_unstable_or_thin_session() {
    let mut runs = session(12);
    let f = runs
        .iter()
        .position(|r| r.arm == Arm::F && r.round == 3)
        .expect("round 3's F");
    let mut p = fixture(Arm::F, &runs[f].id);
    set(&mut p, "b371.e2e_ns", "5300000 5300010 5300020");
    runs[f] = run_of(Arm::F, 3, runs[f].pos, &p);
    let ev = evaluate(&header(12), &runs, true);
    assert!(ev.f_span.is_some_and(|x| x > F_SPAN_MAX));
    assert_eq!(ev.gate4.len(), 1);
    let mut runs = session(12);
    for n in [2, 5, 9] {
        let i = runs
            .iter()
            .position(|r| r.round == n && r.arm == Arm::G)
            .expect("a G run");
        runs[i] = rejudged(runs[i].clone(), on_battery);
    }
    let ev = evaluate(&header(12), &runs, true);
    assert_eq!(ev.valid_rounds.len(), 9);
    assert_eq!(ev.required, 10);
    assert_eq!(ev.gate6.len(), 1);
    assert_eq!(ev.out.len(), 3);
    let mut runs = session(12);
    let i = runs.iter().position(|r| r.round == 2).expect("round 2");
    runs[i] = rejudged(runs[i].clone(), on_battery);
    assert_eq!(
        evaluate(&header(12), &runs, true).outcome,
        Outcome::Valid,
        "two void rounds of twelve are allowed"
    );
    assert_eq!(required_valid(2), 2);
}

#[test]
fn a_defect_stops_the_session_ahead_of_every_gate() {
    let mut runs = session(2);
    runs.truncate(5);
    runs[4] = rejudged(runs[4].clone(), |r| r.m.exit = Some(1));
    let ev = evaluate(&header(2), &runs, true);
    assert!(matches!(ev.outcome, Outcome::Stopped(_)));
    let (text, code) = render(&header(2), &runs, State::Done);
    assert_eq!(code, 2);
    assert!(text.contains("STOPPED by a defect"), "{text}");
}

#[test]
fn derived_figures_are_rows_over_time_and_over_cpu() {
    let r = valid_run(Arm::I, 1, 1);
    let d = derived(&r, TB);
    let get = |k: &str| d.iter().find(|(n, _)| n == k).and_then(|(_, v)| *v);
    let rate = get("b371 rows/s").expect("rows/s");
    assert!((rate - 371.0 / 600_000e-9).abs() < 1e-3, "{rate}");
    let per_cpu = get("b371 rows per CPU-second").expect("per CPU");
    assert!((per_cpu - 371.0 * 500.0 / ((600_000.0 * 500.0 + 1000.0) / 1e9)).abs() < 1e-3);
    assert_eq!(get("RSS (ru_maxrss), bytes"), Some(f64::from(30u32 << 20)));
}

#[test]
fn the_record_carries_every_section_and_its_exit() {
    let runs = session(12);
    let (text, code) = render(&header(12), &runs, State::Done);
    assert_eq!(code, 0);
    for want in [
        "## Gates",
        "**Session: VALID**",
        "## Criteria",
        "SHOWN",
        "not shown",
        "holds (sanity)",
        "## The registered figure",
        "G − I (the medians' difference): 56.0 MiB",
        "## Warm series",
        "### G's split",
        "## Memory at P0–P7",
        "## Every figure",
        "| b371.e2e_ns | 4000000 4000010 4000020 |",
        "- QoS: the child's main thread 0x21 0 (user-interactive), which G3 requires across the session",
        "no mismatch in the 48 runs that completed every series",
        "## Runs and their conditions",
        "| 0x21 0 (pool 10 of 10) |",
        "| 17000.0 / 130.4 |",
        "## Disclosures",
        "Whatever they show, its stated value is the tested substrate and its exposed costs, not a speedup.",
        "aos-gpu-bench result=valid kind=registered exit=0",
    ] {
        assert!(text.contains(want), "missing `{want}`");
    }
    assert!(!text.contains("SMOKE"));
    let (smoke, code) = render(&header(2), &session(2), State::Done);
    assert_eq!(code, 0);
    assert!(smoke.contains("SMOKE (--rounds 2)"));
    assert!(smoke.contains("no claim"));
    assert!(!smoke.contains("| SHOWN |"));
    let mut thin = session(2);
    thin.pop();
    let (_, code) = render(&header(2), &thin, State::Done);
    assert_eq!(code, 1, "an insufficient session");
    let (_, code) = render(&header(2), &thin, State::Interrupted(2));
    assert_eq!(code, 130);
    let (running, code) = render(&header(2), &thin, State::Running);
    assert_eq!(code, 1);
    assert!(running.contains("state: running"));
}

fn g_of(runs: &[Run], round: usize) -> usize {
    runs.iter()
        .position(|r| r.round == round && r.arm == Arm::G)
        .expect("the round's G run")
}

#[test]
fn no_criterion_is_claimed_unless_the_registered_session_is_valid_and_finished() {
    let claims = |text: &str| text.contains("| SHOWN |") || text.contains("| holds (sanity) |");
    let (valid, _) = render(&header(12), &session(12), State::Done);
    assert!(
        claims(&valid),
        "a valid, finished session claims what holds"
    );
    let mut g6 = session(12);
    for n in [2, 5, 9] {
        let i = g_of(&g6, n);
        g6[i] = rejudged(g6[i].clone(), on_battery);
    }
    let mut g4 = session(12);
    let f = g4
        .iter()
        .position(|r| r.arm == Arm::F && r.round == 3)
        .expect("round 3's F");
    let mut p = fixture(Arm::F, &g4[f].id);
    set(&mut p, "b371.e2e_ns", "5300000 5300010 5300020");
    g4[f] = run_of(Arm::F, 3, g4[f].pos, &p);
    let mut stopped = session(12);
    stopped.truncate(17);
    stopped[16] = rejudged(stopped[16].clone(), |r| r.m.exit = Some(1));
    let mut partial = session(12);
    partial.truncate(24);
    let void_by = |runs: &[Run], gate: &str| {
        matches!(&evaluate(&header(12), runs, true).outcome,
            Outcome::Void(w) if w.iter().any(|w| w.starts_with(gate)))
    };
    assert!(void_by(&g6, "G6") && void_by(&g4, "G4"));
    for (runs, state, why) in [
        (&g6, State::Done, "session VOID"),
        (&g4, State::Done, "session VOID"),
        (&stopped, State::Done, "session STOPPED"),
        (&partial, State::Running, "session running"),
        (&partial, State::Interrupted(2), "session interrupted"),
    ] {
        let (text, _) = render(&header(12), runs, state);
        assert!(!claims(&text), "{why}: {text}");
        assert!(
            text.contains(&format!("| holds ({why}, no claim) |")),
            "{why}: {text}"
        );
    }
}

#[test]
fn g2_counts_only_the_runs_that_completed_every_series_and_names_the_partial_ones() {
    let mut runs = session(12);
    let killed = g_of(&runs, 2);
    runs[killed] = rejudged(runs[killed].clone(), |r| {
        r.m.deadline = true;
        r.m.exit = None;
        r.m.signal = Some(9);
        r.summary = Err("no summary".into());
    });
    let failed = g_of(&runs, 5);
    let id = runs[failed].id.clone();
    let mut p: Vec<(String, String)> = fixture(Arm::G, &id)
        .into_iter()
        .filter(|(k, _)| !k.starts_with("b371.") && !k.starts_with("b16384."))
        .collect();
    set(&mut p, "gpu_failure", "a job ended Hung");
    let pos = runs[failed].pos;
    runs[failed] = rejudged(run_of(Arm::G, 5, pos, &p), |r| r.m.exit = Some(EXIT_GPU));
    assert_eq!(gates_of(&runs[killed].verdict), [Gate::G5]);
    assert_eq!(gates_of(&runs[failed].verdict), [Gate::G3]);
    let (text, _) = render(&header(12), &runs, State::Done);
    assert!(
        text.contains("no mismatch in the 46 runs that completed every series"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "  - {} (killed at the 120 s deadline) stopped part way: no summary",
            runs[killed].id
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "  - {id} (exit status 3) stopped part way: 1100 repetitions compared in the series it completed (b1, b1iso), and any of a series it stopped in"
        )),
        "{text}"
    );
}

#[test]
fn the_runner_sidecar_round_trips_every_reading_the_record_derives_from() {
    let mut r = valid_run(Arm::G, 3, 2);
    r.m.stdout = "printed\n".into();
    r.m.stderr = "warned\n".into();
    r.cond.load1 = Some(1.234_567_890_123);
    r.cond.gpu_after = Some(GpuReading {
        device_pct: Some(41),
        renderer_pct: Some(40),
        tiler_pct: Some(2),
        in_use_bytes: Some(123_456_789),
        alloc_bytes: Some(987_654_321),
        recoveries: Some(0),
        cores: Some(10),
        name: Some("AGXAcceleratorG17G".into()),
        unread: None,
    });
    r.cond.driver = Some(GpuDriver {
        bundle: Some("com.apple.AGXG17G".into()),
        version: None,
    });
    r.cond.notes = vec!["-".into(), "two\nlines \\ and a backslash\r".into()];
    let text = runner_text(&r, TB);
    assert!(
        text.lines().all(|l| l.contains('=')),
        "one key=value per line: {text}"
    );
    assert!(text.contains("pmset=System-wide power settings:\\nCurrently in use:\\n"));
    let back = parse_runner(&text).expect("the sidecar");
    let mut m = r.m.clone();
    (m.stdout, m.stderr) = (String::new(), String::new());
    assert_eq!(
        (
            back.id.as_str(),
            back.arm,
            back.round,
            back.pos,
            back.timebase
        ),
        (r.id.as_str(), Arm::G, 3, 2, TB)
    );
    assert_eq!(back.m, m);
    assert_eq!(back.cond, r.cond);
    let rebuilt = rejudged(r.clone(), |x| {
        x.m = back.m.clone();
        x.cond = back.cond.clone();
    });
    assert_eq!(rebuilt.verdict, r.verdict);
    assert_eq!(derived(&rebuilt, back.timebase), derived(&r, TB));
    assert_eq!(
        render(&header(12), std::slice::from_ref(&rebuilt), State::Done),
        render(&header(12), std::slice::from_ref(&r), State::Done),
        "every runner figure and condition in the record"
    );
    let mut killed = valid_run(Arm::T, 1, 4);
    killed.m = Measured {
        spawn_ticks: 77,
        signal: Some(9),
        deadline: true,
        error: Some("waitid: -".into()),
        ..Measured::default()
    };
    killed.cond = Conditions {
        pmset: Some(String::new()),
        gpu_after: Some(GpuReading {
            unread: Some(Why::Unsupported),
            ..GpuReading::default()
        }),
        notes: vec!["pmset -g ps: pmset exited 1".into()],
        ..Conditions::default()
    };
    let back = parse_runner(&runner_text(&killed, TB)).expect("an unread run's sidecar");
    assert_eq!((back.m, back.cond), (killed.m.clone(), killed.cond.clone()));
    assert!(
        parse_runner(&text.replace(RUNNER_FORMAT, "aos-gpu-runner-v0"))
            .unwrap_err()
            .contains("format")
    );
    assert!(
        parse_runner(&format!("{text}spawn_ticks=1\n"))
            .unwrap_err()
            .contains("twice")
    );
    let cut: String = text
        .lines()
        .filter(|l| !l.starts_with("maxrss="))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(parse_runner(&cut).unwrap_err().contains("maxrss"));
    assert!(parse_runner(&text.replace("error=-", "error=\\x")).is_err());
}

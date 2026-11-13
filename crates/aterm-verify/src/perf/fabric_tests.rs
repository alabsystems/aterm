// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn assert_output_path_round_trip(out: PathBuf) {
    let args = Args::default();
    let plain = Path::new("/scratch/plain");
    let control = bench_spec::parse(&args.bench_words(Arm::shipped()), plain).unwrap();
    assert_eq!(control.out, plain);
    let decoded = bench_spec::parse(&args.bench_words(Arm::shipped()), &out).unwrap();
    assert_eq!(decoded.out, out);
}

#[test]
fn output_path_with_spaces_round_trips() {
    assert_output_path_round_trip(PathBuf::from("/scratch/with spaces"));
}

#[cfg(unix)]
#[test]
fn output_path_with_non_utf8_bytes_round_trips() {
    use std::os::unix::ffi::OsStringExt as _;
    assert_output_path_round_trip(
        std::ffi::OsString::from_vec(b"/scratch/non-utf8-\xff".to_vec()).into(),
    );
}

#[test]
fn options_become_the_bench_words_and_bad_values_are_refused() {
    let mut o = BTreeMap::new();
    o.insert("--latency-samples", "50".to_string());
    o.insert("--attach-sessions", "1,4,16".to_string());
    o.insert("--no-windows", String::new());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(a.scenario, bench_spec::Scenario::All);
    assert_eq!(a.samples, Some(50));
    assert_eq!(a.attach, Some(vec![1, 4, 16]));
    let shipped = Arm {
        tier: bench_spec::Tier::Ordered,
        horizon_ms: Some(bench_spec::ORDERED_HORIZON_MS),
        at_rest: true,
        broker_process: bench_spec::BrokerProcess::Inproc,
    };
    assert_eq!(
        a.plan(),
        [shipped],
        "aterm's default for its guarded broker"
    );
    assert_eq!(
        a.bench_words(shipped),
        "samples=50 attach=1,4,16 durability=ordered horizon_ms=100"
    );
    assert_eq!(
        a.command_line(shipped),
        "targo --unverified verify perf fabric --latency-samples 50 --attach-sessions 1,4,16 \
         --durability ordered --horizon-ms 100"
    );
    o.insert("--durability", "relaxed".to_string());
    let a = Args::from_options(&o).expect("parses");
    let relaxed = Arm {
        tier: bench_spec::Tier::Relaxed,
        horizon_ms: None,
        at_rest: true,
        broker_process: bench_spec::BrokerProcess::Inproc,
    };
    assert_eq!(a.plan(), [relaxed]);
    assert_eq!(
        a.bench_words(relaxed),
        "samples=50 attach=1,4,16 durability=relaxed"
    );
    assert!(
        a.command_line(relaxed).ends_with(" --durability relaxed"),
        "{}",
        a.command_line(relaxed)
    );
    o.insert("--pingpong-rounds", "40".to_string());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(a.pingpong, Some(40));
    assert_eq!(
        a.bench_words(relaxed),
        "samples=50 pingpong=40 attach=1,4,16 durability=relaxed"
    );
    assert!(
        a.command_line(relaxed).contains(" --pingpong-rounds 40 "),
        "{}",
        a.command_line(relaxed)
    );
    assert_eq!(
        Args::from_options(&BTreeMap::new())
            .expect("parses")
            .bench_words(shipped),
        "durability=ordered horizon_ms=100"
    );
    for word in ["all", "messaging", "pingpong", "attach", "handoff", "ask"] {
        let mut selected = BTreeMap::new();
        selected.insert("--scenario", word.to_string());
        let a = Args::from_options(&selected).expect(word);
        let spec =
            bench_spec::parse(&a.bench_words(shipped), Path::new("/s")).expect("bench words");
        assert_eq!(spec.scenario.word(), word);
        if word == "all" {
            assert_eq!(a.bench_words(shipped), "durability=ordered horizon_ms=100");
            assert!(!a.command_line(shipped).contains("--scenario"));
        } else {
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --scenario {word} "))
            );
        }
    }
    for (flag, bad) in [
        ("--burst", "0"),
        ("--burst", "x"),
        ("--burst", "6000"),
        ("--pingpong-rounds", "0"),
        ("--pingpong-rounds", "many"),
        ("--attach-sessions", "8,1"),
        ("--attach-sessions", "1,,2"),
        ("--attach-sessions", "1,200"),
        ("--attach-rounds", "-1"),
        ("--durability", "replicated"),
        ("--durability", "Relaxed"),
        ("--durability", "strict,,relaxed"),
        ("--horizon-ms", "100"),
        ("--passes", "0"),
        ("--scenario", ""),
        ("--scenario", "Pingpong"),
        ("--scenario", "pingpong,attach"),
        ("--scenario", "unknown"),
    ] {
        let mut o = BTreeMap::new();
        o.insert(flag, bad.to_string());
        assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
    }
    let mut o = BTreeMap::new();
    o.insert("--burst", bench_spec::BURST_MAX.to_string());
    o.insert("--attach-sessions", format!("1,{}", bench_spec::ATTACH_MAX));
    assert!(Args::from_options(&o).is_ok());
    let mut o = BTreeMap::new();
    o.insert("--warmup", "3".to_string());
    assert!(Args::from_options(&o).is_err());
}

#[test]
fn retention_the_mirror_and_the_tick_reach_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    let mut o = BTreeMap::new();
    o.insert("--retention", "on".to_string());
    o.insert("--log", "off".to_string());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(
        (a.retention, a.log),
        (Some(true), Some(bench_spec::Log::Off))
    );
    assert_eq!(
        a.bench_words(shipped),
        "retention=on log=off durability=ordered horizon_ms=100"
    );
    assert_eq!(
        a.command_line(shipped),
        "targo --unverified verify perf fabric --retention on --log off --durability ordered \
         --horizon-ms 100"
    );
    let mut o = BTreeMap::new();
    o.insert("--retention-interval-ms", "1000".to_string());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(
        a.bench_words(shipped),
        "retention_interval_ms=1000 durability=ordered horizon_ms=100"
    );
    assert!(
        a.command_line(shipped)
            .contains(" --retention-interval-ms 1000 "),
        "{}",
        a.command_line(shipped)
    );
    for pairs in [
        &[("--retention", "yes")][..],
        &[("--log", "turns")],
        &[("--log", "")],
        &[("--retention-interval-ms", "0")],
        &[("--retention", "off"), ("--retention-interval-ms", "1000")],
    ] {
        let o: BTreeMap<&'static str, String> =
            pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
        assert!(Args::from_options(&o).is_err(), "{pairs:?}");
    }
}

/// `--rate-limit off` reaches the bench as `rate_limit=off` and the ledger as the flag that
/// reproduces it (critic item E06); a word other than on or off is refused.
#[test]
fn the_rate_limit_reaches_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    let mut o = BTreeMap::new();
    o.insert("--rate-limit", "off".to_string());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(a.rate_limit, Some(false));
    assert_eq!(
        a.bench_words(shipped),
        "rate_limit=off durability=ordered horizon_ms=100"
    );
    assert_eq!(
        a.command_line(shipped),
        "targo --unverified verify perf fabric --rate-limit off --durability ordered \
         --horizon-ms 100"
    );
    assert!(
        bench_spec::parse(&a.bench_words(shipped), Path::new("/scratch"))
            .is_ok_and(|spec| !spec.rate_limit),
        "the bench reads the word back"
    );
    let mut o = BTreeMap::new();
    o.insert("--rate-limit", "yes".to_string());
    assert!(Args::from_options(&o).is_err());
}

/// `--at-rest off,on --passes 2` is an A B B A set, each arm reproducible alone, and a plain run
/// measures what ships: a sealed log.
#[test]
fn the_at_rest_arms_reach_the_bench_and_the_ledger() {
    let mut o = BTreeMap::new();
    o.insert("--at-rest", "off,on".to_string());
    o.insert("--passes", "2".to_string());
    let a = Args::from_options(&o).expect("parses");
    let plan = a.plan();
    let sealed: Vec<bool> = plan.iter().map(|arm| arm.at_rest).collect();
    assert_eq!(sealed, [false, true, true, false]);
    assert!(
        a.bench_words(plan[0]).ends_with("at_rest=off"),
        "{}",
        a.bench_words(plan[0])
    );
    assert!(!a.bench_words(plan[1]).contains("at_rest"));
    assert!(a.command_line(plan[0]).ends_with(" --at-rest off"));
    assert!(!a.command_line(plan[1]).contains("--at-rest"));
    let bare = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(bare.plan()[0].at_rest, "a bare run measures what ships");
    let spec = bench_spec::parse(&bare.bench_words(bare.plan()[0]), Path::new("/s"))
        .expect("the bench reads it");
    assert!(spec.at_rest, "and the bench is told so");
    let mut o = BTreeMap::new();
    o.insert("--at-rest", "sealed".to_string());
    assert!(
        Args::from_options(&o)
            .unwrap_err()
            .contains("`--at-rest` takes on or off")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_broker_process_arms_reach_the_bench_and_the_ledger() {
    use bench_spec::BrokerProcess;
    let mut o = BTreeMap::new();
    o.insert("--broker-process", "background,interactive".to_string());
    o.insert("--passes", "2".to_string());
    o.insert("--attach-sessions", "8,32".to_string());
    let a = Args::from_options(&o).expect("parses");
    let plan = a.plan();
    let processes: Vec<BrokerProcess> = plan.iter().map(|arm| arm.broker_process).collect();
    assert_eq!(
        processes,
        [
            BrokerProcess::Background,
            BrokerProcess::Interactive,
            BrokerProcess::Interactive,
            BrokerProcess::Background
        ],
        "one invocation is an A B B A set"
    );
    assert_eq!(
        a.bench_words(plan[0]),
        "attach=8,32 durability=ordered horizon_ms=100 broker_process=background"
    );
    assert!(
        a.command_line(plan[1])
            .ends_with(" --durability ordered --horizon-ms 100 --broker-process interactive"),
        "{}",
        a.command_line(plan[1])
    );
    for arm in &plan {
        let spec =
            bench_spec::parse(&a.bench_words(*arm), Path::new("/s")).expect("the bench reads it");
        assert_eq!(
            spec.broker_process, arm.broker_process,
            "and the bench is told so"
        );
        // The entry's title names only the worlds `run/outside.rs` measures, and the arm.
        let title = heading(a.scenario, arm.broker_process);
        assert!(
            ["burst", "handoff", "ask", "cross-node"]
                .iter()
                .all(|world| !title.contains(world))
                && title.contains(arm.broker_process.word()),
            "{title}"
        );
    }
    assert!(
        heading(bench_spec::Scenario::All, BrokerProcess::Inproc).contains("burst"),
        "the in-process arm's `all` measures every world"
    );
    let bare = Args::from_options(&BTreeMap::new()).expect("bare");
    assert_eq!(bare.plan()[0].broker_process, BrokerProcess::Inproc);
    assert!(!bare.bench_words(bare.plan()[0]).contains("broker_process"));
    assert!(
        !bare
            .command_line(bare.plan()[0])
            .contains("--broker-process")
    );
    let mut o = BTreeMap::new();
    o.insert("--broker-process", "launchd".to_string());
    assert!(
        Args::from_options(&o)
            .unwrap_err()
            .contains("`--broker-process` takes inproc, background or interactive")
    );
    let mut o = BTreeMap::new();
    o.insert("--broker-process", "background".to_string());
    o.insert("--scenario", "ask".to_string());
    assert!(
        Args::from_options(&o)
            .unwrap_err()
            .contains("the fabric bench refuses this run"),
        "the bench's own refusal comes before the build"
    );
    // F09: the attach world's backlog seeds the shipped broker's log; its log is sealed, as
    // every fabric root's broker serves it, so a plaintext arm is refused before the build.
    let mut o = BTreeMap::new();
    o.insert("--broker-process", "background".to_string());
    o.insert("--scenario", "attach".to_string());
    o.insert("--attach-sessions", "8,32".to_string());
    o.insert("--attach-backlog", "510000".to_string());
    let a = Args::from_options(&o).expect("the seeded arm parses");
    let arm = a.plan()[0];
    assert!(
        a.command_line(arm).contains(" --attach-backlog 510000")
            && a.bench_words(arm).contains("backlog=510000"),
        "{}",
        a.command_line(arm)
    );
    o.insert("--at-rest", "off".to_string());
    assert!(
        Args::from_options(&o)
            .unwrap_err()
            .contains("the fabric bench refuses this run")
    );
    o.remove("--at-rest");
    // A seed short of the cold reads' records is refused before the build, not after the
    // timed world: 40,000 for each of two populations seeds 80,000.
    o.insert("--attach-backlog", "40000".to_string());
    let why = Args::from_options(&o).expect_err("a seed short of the cold reads");
    assert!(
        why.contains("the fabric bench refuses this run")
            && why.contains(&format!(
                "at least {}",
                bench_spec::COLD_RECORDS.div_ceil(2)
            )),
        "{why}"
    );
}

#[test]
fn the_bystanders_reach_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    for n in ["0", "64"] {
        let mut o = BTreeMap::new();
        o.insert("--bystanders", n.to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            a.bench_words(shipped),
            format!("bystanders={n} durability=ordered horizon_ms=100")
        );
        assert!(
            a.command_line(shipped)
                .contains(&format!(" --bystanders {n} ")),
            "{}",
            a.command_line(shipped)
        );
    }
    let a = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(!a.bench_words(shipped).contains("bystanders"));
    for bad in ["-1", "many", "999"] {
        let mut o = BTreeMap::new();
        o.insert("--bystanders", bad.to_string());
        assert!(Args::from_options(&o).is_err(), "{bad}");
    }
}

/// B09: the attach world's backlog reaches the bench's words and each
/// entry's Run line, and the bench's own refusals reach the flag (500,000
/// records at the default three attach sizes fill past the `ev` rule's bytes).
#[test]
fn the_attach_backlog_reaches_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    for n in ["0", "400000"] {
        let mut o = BTreeMap::new();
        o.insert("--attach-backlog", n.to_string());
        o.insert("--scenario", "attach".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            a.bench_words(shipped),
            format!("scenario=attach backlog={n} durability=ordered horizon_ms=100")
        );
        assert!(
            a.command_line(shipped)
                .contains(&format!(" --scenario attach --attach-backlog {n} ")),
            "{}",
            a.command_line(shipped)
        );
    }
    let a = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(!a.bench_words(shipped).contains("backlog"));
    for (scenario, bad) in [
        ("attach", "-1"),
        ("attach", "many"),
        ("attach", "1000001"),
        ("attach", "500000"),
        ("pingpong", "10"),
    ] {
        let mut o = BTreeMap::new();
        o.insert("--attach-backlog", bad.to_string());
        o.insert("--scenario", scenario.to_string());
        assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
    }
}

/// C04: the ask world's asked prefill reaches the bench's words and each
/// entry's Run line, and the bench's own refusals reach the flag (past the
/// table's cap, or on a run that measures no ask).
#[test]
fn the_asked_prefill_reaches_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    for n in ["0", "1024"] {
        let mut o = BTreeMap::new();
        o.insert("--asked-prefill", n.to_string());
        o.insert("--scenario", "ask".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            a.bench_words(shipped),
            format!("scenario=ask asked_prefill={n} durability=ordered horizon_ms=100")
        );
        assert!(
            a.command_line(shipped)
                .contains(&format!(" --scenario ask --asked-prefill {n} ")),
            "{}",
            a.command_line(shipped)
        );
    }
    let a = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(!a.bench_words(shipped).contains("asked_prefill"));
    for (scenario, bad) in [
        ("ask", "-1"),
        ("ask", "+5"),
        ("ask", "many"),
        ("ask", "1025"),
        ("attach", "10"),
    ] {
        let mut o = BTreeMap::new();
        o.insert("--asked-prefill", bad.to_string());
        o.insert("--scenario", scenario.to_string());
        assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
    }
}

/// C13: the messaging world's paced notes reach the bench's words and each entry's Run
/// line, and the bench's own refusals reach the flag (past their cap, or on a run that
/// measures no messaging).
#[test]
fn the_paced_notes_reach_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    for n in ["0", "60"] {
        let mut o = BTreeMap::new();
        o.insert("--paced-notes", n.to_string());
        o.insert("--scenario", "messaging".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            a.bench_words(shipped),
            format!("scenario=messaging paced_notes={n} durability=ordered horizon_ms=100")
        );
        assert!(
            a.command_line(shipped)
                .contains(&format!(" --scenario messaging --paced-notes {n} ")),
            "{}",
            a.command_line(shipped)
        );
    }
    let a = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(!a.bench_words(shipped).contains("paced_notes"));
    for (scenario, bad) in [
        ("messaging", "-1"),
        ("messaging", "many"),
        ("messaging", "546"),
        ("pingpong", "10"),
    ] {
        let mut o = BTreeMap::new();
        o.insert("--paced-notes", bad.to_string());
        o.insert("--scenario", scenario.to_string());
        assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
    }
}

/// D02: `--tally` reaches the bench's words and each entry's Run line, a run without it
/// names no tally (its worlds boot as before), and the bench's refusal of a ping-pong-only
/// run reaches the flag.
#[test]
fn the_tally_reaches_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    let mut o = BTreeMap::new();
    o.insert("--tally", String::new());
    o.insert("--scenario", "attach".to_string());
    let a = Args::from_options(&o).expect("parses");
    let words = a.bench_words(shipped);
    assert!(
        bench_spec::parse(&words, Path::new("/s"))
            .expect("the bench reads them")
            .tally
    );
    let line = a.command_line(shipped);
    assert!(
        line.split_whitespace().any(|word| word == "--tally"),
        "{line}"
    );
    let bare = Args::from_options(&BTreeMap::new()).expect("bare");
    let words = bare.bench_words(shipped);
    assert!(
        !bench_spec::parse(&words, Path::new("/s"))
            .expect("bare words")
            .tally
    );
    assert!(
        !bare
            .command_line(shipped)
            .split_whitespace()
            .any(|word| word == "--tally")
    );
    let mut o = BTreeMap::new();
    o.insert("--tally", String::new());
    o.insert("--scenario", "pingpong".to_string());
    assert!(Args::from_options(&o).is_err(), "ping-pong counts anyway");
}

#[test]
fn the_asks_and_the_mix_reach_the_bench_and_the_ledger() {
    let shipped = Arm::shipped();
    let mut o = BTreeMap::new();
    o.insert("--scenario", "ask".to_string());
    o.insert("--asks", "40".to_string());
    o.insert("--mix-every", "5".to_string());
    let a = Args::from_options(&o).expect("parses");
    let words = a.bench_words(shipped);
    assert_eq!(
        words,
        "scenario=ask asks=40 mix_every=5 durability=ordered horizon_ms=100"
    );
    let spec = bench_spec::parse(&words, Path::new("/s")).expect("the bench reads them");
    assert_eq!((spec.asks, spec.mix_every), (40, 5));
    let line = a.command_line(shipped);
    assert!(
        line.contains(" --scenario ask ") && line.contains(" --asks 40 --mix-every 5 "),
        "{line}"
    );
    let bare = Args::from_options(&BTreeMap::new()).expect("bare");
    assert!(!bare.bench_words(shipped).contains("asks"));
    for (flag, bad) in [("--asks", "0"), ("--asks", "x"), ("--mix-every", "0")] {
        let mut o = BTreeMap::new();
        o.insert(flag, bad.to_string());
        assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
    }
}

#[test]
fn the_arms_run_in_order_and_alternate_by_pass() {
    use bench_spec::Tier;
    let arm = |tier, horizon_ms| Arm {
        tier,
        horizon_ms,
        at_rest: true,
        broker_process: bench_spec::BrokerProcess::Inproc,
    };
    let mut o = BTreeMap::new();
    o.insert("--durability", "strict,ordered,relaxed".to_string());
    o.insert("--horizon-ms", "1000,100".to_string());
    let a = Args::from_options(&o).expect("parses");
    let (s, o1, o2, r) = (
        arm(Tier::Strict, None),
        arm(Tier::Ordered, Some(1000)),
        arm(Tier::Ordered, Some(100)),
        arm(Tier::Relaxed, None),
    );
    assert_eq!(a.plan(), [s, o1, o2, r]);
    assert_eq!(a.bench_words(o2), "durability=ordered horizon_ms=100");
    assert_eq!(
        a.command_line(o2),
        "targo --unverified verify perf fabric --durability ordered --horizon-ms 100"
    );
    o.insert("--passes", "2".to_string());
    let a = Args::from_options(&o).expect("parses");
    assert_eq!(a.plan(), [s, o1, o2, r, r, o2, o1, s]);
    let mut bare = BTreeMap::new();
    bare.insert("--durability", "ordered".to_string());
    let a = Args::from_options(&bare).expect("parses");
    assert_eq!(
        a.plan(),
        [arm(Tier::Ordered, Some(bench_spec::ORDERED_HORIZON_MS))]
    );
    bare.insert("--json-out", "/tmp/f.json".to_string());
    assert!(Args::from_options(&bare).is_ok(), "one run, one file");
    let mut shipped = BTreeMap::new();
    shipped.insert("--json-out", "/tmp/f.json".to_string());
    assert!(
        Args::from_options(&shipped).is_ok(),
        "no tier named: one run"
    );
    o.insert("--json-out", "/tmp/f.json".to_string());
    assert!(Args::from_options(&o).is_err(), "eight runs, one file");
}

#[test]
fn the_profile_option_picks_the_arm_its_build_and_its_command_line() {
    let dev = Args::from_options(&BTreeMap::new()).expect("parses");
    assert_eq!(dev.profile, Profile::Dev);
    assert_eq!(
        dev.build_args(),
        [
            "--profile",
            "test",
            "-p",
            "aterm",
            "--bench",
            "fabric_bench"
        ]
    );
    assert_eq!(
        dev.command_line(Arm::shipped()),
        "targo --unverified verify perf fabric --durability ordered --horizon-ms 100"
    );
    let mut o = BTreeMap::new();
    o.insert("--profile", "dev".to_string());
    assert_eq!(Args::from_options(&o), Ok(dev));

    let mut o = BTreeMap::new();
    o.insert("--profile", "opt".to_string());
    o.insert("--burst", "150".to_string());
    let opt = Args::from_options(&o).expect("parses");
    assert_eq!(opt.profile, Profile::Opt);
    assert_eq!(
        opt.build_args(),
        [
            "--profile",
            "opt-test",
            "-p",
            "aterm",
            "--bench",
            "fabric_bench"
        ]
    );
    assert_eq!(
        opt.command_line(Arm::shipped()),
        "targo --unverified verify perf fabric --profile opt --burst 150 --durability ordered \
         --horizon-ms 100"
    );
    assert_eq!(
        opt.bench_words(Arm::shipped()),
        "burst=150 durability=ordered horizon_ms=100"
    );

    let mut o = BTreeMap::new();
    o.insert("--profile", "shipped".to_string());
    let shipped = Args::from_options(&o).expect("parses");
    assert_eq!(shipped.profile, Profile::Shipped);
    assert_eq!(
        shipped.build_args(),
        [
            "--profile",
            "fabric-shipped",
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
            "--features",
            "dev-seams"
        ],
        "the shipped arm's seams come from the feature, not from debug assertions"
    );
    assert_eq!(
        shipped.command_line(Arm::shipped()),
        "targo --unverified verify perf fabric --profile shipped --durability ordered \
         --horizon-ms 100"
    );

    for bad in ["", "release", "opt-test", "fabric-shipped", "OPT"] {
        let mut o = BTreeMap::new();
        o.insert("--profile", bad.to_string());
        let why = Args::from_options(&o).expect_err(bad);
        assert!(why.contains("`dev`, `opt` or `shipped`"), "{bad}: {why}");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn profile_pilot_can_measure_without_appending_trends() {
    let ordinary = parse(&args("vte-engine")).unwrap();
    let pilot = parse(&args("vte-engine --no-trend")).unwrap();
    assert_eq!(pilot.lanes, ordinary.lanes);
    assert!(ordinary.runs_trend(true));
    assert!(!pilot.runs_trend(true));
    assert!(!ordinary.runs_trend(false));
}

#[test]
fn profile_pilot_size_ceilings_have_no_timing_admission() {
    let ordinary = parse(&args("wasm")).unwrap();
    let sizes = parse(&args("wasm --size-only --no-trend")).unwrap();
    assert!(ordinary.takes_quiet_claim(Lane::Wasm));
    assert!(!sizes.takes_quiet_claim(Lane::Wasm));
    assert!(parse(&args("vte-engine --size-only")).is_err());
    assert!(parse(&args("--record wasm --size-only")).is_err());
}

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

#[test]
fn a_bare_run_is_every_floor_lane_and_no_arena_lane() {
    let cli = parse(&[]).expect("parses");
    assert_eq!(
        cli.lanes,
        vec![
            Lane::Throughput,
            Lane::Pathological,
            Lane::ScrollScrub,
            Lane::Search,
            Lane::Restore,
            Lane::CrashRestore,
            Lane::Resize,
            Lane::Zstd,
            Lane::Effects,
            Lane::TrailDarkComposer,
            Lane::TrailLightComposer,
            Lane::Deck,
            Lane::Wasm,
            Lane::VteEngine,
            Lane::VteMarks,
            Lane::CliStart,
            Lane::FabricCounts
        ]
    );
    assert!(cli.record.is_empty());
}

#[test]
fn named_lanes_run_alone_in_canonical_order() {
    let cli = parse(&args("restore throughput restore")).expect("parses");
    assert_eq!(cli.lanes, vec![Lane::Throughput, Lane::Restore]);
}

#[test]
fn record_names_exactly_the_lanes_it_rewrites_and_runs_them() {
    let cli = parse(&args("search --record restore resize")).expect("parses");
    assert_eq!(cli.lanes, vec![Lane::Search, Lane::Restore, Lane::Resize]);
    assert_eq!(cli.record, vec![Lane::Restore, Lane::Resize]);
    assert!(parse(&args("--record")).is_err());
    let cli = parse(&args("--record restore start resize")).expect("parses");
    assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Resize, Lane::Start]);
    assert_eq!(cli.record, vec![Lane::Restore, Lane::Resize]);
    let why = parse(&args("--record start")).unwrap_err();
    assert!(why.contains("floor lanes"), "{why}");
}

#[test]
fn an_option_needs_one_of_its_lanes() {
    let cli = parse(&args("start --samples 3 --backend cpu")).expect("parses");
    assert_eq!(cli.options.get("--samples").map(String::as_str), Some("3"));
    let why = parse(&args("restore --samples 3")).unwrap_err();
    assert!(why.contains("--samples") && why.contains("start"), "{why}");
    assert!(parse(&args("start --samples")).is_err(), "a missing value");
    assert!(parse(&args("start --warm --warm")).is_err(), "given twice");
    assert!(parse(&args("bogus")).is_err());
    assert!(
        parse(&args("compare --baseline a")).is_err(),
        "both arms are required"
    );
    assert!(parse(&args("compare --baseline a --candidate b")).is_ok());
    assert!(parse(&args("--bogus")).is_err());
}

#[test]
fn the_fabric_lane_runs_only_when_named_and_takes_its_own_options() {
    let cli = parse(&args(
        "fabric --burst 200 --attach-sessions 1,4 --json-out /tmp/f.json",
    ))
    .expect("parses");
    assert_eq!(cli.lanes, vec![Lane::Fabric]);
    assert_eq!(Lane::Fabric.kind(), Kind::Measured);
    assert!(!Lane::Fabric.opens_windows());
    assert!(!parse(&[]).expect("parses").lanes.contains(&Lane::Fabric));
    let why = parse(&args("restore --burst 9")).unwrap_err();
    assert!(why.contains("--burst") && why.contains("fabric"), "{why}");
    let cli = parse(&args("--record restore fabric")).expect("parses");
    assert_eq!(cli.record, vec![Lane::Restore]);
    assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Fabric]);
}

#[test]
fn the_aos_lane_runs_only_when_named_and_writes_its_json_alone() {
    let cli = parse(&args("aos --json-out /tmp/a.json")).expect("parses");
    assert_eq!(cli.lanes, vec![Lane::Aos]);
    assert_eq!(
        cli.options.get("--json-out").map(String::as_str),
        Some("/tmp/a.json")
    );
    assert_eq!(Lane::Aos.kind(), Kind::Measured);
    assert!(!Lane::Aos.opens_windows());
    assert!(!parse(&[]).expect("parses").lanes.contains(&Lane::Aos));
    let why = parse(&args("aos --burst 9")).unwrap_err();
    assert_eq!(why, "`--burst` is read only by fabric; name one of them");
    assert!(parse(&args("aos fabric --json-out /tmp/a.json")).is_err());
    assert!(parse(&args("aos fabric")).is_ok());
    let cli = parse(&args("--record restore aos")).expect("parses");
    assert_eq!(cli.record, vec![Lane::Restore]);
    assert_eq!(cli.lanes, vec![Lane::Restore, Lane::Aos]);
}

#[test]
fn commons_quiet_is_an_explicit_measured_lane_with_one_json_destination() {
    let cli = parse(&args("commons-quiet --json-out /tmp/commons.json")).unwrap();
    assert_eq!(cli.lanes, [Lane::CommonsQuiet]);
    assert_eq!(Lane::CommonsQuiet.kind(), Kind::Measured);
    assert!(!Lane::CommonsQuiet.opens_windows());
    assert!(!parse(&[]).unwrap().lanes.contains(&Lane::CommonsQuiet));
    assert!(parse(&args("commons-quiet aos --json-out /tmp/out.json")).is_err());
    assert!(parse(&args("commons-quiet --burst 9")).is_err());
    let sustained = parse(&args("commons-quiet --sustained")).unwrap();
    assert!(sustained.options.contains_key("--sustained"));
    assert!(parse(&args("aos --sustained")).is_err());
    let catalog = parse(&args("commons-quiet --catalog --sustained")).unwrap();
    assert!(catalog.options.contains_key("--catalog"));
    assert!(catalog.options.contains_key("--sustained"));
    assert!(parse(&args("aos --catalog")).is_err());
    let long = parse(&args("commons-quiet --long-log --sustained")).unwrap();
    assert!(long.options.contains_key("--long-log"));
    assert!(parse(&args("aos --long-log")).is_err());
    assert!(parse(&args("commons-quiet --catalog --long-log")).is_err());
    assert!(parse(&args("commons-quiet --long-log --late-joiner")).is_err());
    let late = parse(&args("commons-quiet --late-joiner --sustained")).unwrap();
    assert!(late.options.contains_key("--late-joiner"));
    assert!(parse(&args("aos --late-joiner")).is_err());
    let large = parse(&args("commons-quiet --large-table --sustained")).unwrap();
    assert!(large.options.contains_key("--large-table"));
    assert!(parse(&args("aos --large-table")).is_err());
    assert!(parse(&args("commons-quiet --long-log --large-table")).is_err());
}

#[test]
fn list_names_every_lane_once_under_its_kind() {
    let listed = list();
    for lane in Lane::ALL {
        let rows = listed
            .lines()
            .filter(|l| l.starts_with("  ") && l.split_whitespace().next() == Some(lane.name()))
            .count();
        assert_eq!(rows, 1, "{}:\n{listed}", lane.name());
    }
    let mut kind_at = Vec::new();
    for line in listed.lines() {
        if !line.starts_with(' ') {
            kind_at.push(line);
        } else if let Some(lane) = line.split_whitespace().next().and_then(Lane::from_name) {
            let heading = kind_at.len().checked_sub(1).expect("a heading first");
            let kind = [Kind::Floor, Kind::Arena, Kind::Measured][heading];
            assert_eq!(lane.kind(), kind, "{}", lane.name());
        }
    }
    assert_eq!(kind_at.len(), 3, "{listed}");
}

/// The path `main` takes from a command line to each lane family's
/// options: the arena parser, then the fabric parser, each handed only its
/// own flags.
#[cfg(unix)]
#[test]
fn every_lane_family_parses_only_its_own_options_before_anything_runs() {
    let cli = parse(&args(
        "fabric --scenario pingpong --latency-samples 7 --burst 200 --pingpong-rounds 30 --attach-sessions 1,4 \
         --attach-rounds 2 --durability relaxed --json-out /tmp/f.json --no-windows --admit now",
    ))
    .expect("parses");
    let (_, fabric_args, _) = lane_args(&cli).expect("both parsers take it");
    assert_eq!(fabric_args.scenario.word(), "pingpong");
    assert_eq!(fabric_args.burst, Some(200));
    assert_eq!(fabric_args.pingpong, Some(30));
    assert_eq!(fabric_args.json_out, Some(PathBuf::from("/tmp/f.json")));
    assert_eq!(
        fabric_args
            .arms
            .iter()
            .map(|arm| arm.tier.word())
            .collect::<Vec<_>>(),
        ["relaxed"]
    );
    assert!(
        arena::Args::from_options(&options_for(&cli.options, |l| l.kind() == Kind::Arena)).is_ok()
    );
    for line in [
        "fabric --burst 6000",
        "fabric --pingpong-rounds 0",
        "fabric --attach-sessions 1,200",
        "fabric --profile release",
        "fabric --scenario unknown",
    ] {
        let cli = parse(&args(line)).expect(line);
        assert!(lane_args(&cli).is_err(), "{line}");
    }
    let cli = parse(&args("fabric --profile opt --no-windows")).expect("parses");
    let (_, fabric_args, _) = lane_args(&cli).expect("parses");
    assert_eq!(fabric_args.profile.name(), "opt");
    let cli = parse(&args(
        "restore-e2e --sessions 2,4 --arm copy --json-out /tmp/r.json --profile opt \
         --no-windows",
    ))
    .expect("parses");
    let (_, _, restore_args) = lane_args(&cli).expect("the restore parser takes it");
    assert_eq!(restore_args.sessions, Some(vec![2, 4]));
    assert_eq!(restore_args.profile.name(), "opt");
    assert_eq!(restore_args.plan().len(), 1);
    for line in [
        "restore-e2e --sessions 4,2",
        "restore-e2e --json-out /tmp/r.json",
        "restore-e2e --profile shipped",
    ] {
        let cli = parse(&args(line)).expect(line);
        assert!(lane_args(&cli).is_err(), "{line}");
    }
    assert!(
        parse(&args("fabric --sessions 4")).is_err(),
        "restore-e2e's alone"
    );
    let why = parse(&args("start --profile opt")).unwrap_err();
    assert!(why.contains("--profile") && why.contains("fabric"), "{why}");
}

#[cfg(unix)]
#[test]
fn cat_accepts_an_explicit_product_without_losing_its_identity_at_parse() {
    let cli = parse(&args("cat --binary /guest/aterm --dev-smoke")).unwrap();
    let (arena, _, _) = lane_args(&cli).unwrap();
    assert_eq!(arena.binary, Some(PathBuf::from("/guest/aterm")));
    // NEGATIVE CONTROL: this option must not redirect unrelated library floor lanes.
    assert!(parse(&args("throughput --binary /guest/aterm")).is_err());
}

/// `cli-start` is a floor lane that takes no quiet claim, records, and reads `--binary`,
/// `--samples` and `--json-out`, a bad count refused before anything runs.
#[cfg(target_os = "macos")]
#[test]
fn cli_start_is_an_unclaimed_floor_lane_with_its_own_options() {
    assert_eq!(Lane::CliStart.kind(), Kind::Floor);
    assert!(!Lane::CliStart.takes_quiet_claim());
    assert!(!Lane::CliStart.opens_windows());
    let cli = parse(&args(
        "--record cli-start --binary /a/aterm --samples 5 --json-out /tmp/c.json",
    ))
    .expect("parses");
    assert_eq!(cli.record, [Lane::CliStart]);
    let got = cli_start_args(&cli).expect("its options");
    assert_eq!(got.binary, Some(PathBuf::from("/a/aterm")));
    assert_eq!(got.samples, Some(5));
    assert_eq!(got.json_out, Some(PathBuf::from("/tmp/c.json")));
    for line in ["cli-start --samples 0", "cli-start --samples many"] {
        let cli = parse(&args(line)).expect(line);
        assert!(cli_start_args(&cli).is_err(), "{line}");
    }
    let start_only = parse(&args("start --samples 0")).expect("parses");
    assert_eq!(cli_start_args(&start_only).expect("unread").samples, None);
    assert!(parse(&args("cli-start start --json-out /tmp/c.json")).is_err());
}

/// `fabric-counts` is a floor lane in the bare run that takes no quiet claim, records, and
/// reads none of the fabric lane's knobs: its worlds are fixed, so its counts compare.
#[test]
fn fabric_counts_is_an_unclaimed_floor_lane_with_fixed_worlds() {
    assert_eq!(Lane::FabricCounts.kind(), Kind::Floor);
    assert!(!Lane::FabricCounts.takes_quiet_claim());
    assert!(!Lane::FabricCounts.opens_windows());
    assert!(
        parse(&[])
            .expect("parses")
            .lanes
            .contains(&Lane::FabricCounts)
    );
    let cli = parse(&args("--record fabric-counts")).expect("parses");
    assert_eq!(cli.record, [Lane::FabricCounts]);
    let why = parse(&args("fabric-counts --burst 9")).unwrap_err();
    assert_eq!(why, "`--burst` is read only by fabric; name one of them");
    assert!(parse(&args("fabric-counts --json-out /tmp/c.json")).is_err());
}

#[test]
fn two_lanes_never_share_one_json_out() {
    assert!(parse(&args("fabric --json-out /tmp/r.json")).is_ok());
    assert!(parse(&args("start --json-out /tmp/r.json")).is_ok());
    assert!(parse(&args("start fabric --json-out /tmp/r.json")).is_err());
    assert!(
        parse(&args(
            "start compare --baseline /a --candidate /b --json-out /tmp/r.json"
        ))
        .is_err()
    );
}

#[test]
fn no_windows_is_accepted_beside_any_lane_and_names_the_windowed_ones() {
    for line in ["fabric --no-windows", "--no-windows", "start --no-windows"] {
        let cli = parse(&args(line)).expect(line);
        assert!(cli.options.contains_key("--no-windows"), "{line}");
    }
    let windowed: Vec<&str> = Lane::ALL
        .into_iter()
        .filter(|l| l.opens_windows())
        .map(Lane::name)
        .collect();
    assert_eq!(windowed, ["start", "compare", "cat", "scroll", "vte"]);
}

#[test]
fn the_verdict_fails_on_a_failure_and_names_what_was_not_judged() {
    let pass = LaneRun::pass();
    let partial = LaneRun::not_judged("no baseline");
    let skip = LaneRun::skip("no Metal device", "run it on a Mac with a GPU");
    let fail = LaneRun::fail("x < floor");
    let measured = LaneRun::measured("ledger docs/measured/arena/x.md");
    let (text, code) = verdict(&[("throughput", &pass, 1.0)]);
    assert_eq!(code, crate::exit::PASS);
    assert!(text.contains("GREEN — every lane"), "{text}");
    let (text, code) = verdict(&[
        ("throughput", &pass, 1.0),
        ("restore", &partial, 1.0),
        ("wasm", &skip, 0.1),
    ]);
    assert_eq!(code, crate::exit::PASS);
    assert!(
        text.contains("NOT JUDGED: restore (no baseline), wasm (skipped)"),
        "{text}"
    );
    assert!(text.contains("skip  wasm"), "{text}");
    let (text, code) = verdict(&[("throughput", &pass, 1.0), ("compare", &measured, 60.0)]);
    assert_eq!(code, crate::exit::PASS);
    assert!(text.contains("meas  compare"), "{text}");
    assert!(
        text.contains("NOT JUDGED: compare (measured; no floor)"),
        "{text}"
    );
    assert!(!text.contains("GREEN — every lane"), "{text}");
    let (text, code) = verdict(&[("compare", &measured, 60.0)]);
    assert_eq!(code, crate::exit::PASS);
    assert!(
        text.contains("NOTHING JUDGED — compare (measured; no floor)"),
        "{text}"
    );
    let (text, code) = verdict(&[("throughput", &partial, 1.0), ("wasm", &skip, 0.1)]);
    assert_eq!(code, crate::exit::PASS);
    assert!(
        text.contains("NOTHING JUDGED — throughput (no baseline), wasm (skipped)"),
        "{text}"
    );
    let mut fenced = LaneRun::not_judged("no baseline");
    fenced.fence_held("resize");
    let (text, _) = verdict(&[("resize", &fenced, 1.0)]);
    assert!(
        text.contains("GREEN for what was judged; NOT JUDGED: resize floors (no baseline)"),
        "{text}"
    );
    let (text, code) = verdict(&[("throughput", &pass, 1.0), ("search", &fail, 2.0)]);
    assert_eq!(code, crate::exit::FAILED);
    assert!(text.contains("FAILED — search"), "{text}");
    let unrun = LaneRun::could_not_run(
        "no pinned chrome-headless-shell",
        "targo --unverified verify provision",
    );
    let (text, code) = verdict(&[("throughput", &pass, 1.0), ("wasm", &unrun, 0.1)]);
    assert_eq!(code, crate::exit::COULD_NOT_RUN);
    assert!(text.contains("NRUN  wasm"), "{text}");
    assert!(
        text.contains("verify perf: COULD NOT RUN — wasm could not run"),
        "{text}"
    );
    assert!(!text.contains("GREEN"), "{text}");
    let (text, code) = verdict(&[("search", &fail, 1.0), ("wasm", &unrun, 0.1)]);
    assert_eq!(code, crate::exit::FAILED);
    assert!(
        text.contains("FAILED — search; wasm could not run"),
        "{text}"
    );
}

#[test]
fn add_failure_keeps_earlier_findings() {
    let mut run = LaneRun::pass();
    run.add_failure("a");
    run.add_failure("b");
    assert_eq!(run.status, Status::Fail(vec!["a".into(), "b".into()]));
}

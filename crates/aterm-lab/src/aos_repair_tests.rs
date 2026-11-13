// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An attached status line as a debug build's `aos status` answers it (taken from a
/// headless instance with six keys typed under the demonstration fault).
const HOT: &str = "OK window=0 attached=true code=demo-stranded-settle hot=true active=true \
    settling=false build=0.110.0+geee60c4e22d2 code_digest=f337aa19938f rev=0 branch=0 \
    paused=false queued=0/1024 log=8 log_epoch=1 cursor=8 broken=false \
    checkpoint=31ecd090f577 state=e879c5317564 chain=8e764cc23cbe latched=0.3619 \
    fp=0x600100736d4915/0x60010093940ef3 clock=655412167 cold_at=4130106419 \
    query_at=none deadline=none frame_fill=6df179 frame_hot=true frame_fp=0x600100736d4915 \
    quads=72+0 replays=0 publications=0 rejections=0 delivered=8 refused=0 skipped=0 \
    queued_max=0 reanchors=0 breaks=0 restores=0 clamped=0";

fn failed(gates: &[Gate]) -> Vec<&'static str> {
    gates.iter().filter(|g| !g.pass).map(|g| g.name).collect()
}

fn hot_frame() -> Frame {
    Frame {
        fill: Some("6df179".into()),
        hot: true,
        fp: "0x600100736d4915".into(),
        out: 72,
        under: 0,
    }
}

fn inert_frame() -> Frame {
    Frame {
        fill: None,
        hot: false,
        fp: "0x0".into(),
        out: 0,
        under: 0,
    }
}

/// A stranded reading: hot and settling past `cold_at`, asked after it, no wake.
fn stranded_status(query_at: i128) -> Status {
    Status {
        attached: true,
        code: "demo-stranded-settle".into(),
        hot: true,
        settling: true,
        latched: 0.4902,
        fp: "0x1/0x1".into(),
        cold_at: Some(4_000_000_000),
        query_at: Some(query_at),
        deadline: None,
        frame: Some(hot_frame()),
        log: 400,
        ..Status::default()
    }
}

fn stranded() -> Stranded {
    Stranded {
        a: stranded_status(4_600_000_000),
        b: stranded_status(5_600_000_000),
        frames_a: 90,
        frames_b: 90,
        hold_ms: 1001,
        trail: TrailRow {
            block_fill: "momentum".into(),
            momentum_glow: 0.0,
            focused: true,
        },
        key_window: true,
    }
}

fn cold_status(rev: u64, publications: u64, delivered: u64) -> Status {
    Status {
        attached: true,
        code: "shipped".into(),
        rev,
        frame: Some(inert_frame()),
        fp: "0x0/0x1".into(),
        replays: 2 * publications,
        publications,
        delivered,
        log: 500,
        ..Status::default()
    }
}

fn quiet_trail() -> TrailRow {
    TrailRow {
        block_fill: "none".into(),
        momentum_glow: 0.0,
        focused: true,
    }
}

#[test]
fn fields_are_the_key_value_words_and_skip_the_bare_ones() {
    let f = fields("window=1 repaired paused_first=true range=0..9 code=a=b");
    assert_eq!(
        f.into_iter().collect::<Vec<_>>(),
        vec![
            ("code", "a=b"),
            ("paused_first", "true"),
            ("range", "0..9"),
            ("window", "1")
        ]
    );
}

#[test]
fn an_unattached_status_line_is_its_three_predicates() {
    let s = parse_status(
        "OK window=0 attached=false code=shipped hot=false active=false settling=false\n",
    );
    assert_eq!(
        s,
        Ok(Status {
            code: "shipped".into(),
            ..Status::default()
        })
    );
}

#[test]
fn an_attached_status_line_parses_every_field_the_drive_reads() {
    assert_eq!(
        parse_status(HOT),
        Ok(Status {
            window: 0,
            attached: true,
            code: "demo-stranded-settle".into(),
            build: Some("0.110.0+geee60c4e22d2".into()),
            guard: None,
            hot: true,
            active: true,
            settling: false,
            rev: 0,
            paused: false,
            log: 8,
            broken: false,
            latched: 0.3619,
            fp: "0x600100736d4915/0x60010093940ef3".into(),
            clock: Some(655_412_167),
            cold_at: Some(4_130_106_419),
            query_at: None,
            deadline: None,
            frame: Some(hot_frame()),
            replays: 0,
            publications: 0,
            rejections: 0,
            delivered: 8,
            refused: 0,
            reanchors: 0,
            breaks: 0,
            restores: 0,
            clamped: 0,
        })
    );
}

#[test]
fn a_status_line_before_any_tick_has_no_frame_and_signed_offsets_parse() {
    let line = HOT
        .replace(
            " frame_fill=6df179 frame_hot=true frame_fp=0x600100736d4915 quads=72+0",
            "",
        )
        .replace("clock=655412167", "clock=-5");
    let s = parse_status(&line).expect("parses");
    assert_eq!(s.frame, None);
    assert_eq!(s.clock, Some(-5));
}

#[test]
fn a_refusal_or_a_missing_field_is_an_error_naming_it() {
    assert_eq!(
        parse_status("ERR aos needs a debug build"),
        Err("not an OK reply: ERR aos needs a debug build".to_string())
    );
    assert_eq!(
        parse_status(&HOT.replace(" refused=0", "")),
        Err("no refused=".to_string())
    );
    assert_eq!(
        parse_status(&HOT.replace("hot=true active", "hot=yes active")),
        Err("hot=yes is not a boolean".to_string())
    );
    assert_eq!(
        parse_status(&HOT.replace("quads=72+0", "quads=72")),
        Err("quads= is not <out>+<under>".to_string())
    );
}

#[test]
fn a_repair_line_parses_and_a_status_line_is_not_one() {
    let line = "OK window=0 repaired paused_first=true rev=1 code=momentum-glow.shipped \
        live=momentum-glow.demo-stranded-settle live_verdict=fail:HotHasWake@803 \
        candidate_verdict=pass same_code_reproduced=true grade=tested trace=0123456789ab \
        ancestor=31ecd090f577 range=0..812 log_epoch=1 delivered=0";
    assert_eq!(
        parse_repair(line),
        Ok(Repair {
            window: 0,
            paused_first: true,
            rev: 1,
            code: "momentum-glow.shipped".into(),
            live: "momentum-glow.demo-stranded-settle".into(),
            live_verdict: "fail:HotHasWake@803".into(),
            candidate_verdict: "pass".into(),
            same_code_reproduced: true,
            grade: "tested".into(),
            trace: "0123456789ab".into(),
            ancestor: "31ecd090f577".into(),
            range: "0..812".into(),
            log_epoch: 1,
            delivered: 0,
        })
    );
    assert!(
        parse_repair(HOT)
            .expect_err("a status line")
            .starts_with("not a repair reply: ")
    );
}

/// An `aos trace` line of the agreed shape: the summary, then the seal and the
/// per-kind counts at its end.
const TRACE: &str = "OK window=0 log=8 log_epoch=1 first=0 end=8 \
    trace=0356c26b403b57eb92e88d0323cc7c2d3e3de743dcb3e6a7f203383fd6a79c30 \
    checkpoint=31ecd090f577e87f65549fe2b74ac40c921a79cc8a5795079008b9194ddea798 \
    identity=10464a926549 broken=false bytes=536+959 sealed=true keys=6 resets=0 ticks=1 \
    queries=1";

#[test]
fn a_trace_line_parses_with_its_seal_and_per_kind_counts() {
    assert_eq!(
        parse_trace(TRACE),
        Ok(TraceLine {
            log: 8,
            log_epoch: 1,
            first: 0,
            end: 8,
            trace: "0356c26b403b57eb92e88d0323cc7c2d3e3de743dcb3e6a7f203383fd6a79c30".into(),
            checkpoint: "31ecd090f577e87f65549fe2b74ac40c921a79cc8a5795079008b9194ddea798".into(),
            broken: false,
            bytes: "536+959".into(),
            sealed: true,
            keys: 6,
            resets: 0,
            ticks: 1,
            queries: 1,
        })
    );
}

/// The trace line the host printed before the seal and the counts existed is not a
/// trace the drive can judge: each missing word is named.
#[test]
fn a_trace_line_without_its_seal_or_a_count_is_an_error_naming_it() {
    let unsealed = TRACE.replace(" sealed=true keys=6 resets=0 ticks=1 queries=1", "");
    assert_eq!(parse_trace(&unsealed), Err("no sealed=".to_string()));
    assert_eq!(
        parse_trace(&TRACE.replace(" resets=0", "")),
        Err("no resets=".to_string())
    );
    assert_eq!(
        parse_trace(&TRACE.replace("sealed=true", "sealed=1")),
        Err("sealed=1 is not a boolean".to_string())
    );
    assert_eq!(
        parse_trace(&TRACE.replace("keys=6", "keys=six")),
        Err("keys=six is not a number".to_string())
    );
}

#[test]
fn a_sealed_log_of_the_typed_keys_and_no_reset_passes_and_each_miscount_fails() {
    let t = parse_trace(TRACE).expect("parses");
    let pass = log_kinds_gate("k", &t, 6);
    assert!(pass.pass, "{}", pass.observed);
    assert_eq!(
        pass.observed,
        "sealed=true keys=6 typed=6 resets=0 ticks=1 queries=1 log=8"
    );

    let fails = |t: &TraceLine, typed: u64| !log_kinds_gate("k", t, typed).pass;
    assert!(fails(&t, 5));
    assert!(fails(&t, 7));
    let reset = TraceLine {
        resets: 1,
        queries: 0,
        ..t.clone()
    };
    assert!(fails(&reset, 6));
    let unsealed = TraceLine {
        sealed: false,
        ..t.clone()
    };
    assert!(fails(&unsealed, 6));
    let uncounted = TraceLine {
        log: 9,
        end: 9,
        ..t.clone()
    };
    assert!(fails(&uncounted, 6));
    let huge = TraceLine {
        ticks: u64::MAX,
        ..t
    };
    assert!(fails(&huge, 6));
}

#[test]
fn the_post_commit_trace_is_the_new_log_with_no_key_or_reset() {
    let r = Repair {
        range: "0..169".into(),
        log_epoch: 1,
        ..repaired()
    };
    let t = TraceLine {
        log: 12,
        log_epoch: 2,
        first: 169,
        end: 181,
        trace: "ab".repeat(32),
        sealed: true,
        ticks: 9,
        queries: 3,
        ..TraceLine::default()
    };
    assert_eq!(failed(&post_commit_gates(&r, &t)), Vec::<&str>::new());

    let old = TraceLine {
        log_epoch: 1,
        first: 0,
        keys: 24,
        log: 36,
        ..t.clone()
    };
    assert_eq!(
        failed(&post_commit_gates(&r, &old)),
        vec!["post-commit-log", "post-commit-no-key-or-reset"]
    );
    let elsewhere = TraceLine {
        first: 170,
        ..t.clone()
    };
    assert_eq!(
        failed(&post_commit_gates(&r, &elsewhere)),
        vec!["post-commit-log"]
    );
    let reset = TraceLine {
        resets: 1,
        queries: 2,
        ..t.clone()
    };
    assert_eq!(
        failed(&post_commit_gates(&r, &reset)),
        vec!["post-commit-no-key-or-reset"]
    );
    let key = TraceLine {
        keys: 1,
        queries: 2,
        ..t
    };
    assert_eq!(
        failed(&post_commit_gates(&r, &key)),
        vec!["post-commit-no-key-or-reset"]
    );
}

#[test]
fn a_trail_status_row_gives_the_fill_owner_the_integrator_and_focus() {
    let row = "trail style=\"off\" resolved=off config_enabled=false effective=false \
        focused=true motion=full momentum=0.00 momentum_display=0.00 momentum_glow=0.37 \
        block_fill=momentum block_fill_rgb=6df179 block_fill_base=ffffff";
    assert_eq!(
        parse_trail(row),
        Ok(TrailRow {
            block_fill: "momentum".into(),
            momentum_glow: 0.37,
            focused: true,
        })
    );
    assert_eq!(
        parse_trail("OK 1"),
        Err("not a trail status row: OK 1".to_string())
    );
}

#[test]
fn an_image_reply_gives_its_path_last_with_or_without_meta() {
    assert_eq!(
        parse_image("OK 794 462 /private/tmp/at.x/run/images/a.png\n"),
        Ok(Shot {
            width: 794,
            height: 462,
            path: "/private/tmp/at.x/run/images/a.png".into(),
            phase: None,
        })
    );
    assert_eq!(
        parse_image(
            "OK 794 462 image-meta image-meta-version=3 frame-kind=terminal \
             frame-phase=destination window=0 path=\"/private/tmp/at.x/run/images/b.png\""
        ),
        Ok(Shot {
            width: 794,
            height: 462,
            path: "/private/tmp/at.x/run/images/b.png".into(),
            phase: Some("destination".into()),
        })
    );
    assert_eq!(
        parse_image("OK 794 462"),
        Err("not an image reply: 794 462".to_string())
    );
    assert!(parse_image("ERR image: no window to capture").is_err());
}

#[test]
fn metrics_frames_is_read_by_name() {
    let line = "backend=gpu rows=14 cols=72 frames=12 n_present=3";
    assert_eq!(count(line, "frames"), Ok(12));
    assert_eq!(
        count(line, "capture_presents"),
        Err("no capture_presents=".into())
    );
}

#[test]
fn a_stranded_reading_passes_and_each_symptom_fails_its_own_gate() {
    let e = stranded();
    assert_eq!(failed(&stranded_gates(&e)), Vec::<&str>::new());

    let mut presented = e.clone();
    presented.frames_b = 91;
    assert_eq!(
        failed(&stranded_gates(&presented)),
        vec!["no-present-while-stranded"]
    );

    let mut short = e.clone();
    short.hold_ms = 999;
    assert_eq!(
        failed(&stranded_gates(&short)),
        vec!["no-present-while-stranded"]
    );

    let mut woke = e.clone();
    woke.b.deadline = Some(5_610_000_000);
    assert_eq!(failed(&stranded_gates(&woke)), vec!["no-wake-named"]);

    let mut early = e.clone();
    early.a.query_at = Some(3_900_000_000);
    assert_eq!(
        failed(&stranded_gates(&early)),
        vec!["stranded-hot-past-cold"]
    );

    let mut cooled = e.clone();
    cooled.b.hot = false;
    cooled.b.settling = false;
    assert_eq!(
        failed(&stranded_gates(&cooled)),
        vec!["stranded-hot-past-cold"]
    );

    let mut ticked = e.clone();
    ticked.b.latched = 0.4863;
    assert_eq!(
        failed(&stranded_gates(&ticked)),
        vec!["last-tick-unchanged"]
    );

    let mut untinted = e.clone();
    untinted.trail.block_fill = "none".into();
    assert_eq!(
        failed(&stranded_gates(&untinted)),
        vec!["halo-on-last-frame"]
    );

    let mut outline = untinted.clone();
    outline.key_window = false;
    assert_eq!(failed(&stranded_gates(&outline)), Vec::<&str>::new());
    let mut claimed = e.clone();
    claimed.key_window = false;
    assert_eq!(
        failed(&stranded_gates(&claimed)),
        vec!["halo-on-last-frame"]
    );
    let mut dark = outline;
    dark.b.frame = Some(inert_frame());
    dark.a.frame = Some(inert_frame());
    assert!(failed(&stranded_gates(&dark)).contains(&"halo-on-last-frame"));

    let mut warm = e.clone();
    warm.trail.momentum_glow = 0.05;
    assert_eq!(failed(&stranded_gates(&warm)), vec!["integrator-cold"]);

    let mut shipped = e;
    shipped.b.code = "shipped".into();
    assert_eq!(failed(&stranded_gates(&shipped)), vec!["fault-attached"]);
}

fn repaired() -> Repair {
    Repair {
        paused_first: true,
        rev: 1,
        code: "momentum-glow.shipped".into(),
        live: "momentum-glow.demo-stranded-settle".into(),
        live_verdict: "fail:HotHasWake@803".into(),
        candidate_verdict: "pass".into(),
        same_code_reproduced: true,
        grade: "tested".into(),
        ..Repair::default()
    }
}

#[test]
fn a_repair_witness_passes_and_each_wrong_word_fails_its_own_gate() {
    let before = stranded_status(5_600_000_000);
    assert_eq!(
        failed(&repair_gates(&repaired(), &before)),
        Vec::<&str>::new()
    );

    let mut passed_live = repaired();
    passed_live.live_verdict = "pass".into();
    assert_eq!(
        failed(&repair_gates(&passed_live, &before)),
        vec!["live-fails-hot-has-wake"]
    );

    let mut other_law = repaired();
    other_law.live_verdict = "fail:ColdTickClears@12".into();
    assert_eq!(
        failed(&repair_gates(&other_law, &before)),
        vec!["live-fails-hot-has-wake"]
    );

    let mut same_rev = repaired();
    same_rev.rev = 0;
    assert_eq!(
        failed(&repair_gates(&same_rev, &before)),
        vec!["new-revision"]
    );

    let mut queued = repaired();
    queued.delivered = 2;
    assert_eq!(
        failed(&repair_gates(&queued, &before)),
        vec!["nothing-queued"]
    );
}

#[test]
fn a_cleared_reading_passes_and_a_missing_present_or_halo_fails() {
    let e = Cleared {
        frames_before: 90,
        frames_after: 92,
        status: cold_status(1, 1, 400),
        repaired_rev: 1,
        trail: quiet_trail(),
        outside_caret_px: Some(0),
    };
    assert_eq!(failed(&cleared_gates(&e)), Vec::<&str>::new());

    let mut unpresented = e.clone();
    unpresented.frames_after = 90;
    assert_eq!(
        failed(&cleared_gates(&unpresented)),
        vec!["clearing-frame-presented"]
    );

    let mut halo = e.clone();
    halo.outside_caret_px = Some(311);
    assert_eq!(failed(&cleared_gates(&halo)), vec!["clearing-pixels"]);

    let mut unmeasured = e.clone();
    unmeasured.outside_caret_px = None;
    assert_eq!(failed(&cleared_gates(&unmeasured)), vec!["clearing-pixels"]);

    let mut lit = e.clone();
    lit.status.frame = Some(hot_frame());
    assert_eq!(failed(&cleared_gates(&lit)), vec!["clearing-tick-inert"]);

    let mut old_code = e;
    old_code.status.code = "demo-stranded-settle".into();
    assert_eq!(failed(&cleared_gates(&old_code)), vec!["cold-after-repair"]);
}

fn cooled() -> Cooled {
    let mut hot = cold_status(1, 1, 450);
    hot.hot = true;
    hot.active = true;
    let mut settling = cold_status(1, 1, 460);
    settling.hot = true;
    settling.settling = true;
    settling.query_at = Some(9_000_000_000);
    settling.deadline = Some(9_009_000_000);
    Cooled {
        hot,
        settling,
        frames_settling: 120,
        cold: cold_status(1, 1, 700),
        frames_cold: 240,
        trail: quiet_trail(),
        outside_caret_px: Some(0),
    }
}

#[test]
fn a_self_cooling_glow_passes_and_a_silent_settle_fails() {
    let e = cooled();
    assert_eq!(failed(&cooled_gates(&e)), Vec::<&str>::new());

    let mut no_wake = e.clone();
    no_wake.settling.deadline = None;
    assert_eq!(failed(&cooled_gates(&no_wake)), vec!["settling-names-wake"]);

    let mut parked = e.clone();
    parked.frames_cold = parked.frames_settling;
    assert_eq!(failed(&cooled_gates(&parked)), vec!["cleared-by-itself"]);

    let mut refused = e;
    refused.cold.refused = 1;
    assert_eq!(failed(&cooled_gates(&refused)), vec!["admitted"]);
}

#[test]
fn the_continuation_holds_the_published_pair_and_the_trace_to_the_status() {
    let committed = cold_status(1, 1, 400);
    let after = cold_status(1, 1, 700);
    let trace = TraceLine {
        log: 500,
        first: 0,
        end: 500,
        trace: "ab".repeat(32),
        ..TraceLine::default()
    };
    assert_eq!(
        failed(&continuation_gates(&committed, &after, &after, &trace)),
        Vec::<&str>::new()
    );

    let mut republished = after.clone();
    republished.publications = 2;
    assert_eq!(
        failed(&continuation_gates(
            &committed,
            &republished,
            &after,
            &trace
        )),
        vec!["no-further-replay-or-publication"]
    );

    let mut reanchored = after.clone();
    reanchored.reanchors = 1;
    assert_eq!(
        failed(&continuation_gates(&committed, &reanchored, &after, &trace)),
        vec!["no-further-replay-or-publication"]
    );

    let mut torn = trace.clone();
    torn.broken = true;
    torn.trace = "none".into();
    assert_eq!(
        failed(&continuation_gates(&committed, &after, &after, &torn)),
        vec!["trace-agrees"]
    );

    let mut longer = trace;
    longer.log = 501;
    longer.end = 501;
    assert_eq!(
        failed(&continuation_gates(&committed, &after, &after, &longer)),
        vec!["trace-agrees"]
    );

    let undelivered = cold_status(1, 1, 400);
    assert_eq!(
        failed(&continuation_gates(
            &committed,
            &undelivered,
            &after,
            &TraceLine {
                log: 500,
                end: 500,
                trace: "ab".into(),
                ..TraceLine::default()
            }
        )),
        vec!["fresh-keys-delivered"]
    );
}

#[test]
fn the_control_needs_the_shipped_code_never_replaced() {
    let attached = Status {
        attached: true,
        code: "shipped".into(),
        ..Status::default()
    };
    let cold = cold_status(0, 0, 700);
    assert_eq!(failed(&control_gates(&attached, &cold)), Vec::<&str>::new());
    let fault = Status {
        code: "demo-stranded-settle".into(),
        ..attached
    };
    assert_eq!(
        failed(&control_gates(&fault, &cold_status(1, 1, 700))),
        vec!["shipped-attached", "no-publication"]
    );
}

#[test]
fn the_result_line_names_the_failed_gates_and_quotes_the_extras() {
    let gates = [
        gate("a", true, String::new()),
        gate("b", false, String::new()),
        gate("c", false, String::new()),
    ];
    assert_eq!(
        result_line(
            "repair",
            &gates,
            &[
                ("n_present", "41".to_string()),
                ("out", "/tmp/x y".to_string())
            ]
        ),
        "aos-repair result=fail mode=repair gates=1/3 failed=b,c n_present=41 out=/tmp/x_y"
    );
    assert_eq!(
        result_line("control", &gates[..1], &[]),
        "aos-repair result=pass mode=control gates=1/1 failed=none"
    );
    assert_eq!(
        result_line("control", &[], &[]),
        "aos-repair result=fail mode=control gates=0/0 failed=none"
    );
}

#[test]
fn the_instance_config_lights_the_glow_alone_and_holds_the_deck_off() {
    let doc: aterm_toml::Value = aterm_toml::from_str(&world_config()).expect("valid TOML");
    assert_eq!(doc["cursor_momentum_glow"].as_bool(), Some(true));
    assert_eq!(doc["cursor_trail_style"].as_str(), Some("off"));
    assert_eq!(doc["cursor_glow_sdr_boost"].as_float(), Some(0.0));
    assert_eq!(doc["sys"]["stats_bars"].as_str(), Some("off"));
    assert_eq!(doc["harness"]["enabled"].as_bool(), Some(false));
    assert_eq!(doc["update"]["enabled"].as_bool(), Some(false));
}

#[test]
fn the_typed_words_are_the_in_process_tests_counts_of_printable_keys() {
    assert_eq!(typed(STRAND_KEYS), 24);
    assert_eq!(typed(FRESH_KEYS), 12);
    assert!(
        STRAND_KEYS
            .chars()
            .chain(FRESH_KEYS.chars())
            .all(|c| c.is_ascii_lowercase())
    );
}

/// The grid winner's 12 bytes, as `--publish` and `publish plan=` spell them.
const WINNER_HEX: &str = "010100000000000000000000";

fn args(words: &[&str]) -> Result<Args, Fail> {
    parse_args(&words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>())
}

fn usage(words: &[&str]) -> String {
    let f = args(words).expect_err("a usage error");
    assert_eq!(f.code, 2, "{words:?}: usage");
    f.msg.unwrap_or_default()
}

/// The arguments name one mode (no flag the repair, `--control`, or `--publish` with a plan's 24 hex digits of either case), and every malformed form is a usage error.
#[test]
fn the_arguments_name_one_mode_and_publish_takes_a_plans_24_hex_digits() {
    let plain = args(&[]).expect("no flag");
    assert_eq!(
        plain,
        Args {
            aterm: None,
            out: None,
            mode: Mode::Repair,
            keep: false
        }
    );
    assert_eq!(args(&["--control"]).expect("control").mode, Mode::Control);
    let winner = [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        args(&["--publish", WINNER_HEX]).expect("publish").mode,
        Mode::Publish(winner)
    );
    assert_eq!(
        args(&["--publish", "0101000000000000000000AB"])
            .expect("upper case")
            .mode,
        Mode::Publish([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xab])
    );
    assert_eq!(
        args(&[
            "--keep",
            "--publish",
            WINNER_HEX,
            "--aterm",
            "/a/aterm",
            "--out",
            "/o"
        ])
        .expect("every flag"),
        Args {
            aterm: Some("/a/aterm".into()),
            out: Some("/o".into()),
            mode: Mode::Publish(winner),
            keep: true
        }
    );
    assert_eq!(usage(&["--publish"]), "--publish needs a value");
    for bad in [
        "0101",
        "0101000000000000000000000",
        "01010000000000000000000000",
        "zz0100000000000000000000",
        "",
    ] {
        assert_eq!(
            usage(&["--publish", bad]),
            format!("aos-repair: --publish takes a plan's 12 bytes as 24 hex digits, not '{bad}'")
        );
    }
    assert!(
        usage(&["--control", "--publish", WINNER_HEX]).contains("two modes; give one"),
        "both modes"
    );
    assert!(usage(&["--bogus"]).starts_with("aos-repair: unexpected '--bogus'"));
    for (mode, name, stem, code) in [
        (Mode::Repair, "repair", "aos-repair", "demo-stranded-settle"),
        (Mode::Control, "control", "aos-control", "shipped"),
        (Mode::Publish(winner), "publish", "aos-publish", "shipped"),
    ] {
        assert_eq!(
            (mode.name(), mode.stem(), mode.attaches()),
            (name, stem, code)
        );
    }
}

/// A window's publish reply, in the verb's `publish_line` shape.
fn publish_reply() -> String {
    format!(
        "OK window=0 published paused_first=true rev=1 plan={WINNER_HEX} \
         plan_digest=4e3c0f2a9b1d useful=true gain_ppm=234255 s0_gain_ppm=118000 \
         code=momentum-glow.schedule-plan live=momentum-glow.shipped live_verdict=pass \
         candidate_verdict=pass trace=9f00aa11bb22 ancestor=00ff00ff00ff range=0..1742 \
         log_epoch=1 grade=tested bundle=c0ffee0123ab delivered=0"
    )
}

/// A publish reply parses with its witness half, and a refusal, a repair reply and a reply without the witness are errors naming why.
#[test]
fn a_publish_reply_parses_with_its_witness_and_a_refusal_is_an_error() {
    let p = parse_publish(&publish_reply()).expect("a publish reply");
    assert_eq!(
        p,
        PublishLine {
            window: 0,
            paused_first: true,
            rev: 1,
            plan: WINNER_HEX.into(),
            plan_digest: "4e3c0f2a9b1d".into(),
            useful: true,
            gain_ppm: 234_255,
            s0_gain_ppm: 118_000,
            code: PLAN_CODE.into(),
            live: "momentum-glow.shipped".into(),
            live_verdict: "pass".into(),
            candidate_verdict: "pass".into(),
            trace: "9f00aa11bb22".into(),
            ancestor: "00ff00ff00ff".into(),
            range: "0..1742".into(),
            log_epoch: 1,
            grade: "tested".into(),
            bundle: "c0ffee0123ab".into(),
            delivered: 0,
        }
    );
    assert!(
        parse_publish("ERR aos: the plan is not useful over the shipped code")
            .expect_err("a refusal")
            .starts_with("not an OK reply: ERR aos:")
    );
    assert!(
        parse_publish("OK window=0 repaired paused_first=true rev=1")
            .expect_err("a repair reply")
            .starts_with("not a publish reply")
    );
    let unwitnessed = publish_reply().replace(" code=momentum-glow.schedule-plan", "");
    assert_eq!(parse_publish(&unwitnessed), Err("no code=".to_string()));
    let signed = publish_reply().replace("gain_ppm=234255", "gain_ppm=-5");
    assert_eq!(parse_publish(&signed).map(|p| p.gain_ppm), Ok(-5));
}

/// One word of a publish reply made wrong.
type Spoil = fn(&mut PublishLine);

/// A publication over the shipped code passes every gate, each wrong word fails its own, and the plan named in upper case is the same plan.
#[test]
fn a_publication_passes_its_gates_and_each_wrong_word_fails_its_own() {
    let p = parse_publish(&publish_reply()).expect("a publish reply");
    let before = cold_status(0, 0, 700);
    assert_eq!(
        failed(&publish_gates(&p, WINNER_HEX, &before)),
        Vec::<&str>::new()
    );
    assert_eq!(
        failed(&publish_gates(
            &p,
            &WINNER_HEX.to_ascii_uppercase(),
            &before
        )),
        Vec::<&str>::new()
    );
    let cases: [(&str, Spoil); 9] = [
        ("publish-paused-first", |p| p.paused_first = false),
        ("publish-paused-first", |p| p.delivered = 1),
        ("publish-plan", |p| {
            p.plan = "010100000000000000000001".into()
        }),
        ("publish-useful", |p| p.useful = false),
        ("publish-gain", |p| p.gain_ppm = 0),
        ("publish-new-revision", |p| p.rev = 0),
        ("plan-over-shipped", |p| {
            p.code = "momentum-glow.shipped".into()
        }),
        ("publish-witness", |p| {
            p.candidate_verdict = "fail:HotHasWake@3".into()
        }),
        ("publish-bundle-digest", |p| {
            p.bundle = "C0FFEE0123AB".into()
        }),
    ];
    for (name, wrong) in cases {
        let mut q = p.clone();
        wrong(&mut q);
        assert_eq!(failed(&publish_gates(&q, WINNER_HEX, &before)), vec![name]);
    }
    let mut graded = p.clone();
    graded.grade = "replayed".into();
    assert_eq!(
        failed(&publish_gates(&graded, WINNER_HEX, &before)),
        vec!["publish-witness"]
    );

    let live = Status {
        code: PLAN_NAME.into(),
        guard: Some("moved:false,firings:0".into()),
        ..cold_status(1, 1, 700)
    };
    assert!(plan_live_gate(&live, &p).pass);
    assert!(
        plan_live_gate(&live, &p)
            .observed
            .ends_with(" guard=moved:false,firings:0")
    );
    for wrong in [
        Status {
            code: "shipped".into(),
            ..live.clone()
        },
        Status {
            rev: 2,
            ..live.clone()
        },
        Status {
            paused: true,
            ..live.clone()
        },
        Status {
            publications: 2,
            ..live.clone()
        },
    ] {
        assert!(!plan_live_gate(&wrong, &p).pass, "{wrong:?}");
    }
}

/// A bundle reply parses; its bytes are its hex decoded, exactly as long as it says.
#[test]
fn a_bundle_reply_parses_and_its_bytes_are_as_long_as_it_says() {
    let digest = "ab".repeat(32);
    let line = format!("OK window=0 bundle={digest} rev=1 bytes=3 hex=00ff7a");
    let b = parse_bundle_line(&line).expect("a bundle reply");
    assert_eq!(
        b,
        BundleLine {
            window: 0,
            bundle: digest,
            rev: 1,
            bytes: 3,
            hex: "00ff7a".into()
        }
    );
    assert_eq!(bundle_bytes(&b), Ok(vec![0x00, 0xff, 0x7a]));
    let long = BundleLine {
        bytes: 4,
        ..b.clone()
    };
    assert_eq!(
        bundle_bytes(&long),
        Err("hex= holds 3 bytes, bytes=4".to_string())
    );
    let odd = BundleLine {
        hex: "00f".into(),
        ..b
    };
    assert!(bundle_bytes(&odd).is_err());
    assert!(parse_bundle_line("ERR aos: the live code was published by no plan").is_err());
    assert_eq!(
        parse_bundle_line("OK window=0 bundle=ab rev=1 bytes=0"),
        Err("no hex=".to_string())
    );
}

/// A trace hex reply carries the trace file: its hex decoded, as long as it says.
#[test]
fn a_trace_hex_reply_carries_the_trace_file_it_counts() {
    let line = "OK window=0 log=3 log_epoch=2 first=10 end=13 trace=ab checkpoint=cd \
                identity=ef broken=false bytes=1+2 sealed=true keys=0 resets=0 ticks=2 \
                queries=1 file_bytes=3 hex=414f53";
    assert_eq!(trace_file(line), Ok(b"AOS".to_vec()));
    assert_eq!(
        parse_trace(line).map(|t| (t.log, t.ticks, t.queries)),
        Ok((3, 2, 1)),
        "the summary half is an `aos trace` line"
    );
    assert_eq!(
        trace_file(&line.replace("file_bytes=3", "file_bytes=4")),
        Err("hex= holds 3 bytes, file_bytes=4".to_string())
    );
    assert_eq!(
        trace_file(&line.replace(" hex=414f53", "")),
        Err("no hex=".to_string())
    );
    assert_eq!(parse_cursor("OK 3 17 1 steady_block\n"), Some((3, 17)));
    assert_eq!(parse_cursor("ERR no tab"), None);
    assert_eq!(cell_word(Some((3, 17))), "3,17");
    assert_eq!(cell_word(None), "none");
}

/// The 12 fresh keys after a publication are judged by the first burst's gates under their own `fresh-` names and the plan's status name, and the continuation holds the plan's revision.
#[test]
fn the_fresh_burst_is_judged_by_the_first_bursts_gates_under_its_own_names() {
    let mut e = cooled();
    e.hot.code = PLAN_NAME.into();
    let gates = cooled_gates_in(Burst::Fresh, PLAN_NAME, &e);
    assert_eq!(failed(&gates), Vec::<&str>::new());
    assert_eq!(
        gates.iter().map(|g| g.name).collect::<Vec<_>>(),
        [
            "fresh-keys-heat",
            "fresh-settling-names-wake",
            "fresh-cleared-by-itself",
            "fresh-last-tick-inert",
            "fresh-cooled-pixels",
            "fresh-admitted"
        ]
    );
    assert_eq!(
        failed(&cooled_gates_in(Burst::Fresh, "shipped", &e)),
        vec!["fresh-keys-heat"]
    );
    assert_eq!(
        cooled_gates(&cooled()),
        cooled_gates_in(Burst::First, "shipped", &cooled())
    );
    let committed = Status {
        code: PLAN_NAME.into(),
        ..cold_status(1, 1, 400)
    };
    let after = Status {
        code: PLAN_NAME.into(),
        ..cold_status(1, 1, 700)
    };
    let trace = TraceLine {
        log: 500,
        end: 500,
        trace: "ab".repeat(32),
        ..TraceLine::default()
    };
    assert_eq!(
        failed(&continuation_gates_for(
            PLAN_NAME, &committed, &after, &after, &trace
        )),
        Vec::<&str>::new()
    );
    assert_eq!(
        failed(&continuation_gates(&committed, &after, &after, &trace)),
        vec!["same-published-pair"],
        "the repair's continuation is the shipped code's"
    );
    let r = Repair {
        range: "0..169".into(),
        log_epoch: 1,
        ..repaired()
    };
    let t = TraceLine {
        log: 12,
        log_epoch: 2,
        first: 169,
        end: 181,
        sealed: true,
        ticks: 9,
        queries: 3,
        ..TraceLine::default()
    };
    assert_eq!(
        post_commit_gates(&r, &t),
        post_commit_gates_at(1, "0..169", &t)
    );
}

/// The caret moves at one instant after the last key whenever the settling reading came, and a move at or past the cold instant or past the hang detector is refused.
#[test]
fn the_caret_moves_at_one_instant_after_the_last_key_whatever_the_code() {
    let last_key = Instant::now();
    let after = |ms: u64| last_key + Duration::from_millis(ms);
    let cold = Some(after(4_600));
    for read in [105, 630, 2_632] {
        let wait = move_wait(after(read), last_key, cold).expect("in the tail");
        assert_eq!(
            after(read) + wait,
            last_key + MOVE_AFTER_LAST_KEY,
            "a settling reading {read} ms after the last key"
        );
    }
    assert_eq!(
        move_wait(after(3_500), last_key, cold),
        Ok(Duration::ZERO),
        "a reading past the move's instant moves at once"
    );
    for early_cold in [after(3_000), after(2_000)] {
        let why = move_wait(after(105), last_key, Some(early_cold)).expect_err("past cold");
        assert!(why.contains("cold instant"), "{why}");
    }
    let why = move_wait(last_key, after(61_000), None).expect_err("a hang");
    assert!(why.contains("hang detector"), "{why}");
}

/// A real publication's bundle as a window of the build `build` exports it: the grid winner admitted by an evaluator of that build and published over a paused shipped glow.
fn published_bundle(build: &str) -> (Vec<u8>, u64) {
    use aterm_effects::aos_momentum::{MomentumCode, MomentumHost};
    let origin = Instant::now();
    let ev = Evaluator::v1(build, origin).expect("the pinned policy");
    let winner = SchedulePlan::decode(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("a plan");
    let admitted = ev
        .admit_one(&winner.encode())
        .expect("the winner is admitted");
    let mut host = MomentumHost::default();
    host.aos_attach(origin, MomentumCode::Shipped, build, 1);
    host.aos_pause(origin, Duration::from_nanos(16_666_667))
        .expect("a running glow pauses");
    let published = host.aos_publish(admitted).expect("the winner publishes");
    (published.bundle().encode(), published.revision().0)
}

/// A window's bundle is verified in the lab only under the same clean commit and refused offline at any commit, and the bundle gates pass only on the exported, verified bundle the publication names.
#[test]
fn a_published_bundle_passes_its_gates_and_is_verified_only_under_the_same_clean_commit() {
    let build = "0.0.0+g0123456789ab";
    let lab = "aterm-lab 0.0.0+g0123456789ab";
    let (bytes, rev) = published_bundle(build);
    assert_eq!(verify_in_lab(&bytes, build, lab), Verify::Verified);
    for offline_lab in [lab, "aterm-lab 0.0.0+g0123456789ac"] {
        let (code, r) = crate::aos_search::verify_bundle(
            &bytes,
            "w.bundle",
            offline_lab,
            || -> Result<&Evaluator, aterm_effects::aos_momentum::policy::PolicyRefusal> {
                panic!("an evaluator was built for a window's bundle")
            },
        );
        assert_eq!(code, 1, "{r}");
        assert!(
            r.ends_with("aos-search verify=fail reason=ForeignBuild\n"),
            "{offline_lab}: {r}"
        );
    }
    assert!(matches!(
        verify_in_lab(&bytes[..bytes.len() - 1], build, lab),
        Verify::Refused(_)
    ));
    for (instance, lab) in [
        (build, "aterm-lab 0.0.0+g0123456789ac"),
        (build, "aterm-lab 0.0.0+g0123456789ab-dirty"),
        (
            "0.0.0+g0123456789ab-dirty",
            "aterm-lab 0.0.0+g0123456789ab-dirty",
        ),
        ("0.0.0+gunknown", "aterm-lab 0.0.0+gunknown"),
        ("none", lab),
    ] {
        let v = verify_in_lab(&bytes, instance, lab);
        assert_eq!(
            v,
            Verify::Foreign {
                instance: instance.into(),
                lab: lab.into()
            }
        );
        assert!(
            v.word().starts_with("verify=skipped(foreign build) "),
            "{}",
            v.word()
        );
    }

    let digest = Digest::of(BUNDLE_DOMAIN, &[&bytes]).hex();
    let b = BundleLine {
        window: 0,
        bundle: digest.clone(),
        rev,
        bytes: bytes.len(),
        hex: aterm_codec::hex::encode_lower(&bytes),
    };
    let p = PublishLine {
        rev,
        bundle: digest[..12].to_string(),
        ..parse_publish(&publish_reply()).expect("a publish reply")
    };
    let plan = [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        failed(&bundle_gates(&b, &p, &plan, &Verify::Verified)),
        Vec::<&str>::new()
    );
    let skipped = Verify::Foreign {
        instance: build.into(),
        lab: "aterm-lab 0.0.0+g0123456789ac".into(),
    };
    assert_eq!(
        failed(&bundle_gates(&b, &p, &plan, &skipped)),
        vec!["bundle-verifies"]
    );
    let later = PublishLine {
        rev: rev + 1,
        ..p.clone()
    };
    assert_eq!(
        failed(&bundle_gates(&b, &later, &plan, &Verify::Verified)),
        vec!["bundle-exported", "bundle-publication"]
    );
    let other = PublishLine {
        bundle: "000000000000".into(),
        ..p.clone()
    };
    assert_eq!(
        failed(&bundle_gates(&b, &other, &plan, &Verify::Verified)),
        vec!["bundle-exported"]
    );
    let mut another = plan;
    another[11] = 1;
    assert_eq!(
        failed(&bundle_gates(&b, &p, &another, &Verify::Verified)),
        vec!["bundle-plan"]
    );
    let cut = BundleLine {
        hex: aterm_codec::hex::encode_lower(&bytes[..bytes.len() - 1]),
        bytes: bytes.len() - 1,
        ..b
    };
    assert_eq!(
        failed(&bundle_gates(&cut, &p, &plan, &Verify::Verified)),
        vec!["bundle-exported", "bundle-plan", "bundle-publication"]
    );
}

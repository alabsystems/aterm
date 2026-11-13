// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_aos_verb_parses_its_forms_and_a_release_build_refuses_only_the_debug_ones() {
    let demo = MomentumCode::DemoStrandedSettle;
    for (rest, want) in [
        ("", Asked::Status),
        ("status", Asked::Status),
        ("  status ", Asked::Status),
        ("attach", Asked::Attach(MomentumCode::Shipped)),
        ("attach code=shipped", Asked::Attach(MomentumCode::Shipped)),
        (
            "attach code=momentum-glow.shipped",
            Asked::Attach(MomentumCode::Shipped),
        ),
        ("pause", Asked::Pause),
        ("repair", Asked::Repair(MomentumCode::Shipped)),
        ("repair code=shipped", Asked::Repair(MomentumCode::Shipped)),
        ("resume", Asked::Resume),
        ("detach", Asked::Detach),
        ("trace", Asked::Trace),
        ("trace hex", Asked::TraceHex),
        (" trace  hex ", Asked::TraceHex),
    ] {
        assert_eq!(parse(rest, true), Ok(want), "{rest:?}");
        assert_eq!(
            parse(rest, false),
            Ok(want),
            "a release build answers {rest:?}"
        );
    }
    for (rest, want, form) in [
        (
            "attach code=demo-stranded-settle",
            Asked::Attach(demo),
            "code=demo-stranded-settle",
        ),
        (
            "attach code=momentum-glow.demo-stranded-settle",
            Asked::Attach(demo),
            "code=momentum-glow.demo-stranded-settle",
        ),
        (
            "repair code=demo-stranded-settle",
            Asked::Repair(demo),
            "code=demo-stranded-settle",
        ),
        ("bundle", Asked::Bundle, "bundle"),
        (" bundle  ", Asked::Bundle, "bundle"),
    ] {
        assert_eq!(parse(rest, true), Ok(want), "{rest:?}");
        assert_eq!(
            parse(rest, false),
            Err(format!("aos {form} needs a debug build")),
            "a release build refuses {rest:?} by name"
        );
    }
    for junk in [
        "bogus",
        "status now",
        "attach shipped",
        "attach code=",
        "attach code=shipped extra",
        "pause now",
        "repair code=nope",
        "resume 1",
        "trace 5",
        "trace HEX",
        "trace hex 5",
        "bundle now",
        "bundle plan=010100000000000000000000",
    ] {
        let refusal = parse(junk, true).expect_err(junk);
        assert!(refusal.starts_with(USAGE), "{junk:?}: {refusal}");
    }
    for junk in ["bogus", "attach code=", "repair code=nope", "trace HEX"] {
        let refusal = parse(junk, false).expect_err(junk);
        assert!(refusal.starts_with(USAGE), "release {junk:?}: {refusal}");
    }
    assert_eq!(
        parse("repair code=nope", true),
        Err(format!(
            "{USAGE} (no code `nope`; this build carries shipped, safe, demo-stranded-settle)"
        ))
    );
}

#[test]
fn r2s_forms_parse_in_every_build_and_check_takes_the_rest_of_the_line() {
    for debug in [true, false] {
        for (rest, want) in [
            ("ls", Form::Main(Asked::Ls)),
            (" ls ", Form::Main(Asked::Ls)),
            (
                "attach code=safe",
                Form::Main(Asked::Attach(MomentumCode::Safe)),
            ),
            (
                "repair code=momentum-glow.safe",
                Form::Main(Asked::Repair(MomentumCode::Safe)),
            ),
            ("export", Form::Export(None)),
            ("export kept=3", Form::Export(Some(3))),
            ("check x.trace", Form::Check("x.trace".into())),
            (
                "  check /a dir/with space.trace  ",
                Form::Check("/a dir/with space.trace".into()),
            ),
            ("status", Form::Main(Asked::Status)),
        ] {
            assert_eq!(parse_form(rest, debug), Ok(want), "debug={debug} {rest:?}");
        }
        for junk in [
            "ls now",
            "export kept=",
            "export kept=-1",
            "export 3",
            "export kept=1 extra",
            "check",
            "check   ",
            "checkx",
            "lsx",
        ] {
            let refusal = parse_form(junk, debug).expect_err(junk);
            assert!(
                refusal.starts_with(USAGE),
                "debug={debug} {junk:?}: {refusal}"
            );
        }
    }
    assert_eq!(
        parse_form("attach code=demo-stranded-settle", false),
        Err("aos code=demo-stranded-settle needs a debug build".to_string())
    );
}

#[test]
fn the_export_and_check_lines_name_what_they_wrote_and_replayed() {
    let e = Exported {
        window: crate::WindowId(4),
        kept: None,
        log_epoch: 2,
        identity: aterm_aos::Digest([0xcd; 32]),
        bytes: vec![0; 10],
        unseen: 3,
    };
    assert_eq!(
        export_line(&e, "f.trace"),
        format!(
            "window=4 exported kept=live log_epoch=2 identity={} bytes=10 unseen=3 mode=0600 \
             file=f.trace",
            aterm_aos::Digest([0xcd; 32]).short()
        )
    );
    let kept = Exported {
        kept: Some(1),
        ..e.clone()
    };
    assert!(export_line(&kept, "g").contains(" kept=1 "));
    let name = export_name(&e);
    assert!(
        name.starts_with("momentum-w4-live-e2-") && name.ends_with(".trace"),
        "{name}"
    );
    assert_ne!(export_name(&e), name, "each name is fresh");
    assert!(crate::pinned_dir::is_one_component(std::ffi::OsStr::new(
        &name
    )));
}

const WINNER_HEX: &str = "010100000000000000000000";

#[test]
fn publish_parses_a_plans_twelve_bytes_and_refuses_every_malformed_form() {
    let mut want = [0u8; PLAN_BYTES];
    want[0] = 1;
    want[1] = 1;
    for rest in [
        format!("publish plan={WINNER_HEX}"),
        format!("  publish   plan={}  ", WINNER_HEX.to_uppercase()),
    ] {
        assert_eq!(parse(&rest, true), Ok(Asked::Publish(want)), "{rest:?}");
        assert_eq!(
            parse(&rest, false),
            Err("aos publish needs a debug build".to_string()),
            "a release build refuses {rest:?} by name"
        );
    }
    assert_eq!(
        parse("publish plan=0a0B0c0D0e0F102030405060", true),
        Ok(Asked::Publish([
            0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60
        ]))
    );
    let short = &WINNER_HEX[..22];
    for junk in [
        "publish".to_string(),
        "publish plan=".to_string(),
        format!("publish {WINNER_HEX}"),
        format!("publish code={WINNER_HEX}"),
        format!("publish plan={short}"),
        format!("publish plan={WINNER_HEX}0"),
        format!("publish plan={WINNER_HEX}00"),
        format!("publish plan=0x{short}"),
        format!("publish plan={short}zz"),
        format!("publish plan={short} 00"),
        format!("publish plan={WINNER_HEX} extra"),
        "repair code=schedule-plan".to_string(),
        "repair code=momentum-glow.schedule-plan".to_string(),
        "attach code=schedule-plan".to_string(),
    ] {
        let refusal = parse(&junk, true).expect_err(&junk);
        assert!(refusal.starts_with(USAGE), "{junk:?}: {refusal}");
    }
    assert_eq!(
        parse(&format!("publish plan={short}zz"), true),
        Err(format!(
            "{USAGE} (plan= takes 24 hex digits, a schedule plan's 12 bytes; got `{short}zz`)"
        ))
    );
    assert_eq!(hex_of(&want), WINNER_HEX);
}

/// The evaluation refuses, naming why, bytes that are not a plan and two plans the training split refuses, and admits the grid winner as useful in the build it was given with this build's toolchain declared.
#[cfg(debug_assertions)]
#[test]
fn evaluate_refuses_a_plan_the_evaluator_does_not_admit() {
    use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
    if super::refused_off_the_pinned_target() {
        return;
    }
    let build = "aos-verb-test-build";
    let not_a_plan = evaluate(build, &[0u8; PLAN_BYTES]).expect_err("not a plan");
    assert!(
        not_a_plan.starts_with("aos publish: the plan is not admitted: candidate refused: "),
        "{not_a_plan}"
    );
    for (wake, law) in [(Wake::ColdOnly, "NoStepSlept"), (Wake::Never, "HotHasWake")] {
        let plan = SchedulePlan::new(Split::Shipped, wake)
            .expect("in range")
            .encode();
        let refused = evaluate(build, &plan).expect_err(law);
        assert!(
            refused.starts_with("aos publish: the plan is not admitted: ") && refused.contains(law),
            "{law}: {refused}"
        );
    }
    let winner = parse(&format!("publish plan={WINNER_HEX}"), true);
    let Ok(Asked::Publish(plan)) = winner else {
        panic!("the winner parses: {winner:?}");
    };
    let admitted = evaluate(build, &plan).expect("the grid winner is admitted");
    assert!(admitted.useful());
    assert_eq!(admitted.bundle().parts().build, build);
    assert_eq!(
        admitted.bundle().parts().toolchain,
        crate::build_info::compiler_version_line()
    );
}

#[test]
fn an_unattached_status_line_names_the_window_and_the_shipped_code() {
    let s = MomentumAosStatus {
        code: MomentumCode::Shipped.name(),
        ..MomentumAosStatus::default()
    };
    assert_eq!(
        status_line(3, &s, true),
        "window=3 attached=false code=shipped hot=false active=false settling=false"
    );
    assert_eq!(token(Some("log full")), "log_full");
    assert_eq!(token(None), "none");
    assert_eq!(offset_word(Some(-5)), "-5");
    assert_eq!(verdict_word(&Verdict::Pass), "pass");
    assert_eq!(
        verdict_word(&Verdict::Fail {
            cursor: 7,
            law: "HotHasWake",
            detail: "a settling glow named no wake".to_string(),
        }),
        "fail:HotHasWake@7"
    );
}

#[test]
fn a_plans_status_line_names_its_guard_and_a_prebuilt_codes_names_none() {
    let plan = MomentumAosStatus {
        attached: true,
        code: "schedule-plan",
        guard_moved: Some(true),
        guard_firings: Some(3),
        ..MomentumAosStatus::default()
    };
    let line = status_line(1, &plan, false);
    assert!(
        line.contains(" settling=false unseen=off build="),
        "an attached line says whether the grant is live: {line}"
    );
    assert!(
        line.contains(" deadline=none guard=moved:true,firings:3 replays=0 "),
        "{line}"
    );
    let shipped = MomentumAosStatus {
        attached: true,
        code: MomentumCode::Shipped.name(),
        ..MomentumAosStatus::default()
    };
    let line = status_line(1, &shipped, true);
    assert!(line.contains(" unseen=on "), "{line}");
    assert!(
        line.contains(" deadline=none replays=0 ") && !line.contains(" guard="),
        "{line}"
    );
}

#[test]
fn the_trace_line_ends_with_the_seal_and_the_count_of_each_kind() {
    let d = |b: u8| aterm_aos::Digest([b; 32]);
    let mut t = TraceSummary {
        window: crate::WindowId(2),
        log_len: 10,
        log_epoch: 3,
        first: 40,
        end: 50,
        trace: Some(d(0xab)),
        checkpoint: d(0x01),
        identity: d(0xcd),
        broken: None,
        checkpoint_bytes: 120,
        log_bytes: 900,
        counts: aterm_effects::aos_momentum::EventCounts {
            keys: 4,
            unseen: 1,
            resets: 1,
            ticks: 3,
            queries: 2,
        },
        unseen: 2,
    };
    let ab = "ab".repeat(32);
    let one = "01".repeat(32);
    assert_eq!(
        trace_line(&t),
        format!(
            "window=2 log=10 log_epoch=3 first=40 end=50 trace={ab} checkpoint={one} \
             identity={} broken=false bytes=120+900 sealed=true keys=4 resets=1 ticks=3 queries=2 \
             unseen=2",
            d(0xcd).short()
        )
    );
    t.trace = None;
    t.broken = Some("log full");
    assert_eq!(
        trace_line(&t),
        format!(
            "window=2 log=10 log_epoch=3 first=40 end=50 trace=none checkpoint={one} \
             identity={} broken=true bytes=120+900 broken_why=log_full sealed=true keys=4 \
             resets=1 ticks=3 queries=2 unseen=2",
            d(0xcd).short()
        )
    );
    assert_eq!(
        trace_file_line(&t, &[0x41, 0x4f, 0x53, 0x00, 0xff]),
        format!("{} file_bytes=5 hex=414f5300ff", trace_line(&t))
    );
    assert_eq!(
        trace_file_line(&t, &[]),
        format!("{} file_bytes=0 hex=", trace_line(&t))
    );
}

/// `aos check` replays a pane trace (`rewind check export`) under its own bound, past the momentum trace's, and refuses one another build wrote or a forged byte.
#[cfg(unix)]
#[test]
fn aos_check_replays_a_pane_trace_past_the_momentum_bound_and_refuses_a_forged_one() {
    use aterm_aos_pane::{PaneClock, PaneCode, PaneEnv, PaneState, project, screen_digest};
    use aterm_core::terminal::{ClockReading, Terminal};

    let origin = ClockReading::now();
    let env = PaneEnv {
        config: aterm_core::config::TerminalConfig::default(),
        policy: aterm_core::grid::ResizePolicy::Native,
        host_resyncs_alt_screen: false,
        host_alternate_scroll: true,
        cell_px: None,
        security_policy: None,
        hyperlink_schemes: Vec::new(),
        kitty_media_admitted: false,
    };
    let start = PaneState::new(
        project(&Terminal::new(4, 20)).expect("a blank screen projects"),
        env,
    );
    let mut live_engine = start.engine();
    let mut rec = aterm_aos_pane::trace::PaneRecording::new(origin, start);
    let big: std::sync::Arc<[u8]> = vec![b'x'; CHECK_MAX_BYTES + 1].into();
    let clock = PaneClock {
        offset_ns: 0,
        wall_ms: None,
    };
    live_engine.process_at(&big, rec.schema().reading(clock));
    let reply = live_engine.take_response().unwrap_or_default();
    rec.bytes(big, Some(clock), &reply);
    rec.witness(screen_digest(
        &live_engine.checkpoint_visible().expect("at ground"),
    ));
    let bytes = rec
        .trace(PaneCode::ThisBuild, "b")
        .expect("the trace builds")
        .encode();
    assert!(
        bytes.len() > CHECK_MAX_BYTES,
        "precondition: past the momentum bound"
    );
    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let path = write_export(root.path(), "p.aos", &bytes).expect("writes");
    let line = check_file(&path, "b").expect("the pane trace replays");
    assert!(
        line.starts_with(
            "checked task=aterm.pane.screen code=this-build slices=1 resizes=0 witnesses=1 \
             matched=1 replies=1 "
        ) && line.ends_with(&format!("verdict=pass bytes={} file=p.aos", bytes.len())),
        "{line}"
    );
    let other = check_file(&path, "another-build").expect_err("another build's trace");
    assert!(other.starts_with("aos check refused=identity"), "{other}");
    let mut forged = bytes.clone();
    let mid = forged.len() / 2;
    forged[mid] ^= 1;
    let path = write_export(root.path(), "q.aos", &forged).expect("writes");
    let refused = check_file(&path, "b").expect_err("a forged byte");
    assert!(refused.starts_with("aos check refused=seal"), "{refused}");
}

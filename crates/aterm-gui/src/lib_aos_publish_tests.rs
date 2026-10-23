// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A schedule plan published over the front window's glow on the host path, in process with every instant pinned (design §10.6), so the lane's one-shot wakes are exact and a windowed host's coarse-slot rounding (design §10.8) goes unmeasured.

use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_effects::aos_momentum::bundle::{BUNDLE_DOMAIN, CandidateBundle};
use aterm_effects::aos_momentum::m4::Evaluator;
use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
use aterm_effects::aos_momentum::{MomentumCode, MomentumEvent};

use crate::App;
use crate::aos_repair_tests::{
    KEY_GAP, Parked, WID, exported_log, momentum_app, presented_glow, run_lane, status, type_keys,
};
use crate::aos_verb::{Ask, Asked, UNEVALUATED, evaluate, parse};
use crate::scripted_redraw_fixture::surface;

/// When the fresh keys start on both windows, from the attach: far past either window's first clear (τ·ln 50 is about 4.7 s after the last key).
const FRESH_AFTER: Duration = Duration::from_secs(20);

/// A plan's 12 bytes.
fn bytes(split: Split, wake: Wake) -> [u8; 12] {
    SchedulePlan::new(split, wake)
        .expect("fixture: an in-range plan")
        .encode()
}

/// The grid winner's bytes.
fn winner() -> [u8; 12] {
    bytes(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep)
}

/// What one window did after its pause.
struct Story {
    app: App,
    /// The lane from the pause until it parked.
    first: Parked,
    /// The lane from the last fresh key until it parked.
    fresh: Parked,
}

/// A window attached under the shipped code at `t0` and paused after 24 keys, then published over with the grid winner (or, as the control, resumed), its lane run to park before and after 12 fresh keys from `t0 + FRESH_AFTER`.
fn story(light: bool, t0: Instant, publish: bool) -> Story {
    let mut app = momentum_app(light);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("the front window attaches");
    let last = type_keys(&mut app, t0 + KEY_GAP, 24);
    assert!(status(&app).hot, "light={light}: the keys lit the glow");
    app.aos_pause_at(last).expect("a running glow pauses");
    if publish {
        // The host's build, as it attached: the evaluator judges the interpreter it runs.
        let build = status(&app).build.expect("attached");
        let admitted = evaluate(&build, &winner()).expect("the grid winner is admitted");
        assert!(admitted.useful(), "fixture: the winner is useful");
        let published = app
            .aos_publish_at(admitted, last)
            .expect("the winner publishes over the shipped code");
        assert_eq!(published.window, WID);
        assert!(!published.paused_first, "the caller paused it");
        let s = status(&app);
        assert_eq!(s.code, "schedule-plan");
        assert_eq!(s.revision, published.published.revision().0);
        assert!(!s.paused);
        assert_eq!(s.stats.publications, 1);
    } else {
        app.aos_resume_now().expect("the control resumes");
    }
    let first = run_lane(&mut app, last);
    assert!(
        !status(&app).hot && presented_glow(&app, light) == 0,
        "light={light}, publish={publish}: the first burst cleared"
    );
    let fresh_from = t0 + FRESH_AFTER;
    assert!(
        first.at < fresh_from,
        "fixture: cleared before the fresh keys"
    );
    let before = status(&app).stats;
    let last_fresh = type_keys(&mut app, fresh_from, 12);
    assert!(
        status(&app).hot,
        "light={light}, publish={publish}: fresh keys heat it"
    );
    let commits = surface(&mut app).commits();
    let fresh = run_lane(&mut app, last_fresh);
    assert!(
        fresh.saw_settling && fresh.settle_wakes > 0,
        "light={light}, publish={publish}: it settled and the scheduler woke it"
    );
    assert!(
        fresh.last_presented && surface(&mut app).commits() > commits,
        "light={light}, publish={publish}: the clearing frame was presented"
    );
    assert_eq!(presented_glow(&app, light), 0);
    assert_eq!(app.windows[&WID].block_fill, None, "no momentum tint");
    let after = status(&app);
    assert!(!after.hot && !after.is_active && !after.settling);
    assert_eq!(
        (
            after.stats.replays,
            after.stats.publications,
            after.stats.rejections
        ),
        (before.replays, before.publications, before.rejections),
        "light={light}, publish={publish}: no replay, publication or rejection for fresh keys"
    );
    assert_eq!(after.stats.refused, 0, "every pinned instant admitted");
    Story { app, first, fresh }
}

/// The plan published over a paused shipped glow settles the fresh keys in fewer lane wakes than the control's shipped code on both pages, each fresh key reaching the plan once with no reset recorded.
#[test]
fn fresh_keys_after_publication_clear_in_fewer_ticks_than_a_control_host() {
    for light in [false, true] {
        let t0 = Instant::now();
        let control = story(light, t0, false);
        let published = story(light, t0, true);
        eprintln!(
            "light={light}: lane wakes after the fresh keys: control {} (settle {}), published {} \
             (settle {}); after the pause: control {}, published {}",
            control.fresh.wakes,
            control.fresh.settle_wakes,
            published.fresh.wakes,
            published.fresh.settle_wakes,
            control.first.wakes,
            published.first.wakes
        );
        assert!(
            published.fresh.wakes < control.fresh.wakes,
            "light={light}: the published plan clears in fewer ticks: {} against {}",
            published.fresh.wakes,
            control.fresh.wakes
        );
        assert_eq!(status(&control.app).code, "shipped");
        let log = exported_log(&published.app, t0);
        assert_eq!(
            log.iter()
                .filter(|e| matches!(e, MomentumEvent::Key { .. }))
                .count(),
            12,
            "light={light}: each fresh key reached the published pair once"
        );
        assert!(
            !log.iter().any(|e| matches!(e, MomentumEvent::Reset)),
            "light={light}: no reset since the commit"
        );
    }
}

/// The `Wake::Aos` handler's `publish` refuses a bare ask without change and a plan not useful over the shipped code with its pause undone, and answers the winner's line with the plan live.
#[test]
fn the_publish_ask_answers_its_line_and_a_refusal_resumes_the_glow() {
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("the front window attaches");
    let now = type_keys(&mut app, t0 + KEY_GAP, 12);
    let build = status(&app).build.expect("attached");

    assert_eq!(
        app.control_aos(Asked::Publish(winner()), now),
        Err(UNEVALUATED.to_string())
    );
    assert!(!status(&app).paused, "the bare ask paused nothing");

    let cadence = evaluate(&build, &bytes(Split::Shipped, Wake::Cadence))
        .expect("the cadence plan is admitted");
    assert!(!cadence.useful(), "fixture: not useful");
    let refused = app
        .control_aos(Ask::Publish(Box::new(cadence)), now)
        .expect_err("not useful over the shipped code");
    assert!(refused.contains("is not useful"), "{refused}");
    let s = status(&app);
    assert!(!s.paused, "the pause this call took is undone");
    assert_eq!((s.code, s.revision), ("shipped", 0));
    assert_eq!(s.stats.rejections, 1);

    let admitted = evaluate(&build, &winner()).expect("the grid winner is admitted");
    let digest = admitted.plan().digest().short();
    let line = app
        .control_aos(Ask::Publish(Box::new(admitted)), now)
        .expect("the winner publishes");
    let hex: String = winner().iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        line.starts_with(&format!(
            "window=0 published paused_first=true rev=1 plan={hex} plan_digest={digest} \
             useful=true gain_ppm="
        )),
        "{line}"
    );
    for want in [
        " code=momentum-glow.schedule-plan ",
        " live=momentum-glow.shipped ",
        " live_verdict=pass candidate_verdict=pass ",
        " grade=tested bundle=",
    ] {
        assert!(line.contains(want), "{want:?} in {line}");
    }
    let s = status(&app);
    assert_eq!((s.code, s.revision, s.paused), ("schedule-plan", 1, false));
}

/// The value of `key=` in a `k=v` line.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let want = format!("{key}=");
    line.split(' ')
        .find_map(|word| word.strip_prefix(want.as_str()))
        .unwrap_or_else(|| panic!("no {key}= in {line}"))
}

/// The bytes lowercase hex spells.
fn unhex(hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2),
        "an even number of digits: {hex}"
    );
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("ascii");
            u8::from_str_radix(pair, 16).expect("a hex digit pair")
        })
        .collect()
}

/// `aos bundle` exports the live plan's bundle, which verifies in the host's build with its publication part and toolchain line, until a later publication replaces it or a repair, re-attach or detach drops it.
#[test]
fn the_bundle_ask_exports_the_live_plans_bundle_until_the_windows_code_changes() {
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("the front window attaches");
    let mut now = type_keys(&mut app, t0 + KEY_GAP, 12);
    let build = status(&app).build.expect("attached");
    let ask = parse("bundle", true).expect("`aos bundle` parses");
    let refused = |app: &mut App, now: Instant, after: &str| {
        let refusal = app
            .control_aos(ask, now)
            .expect_err(&format!("no published plan is live {after}"));
        assert!(
            refusal.contains("no published plan is live"),
            "{after}: {refusal}"
        );
    };
    let publish = |app: &mut App, now: Instant| {
        let admitted = evaluate(&build, &winner()).expect("the grid winner is admitted");
        app.control_aos(Ask::Publish(Box::new(admitted)), now)
            .expect("the winner publishes")
    };
    refused(&mut app, now, "before any publication");

    let line = publish(&mut app, now);
    let exported = app
        .control_aos(ask, now)
        .expect("the live plan's bundle is exported");
    assert_eq!(field(&exported, "window"), "0", "{exported}");
    assert_eq!(field(&exported, "rev"), field(&line, "rev"), "{exported}");
    let bytes = unhex(field(&exported, "hex"));
    assert_eq!(field(&exported, "bytes"), bytes.len().to_string());
    let digest = Digest::of(BUNDLE_DOMAIN, &[&bytes]);
    assert_eq!(field(&exported, "bundle"), digest.hex());
    assert_eq!(field(&line, "bundle"), digest.short());
    let evaluator = Evaluator::v1(&build, now).expect("the pinned policy");
    let verified = CandidateBundle::verify(&bytes, &evaluator)
        .expect("the exported bundle verifies in the host's build");
    assert_eq!(verified.encode(), bytes);
    let parts = verified.parts();
    assert_eq!(parts.plan, winner());
    assert_eq!(parts.build, build);
    assert_eq!(
        parts.toolchain,
        crate::build_info::compiler_version_line(),
        "the verb declares the host's toolchain"
    );
    let p = parts.publication.as_ref().expect("the publication part");
    assert_eq!(p.revision.to_string(), field(&line, "rev"));
    assert_eq!(p.candidate_code.name, field(&line, "code"));
    assert_eq!(p.live_code.name, field(&line, "live"));

    now += KEY_GAP;
    app.control_aos(Asked::Repair(MomentumCode::Shipped), now)
        .expect("the shipped code repairs over the plan");
    refused(&mut app, now, "after a repair");

    now += KEY_GAP;
    let again = publish(&mut app, now);
    let replaced = app
        .control_aos(ask, now)
        .expect("the later publication's bundle");
    assert_eq!(field(&replaced, "rev"), field(&again, "rev"));
    assert_ne!(field(&replaced, "rev"), field(&exported, "rev"));
    assert_eq!(
        &field(&replaced, "bundle")[..12],
        field(&again, "bundle"),
        "{replaced}"
    );

    now += KEY_GAP;
    app.control_aos(Asked::Attach(MomentumCode::Shipped), now)
        .expect("a re-attach");
    refused(&mut app, now, "after a re-attach");

    let _ = publish(&mut app, now);
    app.control_aos(ask, now)
        .expect("the re-attached window's publication");
    app.control_aos(Asked::Detach, now).expect("a detach");
    refused(&mut app, now, "after a detach");
}

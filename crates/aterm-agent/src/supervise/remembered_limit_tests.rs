// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};

/// Compose the measured limit/resume screens with a shell that outlived the
/// turn, as the measured background-work fixtures draw it. No live turn runs.
fn with_shell(mut rows: Vec<String>) -> Vec<String> {
    let done = rows
        .iter()
        .rposition(|r| r.starts_with('✻'))
        .expect("done row");
    rows[done].push_str(" · 1 shell still running");
    let footer = rows
        .iter()
        .rposition(|r| r.contains("auto mode on"))
        .expect("mode row");
    rows[footer].push_str(" · 1 shell");
    assert!(aterm_phase::background_only(&rows), "{rows:#?}");
    rows
}

/// The session's actual reader and policy fold, with no real-time wait.
fn fold(
    s: &mut Session<'_, Mock>,
    rows: Vec<String>,
    opts: &SuperviseOpts,
) -> (TurnEndReading, TurnEndAction) {
    let screen = read_screen(rows);
    let reading = s.turn_end_reading(&screen, None, opts);
    let now = Instant::now();
    s.observe_point(&reading, now, screen.seq);
    let action = decide_turn_end(&s.turn_end, &reading, &opts.policy, now);
    (reading, action)
}

/// A remembered wall must not manufacture a live countdown from the notice
/// it saved. With a background shell the raw resumed screen reads Busy, so
/// the memory used to reinstate its old auto-continue promise. The same
/// screen with a countdown still drawn remains the vendor's own wait.
#[test]
fn a_remembered_limit_uses_the_countdown_still_drawn_under_background_work() {
    use aterm_phase::prompt::fixtures::{RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, screen};
    for armed in [false, true] {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            reset_grace: Duration::from_secs(7),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        let opts = hosted(30);
        let (_, first) = fold(&mut s, with_shell(armed_conversation_b()), &opts);
        assert!(
            matches!(first, TurnEndAction::WaitUntil { .. }),
            "{first:?}"
        );
        let mut resumed = with_shell(screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300));
        if armed {
            resumed.push("    Continuing automatically at 9:19am · esc to cancel".to_owned());
        }
        let (reading, action) = fold(&mut s, resumed, &opts);
        let reset = reading.reset_at.expect("the remembered reset");
        let expected = if armed {
            s.turn_end.timing.auto_resume_grace
        } else {
            s.turn_end.timing.reset_grace
        };
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { until, why, .. }
                if *until == reset + expected && why == if armed { "usage-session goes on by itself" } else { "usage-session resets" }),
            "armed={armed}: the live countdown determines the wait: {action:?}\n{reading:#?}"
        );
        assert!(s.turn_end.waits_on_named_reset());
        assert!(s.wall_memory.is_some(), "the limit itself still stands");
    }
}

/// An actual different-family /model result ends the remembered bucket even
/// while a shell keeps the raw screen Busy. Same-family and unknown results
/// keep the bucket and its ledger open, including after the next screen read.
#[test]
fn a_remembered_bucket_obeys_the_newest_model_choice_under_background_work() {
    use aterm_phase::prompt::fixtures::{WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, screen};
    for (chosen, leaves) in [
        ("Opus 5.5", true),
        ("Sonnet 5.5", false),
        ("new-default", false),
    ] {
        let (dir, ledger) = ledger_at(&format!("remembered-model-{chosen}"));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_owned()));
        // The capture names Oct 17: put the test two days before its reset,
        // on Oct 15, rather than the older watch fixtures' September clock.
        s.set_clock(TEST_NOW + 28 * 86_400, PDT);
        s.set_zone_lookup(test_zones);
        s.set_approval_ledger(Some(ledger.clone()));
        // Direct folds do not enter Session::watch, which opens the ledger.
        // Use that same initialization so persist_wall and forget_wall write
        // their real rows, rather than seeding either outcome in this test.
        let mut notes = Vec::new();
        s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
        assert!(notes.is_empty(), "the scratch ledger opens: {notes:?}");
        let mut opts = hosted(30);
        opts.policy.model_fallback = None;
        opts.policy.human_grace_s = 0;
        // Learn the bucket from the measured screen first. Its last done
        // row precedes /rate-limit-options: a synthetic hard shell there
        // would correctly require a later done row before admitting the
        // footer as the wall the turn ended on.
        let armed = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
        let (initial, action) = fold(&mut s, armed.clone(), &opts);
        assert!(
            initial.wall == Some(aterm_phase::WallKind::ModelBucket { consent: false })
                && initial.reset_at.is_some()
                && s.turn_end.waits_on_named_reset(),
            "{chosen}: the measured bucket must seed a named wait: {initial:#?}\n{action:?}"
        );
        assert!(
            approvals::open_wall(&ledger, Some("@s-1")).is_some(),
            "the initial read must persist its wait: {initial:#?}\n{action:?}\n{}",
            std::fs::read_to_string(&ledger).unwrap_or_default()
        );
        let mut rows: Vec<String> = armed
            .into_iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("composer");
        rows.splice(
            top..top,
            [
                "❯ /model".to_owned(),
                format!("  ⎿  Set model to {chosen}"),
                String::new(),
            ],
        );
        let rows = with_shell(rows);
        assert!(aterm_phase::background_only(&rows));
        assert!(
            aterm_phase::wall(&rows).is_none(),
            "only the memory supplies a wall"
        );
        for _ in 0..2 {
            let (reading, action) = fold(&mut s, rows.clone(), &opts);
            if leaves {
                assert!(
                    !s.turn_end.waits_on_named_reset(),
                    "{chosen}: the bucket was left: {reading:#?}"
                );
                assert!(
                    s.wall_memory.is_none(),
                    "{chosen}: no stale wall can return"
                );
                assert!(approvals::open_wall(&ledger, Some("@s-1")).is_none());
                // The first point can resume the task; a later raw background
                // screen alone is no newly completed turn.
                assert!(
                    action.rule_id() == Some(RULE_LIMIT_RESUME)
                        || matches!(action, TurnEndAction::Nothing),
                    "{action:?}"
                );
            } else {
                assert!(
                    s.turn_end.waits_on_named_reset(),
                    "{chosen}: the bucket still binds"
                );
                assert!(s.wall_memory.is_some());
                assert!(approvals::open_wall(&ledger, Some("@s-1")).is_some());
                assert!(
                    matches!(action, TurnEndAction::WaitUntil { .. }),
                    "{action:?}"
                );
            }
        }
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}

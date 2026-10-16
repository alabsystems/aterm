// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Upgrade notices queued behind a usage limit, replayed from a recorded Claude Code 2.1.280
//! session through the real driver over the login wall tests' apparatus, and the Tier-1 bind
//! of `harness_upgrade_limit_queue_model` (aterm-spec).

use super::*;

/// The owner's conversation.
const OWNERS_SESSION: &str = "25e3b26e-36c9-41cc-bcd0-d0e0b6a70828";

/// The four notices' READY markers, in the order they were typed (the round's
/// salt `1790373039`).
const MARKERS: [&str; 4] = [
    "ATERM-UPGRADE-READY-504f8ea5",
    "ATERM-UPGRADE-READY-558d0228",
    "ATERM-UPGRADE-READY-921fe1d7",
    "ATERM-UPGRADE-READY-6a57ff7a",
];

/// The round's salt, from the owner's state file.
const OWNERS_SALT: u64 = 1_790_373_039;

/// The recorded state file after a give-up behind the limit, every byte but the tab kept.
const OWNERS_STATE: &str = r#"{"agent":"","asks":0,"at":0,"cause":"","codex_home":"","confirm_by":0,"cwd":"","done_at":0,"exited_at":0,"from":"2.1.280","hold_seen":1790539070,"hold_since":1790539070,"last_seq":4735,"launch_model":"","left_at":0,"left_typed":"","line":"","mark":0,"marker":"ATERM-UPGRADE-READY-6a57ff7a","mode":"","model_before":"","model_list":"","noted":"","notice_pid":6099,"notice_start":"Thu Sep 24 02:03:13 2026","outcome":"","pending_since":0,"phase":"failed","pid":0,"prompt":"","ready_since":0,"release":"","request":"-","request_at":0,"request_tab":"","resumed_on":"","resumed_pid":0,"salt":1790373039,"seq_since":1790539070,"shell":0,"source":"managed","tab":"s-c543f4e0edd3439e5791","thread":"","to":"2.1.283","twin":"","wait":"failed","wait_since":1790539070,"why":"unanswered"}"#;

/// Claude Code's weekly-limit row as recorded (ids cut), its reset the recorded one while that
/// is a day ahead or more, else a day from now.
fn limit_row(ts: &str, session: &str) -> String {
    limit_row_until(ts, session, 1_791_046_800_u64.max(now_s() + 86_400))
}

/// [`limit_row`] naming the reset `resets` (unix seconds).
fn limit_row_until(ts: &str, session: &str, resets: u64) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000031","isSidechain":false,"type":"assistant","uuid":"00000000-0000-4000-8000-000000000032","timestamp":"{ts}","message":{{"diagnostics":null,"id":"00000000-0000-4000-8000-000000000033","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)"}}],"context_management":null}},"requestId":"req_0000000000000000000002","quotaLimits":{{"status":"rejected","resetsAt":{resets},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"perTurnEffort":"xhigh","session_id":"{session}","userType":"external","entrypoint":"cli","cwd":"/Users//owner/aterm","sessionId":"{session}","version":"2.1.280","gitBranch":"main"}}"#
    )
}

/// An API rate limit's row at `at` (unix seconds): no `quotaLimits` or `error` key, so it names
/// no reset and is read by the wall table's words alone.
fn api_limit_row(at: u64) -> String {
    limit_row_until(&ts(at), OWNERS_SESSION, 0)
        .replace(
            r#""quotaLimits":{"status":"rejected","resetsAt":0,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false},"#,
            "",
        )
        .replace(r#""error":"rate_limit","#, "")
        .replace(
            "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)",
            "API Error: Rate limit reached for requests",
        )
}

/// The notice with `marker` as this build types it.
fn notice(marker: &str) -> String {
    upgrade::prepare_prompt(
        &Version::parse("2.1.280").expect("from"),
        &Version::parse("2.1.283").expect("to"),
        Source::Managed,
        marker,
    )
}

/// The owner's task, five hours before `now`: the conversation has one, so
/// the upgrade resumes it rather than starting a fresh one.
fn tasked_at(now: u64) -> String {
    typed_row(
        &ts(now - 5 * 3_600),
        OWNERS_SESSION,
        "Land the review fixes, then push.",
        "typed",
    )
}

/// The owner's rows: the task, each notice of `asked` answered by the limit, then with
/// `went_on = (ago, said)` the owner's `first, continue` and the model's row `ago` seconds
/// before `now`, saying `said`.
fn owners_rows(now: u64, asked: &[&str], went_on: Option<(u64, &str)>) -> String {
    let mut rows = vec![tasked_at(now)];
    let first = now - 4 * 3_600;
    for (k, marker) in asked.iter().enumerate() {
        let at = first + REASK_S * u64::try_from(k).expect("k");
        rows.push(typed_row(&ts(at), OWNERS_SESSION, &notice(marker), "typed"));
        rows.push(limit_row(&ts(at + 1), OWNERS_SESSION));
        rows.push(duration_row(&ts(at + 1), OWNERS_SESSION, 760));
    }
    if let Some((ago, said)) = went_on {
        rows.push(typed_row(
            &ts(now - ago - 10),
            OWNERS_SESSION,
            "first, continue",
            "typed",
        ));
        rows.push(model_row(&ts(now - ago), OWNERS_SESSION, said, "tool_use"));
    }
    rows.join("\n") + "\n"
}

/// An announced upgrade of the owner's session: `asks` notices typed, the
/// latest one's window run out, the notice's fences on the stand-in.
fn announced(asks: usize) -> impl FnOnce(&SessionFile, u64) -> St {
    move |sf, now| {
        let asked: Vec<String> = MARKERS[..asks].iter().map(|m| (*m).to_string()).collect();
        St {
            phase: Phase::Announced {
                at_s: now - REASK_S - 60,
                asks: u32::try_from(asks).expect("asks"),
            },
            from: "2.1.280".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            marker: asked.last().cloned().unwrap_or_default(),
            markers: asked.clone(),
            asked,
            tab: TAB.to_string(),
            notice_pid: sf.pid,
            notice_start: squash(&sf.proc_start),
            last_seq: 77,
            seq_since_s: now - 3_600,
            salt: OWNERS_SALT,
            ..St::default()
        }
    }
}

/// [`OWNERS_STATE`] with the stand-in's pid, start and tab, and `from` the stand-in's build
/// (1.0.0: an older build's record names its process by the build it was found on).
fn owners_state_for(sf: &SessionFile) -> St {
    let text = OWNERS_STATE
        .replace(r#""from":"2.1.280""#, r#""from":"1.0.0""#)
        .replace(
            r#""notice_pid":6099"#,
            &format!(r#""notice_pid":{}"#, sf.pid),
        )
        .replace("Thu Sep 24 02:03:13 2026", &squash(&sf.proc_start))
        .replace("s-c543f4e0edd3439e5791", TAB)
        .replace(r#""last_seq":4735"#, r#""last_seq":77"#);
    St::from_json(&text).expect("the owner's state reads")
}

/// A notice the limit answered, with no model row since, is never typed again behind itself,
/// however long ago its window ran out and whatever the screen shows: the visit waits
/// `limited` and holds the window. Once the model takes it, the window opens then. Negative
/// control: a notice the model read is asked again.
#[cfg(unix)]
#[test]
fn a_notice_queued_behind_the_limit_is_never_asked_again_behind_it() {
    let now = now_s();
    let queued = visit_at(
        "lim-q",
        OWNERS_SESSION,
        announced(1),
        &owners_rows(now, &MARKERS[..1], None),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (queued.step.as_str(), queued.typed.len()),
        ("wait:limited", 0),
        "{queued:?}"
    );
    let held = queued.saved.expect("state");
    assert!(
        matches!(held.phase, Phase::Announced { at_s, asks: 1 } if at_s + 60 >= now),
        "its window waits with it: {:?}",
        held.phase
    );

    let taken = visit_at(
        "lim-t",
        OWNERS_SESSION,
        announced(1),
        &owners_rows(now, &MARKERS[..1], Some((120, "Pushing first."))),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (taken.step.as_str(), taken.typed.len()),
        ("wait:awaiting-ready", 0),
        "{taken:?}"
    );
    let opened = taken.saved.expect("state");
    assert!(
        matches!(opened.phase, Phase::Announced { at_s, asks: 1 } if at_s == now - 120),
        "its window opens when the model took it: {:?}",
        opened.phase
    );

    // Negative control: read as it came, its window run out.
    let first = now - 4 * 3_600;
    let read = [
        tasked_at(now),
        typed_row(&ts(first), OWNERS_SESSION, &notice(MARKERS[0]), "typed"),
        model_row(
            &ts(first + 20),
            OWNERS_SESSION,
            "Noted; the gate is still running.",
            "end_turn",
        ),
    ]
    .join("\n")
        + "\n";
    let again = visit_at(
        "lim-r",
        OWNERS_SESSION,
        announced(1),
        &read,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(again.step, "announced:2", "{again:?}");
    assert_eq!(again.typed.len(), 1);
}

/// An agent that declines the last ask is released for this round only, with no stall shown:
/// it rests [`upgrade::RETRY_S`] and a new round asks with a notice of its own.
#[cfg(unix)]
#[test]
fn a_declined_last_ask_rests_and_is_asked_again() {
    let now = now_s();
    let last = MARKERS[3];
    let at = now - REASK_S - 60;
    let rows = [
        tasked_at(now),
        typed_row(&ts(at), OWNERS_SESSION, &notice(last), "typed"),
        model_row(
            &ts(at + 30),
            OWNERS_SESSION,
            "I cannot stop now: the release gate still runs (about 40 minutes). Ask me again then.",
            "end_turn",
        ),
    ]
    .join("\n")
        + "\n";
    let declined = visit_at(
        "lim-d",
        OWNERS_SESSION,
        announced(4),
        &rows,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert!(
        declined.step == "gave-up" || declined.step == "released:gave-up",
        "{declined:?}"
    );
    let rested = declined.saved.expect("state");
    assert_eq!(rested.phase, Phase::Failed(GAVE_UP.to_string()));
    // It rests from this look (`a_stopped_round_says_when_its_next_round_starts`).
    assert!(rested.failed_at + 60 >= now, "{}", rested.failed_at);
    assert!(!upgrade::retry_due(
        &rested.phase,
        rested.failed_for(now),
        false
    ));
    // Rested: a new round, and its own notice.
    let rearmed = visit_at(
        "lim-dr",
        OWNERS_SESSION,
        // A new stand-in process each visit: the record names the process the first visit
        // stamped.
        move |sf, now| St {
            failed_at: now - upgrade::RETRY_S,
            proc_start: squash(&sf.proc_start),
            ..rested
        },
        &rows,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(rearmed.step, "rearmed:unanswered", "{rearmed:?}");
    let pending = rearmed.saved.expect("state");
    let asked = visit_at(
        "lim-da",
        OWNERS_SESSION,
        move |_, _| pending,
        &rows,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(asked.step, "announced:1", "{asked:?}");
    assert_eq!(asked.typed.len(), 1);
    assert!(
        MARKERS.iter().all(|m| !asked.typed[0].contains(m)),
        "a marker of its own"
    );
}

/// The recorded give-up (four notices queued behind the limit, no stamp of when) re-arms on
/// its first look. While the transcript says the limit holds, the new round's first notice
/// waits `limited`; once the model took the four, it asks with a marker of its own.
#[cfg(unix)]
#[test]
fn the_give_up_0_93_0_spent_behind_the_limit_rests_then_waits_out_the_limit() {
    let to = Version::parse("2.1.283").expect("to");
    assert_eq!(
        upgrade::round_markers(OWNERS_SESSION, &to, OWNERS_SALT),
        MARKERS.map(str::to_string),
        "the round's markers, as 0.93.0 minted them"
    );
    let now = now_s();
    for (label, went_on, then) in [
        ("lim-sq", None, "wait:limited"),
        ("lim-on", Some((300, "Pushing first.")), "announced:1"),
    ] {
        let rows = owners_rows(now, &MARKERS, went_on);
        let rearmed = visit_at(
            label,
            OWNERS_SESSION,
            |sf, _| owners_state_for(sf),
            &rows,
            idle_screen(),
            &to_2_1_283(),
            usize::MAX,
        );
        assert_eq!(
            (rearmed.step.as_str(), rearmed.typed.len()),
            ("rearmed:unanswered", 0),
            "{label}: {rearmed:?}"
        );
        let pending = rearmed.saved.expect("state");
        assert_eq!(pending.phase, Phase::Pending, "{label}");
        let next = visit_at(
            &format!("{label}-2"),
            OWNERS_SESSION,
            move |_, _| pending,
            &rows,
            idle_screen(),
            &to_2_1_283(),
            usize::MAX,
        );
        assert_eq!(next.step, then, "{label}: {next:?}");
        if then == "wait:limited" {
            assert!(next.typed.is_empty(), "{label}: never behind the queue");
        } else {
            assert_eq!(next.typed.len(), 1, "{label}");
            assert!(
                MARKERS.iter().all(|m| !next.typed[0].contains(m)),
                "{label}: a marker of its own"
            );
        }
    }
}

/// A pending upgrade whose session's last word is the limit row waits `limited` however the
/// screen looks. Negative control: the named reset has passed, and the notice is typed.
#[cfg(unix)]
#[test]
fn a_session_whose_last_word_is_the_limit_is_not_asked_until_its_reset() {
    let now = now_s();
    let parked = |resets: u64| {
        [
            tasked_at(now),
            model_row(
                &ts(now - 900),
                OWNERS_SESSION,
                "Running the gate.",
                "tool_use",
            ),
            limit_row_until(&ts(now - 800), OWNERS_SESSION, resets),
            duration_row(&ts(now - 800), OWNERS_SESSION, 760),
        ]
        .join("\n")
            + "\n"
    };
    let pending = |_: &SessionFile, now: u64| St {
        phase: Phase::Pending,
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        last_seq: 77,
        seq_since_s: now - 3_600,
        salt: OWNERS_SALT,
        ..St::default()
    };
    let waits = visit_at(
        "lim-p",
        OWNERS_SESSION,
        pending,
        &parked(now + 3_600),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (waits.step.as_str(), waits.typed.len()),
        ("wait:limited", 0),
        "{waits:?}"
    );
    // Negative control: the reset has passed.
    let asked = visit_at(
        "lim-pr",
        OWNERS_SESSION,
        pending,
        &parked(now - 60),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(asked.step, "announced:1", "{asked:?}");
    assert_eq!(asked.typed.len(), 1);
}

/// The quiet upgrade moves a tab whose weekly-limit notice is still on screen as any idle
/// point once [`LIMIT_GRACE_S`] past the row's named reset: the restart is reached with
/// nothing typed. Negative controls, each `limited` with nothing typed: a minute short of the
/// grace; a row naming no reset (none is guessed); and the ask flow, where typing over a
/// standing notice stays `HarnessUpgradeLimitQueue`'s decision.
#[cfg(unix)]
#[test]
fn a_limit_notice_a_grace_past_its_named_reset_is_moved_as_any_idle_point() {
    let now = now_s();
    let weekly = "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)";
    let screen = || {
        let rows: Vec<String> =
            aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_SESSION_LIMIT)
                .into_iter()
                .map(|r| {
                    r.replace(
                        "You've hit your session limit · resets 3pm (America/Los_Angeles)",
                        weekly,
                    )
                })
                .collect();
        assert!(
            rows.iter().any(|r| r.contains(weekly)) && upgrade::limited(Agent::Claude, &rows),
            "the weekly notice stands on the screen"
        );
        screen_json(&rows)
    };
    let parked = |limit: String| {
        [
            tasked_at(now),
            model_row(
                &ts(now - 9_000),
                OWNERS_SESSION,
                "Running the gate.",
                "tool_use",
            ),
            limit,
            duration_row(&ts(now - 8_000), OWNERS_SESSION, 960),
        ]
        .join("\n")
            + "\n"
    };
    let named = |resets: u64| limit_row_until(&ts(now - 8_000), OWNERS_SESSION, resets);
    let pending = |_: &SessionFile, now: u64| St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        last_seq: 77,
        seq_since_s: now - 3_600,
        salt: OWNERS_SALT,
        ..St::default()
    };
    let past = named(now - LIMIT_GRACE_S - 60);
    let moved = quietly(|| {
        visit_at(
            "c9-quiet",
            OWNERS_SESSION,
            pending,
            &parked(past.clone()),
            screen(),
            &to_2_1_283(),
            4,
        )
    });
    assert_eq!(
        (moved.step.as_str(), moved.typed.len()),
        ("wait:changed-before-signal", 0),
        "the restart reached, nothing typed: {moved:?}"
    );
    // Negative controls.
    for (label, row, quiet) in [
        ("c9-short", named(now - LIMIT_GRACE_S + 60), true),
        ("c9-unnamed", api_limit_row(now - 8_000), true),
        ("c9-ask", past, false),
    ] {
        let visit = || {
            visit_at(
                label,
                OWNERS_SESSION,
                pending,
                &parked(row.clone()),
                screen(),
                &to_2_1_283(),
                4,
            )
        };
        let r = if quiet { quietly(visit) } else { visit() };
        assert_eq!(
            (r.step.as_str(), r.typed.len()),
            ("wait:limited", 0),
            "{label}: {r:?}"
        );
    }
}

/// A queued notice whose limit is over is typed again as the same ask, its own marker, no ask
/// spent: an unbounded hold would wait for a person (a weekly limit continues only after
/// `/rate-limit-options`). Over means the named reset passed, a no-reset row is [`REASK_S`]
/// old, or a `/login` finished after it. Negative controls: the API limit a minute old, and a
/// passed reset while the screen still shows a limit, wait `limited`.
#[cfg(unix)]
#[test]
fn a_queued_notice_whose_limit_is_over_is_typed_again_as_the_same_ask() {
    let now = now_s();
    let first = MARKERS[0];
    let typed_at = now - 4 * 3_600;
    let queued = |answer: String, after: &[String]| {
        let mut rows = vec![
            tasked_at(now),
            typed_row(&ts(typed_at), OWNERS_SESSION, &notice(first), "typed"),
            answer,
        ];
        rows.extend(after.iter().cloned());
        rows.join("\n") + "\n"
    };
    let weekly_over = limit_row_until(&ts(typed_at + 1), OWNERS_SESSION, now - 3 * 86_400);
    let api = api_limit_row;
    assert!(!api(now).contains("quotaLimits"), "{}", api(now));
    assert_eq!(
        upgrade::notice_fate_of(&queued(api(now), &[]), first),
        Some(upgrade::NoticeFate::Queued),
        "an API rate limit queues it too"
    );
    let login = login_rows(&ts(now - 600), OWNERS_SESSION);
    let cases = [
        ("lim-ro", queued(weekly_over.clone(), &[])),
        ("lim-ra", queued(api(now - REASK_S - 60), &[])),
        (
            "lim-rl",
            queued(limit_row(&ts(typed_at + 1), OWNERS_SESSION), &login),
        ),
    ];
    for (label, rows) in cases {
        let again = visit_at(
            label,
            OWNERS_SESSION,
            announced(1),
            &rows,
            idle_screen(),
            &to_2_1_283(),
            usize::MAX,
        );
        assert_eq!(again.step, "announced:1", "{label}: {again:?}");
        assert_eq!(again.typed.len(), 1, "{label}");
        assert!(again.typed[0].contains(first), "{label}: its own marker");
        assert!(
            again
                .ledger
                .iter()
                .any(|l| l.contains("a usage limit that is over")),
            "{label}: {:?}",
            again.ledger
        );
    }
    // The same `/login` ends a first notice's hold.
    let pending = |_: &SessionFile, now: u64| St {
        phase: Phase::Pending,
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        last_seq: 77,
        seq_since_s: now - 3_600,
        salt: OWNERS_SALT,
        ..St::default()
    };
    let mut switched = vec![
        tasked_at(now),
        model_row(
            &ts(now - 900),
            OWNERS_SESSION,
            "Running the gate.",
            "tool_use",
        ),
        limit_row(&ts(now - 800), OWNERS_SESSION),
    ];
    switched.extend(login);
    let first_notice = visit_at(
        "lim-pl",
        OWNERS_SESSION,
        pending,
        &(switched.join("\n") + "\n"),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(first_notice.step, "announced:1", "{first_notice:?}");

    // Negative controls.
    let fresh = visit_at(
        "lim-rf",
        OWNERS_SESSION,
        announced(1),
        &queued(api(now - 60), &[]),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (fresh.step.as_str(), fresh.typed.len()),
        ("wait:limited", 0),
        "{fresh:?}"
    );
    let limit_screen = screen_json(&aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::END_SESSION_LIMIT,
    ));
    let shown = visit_at(
        "lim-rs",
        OWNERS_SESSION,
        announced(1),
        &queued(weekly_over, &[]),
        limit_screen,
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (shown.step.as_str(), shown.typed.len()),
        ("wait:limited", 0),
        "the screen judges: {shown:?}"
    );
}

/// The rows with `copies` copies of the notice `marker`, each answered by an API rate limit,
/// the last row `last_ago` seconds before `now` and earlier ones a re-ask and a minute apart;
/// then, with `went_on`, a person's `first, continue` and the model's row, `ago` seconds back.
fn copies_rows(now: u64, marker: &str, copies: u64, last_ago: u64, went_on: Option<u64>) -> String {
    let mut rows = vec![tasked_at(now)];
    for k in 1..=copies {
        let at = now - last_ago - 1 - (copies - k) * (REASK_S + 60);
        rows.push(typed_row(&ts(at), OWNERS_SESSION, &notice(marker), "typed"));
        rows.push(api_limit_row(at + 1));
    }
    if let Some(ago) = went_on {
        rows.push(typed_row(
            &ts(now - ago - 10),
            OWNERS_SESSION,
            "first, continue",
            "typed",
        ));
        rows.push(model_row(
            &ts(now - ago),
            OWNERS_SESSION,
            "Pushing first.",
            "tool_use",
        ));
    }
    rows.join("\n") + "\n"
}

/// A notice a limit answered is typed again straight away at most [`upgrade::REQUEUE_MAX`]
/// times before the model takes one (a no-reset limit is over every [`REASK_S`], so re-typing
/// alone would pile up a copy every half hour). Past that the visit waits `queued`, its window
/// held, until the model writes a row, which takes every copy and opens the window. Negative
/// control: the full count's last row a minute old waits `limited`.
#[cfg(unix)]
#[test]
fn a_queued_notice_is_typed_again_at_most_requeue_max_times() {
    let now = now_s();
    let first = MARKERS[0];
    let bound = u64::from(upgrade::REQUEUE_MAX);
    assert_eq!(upgrade::REQUEUE_MAX, 2, "the bound this test walks");
    for copies in [1, bound] {
        let rows = copies_rows(now, first, copies, REASK_S + 60, None);
        assert_eq!(
            u64::from(upgrade::queued_copies(&rows, first)),
            copies,
            "{rows}"
        );
        let again = visit_at(
            &format!("lim-b{copies}"),
            OWNERS_SESSION,
            announced(1),
            &rows,
            idle_screen(),
            &to_2_1_283(),
            usize::MAX,
        );
        assert_eq!(again.step, "announced:1", "{copies}: {again:?}");
        assert_eq!(again.typed.len(), 1, "{copies}");
        assert!(again.typed[0].contains(first), "{copies}: the same marker");
        assert!(
            again
                .ledger
                .iter()
                .any(|l| l.contains("a usage limit that is over")),
            "{copies}: {:?}",
            again.ledger
        );
    }
    let full = copies_rows(now, first, bound + 1, REASK_S + 60, None);
    assert_eq!(upgrade::queued_until(&full, first), Some(now - 60));
    let waits = visit_at(
        "lim-bf",
        OWNERS_SESSION,
        announced(1),
        &full,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (waits.step.as_str(), waits.typed.len()),
        ("wait:queued", 0),
        "past the bound it waits: {waits:?}"
    );
    let held = waits.saved.expect("state");
    assert!(
        matches!(held.phase, Phase::Announced { at_s, asks: 1 } if at_s + 60 >= now),
        "its window waits with it: {:?}",
        held.phase
    );
    // The session goes on: every copy taken, the window opened then.
    let taken = visit_at(
        "lim-bt",
        OWNERS_SESSION,
        announced(1),
        &copies_rows(now, first, bound + 1, REASK_S + 60, Some(120)),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (taken.step.as_str(), taken.typed.len()),
        ("wait:awaiting-ready", 0),
        "{taken:?}"
    );
    let opened = taken.saved.expect("state");
    assert!(
        matches!(opened.phase, Phase::Announced { at_s, asks: 1 } if at_s == now - 120),
        "its window opens when the model took the copies: {:?}",
        opened.phase
    );
    // Negative control: a fresh limit row waits as always.
    let fresh = visit_at(
        "lim-bn",
        OWNERS_SESSION,
        announced(1),
        &copies_rows(now, first, bound + 1, 60, None),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (fresh.step.as_str(), fresh.typed.len()),
        ("wait:limited", 0),
        "{fresh:?}"
    );
}

/// A full queue is never held for good: one more copy (same marker, same ask) goes once the
/// last limit row is [`REASK_S`] + [`upgrade::RETRY_S`] old, or at once on the owner's `--now`
/// given after the latest copy; a word given before it is spent. Negative controls: a limit
/// a minute old, or one still on screen, waits `limited` either way.
#[cfg(unix)]
#[test]
fn a_full_queue_rests_and_the_owners_word_types_one_more() {
    let now = now_s();
    let first = MARKERS[0];
    let full = u64::from(upgrade::REQUEUE_MAX) + 1;
    let rested = copies_rows(now, first, full, REASK_S + upgrade::RETRY_S + 60, None);
    let scan = upgrade::notice_scan(&rested, first).expect("the latest copy");
    assert!(scan.rests_until().is_some_and(|t| t <= now), "{scan:?}");
    let again = visit_at(
        "lim-fr",
        OWNERS_SESSION,
        announced(1),
        &rested,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(again.step, "announced:1", "rested: {again:?}");
    assert_eq!(again.typed.len(), 1);
    assert!(again.typed[0].contains(first), "the same marker");

    let unrested = copies_rows(now, first, full, REASK_S + 60, None);
    let with_word = |given: u64| {
        move |sf: &SessionFile, now: u64| St {
            request: Request::Now,
            request_at: now - given,
            ..announced(1)(sf, now)
        }
    };
    let asked = visit_at(
        "lim-fn",
        OWNERS_SESSION,
        with_word(0),
        &unrested,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(asked.step, "announced:1", "the owner's word: {asked:?}");
    assert_eq!(asked.typed.len(), 1);
    // The word came before the latest copy: spent, and the queue waits.
    let spent = visit_at(
        "lim-fs",
        OWNERS_SESSION,
        with_word(REASK_S + 200),
        &unrested,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (spent.step.as_str(), spent.typed.len()),
        ("wait:queued", 0),
        "{spent:?}"
    );

    // Negative controls: the limit stands.
    let fresh = visit_at(
        "lim-ff",
        OWNERS_SESSION,
        with_word(0),
        &copies_rows(now, first, full, 60, None),
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (fresh.step.as_str(), fresh.typed.len()),
        ("wait:limited", 0),
        "{fresh:?}"
    );
    let limit_screen = screen_json(&aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::END_SESSION_LIMIT,
    ));
    let shown = visit_at(
        "lim-fl",
        OWNERS_SESSION,
        announced(1),
        &rested,
        limit_screen,
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (shown.step.as_str(), shown.typed.len()),
        ("wait:limited", 0),
        "{shown:?}"
    );
}

/// No first notice behind another round's queued one while its limit holds (`limited`); once
/// over, the first notice goes; with `1 + REQUEUE_MAX` untaken the queue is full (`queued`).
#[cfg(unix)]
#[test]
fn a_pending_upgrade_waits_behind_another_rounds_queued_notice() {
    let now = now_s();
    let old = "ATERM-UPGRADE-READY-00c0ffee";
    let pending = |_: &SessionFile, now: u64| St {
        phase: Phase::Pending,
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        last_seq: 77,
        seq_since_s: now - 3_600,
        salt: OWNERS_SALT,
        ..St::default()
    };
    for (label, rows, then) in [
        (
            "lim-xf",
            copies_rows(now, old, 1, 600, None),
            "wait:limited",
        ),
        (
            "lim-xo",
            copies_rows(now, old, 1, REASK_S + 60, None),
            "announced:1",
        ),
        (
            "lim-xq",
            copies_rows(
                now,
                old,
                u64::from(upgrade::REQUEUE_MAX) + 1,
                REASK_S + 60,
                None,
            ),
            "wait:queued",
        ),
    ] {
        let visited = visit_at(
            label,
            OWNERS_SESSION,
            pending,
            &rows,
            idle_screen(),
            &to_2_1_283(),
            usize::MAX,
        );
        assert_eq!(visited.step, then, "{label}: {visited:?}");
        let typed = usize::from(then == "announced:1");
        assert_eq!(visited.typed.len(), typed, "{label}");
    }
}

/// A limit row naming no reset holds the session (`limited`) but leaves the record no reset
/// (`St::limit_until` `0`), so the view never says a reset passed. Negative control: the
/// weekly row's reset is kept and told [`LIMIT_GRACE_S`] past it.
#[cfg(unix)]
#[test]
fn a_limit_row_naming_no_reset_leaves_the_view_no_reset() {
    let now = now_s();
    let pending = |_: &SessionFile, now: u64| St {
        phase: Phase::Pending,
        from: "2.1.280".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        last_seq: 77,
        seq_since_s: now - 3_600,
        salt: OWNERS_SALT,
        ..St::default()
    };
    let row_at = now - 600;
    let view = |label: &str, st: &St| {
        let dir = scratch(label);
        let opts = drive(&dir);
        save(&opts, OWNERS_SESSION, st);
        let row = rows(&opts)
            .into_iter()
            .find(|r| r.session == OWNERS_SESSION)
            .expect("the row");
        let _ = std::fs::remove_dir_all(dir);
        row
    };
    let api = format!("{}\n{}\n", tasked_at(now), api_limit_row(row_at));
    let visited = visit_at(
        "lim-api",
        OWNERS_SESSION,
        pending,
        &api,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(visited.step, "wait:limited", "the wait still holds");
    let saved = visited.saved.expect("saved");
    assert_eq!((saved.wait.as_str(), saved.limit_until), ("limited", 0));
    assert!(!saved.limit_spend, "a rate limit is no spend wall");
    let row = view("lim-api-view", &saved);
    let later = row_at + REASK_S + LIMIT_GRACE_S;
    assert!(!row.limit_passed(later), "no named reset has passed");
    assert!(
        row.stall_words(later)
            .is_none_or(|w| !w.contains("reset passed")),
        "{:?}",
        row.stall_words(later)
    );
    // Negative control: the weekly row names its reset. One clock reading names it in the row
    // and in the check: the visit above can take seconds on a loaded machine.
    let reset = 1_791_046_800_u64.max(now_s() + 86_400);
    let weekly = format!(
        "{}\n{}\n",
        tasked_at(now),
        limit_row_until(&ts(row_at), OWNERS_SESSION, reset)
    );
    let visited = visit_at(
        "lim-weekly",
        OWNERS_SESSION,
        pending,
        &weekly,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    let saved = visited.saved.expect("saved");
    assert!(saved.limit_until.abs_diff(reset) <= 2, "{saved:?}");
    let row = view("lim-weekly-view", &saved);
    assert!(row.limit_passed(saved.limit_until + LIMIT_GRACE_S));
    // A spend wall naming no reset: kept as money (`St::limit_spend`), the owner's hold, which
    // the view never bounds by the week nor tells as passed.
    let spend = format!(
        "{}\n{}\n",
        tasked_at(now),
        api_limit_row(row_at).replace(
            "API Error: Rate limit reached for requests",
            "You've hit your monthly spend limit."
        )
    );
    let visited = visit_at(
        "lim-spend",
        OWNERS_SESSION,
        pending,
        &spend,
        idle_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(visited.step, "wait:limited", "{visited:?}");
    let saved = visited.saved.expect("saved");
    assert_eq!((saved.limit_until, saved.limit_spend), (0, true));
    let row = view("lim-spend-view", &saved);
    assert!(row.spend_held());
    assert_eq!(row.limit_ends_at(), None, "no usage window");
}

// ------------------------------------------- Tier-1: HarnessUpgradeLimitQueue

use aterm_spec::derive::Model;

/// A state of `harness_upgrade_limit_queue_model`.
type S = std::collections::BTreeMap<&'static str, i64>;

/// The walk's clock: a unix second after every stamp it writes.
const WALK_NOW: u64 = 1_790_600_000;

/// A day: the model's day-long rest (`rested == 2`), the longest a full queue
/// rests ([`upgrade::QUEUE_REST_MAX_S`], asserted equal in the bind).
const DAY: u64 = 86_400;

/// The model's `untaken` at its top, `RetypeMax + 1 + Daily`: that many
/// notices unread or more, the queue's rest grown to a day.
fn walk_top() -> i64 {
    i64::from(upgrade::REQUEUE_MAX + 1 + upgrade::QUEUE_REST_DOUBLINGS)
}

/// A notice's marker no ask of the walked round mints: one an earlier round,
/// another target or an older build typed.
const FOREIGN_MARKER: &str = "ATERM-UPGRADE-READY-00c0ffee";

/// Ask `k`'s READY marker, as the driver mints it (the round's salt plus the
/// ask), and the one every copy of that ask carries.
fn ask_marker(k: i64) -> String {
    upgrade::ready_marker(
        OWNERS_SESSION,
        &Version::parse("2.1.283").expect("to"),
        OWNERS_SALT + u64::try_from(k).expect("k"),
    )
}

/// The screen a model state stands for: Claude Code's end of turn at the
/// session limit where `shown`, else an idle, empty composer.
fn walk_screen(s: &S) -> Vec<String> {
    if s["shown"] == 1 {
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_SESSION_LIMIT)
    } else {
        let rule = "─".repeat(20);
        vec![rule.clone(), "❯ ".to_string(), rule]
    }
}

/// One rendering of a model state: a transcript's tail, and where the latest
/// ask's window starts by the state's own record (`None`: where the state's
/// `window` puts it).
struct Shape {
    name: String,
    tail: String,
    at_s: Option<u64>,
}

impl Shape {
    fn of(name: &str, tail: String) -> Self {
        Self {
            name: name.to_string(),
            tail,
            at_s: None,
        }
    }
}

/// The transcripts a model state stands for, each read the same by the real readers. Earlier
/// asks are typed and read as they came. The latest ask, with nothing `untaken`: read as it
/// came, or met by the limit and taken later (its recorded window starting when typed, so
/// only the take opens it), then the session's last word. Else `untaken` copies (at the top,
/// also one more), each met by a limit row: its bound ahead where `held`, else over (API
/// limit, passed reset, or a `/login`; the first copy possibly an earlier round's), and past a
/// full queue's rest where `rested`.
fn walk_transcripts(s: &S) -> Vec<Shape> {
    let asks = s["asks"];
    if asks == 0 || s["untaken"] > 0 {
        return walk_shapes(s, None);
    }
    let now = WALK_NOW;
    let marker = ask_marker(asks);
    let read = vec![
        typed_row(
            &ts(now - 2 * REASK_S),
            OWNERS_SESSION,
            &notice(&marker),
            "typed",
        ),
        model_row(
            &ts(now - 2 * REASK_S + 20),
            OWNERS_SESSION,
            "Noted; the gate is still running.",
            "end_turn",
        ),
    ];
    let took = if s["window"] == 1 {
        now - REASK_S - 30
    } else {
        now - 1_000
    };
    let typed = now - 3 * REASK_S;
    let taken = vec![
        typed_row(&ts(typed), OWNERS_SESSION, &notice(&marker), "typed"),
        api_limit_row(typed + 1),
        typed_row(&ts(took - 10), OWNERS_SESSION, "first, continue", "typed"),
        model_row(&ts(took), OWNERS_SESSION, "Pushing first.", "tool_use"),
    ];
    let mut out = walk_shapes(s, Some(read));
    out.extend(walk_shapes(s, Some(taken)).into_iter().map(|shape| Shape {
        name: format!("taken/{}", shape.name),
        at_s: Some(typed),
        ..shape
    }));
    out
}

/// [`walk_transcripts`] with the latest ask's rows `latest` (none: nothing
/// asked, or its copies are `untaken`).
fn walk_shapes(s: &S, latest: Option<Vec<String>>) -> Vec<Shape> {
    // How long ago the rest began: a day and a minute for a day's rest, three minutes short of
    // a day for a shorter one.
    let rest = if s["rested"] == 2 {
        DAY + 60
    } else {
        DAY - 180
    };
    walk_shapes_resting(s, latest, rest)
}

/// [`walk_shapes`] with a `rested` state's word run out `rest` seconds ago.
fn walk_shapes_resting(s: &S, latest: Option<Vec<String>>, rest: u64) -> Vec<Shape> {
    let now = WALK_NOW;
    let asks = s["asks"];
    let mut head = vec![tasked_at(now)];
    let mut t = now - 20 * REASK_S;
    for k in 1..asks {
        head.push(typed_row(
            &ts(t),
            OWNERS_SESSION,
            &notice(&ask_marker(k)),
            "typed",
        ));
        head.push(model_row(
            &ts(t + 20),
            OWNERS_SESSION,
            "Not yet: the gate still runs.",
            "end_turn",
        ));
        t += REASK_S;
    }
    let with = |rows: &[String], more: Vec<String>| -> String {
        rows.iter()
            .cloned()
            .chain(more)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let login = |at: u64| login_rows(&ts(at), OWNERS_SESSION).to_vec();
    let untaken = u64::try_from(s["untaken"]).expect("untaken");
    if untaken > 0 {
        let marker = ask_marker(asks);
        let copy =
            |at: u64, marker: &str| typed_row(&ts(at), OWNERS_SESSION, &notice(marker), "typed");
        // The latest copy's typing time, and its limit's rows, by the
        // transcript's word on it.
        let rested = s["rested"] > 0;
        let lasts: Vec<(&str, u64, Vec<String>)> = if s["held"] == 1 {
            vec![
                ("api-fresh", now - 61, vec![api_limit_row(now - 60)]),
                (
                    "named-ahead",
                    now - 61,
                    vec![limit_row_until(&ts(now - 60), OWNERS_SESSION, now + 3_600)],
                ),
            ]
        } else if rested {
            let back = REASK_S + rest;
            vec![
                (
                    "api-rested",
                    now - back - 1,
                    vec![api_limit_row(now - back)],
                ),
                (
                    "named-rested",
                    now - rest - 3_601,
                    vec![limit_row_until(
                        &ts(now - rest - 3_600),
                        OWNERS_SESSION,
                        now - rest,
                    )],
                ),
                (
                    "login-rested",
                    now - back - 1,
                    [limit_row_until(
                        &ts(now - back),
                        OWNERS_SESSION,
                        now + 86_400,
                    )]
                    .into_iter()
                    .chain(login(now - back + 300))
                    .collect(),
                ),
            ]
        } else {
            vec![
                (
                    "api-over",
                    now - REASK_S - 61,
                    vec![api_limit_row(now - REASK_S - 60)],
                ),
                (
                    "named-passed",
                    now - 3_601,
                    vec![limit_row_until(&ts(now - 3_600), OWNERS_SESSION, now - 60)],
                ),
                (
                    "named-login",
                    now - 3_601,
                    [limit_row_until(
                        &ts(now - 3_600),
                        OWNERS_SESSION,
                        now + 86_400,
                    )]
                    .into_iter()
                    .chain(login(now - 600))
                    .collect(),
                ),
            ]
        };
        // The model's top as that many notices, and as one more.
        let counts: Vec<u64> = if s["untaken"] == walk_top() {
            vec![untaken, untaken + 1]
        } else {
            vec![untaken]
        };
        let mut out = Vec::new();
        for n in counts {
            for foreign in [false, true] {
                // An earlier round's notice as the first in the queue: every
                // upgrade notice counts, whatever its marker.
                if foreign && n < 2 {
                    continue;
                }
                for (name, last, answer) in &lasts {
                    let mut rows = head.clone();
                    for k in 1..n {
                        let at = last - (n - k) * (REASK_S + 60);
                        let m = if foreign && k == 1 {
                            FOREIGN_MARKER
                        } else {
                            marker.as_str()
                        };
                        rows.push(copy(at, m));
                        rows.push(api_limit_row(at + 1));
                    }
                    let latest: Vec<String> = std::iter::once(copy(*last, &marker))
                        .chain(answer.iter().cloned())
                        .collect();
                    let label =
                        format!("{name}/{n}{}", if foreign { "/foreign-first" } else { "" });
                    out.push(Shape::of(&label, with(&rows, latest)));
                }
            }
        }
        return out;
    }
    head.extend(latest.into_iter().flatten());
    let own_turn = |at: u64| model_row(&ts(at), OWNERS_SESSION, "Running the gate.", "tool_use");
    if s["held"] == 1 {
        return vec![
            Shape::of(
                "named-ahead",
                with(
                    &head,
                    vec![
                        own_turn(now - 900),
                        limit_row_until(&ts(now - 800), OWNERS_SESSION, now + 3_600),
                    ],
                ),
            ),
            Shape::of(
                "api-fresh",
                with(&head, vec![own_turn(now - 900), api_limit_row(now - 800)]),
            ),
        ];
    }
    vec![
        Shape::of("quiet", with(&head, Vec::new())),
        Shape::of(
            "api-over",
            with(
                &head,
                vec![
                    own_turn(now - REASK_S - 120),
                    api_limit_row(now - REASK_S - 60),
                ],
            ),
        ),
        Shape::of(
            "named-passed",
            with(
                &head,
                vec![
                    own_turn(now - 900),
                    limit_row_until(&ts(now - 800), OWNERS_SESSION, now - 60),
                ],
            ),
        ),
        Shape::of(
            "named-login",
            with(
                &head,
                [
                    own_turn(now - 900),
                    limit_row_until(&ts(now - 800), OWNERS_SESSION, now + 86_400),
                ]
                .into_iter()
                .chain(login(now - 600))
                .collect(),
            ),
        ),
    ]
}

/// The rest probe for a full queue that has not rested: its renderings with the limit's word
/// run out three minutes short of a day, where a shorter rest has run and a day's has not.
fn walk_rest_probe(s: &S) -> Vec<Shape> {
    let mut probed = s.clone();
    probed.insert("rested", 1);
    walk_shapes(&probed, None)
}

/// The short rest's end for a full queue the model says rested less than a day (`rested == 1`):
/// renderings with the limit's word run out a minute either side of [`upgrade::queue_rest`] of
/// its `untaken` (`true`: still resting, so the real code must wait `queued`). This pins the
/// real rest to the copies the real fold counts (every unread copy, whatever its round).
fn walk_short_rest_probe(s: &S) -> Vec<(Shape, bool)> {
    let untaken = u32::try_from(s["untaken"]).expect("untaken");
    let rest = upgrade::queue_rest(untaken);
    assert!(rest < DAY, "a rest shorter than a day at {s:?}");
    let mut out: Vec<(Shape, bool)> = walk_shapes_resting(s, None, rest - 60)
        .into_iter()
        .map(|shape| (shape, true))
        .collect();
    out.extend(
        walk_shapes_resting(s, None, rest + 60)
            .into_iter()
            .map(|shape| (shape, false)),
    );
    out
}

/// What the real upgrade decided at a look at one rendering of a state.
#[derive(Clone, Debug)]
struct Walked {
    /// The model's action the step is (none: it waited).
    taken: Option<&'static str>,
    /// The step itself.
    step: Step,
    /// The ask a notice goes as ([`upgrade::announce_asks`]).
    asks: u32,
    /// The re-ask clock runs at this look.
    runs: bool,
    /// The facts the fold left: the limit read, a full queue, a notice to
    /// type again.
    limited: bool,
    queued: bool,
    undelivered: bool,
    /// [`queue_facts`] said the notice before it never reached the model.
    requeued: bool,
}

/// The real decision at the look a model state stands for, over one rendering, in the
/// driver's order: the screen's limit and the clock, [`queue_facts`] under `request` (`--now`
/// given at this look), [`upgrade::requested_step`] (no READY in the tail, no round rested:
/// those are the never-strands model's), the ask count, and whether the clock runs (a look a
/// window later, same facts, leaves the window where it was). `s` holds only [`DECIDE_VARS`].
fn walk_decide(s: &S, shape: &Shape, request: &Request) -> Walked {
    let now = WALK_NOW;
    let asks = u32::try_from(s["asks"]).expect("asks");
    let phase = match s["phase"] {
        0 => Phase::Pending,
        1 => Phase::Announced {
            at_s: shape.at_s.unwrap_or(if s["window"] == 1 {
                now - REASK_S - 1
            } else {
                now - 60
            }),
            asks,
        },
        _ => Phase::Failed(GAVE_UP.to_string()),
    };
    let mut st = St {
        phase,
        to: "2.1.283".to_string(),
        marker: if asks > 0 {
            ask_marker(s["asks"])
        } else {
            String::new()
        },
        salt: OWNERS_SALT,
        request: request.clone(),
        request_at: if *request == Request::None { 0 } else { now },
        ..St::default()
    };
    let mut f = Facts {
        limited: upgrade::limited(Agent::Claude, &walk_screen(s)),
        ..idle()
    };
    st.phase = clock_held(&st.phase, &f, now);
    let requeued = queue_facts(&mut st, &mut f, Some(&shape.tail), now, request);
    let step = upgrade::requested_step(request, &st.phase, &f, false, now, &st.to);
    let taken = match step {
        Step::Announce if f.undelivered => Some("Retype"),
        Step::Announce => Some("Announce"),
        Step::GiveUp => Some("GiveUp"),
        _ => None,
    };
    let runs = match &st.phase {
        Phase::Announced { asks, .. } => {
            let t0 = now - 60;
            let asked = Phase::Announced {
                at_s: t0,
                asks: *asks,
            };
            matches!(
                clock_held(&asked, &f, t0 + REASK_S),
                Phase::Announced { at_s, .. } if at_s == t0
            )
        }
        _ => false,
    };
    Walked {
        taken,
        asks: announce_asks(&st.phase, f.undelivered),
        step,
        runs,
        limited: f.limited,
        queued: f.queued,
        undelivered: f.undelivered,
        requeued,
    }
}

/// The variables [`walk_decide`] reads; the rendering-only ones (`held`, `untaken`, `rested`)
/// and the model's own records it never reads.
const DECIDE_VARS: [&str; 4] = ["asks", "phase", "window", "shown"];

/// [`walk_decide`] memoised on its inputs: [`DECIDE_VARS`], the rendering's tail and window
/// start, and the request. It is pure (fresh `St` and `Facts` at [`WALK_NOW`]) and is handed
/// only those variables, so reading another panics rather than going unkeyed. About 16k
/// decisions share under 3k inputs (15.7 s to 3.1 s).
#[derive(Default)]
struct Decisions(std::collections::BTreeMap<(S, String, Option<u64>, bool), Walked>);

impl Decisions {
    fn decide(&mut self, s: &S, shape: &Shape, request: &Request) -> Walked {
        let owner = match request {
            Request::None => false,
            Request::Now => true,
            other => panic!("the walk asks no {other:?}"),
        };
        let vars: S = DECIDE_VARS.iter().map(|&k| (k, s[k])).collect();
        self.0
            .entry((vars, shape.tail.clone(), shape.at_s, owner))
            .or_insert_with_key(|(vars, ..)| walk_decide(vars, shape, request))
            .clone()
    }
}

/// The variables [`walk_transcripts`] renders from; `phase`, `shown` and the model's own
/// records it never reads.
const RENDER_VARS: [&str; 5] = ["asks", "untaken", "window", "rested", "held"];

/// [`walk_transcripts`] memoised on [`RENDER_VARS`]: it is pure and is handed only those
/// variables, so reading another panics rather than going unkeyed (a seventh of the test's
/// instructions saved).
#[derive(Default)]
struct Renderings(std::collections::BTreeMap<S, Vec<Shape>>);

impl Renderings {
    fn of(&mut self, s: &S) -> &[Shape] {
        let vars: S = RENDER_VARS.iter().map(|&k| (k, s[k])).collect();
        self.0.entry(vars).or_insert_with_key(walk_transcripts)
    }
}

/// Whether `m` at `s` enables exactly the step the real code took (under the owner's `--now`,
/// `NowAsks` stands in for `Retype`), to the same ask count, and lets the window run out
/// (`Elapse`) exactly where the real clock runs.
fn walk_agrees(m: &Model, s: &S, w: &Walked, owner: bool) -> bool {
    let enabled = |a: &str| {
        m.action_enabled(a, s) || (owner && a == "Retype" && m.action_enabled("NowAsks", s))
    };
    let steps = ["Announce", "Retype", "GiveUp"]
        .iter()
        .all(|a| enabled(a) == (w.taken == Some(*a)));
    let asks = match w.taken {
        Some(a @ ("Announce" | "Retype")) => {
            let fired = if m.action_enabled(a, s) { a } else { "NowAsks" };
            let mut next = s.clone();
            m.fire(fired, &mut next) && next["asks"] == i64::from(w.asks)
        }
        _ => true,
    };
    let clock =
        owner || !(s["phase"] == 1 && s["window"] == 0) || m.action_enabled("Elapse", s) == w.runs;
    steps && asks && clock
}

/// Every state `m` reaches, invariants unchecked.
fn walk_reachable(m: &Model) -> Vec<S> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

/// The model's value of the constant `name`.
fn walk_const(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .expect("a constant")
}

/// Tier-1: on every reachable state of `harness_upgrade_limit_queue_model`, over every
/// rendering, the real readers, fold, clock and step take exactly the steps the model's guards
/// allow, under the real bounds and queue rest ([`walk_short_rest_probe`]). Negative controls:
/// `Buggy` and each defect knob disagree with the real code somewhere.
#[test]
fn the_real_queue_conforms_to_the_limit_queue_model() {
    use aterm_spec::derive::harness_upgrade_limit_queue_model;
    use aterm_spec::interp;
    let model = harness_upgrade_limit_queue_model();
    assert_eq!(walk_const(&model, "MaxAsks"), i64::from(MAX_ASKS));
    assert_eq!(
        walk_const(&model, "RetypeMax"),
        i64::from(upgrade::REQUEUE_MAX)
    );
    assert_eq!(
        walk_const(&model, "Daily"),
        i64::from(upgrade::QUEUE_REST_DOUBLINGS)
    );
    assert_eq!(upgrade::QUEUE_REST_MAX_S, DAY);
    let retype_max = i64::from(upgrade::REQUEUE_MAX);
    let defective: Vec<(&str, Model)> = std::iter::once(("Buggy", interp::with_buggy(&model, 1)))
        .chain(
            ["CountRetype", "Unbounded", "NoBound", "ForGood", "Flat"]
                .into_iter()
                .map(|knob| (knob, interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect();
    let mut disagree: std::collections::BTreeMap<&str, usize> =
        defective.iter().map(|(n, _)| (*n, 0)).collect();
    let mut taken = std::collections::BTreeSet::new();
    let mut owned = std::collections::BTreeSet::new();
    let states = walk_reachable(&model);
    assert!(states.len() > 100, "{} states", states.len());
    let mut looks = 0;
    let mut full = 0;
    let mut rests = std::collections::BTreeSet::new();
    let mut short = std::collections::BTreeSet::new();
    let mut decisions = Decisions::default();
    let mut renderings = Renderings::default();
    for s in &states {
        for inv in &model.invariants {
            assert!(model.check_invariant(inv.name, s), "{} at {s:?}", inv.name);
        }
        assert_eq!(
            upgrade::limited(Agent::Claude, &walk_screen(s)),
            s["shown"] == 1,
            "the screen's limit at {s:?}"
        );
        let reads_limit = s["shown"] == 1 || s["held"] == 1;
        let is_full = s["phase"] == 1 && s["untaken"] > retype_max && !reads_limit;
        if is_full && s["rested"] == 1 {
            for (shape, running) in walk_short_rest_probe(s) {
                let at = format!("{s:?} ({}, rest running {running})", shape.name);
                let w = decisions.decide(s, &shape, &Request::None);
                assert_eq!(w.queued, running, "the short rest's end at {at}");
                short.insert(running);
            }
        }
        if is_full && s["rested"] == 0 {
            for shape in walk_rest_probe(s) {
                let at = format!("{s:?} ({})", shape.name);
                let w = decisions.decide(s, &shape, &Request::None);
                let agrees = |m: &Model| {
                    let mut next = s.clone();
                    m.fire("Rests", &mut next) && (next["rested"] == 2) == w.queued
                };
                assert!(
                    agrees(&model),
                    "at {at}: a day's rest by the model, the real code queued {}",
                    w.queued
                );
                rests.insert(w.queued);
                for (name, m) in &defective {
                    if !agrees(m) {
                        *disagree.get_mut(name).expect("a knob") += 1;
                    }
                }
            }
        }
        for shape in renderings.of(s) {
            looks += 1;
            let at = format!("{s:?} ({})", shape.name);
            let w = decisions.decide(s, shape, &Request::None);
            assert_eq!(w.limited, reads_limit, "the limit read at {at}");
            assert_eq!(
                w.queued,
                is_full && s["rested"] == 0,
                "a full queue at {at}"
            );
            assert_eq!(
                w.undelivered,
                s["phase"] == 1 && s["untaken"] > 0 && !reads_limit,
                "a notice to type again at {at}"
            );
            assert_eq!(w.requeued, w.undelivered, "{at}");
            if w.queued {
                full += 1;
                assert_eq!(w.step, Step::Wait("queued"), "a full queue at {at}");
            }
            assert!(
                walk_agrees(&model, s, &w, false),
                "at {at}: the real upgrade took {:?} (asks {}, clock runs {})",
                w.step,
                w.asks,
                w.runs
            );
            taken.extend(w.taken);
            for (name, m) in &defective {
                if !walk_agrees(m, s, &w, false) {
                    *disagree.get_mut(name).expect("a knob") += 1;
                }
            }
            // The owner's `--now`, given after the latest notice.
            let now = decisions.decide(s, shape, &Request::Now);
            assert_eq!(now.limited, reads_limit, "the owner's word at {at}");
            assert!(!now.queued, "the owner's word lifts a full queue at {at}");
            if is_full {
                owned.extend(now.taken);
            }
            assert!(
                walk_agrees(&model, s, &now, true),
                "at {at}, on the owner's word: the real upgrade took {:?} (asks {})",
                now.step,
                now.asks
            );
        }
    }
    assert!(looks > states.len(), "several transcripts per state");
    assert!(full > 0, "full queues are walked");
    assert_eq!(
        rests,
        std::collections::BTreeSet::from([false, true]),
        "rests shorter than a day and a day's are both probed"
    );
    assert_eq!(
        short,
        std::collections::BTreeSet::from([false, true]),
        "a short rest is probed on both sides of its end"
    );
    assert_eq!(
        taken,
        std::collections::BTreeSet::from(["Announce", "Retype", "GiveUp"]),
        "every decision of the real code is taken somewhere"
    );
    assert_eq!(
        owned,
        std::collections::BTreeSet::from(["Retype"]),
        "the owner's word types into a full queue"
    );
    for (name, n) in &disagree {
        assert!(
            *n > 0,
            "the real code has the `{name}` defect: {disagree:?}"
        );
    }
}

// ------------------------------------------- a limit that lasts for days

/// A limit naming no reset that lasts a week, looked at every five minutes: copies go twice
/// straight away, then after rests of 2, 4, 8 and 16 hours, then one a day (six the first day,
/// twelve in the week), each the same ask. Negative controls: the owner's `--now` mid-rest
/// types one at once; once the model takes the copies, the next full queue rests two hours.
#[test]
fn a_limit_that_lasts_for_days_adds_a_handful_of_copies_then_one_a_day() {
    let marker = ask_marker(1);
    let t0: u64 = 1_790_000_000;
    let copy = |at: u64| {
        [
            typed_row(&ts(at), OWNERS_SESSION, &notice(&marker), "typed"),
            api_limit_row(at + 1),
        ]
    };
    let screen = {
        let rule = "─".repeat(20);
        vec![rule.clone(), "❯ ".to_string(), rule]
    };
    let look = |rows: &[String], now: u64, request: &Request, request_at: u64| {
        let tail = rows.join("\n") + "\n";
        let mut st = St {
            phase: Phase::Announced { at_s: t0, asks: 1 },
            to: "2.1.283".to_string(),
            marker: marker.clone(),
            salt: OWNERS_SALT,
            request: request.clone(),
            request_at,
            ..St::default()
        };
        let mut f = Facts {
            limited: upgrade::limited(Agent::Claude, &screen),
            ..idle()
        };
        st.phase = clock_held(&st.phase, &f, now);
        let _ = queue_facts(&mut st, &mut f, Some(&tail), now, request);
        let step = upgrade::requested_step(request, &st.phase, &f, false, now, &st.to);
        (step, f)
    };
    let mut rows = vec![tasked_at(t0)];
    rows.extend(copy(t0));
    let mut typed = vec![t0];
    let week = t0 + 7 * DAY;
    let mut now = t0;
    while now < week {
        now += 300;
        let (step, f) = look(&rows, now, &Request::None, 0);
        if step == Step::Announce {
            assert!(f.undelivered, "the same ask again at {now}");
            assert_eq!(
                announce_asks(&Phase::Announced { at_s: t0, asks: 1 }, true),
                1
            );
            rows.extend(copy(now));
            typed.push(now);
        } else {
            assert!(
                matches!(step, Step::Wait("limited" | "queued")),
                "{step:?} at {now}"
            );
        }
    }
    // Each gap is `REASK_S` plus, past the queue's room, the rest for the copies then unread
    // (to the five-minute look).
    let full = usize::try_from(upgrade::REQUEUE_MAX).expect("small");
    for (k, pair) in typed.windows(2).enumerate() {
        let unread = u32::try_from(k + 1).expect("small");
        let rest = if k < full {
            0
        } else {
            upgrade::queue_rest(unread)
        };
        let gap = pair[1] - pair[0];
        assert!(
            (REASK_S + rest..=REASK_S + rest + 300).contains(&gap),
            "copy {}: {gap}s after the one before, {} unread",
            k + 2,
            unread
        );
    }
    let per_day: Vec<usize> = (0..7)
        .map(|d| {
            typed
                .iter()
                .filter(|t| (t0 + d * DAY..t0 + (d + 1) * DAY).contains(*t))
                .count()
        })
        .collect();
    assert_eq!(per_day, [6, 1, 1, 1, 1, 1, 1], "{typed:?}");
    assert_eq!(typed.len(), 12, "a week at the limit: twelve notices");

    // Negative controls: the owner's word mid-rest types one more at once.
    let last = *typed.last().expect("typed");
    let halfway = last + REASK_S + DAY / 2;
    assert_eq!(
        look(&rows, halfway, &Request::None, 0).0,
        Step::Wait("queued"),
        "resting"
    );
    let (asked, f) = look(&rows, halfway, &Request::Now, halfway);
    assert_eq!(asked, Step::Announce, "the owner's word");
    assert!(f.undelivered);
    // The model takes every copy; the next limit's queue rests two hours.
    rows.push(typed_row(&ts(week), OWNERS_SESSION, "go on", "typed"));
    rows.push(model_row(
        &ts(week + 10),
        OWNERS_SESSION,
        "Carrying on.",
        "tool_use",
    ));
    for k in 0..=u64::from(upgrade::REQUEUE_MAX) {
        rows.extend(copy(week + 60 + k * (REASK_S + 60)));
    }
    let scan = upgrade::notice_scan(&(rows.join("\n") + "\n"), &marker).expect("queued");
    assert_eq!(
        scan.untaken,
        upgrade::REQUEUE_MAX + 1,
        "the count starts over"
    );
    let answered = scan.answered_at.expect("the limit row's time");
    assert_eq!(
        scan.rests_until(),
        Some(answered + REASK_S + upgrade::RETRY_S),
        "two hours again"
    );
}

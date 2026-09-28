// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE NOTICES QUEUED BEHIND THE WEEKLY LIMIT (the owner's report of
//! 2026-09-27, tab `s-c543f4e0edd3439e5791`, Claude session
//! `25e3b26e-36c9-41cc-bcd0-d0e0b6a70828`, Claude Code 2.1.280): 0.93.0 typed
//! four notices half an hour apart into a session parked at its weekly limit
//! — each answered within two seconds by Claude Code's `You've hit your
//! weekly limit` row — gave up at 08:07, and when the owner's switch of
//! account let the session go on, all four reached the agent at once. The
//! window then said `Couldn't upgrade Claude · no READY answer after 4
//! notices` of a session that had never been able to answer one. Replayed
//! through the real driver, over the login wall's apparatus: no notice is
//! typed behind one the limit holds — nor a pending first notice behind
//! another round's — a notice whose limit is over is typed again as the same
//! ask straight away while at most `upgrade::REQUEUE_MAX` more wait untaken,
//! past that once the full queue has rested or on the owner's `--now`, and
//! the give-up 0.93.0 left rests and re-arms by main's rule (no stop is for
//! good, `upgrade::RETRY_S`) — its new round waiting out the limit the
//! transcript still says stands. The Tier-1 bind of
//! `harness_upgrade_limit_queue_model` (aterm-spec) closes the file.

use super::*;

/// The owner's conversation.
const OWNERS_SESSION: &str = "25e3b26e-36c9-41cc-bcd0-d0e0b6a70828";

/// The four notices' READY markers, in the order 0.93.0 typed them (its
/// ledger, 2026-09-27 15:06-16:36 UTC; the round's salt `1790373039`).
const MARKERS: [&str; 4] = [
    "ATERM-UPGRADE-READY-504f8ea5",
    "ATERM-UPGRADE-READY-558d0228",
    "ATERM-UPGRADE-READY-921fe1d7",
    "ATERM-UPGRADE-READY-6a57ff7a",
];

/// The round's salt, from the owner's state file.
const OWNERS_SALT: u64 = 1_790_373_039;

/// The owner's state file as 0.94.0 rewrote it after 0.93.0's give-up (read
/// 2026-09-27), every byte but the tab kept.
const OWNERS_STATE: &str = r#"{"agent":"","asks":0,"at":0,"cause":"","codex_home":"","confirm_by":0,"cwd":"","done_at":0,"exited_at":0,"from":"2.1.280","hold_seen":1790539070,"hold_since":1790539070,"last_seq":4735,"launch_model":"","left_at":0,"left_typed":"","line":"","mark":0,"marker":"ATERM-UPGRADE-READY-6a57ff7a","mode":"","model_before":"","model_list":"","noted":"","notice_pid":6099,"notice_start":"Thu Sep 24 02:03:13 2026","outcome":"","pending_since":0,"phase":"failed","pid":0,"prompt":"","ready_since":0,"release":"","request":"-","request_at":0,"request_tab":"","resumed_on":"","resumed_pid":0,"salt":1790373039,"seq_since":1790539070,"shell":0,"source":"managed","tab":"s-c543f4e0edd3439e5791","thread":"","to":"2.1.283","twin":"","wait":"failed","wait_since":1790539070,"why":"unanswered"}"#;

/// CLAUDE CODE'S ANSWER AT THE WEEKLY LIMIT, as the owner's transcript holds
/// it (row 2427, the first notice's own turn, 2026-09-27T15:06:40.380Z: ids
/// cut, every key the readers look at kept) — its reset the owner's
/// (`1791046800`, Oct 3) while that is a day ahead or more, else a day from
/// now: a reset that has passed is another case
/// ([`a_queued_notice_whose_limit_is_over_is_typed_again_as_the_same_ask`]).
fn limit_row(ts: &str, session: &str) -> String {
    limit_row_until(ts, session, 1_791_046_800_u64.max(now_s() + 86_400))
}

/// [`limit_row`] naming the reset `resets` (unix seconds).
fn limit_row_until(ts: &str, session: &str, resets: u64) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000031","isSidechain":false,"type":"assistant","uuid":"00000000-0000-4000-8000-000000000032","timestamp":"{ts}","message":{{"diagnostics":null,"id":"00000000-0000-4000-8000-000000000033","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)"}}],"context_management":null}},"requestId":"req_0000000000000000000002","quotaLimits":{{"status":"rejected","resetsAt":{resets},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"perTurnEffort":"xhigh","session_id":"{session}","userType":"external","entrypoint":"cli","cwd":"/Users//owner/aterm","sessionId":"{session}","version":"2.1.280","gitBranch":"main"}}"#
    )
}

/// AN API RATE LIMIT'S ROW at `at` (unix seconds): the limit row with no
/// `quotaLimits` and no `error` key, its text the API's — a limit that NAMES
/// NO RESET, read by the wall table's words alone.
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

/// The owner's rows from the notices on, each `at` seconds before `now`:
/// every notice of `asked` answered by the limit, then — `went_on` — the
/// owner's `first, continue` and the model's first row after it, saying
/// `said`.
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

/// The owner's state with the stand-in's pid, start and tab where 0.93.0's
/// notice named the owner's (6099, `Thu Sep 24 02:03:13 2026`,
/// `s-c543f4e0edd3439e5791`): every other byte as read.
fn owners_state_for(sf: &SessionFile) -> St {
    let text = OWNERS_STATE
        .replace(
            r#""notice_pid":6099"#,
            &format!(r#""notice_pid":{}"#, sf.pid),
        )
        .replace("Thu Sep 24 02:03:13 2026", &squash(&sf.proc_start))
        .replace("s-c543f4e0edd3439e5791", TAB)
        .replace(r#""last_seq":4735"#, r#""last_seq":77"#);
    St::from_json(&text).expect("the owner's state reads")
}

/// THE NOTICE 0.93.0 WOULD TYPE AGAIN IS NEVER TYPED BEHIND THE FIRST (the
/// report's defect 1): the latest notice's own turn was the limit's and the
/// model has written nothing since — so, though the screen shows no limit
/// (the banner a person's Esc dismissed, a screen the recogniser misses) and
/// its window ran out long ago, the visit waits `limited`, types nothing, and
/// holds its window. Taken by the model at last (the owner's `first,
/// continue`), its window opens THEN: the visit waits for the READY, nothing
/// typed. NEGATIVE CONTROL: the same notice read by the model as it came,
/// its window run out, is asked again.
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

    // NEGATIVE CONTROL: read as it came, its window run out.
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

/// AN AGENT THAT SAYS IT CANNOT STOP YET IS NEVER GIVEN UP ON FOR GOOD (the
/// report's defect 2: "an agent that replied 'cannot stop now, because …' —
/// the notice explicitly invites that — should re-ask later"). The last
/// ask's notice answered so, its window run out: the ROUND gives up, and the
/// agent is released — but for this round only, and no stall the owner is
/// shown (main's 2026-09-27 rule: no stop is for good). It rests one round's
/// worth of asking (`RETRY_S`), and a new round asks again with a notice of
/// its own.
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
    // It rests from this look (the owner's view: `next_round=2h`, no stall —
    // `a_stopped_round_says_when_its_next_round_starts`).
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
        move |_, now| St {
            failed_at: now - upgrade::RETRY_S,
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

/// THE GIVE-UP 0.93.0 LEFT IS NO STALL FOR GOOD (the report's defect 2, and
/// the band row it put up): the owner's state, byte for byte, gave up on four
/// notices each the limit's, and carries no stamp of when — so by main's
/// rule (no stop is for good) its first look re-arms it, typing nothing.
/// Still at the limit by the transcript's word (the screen shows none), the
/// new round's first notice WAITS `limited`: nothing is typed behind the four
/// still queued. Once the model took them (the owner's `first, continue`),
/// the new round asks, with a marker of its own — one notice, not a fifth
/// stacked behind four.
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

/// NOT EVEN A FIRST NOTICE AT THE LIMIT, WHATEVER THE SCREEN SHOWS (the
/// report's defect 1): the session's last word is Claude Code's limit row,
/// its reset an hour off, and its banner gone from the screen — the pending
/// upgrade waits `limited` and types nothing. NEGATIVE CONTROL: the reset
/// the row names has passed, and the notice is typed.
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
    // NEGATIVE CONTROL: the reset has passed.
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

/// A QUEUED NOTICE WHOSE LIMIT IS OVER IS TYPED AGAIN AS THE SAME ASK (review
/// of 2026-09-27: a queued notice held the upgrade `limited` with no bound —
/// a weekly limit continues on its own only after `/rate-limit-options`, and
/// a limit row may name no reset — so a session left idle waited until a
/// person typed). The limit that answered the notice is over by the
/// transcript's word, and the screen shows none: the notice never reached
/// the model, and it is typed again with its own marker, no ask spent, the
/// ledger saying why. Over: the reset its row names has passed; a row naming
/// no reset (an API rate limit) was written [`REASK_S`] ago; a `/login`
/// finished after it (an account switched — and the same `/login` ends a
/// FIRST notice's hold, the reset still days off). NEGATIVE CONTROLS: the
/// API limit a minute old, and a passed reset while the screen still shows a
/// limit, wait `limited` and type nothing.
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
    // The same `/login` ends a FIRST notice's hold.
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

    // NEGATIVE CONTROLS.
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

/// The owner's rows with `copies` copies of the notice `marker`, each
/// answered by an API rate limit that NAMES NO RESET — every copy's own turn
/// the limit's — the last row written `last_ago` seconds before `now`, the
/// ones before it half an hour apart; then, `went_on`, the owner's
/// `first, continue` and the model's first row after it, `ago` seconds back.
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

/// A NOTICE A LIMIT ANSWERED IS TYPED AGAIN STRAIGHT AWAY AT MOST
/// [`upgrade::REQUEUE_MAX`] TIMES BEFORE THE MODEL TAKES ONE (the owner's
/// report of 2026-09-27 was copies piling up in the conversation; a limit row
/// naming no reset is over by the transcript's word every [`REASK_S`] however
/// long the limit really stands, so the re-type alone put a copy there every
/// half hour). An API rate limit answered the notice and every copy typed
/// again, the last row half an hour old, the screen clear: with
/// `1 + REQUEUE_MAX` copies in the conversation the queue is FULL, and the
/// visit WAITS `queued` — nothing typed, its window held — for the model to
/// write a row of its own, or for the queue's rest
/// ([`a_full_queue_rests_and_the_owners_word_types_one_more`]). The session
/// goes on (the owner's `first, continue`): every copy taken at once, the
/// window opened at the model's row. NEGATIVE CONTROLS: one copy, and
/// `REQUEUE_MAX`, are typed again as the same ask (the same marker, the
/// ledger's `a usage limit that is over`); the full count's last row a minute
/// old waits `limited` as any fresh limit does.
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
    // NEGATIVE CONTROL: a fresh limit row waits as it always did.
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

/// A FULL QUEUE IS NEVER HELD FOR GOOD (review of 2026-09-27: past the bound
/// the upgrade waited `limited` until the model wrote a row, `Upgrade now`
/// did not move it, and the band said it "moves once that ends" of a limit
/// over by every word — an idle session whose limit had long ended waited
/// for good). `1 + REQUEUE_MAX` copies wait untaken, each met by an API rate
/// limit naming no reset, the screen clear:
///
/// * the last limit row [`REASK_S`] + [`upgrade::RETRY_S`] old — the queue
///   has RESTED: one copy more is typed, the same marker, the same ask;
/// * the owner's `--now` given after the latest copy was typed: one copy
///   more at once — the person asking;
/// * that word given BEFORE the latest copy (it was spent on a copy
///   already), the queue unrested: `wait:queued`, nothing typed.
///
/// NEGATIVE CONTROLS: the owner's word with the latest copy's limit a minute
/// old waits `limited` (it never types into a limit that stands), and so
/// does a rested queue whose screen still shows the limit.
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

    // NEGATIVE CONTROLS: the limit stands.
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

/// NO FIRST NOTICE BEHIND ANOTHER ROUND'S QUEUED ONE WHILE ITS LIMIT HOLDS
/// (review of 2026-09-27: after a retarget, a new round or 0.93.0's notices,
/// the state names none of the queued notice's markers, and a limit row
/// naming no reset read as no limit — so a pending upgrade typed a fresh
/// notice minutes after an API limit answered an earlier one). An earlier
/// round's notice (a marker this state never minted) was answered by an API
/// rate limit ten minutes ago, the screen clear: the pending upgrade waits
/// `limited`. Half an hour on, the limit is over by the transcript's word and
/// the queue has room: the first notice goes, joining it. With
/// `1 + REQUEUE_MAX` such notices untaken the queue is full: `wait:queued`.
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

// ------------------------------------------- TIER-1: HarnessUpgradeLimitQueue

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

/// ONE RENDERING of a model state: a transcript's tail, and where the latest
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

/// THE TRANSCRIPTS a model state stands for — several, each read the same by
/// the real readers, so the bind is checked against every shape the state
/// covers. Every ask before the latest: typed, and read by the model as it
/// came. The latest ask: with nothing `untaken`, typed and read as it came,
/// or met by the limit and TAKEN later by the model (a person's `continue`,
/// the model's row) — its window, on the state's record, starting when it was
/// typed long before, so that only the take opens it where `window` says;
/// else `untaken` copies of it (the model's top, `RetypeMax + 1 + Daily`, as
/// that many and one more), each met by a limit row — the last one's bound
/// ahead where `held` (an API limit a minute old, or a named reset an hour
/// off), else over (the API limit half an hour old, a named reset just
/// passed, or a named one followed by a person's `/login`, the first copy one
/// of an earlier round's too), and past a full queue's rest where `rested` —
/// a day and a minute on where a day's rest ran (`rested == 2`), all but three
/// minutes of a day where a shorter one did (so every rest the real code
/// takes there must be shorter than a day). With
/// nothing untaken, the session's last word: where `held`, the agent's own
/// turn met a limit whose word still holds (a named reset ahead, an API limit
/// minutes old); else nothing more, or a limit naming no reset half an hour
/// old, one whose reset passed, or one a `/login` ended.
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
    // How long ago the rest began (the transcript's word ran out): a day
    // and a minute for a day's rest, three minutes short of a day for one
    // the model says is shorter.
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

/// THE REST PROBE for a full queue that has not rested (`s`): its
/// renderings with the limit's word on the latest copy run out all but three
/// minutes of a day ago (an API limit's row, a named reset, a `/login` after
/// a named one — [`walk_shapes`]' renderings of the state the model's
/// shorter rest leaves), at which a rest shorter than a day has run and a
/// day's has not.
fn walk_rest_probe(s: &S) -> Vec<Shape> {
    let mut probed = s.clone();
    probed.insert("rested", 1);
    walk_shapes(&probed, None)
}

/// THE SHORT REST'S EXACT END for a full queue the model says rested less
/// than a day (`s`, `rested == 1`): its renderings with the limit's word run
/// out a minute short of [`upgrade::queue_rest`] of its `untaken` copies ago
/// (the rest still running: `true`, the real code must wait `queued`) and a
/// minute past it (the rest run: `false`). The model says only "shorter
/// than a day"; this pins the real rest to the copies the real fold counts
/// from the transcript — every unread copy, whatever its round — which are
/// the model's `untaken`.
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
#[derive(Debug)]
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

/// THE REAL DECISION at the look a model state stands for, over one of its
/// renderings, in the driver's order: the screen's limit
/// ([`upgrade::limited`]) and the look's clock ([`upgrade::clock_held`]);
/// the transcript's limit and queue ([`queue_facts`]), under the owner's
/// `request` — `--now` given at this look, after every notice was typed; the
/// step ([`upgrade::requested_step`], no READY in the tail, no round rested:
/// the rest and the new round are the never-strands model's); the ask a
/// notice goes as ([`upgrade::announce_asks`]); and whether the clock runs —
/// a look a whole window on, the same facts, still finds the window where it
/// was.
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

/// Whether the model `m` agrees at `s` with what the real code decided: it
/// takes exactly the step the real code took (`Announce`, `Retype`,
/// `GiveUp`; under the owner's `--now`, the same ask typed again is `Retype`
/// or the owner's `NowAsks`), to the ask the real code counts, and — the
/// upgrade's own look — lets the window run out exactly where the real clock
/// runs (under the owner's word a full queue's copy is typed at this very
/// look, and its window starts from it).
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

/// TIER-1: on EVERY reachable state of `harness_upgrade_limit_queue_model`
/// (aterm-spec), over EVERY rendering the state stands for, the REAL code —
/// the screen's limit reader, the transcript's ([`upgrade::notice_scan`],
/// `transcript_limit_until`), the driver's fold ([`queue_facts`]), the clock,
/// the reducer under the owner's word ([`upgrade::requested_step`]) and the
/// ask count — reads what the state says and takes exactly the step the
/// model's guards allow: the limit read where the screen shows it or the
/// transcript's word holds; a FULL queue (more than `REQUEUE_MAX` notices
/// untaken) read where it has not rested, waiting `queued`; the same ask typed
/// again exactly where `Retype` is enabled (and to the same ask), a fresh one
/// exactly where `Announce` is (to the next), the give-up exactly where
/// `GiveUp` is, the window let run exactly where `Elapse` may. And under the
/// owner's `--now`, given after the latest notice: the same, the owner's
/// `NowAsks` a copy more into a full queue — never where the limit stands.
/// The model's bounds are the real ones (`MaxAsks` = [`MAX_ASKS`],
/// `RetypeMax` = [`upgrade::REQUEUE_MAX`], `Daily` =
/// [`upgrade::QUEUE_REST_DOUBLINGS`]).
///
/// THE REST'S LENGTH, by the real code's own decision: at every full queue
/// that has not rested, the transcript is rendered once more with the
/// limit's word run out all but three minutes of a DAY ago, and the real code
/// still waits `queued` there exactly where the model's `Rests` from that
/// state is a day's (`rested == 2`) — a rest shorter than a day has run, and
/// the copy goes. With the renderings of `rested` states (a day and a minute
/// on, or three minutes short of a day), the real rest is shorter than a day
/// for the first `Daily` copies past the queue's room and a day, no more,
/// from then on. And at every full queue the model says rested less than a
/// day, the real rest ends at [`upgrade::queue_rest`] of the model's
/// `untaken` exactly: still `queued` a minute before, gone a minute after
/// ([`walk_short_rest_probe`]).
///
/// NEGATIVE CONTROLS: the model with any ONE defect switched on — the owner's
/// build (`Buggy`), and each knob, the flat rest (`Flat`) among them —
/// disagrees with the real code somewhere, so a green walk is not vacuous.
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
                let w = walk_decide(s, &shape, &Request::None);
                assert_eq!(w.queued, running, "the short rest's end at {at}");
                short.insert(running);
            }
        }
        if is_full && s["rested"] == 0 {
            for shape in walk_rest_probe(s) {
                let at = format!("{s:?} ({})", shape.name);
                let w = walk_decide(s, &shape, &Request::None);
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
        for shape in walk_transcripts(s) {
            looks += 1;
            let at = format!("{s:?} ({})", shape.name);
            let w = walk_decide(s, &shape, &Request::None);
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
            let now = walk_decide(s, &shape, &Request::Now);
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

/// A LIMIT THAT NAMES NO RESET AND LASTS A WEEK, through the real readers,
/// fold and reducer (the owner, 2026-09-27: such a limit still added one copy
/// of the notice per rest, every `REASK_S + RETRY_S` — two and a half hours,
/// about ten a day — for as long as it stood). An announced upgrade's notice
/// was met by an API rate limit; the session is looked at every five
/// minutes for seven days, the screen clear, and every copy the upgrade
/// types is met by the limit again, which never ends. The copies go straight
/// away twice (half an hour apart: the limit's word runs out), then after
/// rests of two, four, eight and sixteen hours, then a day each: the first
/// day holds six notices, every later day one, the week twelve — never a
/// pile. Each copy is the same ask, and none goes before its rest has run.
/// NEGATIVE CONTROLS: the owner's `Upgrade now` in the middle of a day-long
/// rest types one more at once (the rest never outlasts the person's word);
/// and once the model takes the copies (the session went on), the next
/// limit's full queue rests two hours again — the rest grows with copies
/// UNREAD, not for good.
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
    // Every gap is the limit's word (`REASK_S`) and, past the queue's room,
    // the rest for the copies then unread — to the look.
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

    // NEGATIVE CONTROLS. The owner's word, halfway through a day's rest:
    // one more at once.
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

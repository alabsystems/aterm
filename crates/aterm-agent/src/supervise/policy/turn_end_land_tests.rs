// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use alab_policy::config::TurnEndTiming;

/// THE PROXY'S COMPOSITION (the land check's measure M1): each holdout case's check as this build
/// types it ([`land_check::land_check_text`]), hinted where the reader reads a clause open, with
/// the plain and hinted templates' sha printed for the proxy run's `--frozen`.
#[test]
#[ignore = "a measurement: ATERM_LAND_HOLDOUT=<tsv> ATERM_LAND_CHECK_PROMPTS=<out.tsv> --ignored --nocapture"]
fn land_check_prompts_for_a_holdout() {
    let sha = |s: &str| aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(s.as_bytes()));
    let plain = land_check::land_check_text(None, false, None);
    let hinted = land_check::land_check_text(Some("\u{ab}CLAUSE\u{bb}"), false, None);
    println!("TEMPLATE plain sha256={}", sha(&plain));
    println!("TEMPLATE hinted sha256={}", sha(&hinted));
    let (Some(holdout), Some(out)) = (
        std::env::var_os("ATERM_LAND_HOLDOUT"),
        std::env::var_os("ATERM_LAND_CHECK_PROMPTS"),
    ) else {
        return;
    };
    let text = std::fs::read_to_string(&holdout).expect("the holdout reads");
    let mut rows = String::from("id\tform\tsha256\tprompt\n");
    for (_, id, _, reply) in alab_policy::corpus::open_work::holdout_cases(&text) {
        let hint = land_check::hint_clause(&reply);
        let prompt = land_check::land_check_text(hint.as_deref(), false, None);
        let form = if hint.is_some() { "hinted" } else { "plain" };
        rows.push_str(&format!(
            "{id}\t{form}\t{}\t{}\n",
            sha(&prompt),
            prompt.replace(['\t', '\n'], " ")
        ));
    }
    std::fs::write(&out, rows).expect("the prompts write");
}

/// UNDER LAND A HELD CARRY-ON'S SHORT REPLY MEASURES THE BACK-OFF FROM THE REPLY, ON THE SAME CLOCK
/// AS CRUISE'S (`a_held_carry_ons_short_reply_measures_the_back_off_from_the_reply`): once the
/// instruction is typed, Land's nudge waits out a short streak as Cruise's continue does. A hold
/// that outlasted the back-off used to bring the nudge on the reply's heels. CONTROL: the
/// instruction itself still comes at once, on no clock.
#[test]
fn a_held_carry_ons_short_reply_under_land_measures_the_back_off_from_the_reply() {
    let backoff = TurnEndTiming::default().short_backoff;
    let short = Duration::from_secs(20);
    let mut st = TurnEndState::default();
    let mut now = t0();
    let a = act(
        &mut st,
        &land(idle("The parser reads the first half.", Some(short))),
        now,
    );
    assert!(
        matches!(&a, TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LAND),
        "CONTROL: the instruction at once: {a:?}"
    );
    now += short;
    let a = at(
        &mut st,
        &land(idle("The second half is next.", Some(short))),
        now,
    );
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { .. }),
        "a short reply backs off: {a:?}"
    );
    // The hold: the back-off runs out behind a draft, with nothing typed.
    now += backoff * 8;
    st.host_typed(now, HostTurn::CarryOn);
    let worked = Duration::from_secs(3);
    now += worked;
    let a = at(
        &mut st,
        &land(idle("Picked it up; the lexer is next.", Some(worked))),
        now,
    );
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { until, .. } if *until >= now + backoff),
        "the back-off from the reply: {a:?}"
    );
}

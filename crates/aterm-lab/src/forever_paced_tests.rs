// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for HC under continuous forward, over `same-cruise`'s journal as main recorded it on
//! Claude Code 2.1.316 (bo1's lander, `l-cl-cru`, 2026-11-02: its done checks at +134.0 s and
//! +381.3 s, and the recovery's wait at +384.9 s), carried on to the request at its due.

use super::*;

/// The first prompt (unix ms).
const T0: i64 = 1_792_922_000_000;

/// A journal line at `t` ms after [`T0`].
fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

/// `until=`'s stamp for `t` ms after [`T0`], to the second as the journal writes it.
fn until(t: i64) -> String {
    crate::util::utc_iso((T0 + t).div_euclid(1000)).replace(".000Z", "Z")
}

/// A back-off the loop named at `t`, its step `step` ms for `streak` short turns.
fn backoff(t: i64, seq: u32, streak: u32, step: i64) -> (i64, String) {
    let words = if step < 120_000 {
        format!("{} s", step / 1000)
    } else {
        format!("{} min", step / 60_000)
    };
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} {streak} short turn(s) in a row: {words} back-off",
            until(t + step)
        ),
    )
}

/// The recovery's wait at `t`, `step` ms.
fn recovery(t: i64, seq: u32, step: i64) -> (i64, String) {
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} {}",
            until(t + step),
            turn_end::REPEAT_RECOVERY_WAIT
        ),
    )
}

const CLAIM: &str = "⏺ Merged the parser and pushed it to origin/main. All done.";
const SAME: &str = "⏺ Nothing left: the parser is committed and pushed to origin/main.";
const CHECK: &str = "[aterm harness] Autopilot is Cruise: done is never the end. This time, run \
                     the widest check this repository has on the merged tree and fix what it finds.";

/// The request's journal line, as `g2-cl-lad` journaled main's (the head of
/// [`turn_end::FORWARD_RECOVERY`], cut where the journal cuts it).
fn request(t: i64, seq: u32) -> (i64, String) {
    let text: String = turn_end::FORWARD_RECOVERY.chars().take(200).collect();
    j(t, &format!("CONTINUED seq={seq} rule=continue@v1 {text}"))
}

/// Main's `same-cruise` (the module's header), its request typed at its due and answered alike.
fn main_paced() -> Seen {
    Seen {
        journal: vec![
            j(11_500, &format!("EVENT idle seq=871 {CLAIM}")),
            backoff(12_500, 871, 1, 120_000),
            j(
                134_000,
                &format!("CONTINUED seq=871 rule=continue-done-check@v1 {CHECK}"),
            ),
            j(140_600, &format!("EVENT idle seq=1751 {SAME}")),
            backoff(141_200, 1751, 2, 240_000),
            j(
                381_300,
                &format!("CONTINUED seq=1751 rule=continue-done-check@v1 {CHECK}"),
            ),
            j(384_800, &format!("EVENT idle seq=2640 {SAME}")),
            recovery(384_900, 2640, 480_000),
            request(866_000, 2640),
            j(869_500, &format!("EVENT idle seq=3500 {SAME}")),
            recovery(869_600, 3500, 960_000),
        ],
        end_ms: T0 + 880_000,
        ..Seen::default()
    }
}

#[test]
fn main_paces_the_repeated_report_and_asks_for_a_different_approach() {
    let v = judge(&main_paced(), SHIPPED, T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        v.why().contains("a different approach asked at +866.0 s"),
        "{v:?}"
    );
    assert!(v.why().contains("each answered"), "{v:?}");
    assert!(
        v.why()
            .contains("its answer's next step decided at +869.6 s"),
        "{v:?}"
    );
}

/// THE NEGATIVE CONTROLS OF THE STEP AFTER THE REQUEST: a build that asks for a different
/// approach once and then parks its answer — left to the person, ended, waiting in other words, or
/// waiting off the ladder's fourth step (16 min) — stopped, though it never held.
#[test]
fn a_build_that_parks_after_its_request_fails() {
    let parked = |line: String| {
        let mut s = main_paced();
        s.journal[10] = j(869_600, &line);
        judge(&s, SHIPPED, T0)
    };
    let ten_hours = 10 * 60 * 60 * 1000;
    for (line, why) in [
        (
            format!(
                "WAITING seq=3500 until={} the agent answered with nothing; trying it again later",
                until(869_600 + ten_hours)
            ),
            "as a wait in other words",
        ),
        (
            "LEFT seq=3500 rule=left-to-person@v1 the same report again".to_string(),
            "as left to the person",
        ),
        (
            "DONE seq=3500 rule=continue@v1 x".to_string(),
            "ENDED at +869.6 s",
        ),
        (
            recovery(869_600, 3500, 480_000).1,
            "not the ladder's step 4",
        ),
        (
            recovery(869_600, 3500, ten_hours).1,
            "not the ladder's step 4",
        ),
        (
            backoff(869_600, 3500, 4, ten_hours).1,
            "not the ladder's step 4",
        ),
    ] {
        let v = parked(line.clone());
        assert!(
            matches!(v, Verdict::Fail(_)) && v.why().contains(why),
            "{line}: {v:?}"
        );
    }
    // The ladder's fourth step said as a back-off, not a request's wait, is a step too.
    let v = parked(backoff(869_600, 3500, 4, 960_000).1);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
}

/// A request whose answer the loop never decides is judged by the silence: unobserved while the
/// watch is short (never a pass), PERSISTENCE LOST once it ran past
/// [`super::super::UNJUDGED_MS`].
#[test]
fn an_undecided_answer_to_the_request_is_silence() {
    let mut s = main_paced();
    s.journal.truncate(10);
    s.end_ms = T0 + 900_000;
    assert!(!watched(&s, s.end_ms, T0));
    let v = judge(&s, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Unobserved(_)) && v.why().contains("not decided within"),
        "{v:?}"
    );
    s.end_ms = T0 + 866_000 + super::super::UNJUDGED_MS;
    let v = judge(&s, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("PERSISTENCE LOST"),
        "{v:?}"
    );
}

/// THE NEGATIVE CONTROL: a build that types at a constant rate (an act 125 s after each reply,
/// under back-offs that name the first step each time) fails the gap and the hour.
#[test]
fn a_build_typing_at_a_constant_rate_fails() {
    let mut s = Seen {
        journal: vec![j(11_500, &format!("EVENT idle seq=1 {CLAIM}"))],
        ..Seen::default()
    };
    let mut t = 11_500;
    for n in 1..=8u32 {
        s.journal.push(backoff(t + 100, n, 1, 120_000));
        let rule = if n <= 2 {
            "continue-done-check@v1"
        } else {
            "continue@v1"
        };
        let act = t + 125_000;
        let text = if n <= 2 {
            CHECK.to_string()
        } else {
            turn_end::FORWARD_RECOVERY.chars().take(200).collect()
        };
        s.journal
            .push(j(act, &format!("CONTINUED seq={n} rule={rule} {text}")));
        t = act + 3_500;
        s.journal
            .push(j(t, &format!("EVENT idle seq={} {SAME}", n + 1)));
    }
    s.end_ms = T0 + t + 10_000;
    let v = judge(&s, SHIPPED, T0);
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(v.why().contains("SPAM: act 2"), "{v:?}");
    assert!(v.why().contains("acts within an hour"), "{v:?}");
}

/// A build that holds at the repeated report (ruling F, `bin-w4`'s) stopped for good.
#[test]
fn a_hold_is_a_permanent_stop() {
    let mut held = main_paced();
    held.journal.truncate(7);
    held.journal.push(j(
        384_900,
        "EXCEPTION seq=2640 kind=stalled when=later held=same-reply Its program answers aterm's \
         acts with the same reply and no work, so aterm types nothing more.",
    ));
    held.end_ms = T0 + 445_000;
    let v = judge(&held, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().starts_with("STOPPED FOR GOOD at +384.9 s"),
        "{v:?}"
    );
    assert!(watched(&held, T0 + 444_900, T0));
    assert!(!watched(&held, T0 + 444_800, T0));
}

/// A wait past which nothing comes is a stop; a watch that ends short of its due is unobserved.
#[test]
fn nothing_past_the_due_fails_and_a_watch_short_of_it_is_unobserved() {
    let mut s = main_paced();
    s.journal.truncate(8);
    s.end_ms = T0 + 384_900 + 480_000 + PERSISTENCE_GRACE_MS + 1_000;
    let v = judge(&s, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("PERSISTENCE LOST"),
        "{v:?}"
    );
    assert!(watched(&s, s.end_ms, T0));
    s.end_ms = T0 + 600_000;
    let v = judge(&s, SHIPPED, T0);
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
    assert!(v.why().contains("the next due at +864"), "{v:?}");
    assert!(!watched(&s, s.end_ms, T0));
}

/// The act at the recovery point must be the request, after its wait, at its due.
#[test]
fn the_request_is_the_recovery_after_its_wait_at_its_due() {
    let mut checked = main_paced();
    checked.journal[8] = j(
        866_000,
        &format!("CONTINUED seq=2640 rule=continue-done-check@v1 {CHECK}"),
    );
    let v = judge(&checked, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("not the request for a different"),
        "{v:?}"
    );
    let mut unwaited = main_paced();
    unwaited.journal[7] = backoff(384_900, 2640, 3, 480_000);
    let v = judge(&unwaited, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("followed no wait for it"),
        "{v:?}"
    );
    let mut early = main_paced();
    early.journal[8] = request(700_000, 2640);
    let v = judge(&early, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("SPAM: act 3"),
        "{v:?}"
    );
    assert!(v.why().contains("before the due"), "{v:?}");
    let mut late = main_paced();
    late.journal[8] = request(1_000_000, 2640);
    late.journal[9].0 = T0 + 1_003_500;
    late.journal[10].0 = T0 + 1_003_600;
    late.end_ms = T0 + 1_010_000;
    let v = judge(&late, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("past the due"),
        "{v:?}"
    );
}

/// The chain's premise: every act answered alike; a need, an end or a first act that is no
/// done check fails.
#[test]
fn answers_that_differ_an_end_a_need_or_another_first_act_fail() {
    let mut differ = main_paced();
    differ.journal[6] = j(
        384_800,
        "EVENT idle seq=2640 ⏺ Implemented the parser module.",
    );
    let v = judge(&differ, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("its answers differ"),
        "{v:?}"
    );
    let mut ended = main_paced();
    ended
        .journal
        .push(j(870_000, "DONE seq=3500 rule=continue@v1 x"));
    assert!(matches!(judge(&ended, SHIPPED, T0), Verdict::Fail(_)));
    let mut needed = main_paced();
    needed
        .journal
        .push(j(870_000, "NEEDS seq=3500 rotate the deploy key"));
    assert!(matches!(judge(&needed, SHIPPED, T0), Verdict::Fail(_)));
    let mut other = main_paced();
    other.journal[2] = j(
        134_000,
        "CONTINUED seq=871 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
    );
    let v = judge(&other, SHIPPED, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("not continue-done-check@v1"),
        "{v:?}"
    );
    let none = Seen {
        end_ms: T0 + 100_000,
        ..Seen::default()
    };
    assert!(matches!(judge(&none, SHIPPED, T0), Verdict::Fail(_)));
}

/// The watch ends past the decision on the request's answer, not before.
#[test]
fn the_watch_ends_past_the_requests_answer() {
    let s = main_paced();
    assert!(!watched(&s, T0 + 869_600 + PAST_ANSWER_MS - 1, T0));
    assert!(watched(&s, T0 + 869_600 + PAST_ANSWER_MS, T0));
    let mut asked = s;
    asked.journal.truncate(9);
    assert!(!watched(&asked, T0 + 900_000, T0));
}

/// The hour's bound is the ladder's: five acts on the shipped ladder, twelve at an eighth.
#[test]
fn the_hours_bound_is_the_ladders() {
    assert_eq!(per_hour(SHIPPED), 5);
    assert_eq!(per_hour(super::super::LADDER_SCALE), 12);
    let head = recovery_head();
    assert!(!head.is_empty() && !head.contains(']'), "{head:?}");
    assert!(turn_end::FORWARD_RECOVERY.contains(head));
    assert!(request(0, 1).1.contains(head));
}

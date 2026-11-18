// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN ACCOUNT MOVE MADE AT A USAGE LIMIT, AND THE LIMIT THE RESUMED TRANSCRIPT DRAWS AGAIN. The
//! live sequence of 2026-11-16 (Claude Code 2.1.323): the turn ended on m2's weekly limit, the
//! host relaunched the conversation under m2-3 (`done:resumed`, nothing typed), and the resumed
//! screen drew m2's limit again. That limit's notice was written before the move was made
//! ([`TurnEndState::limit_left`]). It is no wall of the session's, and the move's own
//! continuation, in its own words ([`MOVED_TEXT`]), is what asks the move's answer. A limit the
//! moved-to account meets still walls it, whatever process it meets it in.

use super::*;

/// The process the move ended started an hour before its limit.
const OLD_START: i64 = 1_794_850_000;

/// The left account's limit notice, written in that process's last turn.
const NOTICE: i64 = OLD_START + 3600;

/// The second the loop booked the move, seven seconds after the notice.
const MADE: i64 = NOTICE + 7;

/// The moved-to process, started as the move was booked.
const NEW_START: i64 = MADE;

const TO: &str = "m2-3";

fn answering(words: &str) -> ScreenAnswer {
    ScreenAnswer {
        prompt: crate::supervise::approvals::words_hash(words),
        work: None,
        harness: false,
    }
}

/// A session limit on screen, answering `asked`.
fn limit_on(t: Instant, asked: &str, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        answered: Some(answering(asked)),
        worked,
        ..session_wall(t)
    }
}

/// A session at its account's limit (its process started at [`OLD_START`], the notice written
/// at [`NOTICE`]), moved to [`TO`] there and the move booked at [`MADE`], the moved-to process
/// named started at [`NEW_START`].
fn moved_at_the_limit(t: Instant) -> TurnEndState {
    let mut st = TurnEndState::default();
    st.agent_started_unix(OLD_START);
    st.notice_written_at(Some(NOTICE));
    let wall = advised(&limit_on(t, "carry on with stage 4", Some(3 * MIN)), TO);
    let a = at(&mut st, &wall, t);
    let to = to_account(TO, AccountAt::Wall);
    assert!(restarts(&a, &to, RULE_ACCOUNT), "{a:?}");
    st.restarted(&to, true, &wall, t);
    st.account_made_at(MADE);
    st.agent_started_unix(NEW_START);
    st
}

#[test]
fn the_limit_a_move_left_is_no_wall_and_the_move_continues_once_in_its_words() {
    let t = t0();
    let mut st = moved_at_the_limit(t);
    let replayed = limit_on(t, "carry on with stage 4", None);
    st.notice_written_at(Some(NOTICE));
    assert!(st.limit_left(&replayed));
    assert_eq!(
        st.account_answer(&replayed),
        None,
        "the left limit answers nothing"
    );
    assert_eq!(st.account_answer_unseen(&replayed, t + MIN), None);
    st.observe(&replayed, t + MIN);
    assert!(!st.wall_standing(), "no track of the left account's limit");
    assert_eq!(
        st.account_point(&replayed, &cfg()),
        None,
        "the host is not asked"
    );
    let a = decide_turn_end(&st, &replayed, &cfg(), t + MIN);
    assert_eq!(a, moved_on(), "the move's own continuation");
    let TurnEndAction::Type { text, .. } = &a else {
        unreachable!()
    };
    assert_eq!(
        text,
        "[aterm harness] This session moved to another account; continue where you stopped."
    );
    assert!(!text.contains("cleared"), "no limit cleared: {text}");
    assert!(!text.contains(TO), "no roster label typed: {text}");
    assert!(is_cleared_line(text), "it delivers the cut prompt again");
    st.acted(&a, &replayed, t + MIN);
    assert!(!st.account_resume_owed(), "typed once: owed no more");
    let served = idle("Stage 4 resumed; the suites are green.", Some(4 * MIN));
    assert_eq!(
        st.account_answer(&served),
        Some(AccountAnswer::Served),
        "the continuation's turn proves the moved-to account"
    );
}

/// THE MOVE'S OWN SECOND DATES THE LIMIT IT LEFT, never a process start (review findings 1 and
/// 3 of 2026-11-16): the host may name the moved-to process before the loop books the move (a
/// move it told as made at a later point), name the ended one for a while after it (a snapshot
/// that lags the relaunch), name a later relaunch of the moved-to account, or name none. Every
/// order gives the same verdict: the left notice is left, the moved-to account's own walls it.
/// The control: with no second told, nothing is judged left.
#[test]
fn the_left_limit_is_dated_by_the_move_whatever_process_the_host_names() {
    let t = t0();
    let wall = advised(&limit_on(t, "carry on with stage 4", Some(3 * MIN)), TO);
    let to = to_account(TO, AccountAt::Wall);
    let replayed = limit_on(t, "carry on with stage 4", None);
    let orders: [(&str, Option<i64>, Option<i64>); 5] = [
        (
            "the moved-to start named before the move is booked",
            Some(NEW_START),
            None,
        ),
        (
            "the ended process named after it",
            Some(OLD_START),
            Some(OLD_START),
        ),
        (
            "the moved-to start named after it",
            Some(OLD_START),
            Some(NEW_START),
        ),
        ("a later relaunch named", Some(OLD_START), Some(MADE + 3600)),
        ("no start named", None, None),
    ];
    for (what, before, after) in orders {
        let mut st = TurnEndState::default();
        if let Some(start) = before {
            st.agent_started_unix(start);
        }
        st.notice_written_at(Some(NOTICE));
        let a = at(&mut st, &wall, t);
        assert!(restarts(&a, &to, RULE_ACCOUNT), "{what}: {a:?}");
        st.restarted(&to, true, &wall, t);
        let unbooked = st.clone();
        st.account_made_at(MADE);
        if let Some(start) = after {
            st.agent_started_unix(start);
        }
        st.notice_written_at(Some(NOTICE));
        assert!(st.limit_left(&replayed), "{what}");
        assert!(!unbooked.limit_left(&replayed), "{what}: the control");
        assert_eq!(
            decide_turn_end(&st, &replayed, &cfg(), t + MIN),
            moved_on(),
            "{what}"
        );
        st.notice_written_at(Some(MADE + 120));
        assert!(
            !st.limit_left(&replayed),
            "{what}: the moved-to account's own"
        );
    }
}

#[test]
fn a_limit_the_moved_to_account_meets_walls_it_and_answers_the_move() {
    let t = t0();
    // A prompt someone asked under the move, answered by the moved-to account's own limit before
    // any point saw it: its notice was written after the move.
    let mut st = moved_at_the_limit(t);
    let own = limit_on(t, "what is left of stage 4?", None);
    st.notice_written_at(Some(MADE + 30));
    assert!(!st.limit_left(&own));
    assert_eq!(
        st.account_answer_unseen(&own, t + MIN),
        Some(AccountAnswer::Bad(AccountBad::Wall))
    );

    // The move's continuation, answered by the moved-to account's own limit.
    let mut st = moved_at_the_limit(t);
    let replayed = limit_on(t, "carry on with stage 4", None);
    st.notice_written_at(Some(NOTICE));
    let a = at(&mut st, &replayed, t + MIN);
    assert_eq!(a, moved_on());
    st.acted(&a, &replayed, t + MIN);
    let met = advised(&limit_on(t, MOVED_TEXT, Some(2 * MIN)), "pool-a");
    st.notice_written_at(Some(MADE + 120));
    assert!(!st.limit_left(&met));
    assert_eq!(
        st.account_answer(&met),
        Some(AccountAnswer::Bad(AccountBad::Wall)),
        "the moved-to account is struck"
    );
    st.account_told(AccountAnswer::Bad(AccountBad::Wall));
    st.observe(&met, t + 3 * MIN);
    assert!(st.wall_standing(), "its own limit is tracked");
    assert_eq!(
        st.account_point(&met, &cfg()),
        Some(AccountPoint::Wall {
            kind: WallKind::UsageSession
        })
    );
    let next = decide_turn_end(&st, &met, &cfg(), t + 3 * MIN);
    assert!(
        restarts(&next, &to_account("pool-a", AccountAt::Wall), RULE_ACCOUNT),
        "it moves on: {next:?}"
    );
}

/// The move's continuation typed and its turn not yet taken up: the left limit still drawn when
/// that act's wait lapses is no wall either. The session is not moved off the moved-to account
/// for it, and no reset is waited for, for a limit that account never met.
#[test]
fn the_left_limit_still_drawn_after_the_continuation_moves_nothing_and_waits_no_reset() {
    let t = t0();
    let mut st = moved_at_the_limit(t);
    let replayed = limit_on(t, "carry on with stage 4", None);
    st.notice_written_at(Some(NOTICE));
    let a = at(&mut st, &replayed, t + MIN);
    assert_eq!(a, moved_on());
    st.acted(&a, &replayed, t + MIN);
    let still = advised(&limit_on(t, "carry on with stage 4", None), "pool-a");
    st.notice_written_at(Some(NOTICE));
    assert!(st.limit_left(&still));
    let first = t + 2 * MIN;
    let taking = at(&mut st, &still, first);
    assert!(
        matches!(&taking, TurnEndAction::WaitUntil { why, .. } if why.contains(RULE_LIMIT_RESUME)),
        "the continuation is waited for: {taking:?}"
    );
    let lapsed = first + TurnEndTiming::default().take_within + Duration::from_secs(1);
    let next = at(&mut st, &still, lapsed);
    assert!(
        !matches!(next, TurnEndAction::Restart { .. }),
        "no move off the moved-to account for the left limit: {next:?}"
    );
    assert!(
        !waits_until(&next, t + 60 * MIN + GRACE),
        "no wait for the left limit's reset: {next:?}"
    );
    assert!(!st.wall_standing(), "{next:?}");
}

/// Risk B of the 2026-11-16 review: the resumed transcript shows another prompt than the one the
/// move's point closed, answered by the left account's limit (a background task's notice the
/// old process took up after that point, or the same prompt drawn otherwise). It was the left
/// account's answer, so it strikes nothing and latches no answer.
#[test]
fn a_prompt_the_left_limit_answered_is_no_answer_of_the_moves() {
    let t = t0();
    let mut st = moved_at_the_limit(t);
    let other = limit_on(t, "<task-notification> the suite finished", None);
    st.notice_written_at(Some(NOTICE + 4));
    assert!(st.limit_left(&other));
    assert!(st.account_answer_open(&other));
    assert_eq!(st.account_answer_unseen(&other, t + MIN), None, "no strike");
    assert_eq!(st.account_answer(&other), None);
    st.observe(&other, t + MIN);
    let a = decide_turn_end(&st, &other, &cfg(), t + MIN);
    assert_eq!(a, moved_on());
}

/// Risk A of the 2026-11-16 review: a `Bad(Wall)` told for the left limit latches the move's
/// answer, and the moved-to account's own limit at the end of the continuation's turn is then
/// told nothing. The left limit tells no answer, so the latch stays open for that limit.
#[test]
fn the_moved_to_accounts_own_limit_after_the_left_one_is_still_told() {
    let t = t0();
    let run = |latched: bool| {
        let mut st = moved_at_the_limit(t);
        let other = limit_on(t, "<task-notification> the suite finished", None);
        st.notice_written_at(Some(NOTICE + 4));
        if let Some(answer) = st.account_answer_unseen(&other, t + MIN) {
            st.account_told(answer);
        }
        if latched {
            st.account_told(AccountAnswer::Bad(AccountBad::Wall));
        }
        let a = at(&mut st, &other, t + MIN);
        st.acted(&a, &other, t + MIN);
        let met = limit_on(t, MOVED_TEXT, Some(2 * MIN));
        st.notice_written_at(Some(MADE + 120));
        st.account_answer(&met)
    };
    assert_eq!(run(false), Some(AccountAnswer::Bad(AccountBad::Wall)));
    assert_eq!(
        run(true),
        None,
        "the latch the left limit set would swallow the moved-to account's own limit"
    );
}

#[test]
fn a_later_loop_continues_a_move_at_the_limit_it_left_in_the_moves_words() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.seed_account(
        TO.to_string(),
        AccountAt::Wall,
        5 * MIN,
        false,
        false,
        false,
        t,
    );
    st.account_made_at(MADE);
    st.agent_started_unix(NEW_START);
    assert!(st.account_resume_owed());
    let replayed = limit_on(t, "carry on with stage 4", None);
    st.notice_written_at(Some(NOTICE));
    assert!(st.limit_left(&replayed));
    let a = at(&mut st, &replayed, t);
    assert!(!st.wall_standing());
    assert_eq!(
        a,
        moved_on(),
        "the owed continuation survives the seed's first point"
    );
}

/// A LATER LOOP TAKES UP A MOVE PAST A RELAUNCH (review findings 2 and 4 of 2026-11-16): the
/// move to B was made and its continuation typed; B met its own limit; then B's conversation was
/// relaunched (a memory or model restart, a crash's) and a new loop took the move up from the
/// ledger, its answer told (`until=served`) or not yet. The process that loop finds started
/// after B's own notice, and the loop's own reads name it first. B's limit is still B's, by the
/// move's own second: it walls B, B is struck where its answer is owed, and nothing is typed
/// into the limited account. The control: dated by that later process, B's notice would read as
/// the left account's.
#[test]
fn a_later_loop_past_a_relaunch_still_walls_the_moved_to_accounts_own_limit() {
    let t = t0();
    let own = MADE + 600;
    let relaunched = own + 900;
    for told in [false, true] {
        let mut st = TurnEndState::default();
        st.agent_started_unix(relaunched);
        st.seed_account(
            TO.to_string(),
            AccountAt::Wall,
            20 * MIN,
            true,
            told,
            told,
            t,
        );
        let mut control = st.clone();
        control.account_made_at(relaunched);
        st.account_made_at(MADE);
        let met = limit_on(t, MOVED_TEXT, Some(2 * MIN));
        st.notice_written_at(Some(own));
        control.notice_written_at(Some(own));
        assert!(control.limit_left(&met), "told {told}: the control");
        assert!(!st.limit_left(&met), "told {told}: B's own limit");
        if !told {
            assert_eq!(
                st.account_answer(&met),
                Some(AccountAnswer::Bad(AccountBad::Wall)),
                "B is struck"
            );
        }
        let a = at(&mut st, &met, t);
        assert!(
            st.wall_standing(),
            "told {told}: B's limit is tracked: {a:?}"
        );
        assert!(
            !matches!(a, TurnEndAction::Type { .. }),
            "told {told}: nothing typed into a limited account: {a:?}"
        );
    }
}

#[test]
fn a_limit_drawn_again_with_no_move_still_walls_the_session() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.agent_started_unix(OLD_START);
    st.notice_written_at(Some(NOTICE));
    let wall = limit_on(t, "carry on with stage 4", Some(3 * MIN));
    let first = at(&mut st, &wall, t);
    assert!(waits_until(&first, t + 60 * MIN + GRACE), "{first:?}");
    st.restarted(&Restart::Memory, true, &wall, t);
    st.account_made_at(MADE);
    st.agent_started_unix(NEW_START);
    let replayed = limit_on(t, "carry on with stage 4", None);
    st.notice_written_at(Some(NOTICE));
    assert!(
        !st.limit_left(&replayed),
        "no move: the same account's limit"
    );
    let a = at(&mut st, &replayed, t + MIN);
    assert!(st.wall_standing());
    assert!(waits_until(&a, t + 60 * MIN + GRACE), "{a:?}");
}

#[test]
fn a_move_off_a_lost_login_continues_in_the_moves_words() {
    let t = t0();
    let mut st = TurnEndState::default();
    let lost = walled(
        WallKind::Auth,
        "Not logged in · Please run /login",
        Some(Duration::from_secs(1)),
    );
    st.observe(&lost, t);
    st.restarted(&to_account(TO, AccountAt::Wall), true, &lost, t);
    let repainted = TurnEndReading {
        worked: None,
        ..lost.clone()
    };
    assert!(st.login_left(&repainted));
    let a = at(&mut st, &repainted, t + MIN);
    assert_eq!(a, moved_on(), "never that the expired login cleared");
}

/// A LOGIN MOVED OFF A STANDING LIMIT (review findings 5 and 6 of 2026-11-16): the session waits
/// at a usage limit, and its login is rebound to another account (the owner's `aterm harness
/// accounts use`, another tab's renewal). The resume that owes types the login's own words,
/// never that the limit cleared: it did not, the login moved off it. The control: the same
/// limit past its reset, with no login moved, types the cleared line.
#[test]
fn a_login_moved_off_a_limit_resumes_in_the_logins_words() {
    let t = t0();
    let wall = limit_on(t, "carry on with stage 4", Some(3 * MIN));
    let mut st = TurnEndState::default();
    let first = at(&mut st, &wall, t);
    assert!(waits_until(&first, t + 60 * MIN + GRACE), "{first:?}");
    let moved = TurnEndReading {
        login_moved: true,
        worked: None,
        ..wall.clone()
    };
    let a = at(&mut st, &moved, t + MIN);
    assert_eq!(a, login_moved_on());
    let TurnEndAction::Type { text, .. } = &a else {
        unreachable!()
    };
    assert_eq!(
        text,
        "[aterm harness] This session's login moved to another account; continue where you \
         stopped."
    );
    assert!(!text.contains("cleared"), "{text}");
    assert!(is_cleared_line(text), "it delivers the cut prompt again");
    st.acted(&a, &moved, t + MIN);
    assert!(!st.login_resume_owed(), "typed once");

    let mut st = TurnEndState::default();
    at(&mut st, &wall, t);
    let past = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    let reset = t + 60 * MIN + GRACE + MIN;
    assert_eq!(
        at(&mut st, &past, reset),
        typed(RULE_LIMIT_RESUME),
        "the control"
    );
}

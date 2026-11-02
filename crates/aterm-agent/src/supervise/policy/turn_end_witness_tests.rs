// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// AN OUTAGE CLEARS ON A WITNESS, NOT A BLIND TRY (v6 §14.17.5.4, slice 12c): another session's
// turn on the same route, finished after the wall's last try, resumes the wait at once and once;
// a rate limit is the account's, so no witness clears it, and its own words' retry-after times its
// first try; nothing is typed into a route measured down.

const SEC: Duration = Duration::from_secs(1);
const OVERLOADED: &str = "API Error: 529 Overloaded.";

/// Muse Code's 429 as its screen draws it, the Responses API's sentence after its head: a
/// program whose rate limit's words name the wait. Claude Code's own 429 names none (the API
/// gives its wait in a header Claude Code does not draw).
const MUSE_429: &str = "model failed: API error 429: Rate limit reached for requests per min.";

/// Claude Code's 429 as the real API's body words it: no number, no wait named.
const CLAUDE_429: &str = "API Error: Request rejected (429) · This request would exceed your \
                          organization's rate limit of 50 requests per minute.";

fn rate_limit(words: &str) -> TurnEndReading {
    TurnEndReading {
        program: aterm_phase::Program::Muse,
        ..walled(
            WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            },
            words,
            Some(3 * MIN),
        )
    }
}

fn witnessed(r: &TurnEndReading, at: Instant) -> TurnEndReading {
    TurnEndReading {
        witness: Some(at),
        ..again(r)
    }
}

/// The wait an act is, as `(until, tries)` at outage `outage`; `None` for anything else.
fn waits_for(a: &TurnEndAction, outage: Outage) -> Option<(Instant, bool)> {
    match a {
        TurnEndAction::WaitUntil {
            until,
            wait: WaitKind::Outage { wall, tries },
            ..
        } if *wall == outage => Some((*until, *tries)),
        _ => None,
    }
}

/// A witness after the wall resumes an overload at once, *has cleared*, where the ladder would
/// wait two minutes; the same wall met again is left to the ladder however many witnesses come,
/// until the agent works again; a witness from before the wall, or none, clears nothing.
#[test]
fn a_witness_on_its_route_resumes_an_overload_at_once_and_once_per_outage() {
    let t = t0();
    let wall = walled(WallKind::Overloaded, OVERLOADED, Some(3 * MIN));
    let mut st = TurnEndState::default();
    let first = act(&mut st, &wall, t);
    assert_eq!(
        waits_for(&first, Outage::Overload),
        Some((t + 2 * MIN, true))
    );
    // CONTROL: no witness, and one older than the wall, leave the ladder's wait standing.
    let before = witnessed(&wall, t - Duration::from_secs(1));
    let mut control = st.clone();
    assert_eq!(
        waits_for(&act(&mut control, &before, t + 30 * SEC), Outage::Overload),
        Some((t + 2 * MIN, true))
    );
    assert_eq!(
        waits_for(
            &act(&mut control, &again(&wall), t + 30 * SEC),
            Outage::Overload
        ),
        Some((t + 2 * MIN, true))
    );
    let seen = witnessed(&wall, t + 20 * SEC);
    assert_eq!(st.witness_clearing(&seen), Some(t + 20 * SEC));
    assert_eq!(
        act(&mut st, &seen, t + 30 * SEC),
        resumed(Outage::Overload, Clearing::Measured)
    );
    // The resume met the wall again: later witnesses clear nothing; the ladder's next rung tries.
    let met = TurnEndReading {
        worked: Some(Duration::from_secs(4)),
        ..wall.clone()
    };
    let back = t + 40 * SEC;
    let a = act(&mut st, &met, back);
    assert_eq!(
        waits_for(&a, Outage::Overload),
        Some((back + 4 * MIN, true))
    );
    let mut now = back + MIN;
    let late = witnessed(&wall, now);
    assert_eq!(st.witness_clearing(&late), None);
    let mut lines = Vec::new();
    while now <= back + 4 * MIN {
        let a = act(&mut st, &late, now);
        if a.rule_id().is_some() {
            lines.push((now, a));
        }
        now += MIN;
    }
    assert_eq!(
        lines,
        [(back + 4 * MIN, resumed(Outage::Overload, Clearing::Blind))],
        "one blind try on its rung, no second witnessed resume"
    );
    // The agent works again: a new wall takes a witness again.
    let worked = idle("Stage done.", Some(10 * MIN));
    act(&mut st, &worked, now + 10 * MIN);
    let anew = now + 20 * MIN;
    act(&mut st, &wall, anew);
    assert_eq!(
        act(&mut st, &witnessed(&wall, anew + SEC), anew + 2 * SEC),
        resumed(Outage::Overload, Clearing::Measured)
    );
}

/// A rate limit is the account's: a witness on its route clears nothing. Its own words' retry-after
/// times its first try (bounded by the cap), and no later try comes sooner; words naming no number
/// keep the ladder's two minutes.
#[test]
fn a_rate_limit_waits_its_own_retry_after_and_no_witness_clears_it() {
    let t = t0();
    let five = rate_limit(&format!(
        "{MUSE_429} Please try again in 300s. (rate_limit_exceeded)"
    ));
    let mut st = TurnEndState::default();
    let a = act(&mut st, &five, t);
    assert_eq!(
        waits_for(&a, Outage::ApiError),
        Some((t + 5 * MIN, true)),
        "{a:?}"
    );
    let seen = witnessed(&five, t + MIN);
    assert_eq!(st.witness_clearing(&seen), None);
    assert_eq!(
        waits_for(&act(&mut st, &seen, t + 2 * MIN), Outage::ApiError),
        Some((t + 5 * MIN, true)),
        "a witness clears no rate limit"
    );
    assert_eq!(
        act(&mut st, &seen, t + 5 * MIN),
        resumed(Outage::ApiError, Clearing::Blind)
    );
    let met = TurnEndReading {
        worked: Some(Duration::from_secs(4)),
        ..five.clone()
    };
    let back = t + 5 * MIN + 10 * SEC;
    assert_eq!(
        waits_for(&act(&mut st, &met, back), Outage::ApiError),
        Some((back + 5 * MIN, true)),
        "the ladder's second rung (4 min) is shorter than the retry-after"
    );
    let cases = [
        (format!("{MUSE_429} Please try again in 2h."), 60 * MIN),
        (format!("{MUSE_429} Please try again in 30s."), 30 * SEC),
        (MUSE_429.to_string(), 2 * MIN),
        (CLAUDE_429.to_string(), 2 * MIN),
    ];
    for (words, gap) in cases {
        let r = TurnEndReading {
            program: if words == CLAUDE_429 {
                aterm_phase::Program::Claude
            } else {
                aterm_phase::Program::Muse
            },
            ..rate_limit(&words)
        };
        let mut st = TurnEndState::default();
        assert_eq!(
            waits_for(&act(&mut st, &r, t), Outage::ApiError),
            Some((t + gap, true)),
            "{words}"
        );
    }
    // CONTROL: an overload naming the same wait is not a rate limit, so it keeps the ladder.
    let overload = walled(
        WallKind::Overloaded,
        "API Error: 529 Overloaded. Please try again in 300s.",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    assert_eq!(
        waits_for(&act(&mut st, &overload, t), Outage::Overload),
        Some((t + 2 * MIN, true))
    );
}

/// No witness types into a route the host measures down; its edge up still resumes it, once.
#[test]
fn no_witness_types_into_a_route_measured_down() {
    let t = t0();
    let down = api_wall(
        ApiCause::Unreachable,
        ENOTFOUND,
        Some(3 * MIN),
        Reach::Down { since: t },
    );
    let mut st = TurnEndState::default();
    act(&mut st, &down, t);
    let seen = witnessed(&down, t + MIN);
    assert_eq!(st.witness_clearing(&seen), None);
    assert!(matches!(
        waits_for(&act(&mut st, &seen, t + 2 * MIN), Outage::Connection),
        Some((_, false))
    ));
    // CONTROL: the same witness with the route read unknown resumes it.
    let unknown = TurnEndReading {
        reach: Reach::Unknown,
        ..seen.clone()
    };
    assert_eq!(
        act(&mut st, &unknown, t + 3 * MIN),
        resumed(Outage::Connection, Clearing::Measured)
    );
}

#[test]
fn the_gap_to_each_try_is_the_ladders_or_the_retry_after_bounded_by_the_cap() {
    let t = TurnEndTiming::default();
    let gaps = |after: Option<Duration>| -> Vec<u64> {
        (0..8).map(|n| outage_gap(&t, n, after).as_secs()).collect()
    };
    assert_eq!(gaps(None), [120, 240, 480, 960, 1920, 3600, 3600, 3600]);
    assert_eq!(
        gaps(Some(5 * MIN)),
        [300, 300, 480, 960, 1920, 3600, 3600, 3600]
    );
    assert_eq!(
        gaps(Some(5 * SEC)),
        [5, 240, 480, 960, 1920, 3600, 3600, 3600]
    );
    assert_eq!(
        gaps(Some(3 * 60 * MIN)),
        [3600, 3600, 3600, 3600, 3600, 3600, 3600, 3600]
    );
}

/// NO WITNESS TYPES INTO A WALL WHOSE ROUTE IS MEASURED DOWN, whatever outage its words name (v6
/// §14.17.5.4): a code-less server error read while the route is down (the connection's outage,
/// which its own words do not name, so the Down's early wait does not hold it) and an overload
/// under a Down each wait on a witness after the wall, typing nothing before the ladder's first
/// rung (2 min); read unknown, the same witness resumes each, measured. NEGATIVE CONTROL (run): with `witness_clears` refusing no Down
/// (the review's mutant M3), each types its measured line into the Down and the test fails.
#[test]
fn no_witness_types_into_a_server_error_or_an_overload_measured_down() {
    let t = t0();
    let down = Reach::Down { since: t };
    for (what, wall, outage) in [
        (
            "a code-less server error",
            api_wall(
                ApiCause::Server,
                "API Error: Internal server error",
                Some(3 * MIN),
                down,
            ),
            Outage::Connection,
        ),
        (
            "an overload",
            TurnEndReading {
                reach: down,
                ..walled(WallKind::Overloaded, OVERLOADED, Some(3 * MIN))
            },
            Outage::Overload,
        ),
    ] {
        let mut st = TurnEndState::default();
        let first = act(&mut st, &wall, t);
        assert!(
            first.rule_id().is_none(),
            "{what}: the wall waits: {first:?}"
        );
        let seen = witnessed(&wall, t + 20 * SEC);
        for now in [t + 30 * SEC, t + 90 * SEC] {
            let a = act(&mut st, &seen, now);
            assert!(
                a.rule_id().is_none(),
                "{what}: nothing typed into a Down on a witness: {a:?}"
            );
        }
        let unknown = TurnEndReading {
            reach: Reach::Unknown,
            ..seen.clone()
        };
        assert_eq!(
            act(&mut st, &unknown, t + 100 * SEC),
            resumed(outage, Clearing::Measured),
            "{what}: the control, the route read unknown"
        );
    }
}

/// A BLIND TRY ON A WITNESS THAT LANDED AS IT WAS TYPED LATCHES NOTHING (v6 §14.17.5.4): the
/// decision tried the overload blind, no witness read; the point's read after the act sees one,
/// but the line typed was the blind one, so no witness is said for it (`witness_resumed`) and the
/// track is not latched as resumed on a witness: the next witness after the wall met again still
/// resumes it, measured, and is said. NEGATIVE CONTROL (run): with `acted` latching on the read
/// after the act whatever line was typed, that witness is ignored and the test fails.
#[test]
fn a_blind_try_on_a_witness_landing_as_it_is_typed_latches_nothing() {
    let t = t0();
    let wall = walled(WallKind::Overloaded, OVERLOADED, Some(3 * MIN));
    let mut st = TurnEndState::default();
    act(&mut st, &wall, t);
    let rung = t + 2 * MIN;
    let blind = at(&mut st, &again(&wall), rung);
    assert_eq!(blind, resumed(Outage::Overload, Clearing::Blind));
    let landed = witnessed(&wall, rung + SEC);
    assert_eq!(
        st.witness_resumed(&blind, &landed),
        None,
        "the blind line was typed on no witness"
    );
    st.acted(&blind, &landed, rung);
    let met = TurnEndReading {
        worked: Some(Duration::from_secs(4)),
        ..wall.clone()
    };
    let back = rung + 10 * SEC;
    act(&mut st, &met, back);
    let seen = witnessed(&wall, back + 20 * SEC);
    let measured = at(&mut st, &seen, back + 30 * SEC);
    assert_eq!(measured, resumed(Outage::Overload, Clearing::Measured));
    assert_eq!(
        st.witness_resumed(&measured, &seen),
        Some(back + 20 * SEC),
        "the measured line on a witness is said"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall classification tests across background jobs and `/goal` echoes.

use crate::prompt::fixtures::screen;
use crate::{Phase, read};

const RULE: &str = "────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────";

const WALL_SHELL: &str =
    include_str!("fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt");
const WALL_SPINNER: &str =
    include_str!("fixtures/claude-2.1.283-wall-weekly-goal-paused-spinner.txt");
const FABLE_SHELL: &str =
    include_str!("fixtures/claude-2.1.283-wall-fable-goal-rate-limited-shell.txt");
const WALL_529_SHELL: &str = include_str!("fixtures/claude-2.1.283-wall-529-shell.txt");
const WALL_AUTO_SHELL: &str =
    include_str!("fixtures/claude-2.1.283-wall-weekly-goal-auto-shell.txt");
const WRAPUP_WAITING: &str =
    include_str!("fixtures/claude-2.1.283-wall-session-wrapup-waiting-measured.txt");
const CHECKIN_SHELL: &str =
    include_str!("fixtures/claude-2.1.283-wall-session-goal-checkin-shell-measured.txt");

const TOASTS_284: &str =
    include_str!("fixtures/claude-2.1.284-wall-session-limit-toasts-hand-built.txt");
const STALE: &str = include_str!("fixtures/claude-2.1.283-wall-stale-press-enter.txt");

const WEEKLY: &[&str] = &[
    "⏺ Dynamic workflow \"a neutral title\" completed · 4h 42m 19s",
    "  ⎿  You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
    "     Use your limit reset to reset it now: clau.de/reset",
    "     /usage-credits to finish what you’re working on.",
    "",
];
const ECHO_PAUSED: &str =
    "⏺ Goal paused · usage limit reached · send a message after it resets to continue";
const ECHO_RATE: &str = "⏺ Goal paused · the request was rate limited · send a message to retry";
const DONE_SHELL: &str = "✻ Sautéed for 1d 2h 58m · done 12:19 PM · 1 shell still running";
const DONE: &str = "✻ Sautéed for 1d 2h 58m · done 12:19 PM";
const FOOT_SHELL: &str = "  ⏵⏵ bypass permissions on · 1 shell · ← for agents · ↓ to manage";
const FOOT_LIVE: &str = "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell";
const FOOT_PLAIN: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";

fn frame(body: &[&str], status: &str, hint: &str, footer: &[&str]) -> Vec<String> {
    let mut r: Vec<String> = body.iter().map(|s| (*s).to_string()).collect();
    r.push(String::new());
    if !status.is_empty() {
        r.push(status.to_string());
    }
    if !hint.is_empty() {
        r.push(hint.to_string());
    }
    r.push(RULE.to_string());
    r.push("❯ ".to_string());
    r.push(RULE.to_string());
    r.extend(footer.iter().map(|s| (*s).to_string()));
    r
}

struct Look {
    phase: Phase,
    kind: Option<&'static str>,
    reset: Option<String>,
    message: String,
}

fn look(rows: &[String]) -> Look {
    let r = read(Some("claude"), rows, Some(2));
    Look {
        phase: r.phase,
        kind: r.wall.as_ref().map(|w| w.kind.name()),
        reset: r.wall.as_ref().and_then(|w| w.reset.clone()),
        message: r.wall.map(|w| w.message).unwrap_or_default(),
    }
}

fn limited(l: &Look) -> bool {
    matches!(l.phase, Phase::Limited { .. })
}

#[test]
fn a_wall_the_turn_ended_on_outranks_the_agents_own_background_work() {
    let mut weekly_paused: Vec<&str> = WEEKLY.to_vec();
    weekly_paused.extend([ECHO_PAUSED, ""]);
    let mut same = vec![
        ("measured", screen(WALL_SHELL)),
        (
            "goal echo, shell on the done row and in the footer",
            frame(&weekly_paused, DONE_SHELL, "", &[FOOT_SHELL]),
        ),
        ("no echo", frame(WEEKLY, DONE_SHELL, "", &[FOOT_SHELL])),
        (
            "no shell at all",
            frame(&weekly_paused, DONE, "", &[FOOT_PLAIN]),
        ),
        ("the goal's auto-continue echo", screen(WALL_AUTO_SHELL)),
        (
            "a monitor only (soft)",
            frame(
                WEEKLY,
                "✻ Churned for 0s · done 8:52 PM · 1 monitor still running",
                "",
                &["  ⏵⏵ bypass permissions on · 1 monitor"],
            ),
        ),
        (
            "a workflow line under the footer",
            frame(
                &weekly_paused,
                DONE_SHELL,
                "",
                &[
                    FOOT_SHELL,
                    "",
                    "  ◯ trust-cg-darwin-wave-11  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱  16/21 agents done · 4h42m · ↓ 7.4m tokens",
                ],
            ),
        ),
    ];
    for (name, rows) in same.drain(..) {
        let l = look(&rows);
        assert!(limited(&l), "{name}: {:?}", l.phase);
        assert_eq!(l.kind, Some("usage-weekly"), "{name}");
        assert_eq!(
            l.reset.as_deref(),
            Some("4pm (America/Los_Angeles)"),
            "{name}"
        );
    }
    let l = look(&screen(WALL_529_SHELL));
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("overloaded")));
    let l = look(&frame(
        &["⏺ Login expired · Please run /login", ""],
        DONE_SHELL,
        "",
        &[FOOT_SHELL],
    ));
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("auth")));
    let l = look(&screen(FABLE_SHELL));
    assert!(limited(&l));
    assert_eq!(l.kind, Some("model-bucket"));
    assert_eq!(l.reset, None);
    for (name, rows) in [
        ("a spinner under the wall", screen(WALL_SPINNER)),
        (
            "the spinner on the status row",
            frame(
                &weekly_paused,
                "✻ Cooking… (12s · esc to interrupt)",
                "",
                &[FOOT_SHELL],
            ),
        ),
        (
            "esc to interrupt in the footer",
            frame(&weekly_paused, DONE_SHELL, "", &[FOOT_LIVE]),
        ),
        (
            "the worker's prose after it",
            frame(&["⏺ All done.", ""], DONE_SHELL, "", &[FOOT_SHELL]),
        ),
    ] {
        let l = look(&rows);
        assert_eq!((l.phase, l.kind), (Phase::Busy, None), "{name}");
    }
    let banner = format!(
        "{}Critical memory usage (140.4GB) — restart and resume with claude --continue",
        " ".repeat(74)
    );
    let l = look(&frame(&["⏺ Working."], DONE_SHELL, &banner, &[FOOT_SHELL]));
    assert_eq!((l.phase, l.kind), (Phase::Busy, Some("memory")));
}

#[test]
fn a_non_limit_wall_behind_a_monitor_or_a_joined_list_reads_as_behind_a_shell() {
    let walls: [(&str, &[&str], &str); 3] = [
        (
            "an API error",
            &[
                "⏺ API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.",
                "",
            ],
            "overloaded",
        ),
        (
            "a login",
            &["⏺ Login expired · Please run /login", ""],
            "auth",
        ),
        (
            "a full context",
            &[
                "⏺ Context limit reached · /compact or /clear to continue",
                "",
            ],
            "context",
        ),
    ];
    let footers: [(&str, &str, &str); 5] = [
        (
            "a monitor only",
            "✻ Churned for 3m · done 8:52 PM · 1 monitor still running",
            "  ⏵⏵ bypass permissions on · 1 monitor · ← for agents · ↓ to manage",
        ),
        (
            "a shell and a monitor, joined",
            "✻ Churned for 3m · done 8:52 PM · 1 shell, 1 monitor still running",
            "  ⏵⏵ bypass permissions on · 1 shell, 1 monitor · ← for agents · ↓ to manage",
        ),
        (
            "two shells and a monitor, joined",
            "✻ Churned for 3m · done 8:52 PM · 2 shells, 1 monitor still running",
            "  ⏵⏵ bypass permissions on · 2 shells, 1 monitor · ← for agents · ↓ to manage",
        ),
        (
            "a monitor and a shell, joined",
            "✻ Churned for 3m · done 8:52 PM · 1 monitor, 1 shell still running",
            "  ⏵⏵ bypass permissions on · 1 monitor, 1 shell",
        ),
        ("a plain shell (the known case)", DONE_SHELL, FOOT_SHELL),
    ];
    for (wall_name, body, kind) in walls {
        for (name, done, footer) in footers {
            let l = look(&frame(body, done, "", &[footer]));
            assert_eq!(
                (l.phase.clone(), l.kind),
                (Phase::Idle, Some(kind)),
                "{wall_name} behind {name}"
            );
        }
    }
    for (name, done, footer) in footers {
        let l = look(&frame(WEEKLY, done, "", &[footer]));
        assert!(limited(&l), "a weekly limit behind {name}: {:?}", l.phase);
        assert_eq!(l.kind, Some("usage-weekly"), "{name}");
    }
    for (name, done, footer) in footers {
        let live = look(&frame(
            &walls[0].1[..1],
            "✻ Cooking… (12s · esc to interrupt)",
            "",
            &[footer],
        ));
        assert_eq!(live.phase, Phase::Busy, "a spinner over {name}");
        let prose = look(&frame(&["⏺ All done.", ""], done, "", &[footer]));
        assert_eq!((prose.phase, prose.kind), (Phase::Busy, None), "{name}");
    }
}

#[test]
fn a_goal_echo_is_read_through_only_to_the_wall_its_reason_can_come_from() {
    let l = look(&screen(WALL_SHELL));
    assert_eq!(l.kind, Some("usage-weekly"));
    assert_eq!(l.reset.as_deref(), Some("4pm (America/Los_Angeles)"));
    assert!(
        l.message.contains("Goal paused"),
        "the echo stays in the message: {}",
        l.message
    );
    let l = look(&screen(FABLE_SHELL));
    assert_eq!(l.kind, Some("model-bucket"));
    let l = look(&frame(
        &[
            "⏺ You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
            "",
            ECHO_PAUSED,
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(
        (l.kind, l.reset.as_deref()),
        (Some("usage-weekly"), Some("4pm (America/Los_Angeles)"))
    );
    let l = look(&frame(
        &[
            "⏺ API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment.",
            "",
            "⏺ Goal paused · the API rejected the last request · send a message to retry",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, Some("overloaded"));
    let l = look(&frame(
        &[
            "⏺ API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment.",
            "",
            "⏺ Goal still active · the API was unavailable or the connection dropped · retrying in 5 min (1/3) · send a message to retry now",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, Some("overloaded"));
    let cleared = |label: &str| {
        format!(
            "⏺ Goal cleared after an unrecoverable error ({label}): \"finish the work\". Run /goal again to continue."
        )
    };
    let l = look(&frame(
        &[
            "⏺ Login expired · Please run /login",
            "",
            &cleared("authentication failed"),
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, Some("auth"));
    assert!(l.message.contains("Goal cleared"), "{}", l.message);
    let l = look(&frame(
        &["⏺ Some prose the worker said.", "", ECHO_RATE, ""],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, Some("api-error"));
    for prose in [
        "⏺ Token expired · I rotated the key and re-ran the suite",
        "⏺ Authentication failed · the test server rejected the token",
        "⏺ Context limit reached · /compact or /clear to continue",
    ] {
        let l = look(&frame(&[prose, "", ECHO_RATE, ""], DONE, "", &[FOOT_PLAIN]));
        assert_eq!(l.kind, Some("api-error"), "{prose}");
    }
    let l = look(&frame(
        &[
            "⏺ Login expired · Please run /login",
            "",
            &cleared("context limit reached"),
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, None);
    let l = look(&frame(
        &[
            "⏺ You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
            "",
            "⏺ Goal paused · a hook ended the turn",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(l.kind, None);
}

/// The owner's tab of 2026-10-26 (rows MEASURED): a `/goal` turn's request failed ENOTFOUND
/// twice, Claude Code 2.1.310 drawing its error row over each `Goal still active · the API was
/// unavailable or the connection dropped · retrying in N min (k/3)` echo. The last echo is read
/// through to the error row over it: a network wall, whose row is that later error row.
#[test]
fn the_owners_goal_outage_reads_through_its_echo_to_the_unreachable_error_row() {
    use crate::wall::{ApiCause, Placement, WallKind, wall};
    let rows = screen(crate::prompt::fixtures::WALL_GOAL_API_UNREACHABLE_2_1_310);
    let w = wall(&rows).expect("the wall");
    assert_eq!(
        w.kind,
        WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        }
    );
    assert_eq!(w.placement, Placement::ErrorRow);
    assert!(
        w.message.contains("(ENOTFOUND) — Goal still active"),
        "{}",
        w.message
    );
    assert!(
        w.message.ends_with("(2/3) · send a message to retry now"),
        "the last echo: {}",
        w.message
    );
    let error = rows
        .iter()
        .rposition(|r| r.starts_with("⏺ API Error"))
        .expect("the later error row");
    assert_eq!(w.row, error);
    let l = look(&rows);
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("api-error")));

    let prose: Vec<String> = rows
        .iter()
        .map(|r| {
            if r.starts_with("⏺ API Error") {
                "⏺ Some prose the worker said.".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let alone = wall(&prose).expect("the echo read alone");
    assert_eq!(
        alone.kind,
        WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Server,
        },
        "the echo read alone is a generic API error: only its error row names the route"
    );
    assert!(
        alone.message.starts_with("Goal still active"),
        "the echo's own words, no prose: {}",
        alone.message
    );
    let echo = rows
        .iter()
        .rposition(|r| r.starts_with("⏺ Goal still active"))
        .expect("the later echo");
    assert_eq!(alone.row, echo, "its row is the echo's");
    let l = look(&prose);
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("api-error")));
}

/// Claude Code's `/goal` echo that the API was unavailable, read with no error row over it to read
/// it through to (HAND-BUILT: the owner's measured rows with their error rows taken out; the
/// measurement is still owed): a wall in its own right, retrying (`Goal still active · … (k/3)`)
/// or past its retries (`Goal paused after 3 automatic retries · …`), and a generic API error,
/// since Claude Code gives that reason an overload and a server error too; only an error row it
/// reads through to names an unreachable route. The lab's measured 2.1.311 screen, a fresh
/// `/goal` whose first request failed, draws its error row over the echo under the person's
/// `❯ /goal` prompt: the echo is read through to that row (unreachable), the prompt above being
/// no composer to read above. Its guard is on the echo's opening, so a message read through to an
/// error row keeps that row's kind when it is read again by its words (aterm-agent's
/// `words_kind`): a 529 under the same echo stays an overload, a fresh `/goal`'s included. A
/// different reason under the same opening is no wall.
#[test]
fn a_goal_echo_that_the_api_was_unavailable_read_alone_is_a_generic_api_error() {
    use crate::wall::{ApiCause, Placement, WallKind, classify_wall, wall};
    let unreachable = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::Unreachable,
    };
    let generic = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::Server,
    };
    let rows = screen(crate::prompt::fixtures::WALL_GOAL_API_UNAVAILABLE_ALONE_2_1_310);
    assert!(
        !rows.iter().any(|r| r.contains("API Error")),
        "no error row"
    );
    let w = wall(&rows).expect("the echo is the wall");
    assert_eq!(w.kind, generic);
    let echo = rows
        .iter()
        .rposition(|r| r.starts_with("⏺ Goal still active"))
        .expect("the later echo");
    assert_eq!(w.row, echo);
    assert!(
        w.message
            .starts_with("Goal still active · the API was unavailable or the connection dropped")
            && w.message.ends_with("(2/3) · send a message to retry now"),
        "{}",
        w.message
    );
    let l = look(&rows);
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("api-error")));

    let paused: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i == echo {
                "⏺ Goal paused after 3 automatic retries · the API was unavailable or the connection dropped · send a message to continue".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let w = wall(&paused).expect("the paused echo is the wall");
    assert_eq!(w.kind, generic);
    assert!(
        w.message
            .starts_with("Goal paused after 3 automatic retries"),
        "{}",
        w.message
    );
    let l = look(&paused);
    assert_eq!((l.phase, l.kind), (Phase::Idle, Some("api-error")));

    let measured = screen(crate::prompt::fixtures::WALL_GOAL_API_UNAVAILABLE_ECONNREFUSED_2_1_311);
    let w = wall(&measured).expect("the lab's measured echo under its error row");
    assert_eq!(w.kind, unreachable);
    assert_eq!(w.placement, Placement::ErrorRow);
    assert!(
        w.message.contains("(ECONNREFUSED) — Goal still active"),
        "read through: {}",
        w.message
    );
    let error = measured
        .iter()
        .position(|r| r.starts_with("⏺ API Error"))
        .expect("the error row");
    assert_eq!(w.row, error);
    let mut alone = measured.clone();
    alone.drain(error..=error + 1);
    let w = wall(&alone).expect("the measured echo, its error row taken out");
    assert_eq!(
        w.kind, generic,
        "its error row named the route; the echo alone does not"
    );
    assert!(w.message.starts_with("Goal still active"), "{}", w.message);
    assert_eq!(w.row, error, "the echo's row, where the error row stood");

    for echo in [
        "⏺ Goal still active · the API was unavailable or the connection dropped · retrying in 1 min (1/3) · send a message to retry now",
        "⏺ Goal paused after 3 automatic retries · the API was unavailable or the connection dropped · send a message to continue",
    ] {
        let l = look(&frame(
            &["⏺ Some prose the worker said.", "", echo, ""],
            DONE,
            "",
            &[FOOT_PLAIN],
        ));
        assert_eq!(
            (l.phase, l.kind),
            (Phase::Idle, Some("api-error")),
            "{echo}"
        );
        assert_eq!(classify_wall(&l.message), Some(generic), "{echo}");
    }

    let overloaded = look(&frame(
        &[
            "⏺ API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment.",
            "",
            "⏺ Goal still active · the API was unavailable or the connection dropped · retrying in 1 min (1/3) · send a message to retry now",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(overloaded.kind, Some("overloaded"));
    assert!(
        overloaded
            .message
            .contains(" — Goal still active · the API was unavailable"),
        "read through: {}",
        overloaded.message
    );
    assert_eq!(
        classify_wall(&overloaded.message),
        Some(WallKind::Overloaded),
        "the read-through message read again by its words is the 529's, not the echo's"
    );
    let first_try = look(&frame(
        &[
            "❯ /goal finish the work",
            "  ⎿  Goal set: finish the work",
            "",
            "⏺ API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment.",
            "",
            "⏺ Goal still active · the API was unavailable or the connection dropped · retrying in 1 min (1/3) · send a message to retry now",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(
        first_try.kind,
        Some("overloaded"),
        "a person's prompt above the error row is no composer to read above: {}",
        first_try.message
    );

    let other = look(&frame(
        &[
            "⏺ Some prose the worker said.",
            "",
            "⏺ Goal still active · the API returned an unexpected response · retrying in 1 min (1/3) · send a message to retry now",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!((other.phase, other.kind), (Phase::Idle, None));
}

#[test]
fn a_reset_is_a_time_or_nothing() {
    let alone = |echo: &str| {
        look(&frame(
            &["⏺ Some prose the worker said.", "", echo, ""],
            DONE,
            "",
            &[FOOT_PLAIN],
        ))
    };
    assert_eq!(alone(ECHO_PAUSED).reset, None);
    let dated = look(&frame(
        &[
            "⏺ You've hit your weekly limit · resets Sep 21 at 4pm (America/Los_Angeles)",
            "",
        ],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(
        dated.reset.as_deref(),
        Some("Sep 21 at 4pm (America/Los_Angeles)")
    );
    let span = look(&frame(
        &["⏺ You've hit your weekly limit · resets in 5s", ""],
        DONE,
        "",
        &[FOOT_PLAIN],
    ));
    assert_eq!(span.reset.as_deref(), Some("in 5s"));
}

#[test]
fn an_armed_countdown_and_the_moment_it_fires_stay_with_the_wall() {
    let under = |toast: &str| {
        let pad = " ".repeat(90);
        look(&frame(
            &[
                "⏺ x",
                "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
                "",
            ],
            DONE,
            &format!("{pad}{toast}"),
            &[FOOT_PLAIN],
        ))
    };
    for toast in [
        "Continuing automatically at 3pm · esc to cancel",
        "⚠ Usage limit reset · continuing automatically",
        "Usage limit available again · continuing now",
        "⚠ Usage limit reached · continuing automatically at 3pm · esc or type to cancel",
    ] {
        let l = under(toast);
        assert_eq!(l.kind, Some("usage-session"), "{toast}");
        assert!(
            l.reset.as_deref().is_some_and(|r| r.starts_with("3pm")),
            "{toast}: {:?}",
            l.reset
        );
        let said = l.message.to_lowercase();
        assert!(
            said.contains("continuing automatically") || said.contains("continuing now"),
            "{toast}: {}",
            l.message
        );
    }
    let l = under("⚠ Your usage limit has reset · press enter to continue");
    assert_eq!(l.kind, Some("usage-session"));
    assert!(
        !l.message.to_lowercase().contains("continuing"),
        "{}",
        l.message
    );
    let l = look(&screen(TOASTS_284));
    assert!(
        l.message
            .to_lowercase()
            .contains("continuing automatically"),
        "{}",
        l.message
    );
    let l = look(&screen(STALE));
    assert!(
        !l.message.to_lowercase().contains("continuing"),
        "{}",
        l.message
    );
    let toast = "Continuing automatically at 3pm \u{b7} esc to cancel";
    let wall = "Usage limit reached \u{b7} limit resets 3pm";
    let gutter = [
        "\u{23fa} Working.",
        "  \u{23bf}\u{a0}You've hit your session limit \u{b7} resets 3pm (America/Los_Angeles)",
        "",
    ];
    for (name, rows) in [
        (
            "a hint above the composer",
            frame(
                &gutter,
                DONE,
                &format!("{}{toast}", " ".repeat(80)),
                &[FOOT_PLAIN],
            ),
        ),
        (
            "a notification under the composer",
            frame(&gutter, DONE, "", &[&format!("  {toast}"), FOOT_PLAIN]),
        ),
        (
            "beside a footer wall",
            frame(
                &["\u{23fa} Working.", ""],
                DONE,
                "",
                &[
                    &format!("  \u{26a0} {wall}"),
                    &format!("  {toast}"),
                    FOOT_PLAIN,
                ],
            ),
        ),
        (
            "beside a banner wall",
            frame(
                &["\u{23fa} Working.", ""],
                DONE,
                &format!("{}\u{26a0} {wall}", " ".repeat(60)),
                &[&format!("  {toast}"), FOOT_PLAIN],
            ),
        ),
    ] {
        let l = look(&rows);
        assert_eq!(l.kind, Some("usage-session"), "{name}");
        assert!(
            l.message.contains("Continuing automatically at 3pm"),
            "{name}: {}",
            l.message
        );
    }
    let plain = look(&frame(&gutter, DONE, "", &[FOOT_PLAIN]));
    assert!(
        !plain.message.to_lowercase().contains("continuing"),
        "{}",
        plain.message
    );
    let mut drafted = frame(&gutter, DONE, "", &[FOOT_PLAIN]);
    let caret = drafted
        .iter()
        .position(|r| r.starts_with("\u{276f}"))
        .unwrap();
    drafted[caret] = "\u{276f} continuing automatically at 3pm".to_string();
    assert!(
        !look(&drafted).message.to_lowercase().contains("continuing"),
        "{}",
        look(&drafted).message
    );
}

#[test]
fn a_wall_behind_a_displaced_screen_still_reads_and_a_live_turn_in_a_copy_still_wins() {
    let base = screen(WALL_SHELL);
    let mut up = base[1..].to_vec();
    up.push(String::new());
    assert_eq!(look(&up).kind, Some("usage-weekly"), "shifted up one row");
    let foot = base
        .iter()
        .position(|r| r.contains("bypass permissions"))
        .unwrap();
    let mut dup = base.clone();
    dup.insert(
        foot + 1,
        "  2      s, 1 monitor ← for agents · v to manage".to_string(),
    );
    assert_eq!(look(&dup).kind, Some("usage-weekly"), "footer duplicated");
    let unfused: Vec<String> = base
        .iter()
        .map(|r| r.replace("Restart to update◎", "Restart to update ◎"))
        .collect();
    assert_eq!(
        look(&unfused).kind,
        Some("usage-weekly"),
        "hint row unfused"
    );
    let mut live = base.clone();
    live.insert(
        foot + 1,
        "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string(),
    );
    let l = look(&live);
    assert_eq!(
        (l.phase, l.kind),
        (Phase::Busy, None),
        "a live signal in the copy"
    );
}

#[test]
fn a_wall_drawn_while_the_turn_wraps_up_is_not_yet_the_end_of_it() {
    let l = look(&screen(WRAPUP_WAITING));
    assert_eq!(
        (l.phase.clone(), l.kind),
        (Phase::Busy, None),
        "{:?}",
        l.phase
    );
    let l = look(&screen(CHECKIN_SHELL));
    assert!(limited(&l), "{:?}", l.phase);
    assert_eq!(l.kind, Some("usage-session"));
    assert_eq!(
        l.reset.as_deref(),
        Some("9:50pm"),
        "the banner is the wall the reader places first"
    );
    let rows = screen(CHECKIN_SHELL);
    let done = rows
        .iter()
        .position(|r| r.contains("done 9:22 PM"))
        .unwrap();
    let banner_only: Vec<String> = rows[..1].iter().chain(&rows[done..]).cloned().collect();
    let l = look(&banner_only);
    assert!(limited(&l), "{:?}", l.phase);
    assert_eq!(l.reset.as_deref(), Some("9:50pm"));
    let waiting: Vec<String> = banner_only
        .iter()
        .map(|r| {
            r.replace(
                "✻ Churned for 4h 18m 23s · done 9:22 PM · 1 shell still running",
                "✻ Waiting for 1 background agent and 1 dynamic workflow to finish",
            )
        })
        .collect();
    assert_eq!(
        look(&waiting).phase,
        Phase::Busy,
        "Waiting for … is not a done row"
    );
    let no_status: Vec<String> = banner_only
        .iter()
        .filter(|r| !r.starts_with("✻ "))
        .cloned()
        .collect();
    assert_eq!(
        look(&no_status).phase,
        Phase::Busy,
        "no status row: nothing says the turn ended"
    );
    let gutter_waiting: Vec<String> = rows
        .iter()
        .filter(|r| !r.trim_start().starts_with('⚠'))
        .map(|r| {
            r.replace(
                "✻ Churned for 4h 18m 23s · done 9:22 PM · 1 shell still running",
                "✻ Waiting for 1 background agent and 1 dynamic workflow to finish",
            )
        })
        .collect();
    let l = look(&gutter_waiting);
    assert!(limited(&l), "{:?}", l.phase);
    assert_eq!(l.kind, Some("usage-session"));
    let gutter_only: Vec<String> = rows
        .iter()
        .filter(|r| !r.trim_start().starts_with('⚠'))
        .cloned()
        .collect();
    let l = look(&gutter_only);
    assert!(
        limited(&l) && l.kind == Some("usage-session"),
        "{:?}",
        l.phase
    );
    assert_eq!(l.reset.as_deref(), Some("9:50pm (America/Los_Angeles)"));
    let no_shell: Vec<String> = banner_only
        .iter()
        .map(|r| {
            r.replace(" · 1 shell still running", "")
                .replace(" · 1 shell ·", " ·")
        })
        .collect();
    let l = look(&no_shell);
    assert!(limited(&l), "{:?}", l.phase);
}

#[test]
fn a_message_just_submitted_under_the_status_row_is_a_turn_beginning_not_a_wall() {
    let rows = screen(WALL_SHELL);
    assert!(limited(&look(&rows)), "the measured wall");
    let status = rows
        .iter()
        .position(|r| r.starts_with("✻ Sautéed"))
        .unwrap();
    let mut queued = rows.clone();
    queued.insert(status + 2, "❯ keep going".to_string());
    let l = look(&queued);
    assert_eq!((l.phase, l.kind), (Phase::Busy, None));
    assert_eq!(
        crate::phase::background_wait(&queued),
        None,
        "the two agree"
    );
    assert!(!crate::background_only(&queued));
}

#[test]
fn a_screen_busy_only_with_background_work_is_told_from_a_live_turn() {
    let measured = screen(WALL_SHELL);
    assert!(
        crate::background_only(&measured),
        "the measured wall behind a shell"
    );
    let from = measured
        .iter()
        .position(|r| r.starts_with("⏺ Dynamic workflow"))
        .unwrap();
    let to = measured
        .iter()
        .position(|r| r.starts_with("⏺ Goal paused"))
        .unwrap();
    let mut scrolled = measured.clone();
    scrolled.drain(from..=to);
    let l = look(&scrolled);
    assert_eq!(
        (l.phase, l.kind),
        (Phase::Busy, None),
        "the reader sees no wall there"
    );
    assert!(crate::background_only(&scrolled));
    let status = measured
        .iter()
        .position(|r| r.starts_with("✻ Sautéed"))
        .unwrap();
    for (name, extra) in [
        (
            "/model picked",
            vec!["❯ /model", "  ⎿  Kept model as Fable 5.1", ""],
        ),
        (
            "/usage shown",
            vec!["❯ /usage", "  ⎿  Status dialog dismissed", ""],
        ),
        (
            "/model then Esc",
            vec!["❯ /model", "  ⎿  Model dialog dismissed", ""],
        ),
    ] {
        let mut after = measured.clone();
        for (i, e) in extra.iter().enumerate() {
            after.insert(status + i, (*e).to_string());
        }
        let l = look(&after);
        assert_eq!(
            (l.phase, l.kind),
            (Phase::Busy, None),
            "{name}: the wall is no longer the last thing said"
        );
        assert!(crate::background_only(&after), "{name}");
    }
    let monitor = frame(
        &["⏺ Working."],
        "✻ Churned for 0s · done 8:52 PM · 1 monitor still running",
        "",
        &["  ⏵⏵ bypass permissions on · 1 monitor"],
    );
    assert!(crate::background_only(&monitor));
    let mut live = measured.clone();
    let foot = live
        .iter()
        .position(|r| r.contains("bypass permissions"))
        .unwrap();
    live.insert(
        foot + 1,
        "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string(),
    );
    assert!(!crate::background_only(&live), "esc to interrupt");
    assert!(!crate::background_only(&screen(WALL_SPINNER)), "a spinner");
    assert!(
        !crate::background_only(&frame(&["⏺ All done."], DONE, "", &[FOOT_PLAIN])),
        "idle, no shell"
    );
    assert!(
        !crate::background_only(&[
            "⏺ A build is running.".to_string(),
            "1 shell still running".to_string()
        ]),
        "no composer frame"
    );
}

#[test]
fn a_wall_behind_background_work_is_one_reading() {
    let rows = screen(WALL_SHELL);
    let caret = rows.iter().rposition(|r| r.starts_with('❯')).unwrap();
    let r = crate::read_at(Some("claude"), &rows, Some(caret), Some(2));
    assert!(matches!(r.phase, Phase::Limited { .. }), "{:?}", r.phase);
    assert!(r.phase_authoritative, "the composer holds the cursor");
    let wall = r.wall.expect("the wall is part of the reading");
    assert_eq!(wall.kind.name(), "usage-weekly");
    assert_eq!(wall.reset.as_deref(), Some("4pm (America/Los_Angeles)"));
    let away = crate::read_at(Some("claude"), &rows, Some(0), Some(2));
    assert!(away.phase_authoritative, "a wall needs no cursor");

    let live = screen(WALL_SPINNER);
    let caret = live.iter().rposition(|r| r.starts_with('❯')).unwrap();
    let r = crate::read_at(Some("claude"), &live, Some(caret), Some(2));
    assert_eq!(r.phase, Phase::Busy, "a live turn under the wall");
    assert!(r.wall.is_none(), "and no wall is read under a hard busy");
}

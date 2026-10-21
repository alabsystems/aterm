// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A wall the turn ended on is a wall whatever the agent left running in the background;
//! a `/goal` echo is read through only to the row its reason can come from; the vendor's
//! going-on notices stay with their wall. The measured screen is
//! `claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt`; the others vary it.

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

/// What the reader hands the supervisor: the phase (with a limit's reset), the
/// wall's kind and reset, and its message.
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

/// The turn ended on the weekly limit with `1 shell` counted: a wall the turn ended on
/// outranks work the agent left running, and a live turn anywhere still wins.
#[test]
fn a_wall_the_turn_ended_on_outranks_the_agents_own_background_work() {
    let mut weekly_paused: Vec<&str> = WEEKLY.to_vec();
    weekly_paused.extend([ECHO_PAUSED, ""]);
    // The measured screen, and the variants that must read the same.
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
    // Other kinds behind a shell: an API error and a login are idle with the wall named;
    // a bucket is a limit.
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
    // Controls: a live turn anywhere wins (a spinner, `esc to interrupt` in the footer),
    // and a shell under the worker's prose, no wall, is busy.
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
    // The critical-memory banner over a shell is still busy, its wall named.
    let banner = format!(
        "{}Critical memory usage (140.4GB) — restart and resume with claude --continue",
        " ".repeat(74)
    );
    let l = look(&frame(&["⏺ Working."], DONE_SHELL, &banner, &[FOOT_SHELL]));
    assert_eq!((l.phase, l.kind), (Phase::Busy, Some("memory")));
}

/// A wall the turn ended on is a wall whatever background work the footer counts (the
/// vendor joins the list with `, `; a monitor alone is soft busy): an API error, a login
/// or a full context behind it reads idle with the wall named, so the retry, the login
/// escalation and `/compact` run. Controls: a spinner still wins, and the worker's prose
/// with no wall stays busy under the same footers.
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
    // (name, the done row, the footer)
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
    // A limit wall behind the same lists is still a limit.
    for (name, done, footer) in footers {
        let l = look(&frame(WEEKLY, done, "", &[footer]));
        assert!(limited(&l), "a weekly limit behind {name}: {:?}", l.phase);
        assert_eq!(l.kind, Some("usage-weekly"), "{name}");
    }
    // Controls.
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

/// Claude Code echoes a wall as a `/goal` notice under the row that caused it, and the
/// echo is not the wall (its `to continue` is no reset). It is read through only to a row
/// its reason can come from: a rate limit follows a limit or a 429, an API rejection an
/// API error, and `Goal cleared … (<label>)` names its cause's kind. The worker's prose
/// opening with a login's or a full context's words is no cause.
#[test]
fn a_goal_echo_is_read_through_only_to_the_wall_its_reason_can_come_from() {
    // The measured shape: the limit in a gutter block, the echo under it.
    let l = look(&screen(WALL_SHELL));
    assert_eq!(l.kind, Some("usage-weekly"));
    assert_eq!(l.reset.as_deref(), Some("4pm (America/Los_Angeles)"));
    assert!(
        l.message.contains("Goal paused"),
        "the echo stays in the message: {}",
        l.message
    );
    // The bucket behind the rate-limit echo.
    let l = look(&screen(FABLE_SHELL));
    assert_eq!(l.kind, Some("model-bucket"));
    // The limit as a `⏺` error row.
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
    // An API error behind `the API rejected the last request`.
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
    // ... and the goal's own retry notice, whose reason is the API's.
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
    // A cleared goal names its cause: the login row under `(authentication failed)`.
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
    // Controls. The echo alone is a 429. The worker's prose about a login or a full
    // context above a rate-limit echo is not its cause. A label naming another kind than
    // the row above, and a reason no wall row can cause (a hook), read through to nothing.
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

/// A reset is a time, or nothing: `send a message after it resets to continue`
/// names none, and `to continue` is no reset.
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

/// The vendor's going-on notices stay with the wall they belong to, where the supervisor
/// reads them as "Claude Code continues by itself": the armed countdown, and the moment
/// it fires (`Usage limit reset · continuing automatically`), when a `keep going` would
/// duplicate the vendor's own resume. Negative control: the stale banner (`… has reset ·
/// press enter to continue`) says nothing goes on by itself.
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
        // The banner (2.1.283's armed form) is a wall of its own and names the time; the
        // toasts stand under the gutter row, whose reset is the wall's.
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
    // The hand-built 2.1.284 fixture and the 2.1.283 stale banner.
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
    // Where 2.1.284 draws the two notifications is not measured, so the countdown is
    // found by its wording anywhere outside the composer, never in it (a person's draft).
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
    // Controls: no notification, no countdown; a draft that says it is none.
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

/// A screen displaced by a render fault (every row one higher, a footer row twice, a
/// hint row fused) still shows the wall, and a duplicated footer whose copy carries a
/// live signal is still a live turn: every footer row is scanned.
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

/// The vendor draws its session-limit banner in the footer while the turn is still
/// wrapping up (`✻ Waiting for 1 dynamic workflow to finish`): a banner or footer notice
/// behind background work counts once the status row is a done row, not before. The
/// `⎿` block and `⏺` error row are the turn's own last words and need no done row.
#[test]
fn a_wall_drawn_while_the_turn_wraps_up_is_not_yet_the_end_of_it() {
    // Measured: the banner over `Waiting for …`. The agent works on.
    let l = look(&screen(WRAPUP_WAITING));
    assert_eq!(
        (l.phase.clone(), l.kind),
        (Phase::Busy, None),
        "{:?}",
        l.phase
    );
    // Measured: the turn ended (`done 9:22 PM · 1 shell still running`), the banner and
    // the gutter blocks both on the screen.
    let l = look(&screen(CHECKIN_SHELL));
    assert!(limited(&l), "{:?}", l.phase);
    assert_eq!(l.kind, Some("usage-session"));
    assert_eq!(
        l.reset.as_deref(),
        Some("9:50pm"),
        "the banner is the wall the reader places first"
    );
    // The banner alone (the gutter blocks cut away) is the wall too, once a
    // done row says the turn is over ...
    let rows = screen(CHECKIN_SHELL);
    let done = rows
        .iter()
        .position(|r| r.contains("done 9:22 PM"))
        .unwrap();
    let banner_only: Vec<String> = rows[..1].iter().chain(&rows[done..]).cloned().collect();
    let l = look(&banner_only);
    assert!(limited(&l), "{:?}", l.phase);
    assert_eq!(l.reset.as_deref(), Some("9:50pm"));
    // ... but not over a status row that is work in flight, a status row that
    // is missing (no evidence the turn ended), or a spinner.
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
    // Controls. The gutter block is the turn's last word: over a `Waiting for …` row it
    // is still read ...
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
    // ... and with the banner cut away and the turn over, the wall is the session window
    // the gutter block names.
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
    // A tab with no background work reads the banner at once: a done row, no shell.
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

/// A message submitted under the status row is drawn a frame before its turn's spinner;
/// until then the wall above it is history, as `background_wait` already says.
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

/// A screen busy only with the agent's own background work is told from a live turn,
/// so the loop keeps waiting on a wall whose rows have left the screen (scrolled away,
/// or under a local command's output).
#[test]
fn a_screen_busy_only_with_background_work_is_told_from_a_live_turn() {
    let measured = screen(WALL_SHELL);
    assert!(
        crate::background_only(&measured),
        "the measured wall behind a shell"
    );
    // The wall's rows scrolled out of view: the workflow notice, the gutter
    // block and the echo. The screen reads busy and holds no wall ...
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
    // ... and so does a local command's output under the wall: `/model`,
    // `/usage`, `/model` then Esc.
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
    // A monitor only (soft busy) is background work too.
    let monitor = frame(
        &["⏺ Working."],
        "✻ Churned for 0s · done 8:52 PM · 1 monitor still running",
        "",
        &["  ⏵⏵ bypass permissions on · 1 monitor"],
    );
    assert!(crate::background_only(&monitor));
    // Controls: a spinner, `esc to interrupt`, a shell-less idle screen and a screen with
    // no composer frame are not.
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

/// A wall behind background work is one reading: the limit phase with its reset, the
/// wall, and the phase authoritative with the cursor. Negative control: the same frame
/// with a spinner under it reads busy with no wall.
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
    // A wall stands wherever the cursor is: only an `idle` needs the cursor.
    let away = crate::read_at(Some("claude"), &rows, Some(0), Some(2));
    assert!(away.phase_authoritative, "a wall needs no cursor");

    let live = screen(WALL_SPINNER);
    let caret = live.iter().rposition(|r| r.starts_with('❯')).unwrap();
    let r = crate::read_at(Some("claude"), &live, Some(caret), Some(2));
    assert_eq!(r.phase, Phase::Busy, "a live turn under the wall");
    assert!(r.wall.is_none(), "and no wall is read under a hard busy");
}

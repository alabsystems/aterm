// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_vendor_limit_catalog_classifies() {
    let catalog: &[(&str, Option<WallKind>)] = &[
        (
            "You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
            Some(WallKind::UsageWeekly),
        ),
        (
            "You've hit your session limit · resets 3pm (America/Los_Angeles)",
            Some(WallKind::UsageSession),
        ),
        (
            "You've hit your Opus limit · resets 4pm (America/Los_Angeles)",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "You've reached your Fable limit. /model to switch models.",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "Fable 5 requires usage credits. Run /usage-credits to continue or switch models with /model.",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "You're out of usage credits. Run /usage-credits to keep using Fable or /model to switch models.",
            Some(WallKind::Spend),
        ),
        (
            "You're out of extra usage · resets 4pm",
            Some(WallKind::Spend),
        ),
        (
            "You've hit your monthly spend limit.",
            Some(WallKind::Spend),
        ),
        (
            "You've hit your team's shared budget. /model to switch models.",
            Some(WallKind::Spend),
        ),
        (
            "Your org is out of usage · add funds to continue",
            Some(WallKind::Spend),
        ),
        (
            "Your org is out of usage · contact your admin",
            Some(WallKind::Spend),
        ),
        (
            "Your seat type doesn't include usage credits",
            Some(WallKind::Spend),
        ),
        (
            "Your seat type doesn't include extra usage",
            Some(WallKind::Spend),
        ),
        (
            "Your usage allocation has been disabled by your admin",
            Some(WallKind::Spend),
        ),
        (
            "Your group's usage limit is set to $0",
            Some(WallKind::Spend),
        ),
        (
            "This service is disabled for your org",
            Some(WallKind::Spend),
        ),
        (
            "Your organization has disabled Claude subscription access · Sign in again",
            Some(WallKind::Spend),
        ),
        ("Credit balance is too low", Some(WallKind::Spend)),
        (
            "Usage limit reached · continuing automatically at 2:10pm · esc to cancel",
            Some(WallKind::UsageSession),
        ),
        (
            "Usage limit reached · continuing automatically when it resets · esc to cancel",
            Some(WallKind::UsageSession),
        ),
        (
            "Usage limit reached · continuing shortly · esc to cancel",
            Some(WallKind::UsageSession),
        ),
        (
            "Usage limit reached again · continuing automatically at 2:10pm · esc to cancel",
            Some(WallKind::UsageSession),
        ),
        ("Continuing automatically at 2:10pm · esc to cancel", None),
        ("Usage limit reset · continuing automatically", None),
        ("Usage limit available again · continuing now", None),
        ("Usage limit has reset · press enter to continue", None),
        ("Your usage limit has reset · press enter to continue", None),
        (
            "Automatic continue cancelled · /rate-limit-options to re-arm",
            None,
        ),
        (
            "Automatic continue stopped · the usage limit now resets more than 24 hours out, so this task will not resume on its own (/rate-limit-options to wait anyway)",
            None,
        ),
        (
            "Automatic continue was turned off · this task will not resume on its own",
            None,
        ),
        (
            "Goal paused · usage limit reached · send a message after it resets to continue",
            Some(WallKind::UsageSession),
        ),
        (
            "Goal paused · usage limit reached · continues automatically when it resets",
            Some(WallKind::UsageSession),
        ),
        (
            "Goal paused · the request was rate limited · send a message to retry",
            Some(WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            }),
        ),
        (
            "Goal paused · the API rejected the last request · send a message to continue, or run /goal clear",
            None,
        ),
        (
            "Goal paused · the last turn could not finish · send a message to continue",
            None,
        ),
        (
            "Goal paused · a hook ended the turn · send a message to continue",
            None,
        ),
        (
            "Goal still active · the API was unavailable or the connection dropped · retrying in 5 min (2/3) · send a message to retry now",
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            }),
        ),
        (
            "Goal paused after 3 automatic retries · the API was unavailable or the connection dropped · send a message to continue",
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            }),
        ),
        (
            "Goal still active · the API returned an unexpected response · retrying in 1 min (1/3) · send a message to retry now",
            None,
        ),
        (
            "Goal cleared after an unrecoverable error (authentication failed): \"x\". Run /goal again to continue.",
            Some(WallKind::Auth),
        ),
        (
            "Goal cleared after an unrecoverable error (credit balance too low): \"x\". Run /goal again to continue.",
            Some(WallKind::Spend),
        ),
        (
            "Goal cleared after an unrecoverable error (context limit reached): \"x\". Run /goal again to continue.",
            Some(WallKind::Context),
        ),
        (
            "Goal cleared after an unrecoverable error (model unavailable): \"x\". Run /goal again to continue.",
            None,
        ),
        (
            "Authentication error · This may be a temporary network issue, please try again",
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Unreachable,
            }),
        ),
        (
            "Authentication error · The gateway could not authenticate with its upstream provider — contact your gateway administrator",
            Some(WallKind::Auth),
        ),
        (
            "Could not refresh your login because another Claude Code process is refreshing it (or exited mid-refresh) · Try again in a minute",
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Unreachable,
            }),
        ),
        ("Login expired · Please run /login", Some(WallKind::Auth)),
        (
            "OAuth token revoked · Please run /login",
            Some(WallKind::Auth),
        ),
        ("Not logged in · Please run /login", Some(WallKind::Auth)),
        (
            "Authentication required · Sign in again to continue",
            Some(WallKind::Auth),
        ),
        (
            "Invalid API key · Fix external API key",
            Some(WallKind::Auth),
        ),
        (
            "Your organization has disabled API key authentication · Sign in again with your claude.ai account",
            Some(WallKind::Auth),
        ),
        (
            "There's an issue with the selected model (x). It may not exist or you may not have access to it. Run /model to pick a different model.",
            Some(WallKind::ApiError {
                code: None,
                retryable: false,
                cause: ApiCause::Request,
            }),
        ),
        (
            "Context exceeds the 200000-token limit · run /compact",
            Some(WallKind::Context),
        ),
        (
            "Extra usage is required for long context · run /extra-usage",
            Some(WallKind::Spend),
        ),
        (
            "API Error: Usage credits required for 1M context · run /model to switch to standard context",
            Some(WallKind::Spend),
        ),
        (
            "API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.",
            Some(WallKind::Overloaded),
        ),
        ("Repeated 529 Overloaded errors", Some(WallKind::Overloaded)),
        (
            "Opus is experiencing high load, please use /model to switch to Sonnet",
            Some(WallKind::Overloaded),
        ),
        (
            "Server is temporarily limiting requests (not your usage limit)",
            Some(WallKind::Overloaded),
        ),
        (
            "We are experiencing high demand for Opus 4.",
            Some(WallKind::Overloaded),
        ),
        (
            "We are experiencing high demand for Fable.",
            Some(WallKind::Overloaded),
        ),
        (
            "API Error: 401 Invalid API key · Please run /login",
            Some(WallKind::Auth),
        ),
        (
            "Context limit reached · /compact or /clear to continue",
            Some(WallKind::Context),
        ),
        (
            "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            Some(WallKind::UsageSession),
        ),
        (
            "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)",
            Some(WallKind::UsageWeekly),
        ),
        (
            "You've reached your Fable 5 limit.",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "Opus 4.1 requires usage credits.",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "Sonnet 4.5 limit reached ∙ resets 3am",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "Opus 4.1 limit reached, now using Sonnet 4.5",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        (
            "Opus 4.1 now uses usage credits",
            Some(WallKind::ModelBucket { consent: false }),
        ),
        ("Opus 4.1 is the default model. Limit reached nowhere", None),
        (
            "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
            Some(WallKind::ModelBucket { consent: true }),
        ),
        (
            "You're out of usage credits. Switch to another model",
            Some(WallKind::Spend),
        ),
        (
            "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel",
            Some(WallKind::UsageSession),
        ),
        (
            "Claude usage limit reached. Your limit will reset at 3pm (America/New_York).",
            Some(WallKind::UsageSession),
        ),
        (
            "5-hour limit reached ∙ resets 3am",
            Some(WallKind::UsageSession),
        ),
        (
            "You've hit your limit · resets 3am (Europe/London)",
            Some(WallKind::UsageSession),
        ),
        (
            "API Error: Rate limit reached for requests",
            Some(WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            }),
        ),
        (
            "API Error: 500 Internal server error",
            Some(WallKind::ApiError {
                code: Some(500),
                retryable: true,
                cause: ApiCause::Server,
            }),
        ),
        (
            "API Error: 400 due to tool use concurrency issues.",
            Some(WallKind::ApiError {
                code: Some(400),
                retryable: false,
                cause: ApiCause::Request,
            }),
        ),
        (
            "API Error: Server error mid-response. The response above may be incomplete.",
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::CutOff,
            }),
        ),
    ];
    for (text, want) in catalog {
        assert_eq!(classify_wall(text), *want, "{text}");
    }
    for text in [
        "Concurrent subagent limit reached. You can run 5 subagents at once. Do not retry.",
        "Subagent nesting limit reached (depth 3 of 3). Complete this task directly",
        "Budget limit reached ($5.01 spent of the $5.00 maximum). New agents cannot be started.",
        "Fast limit reached and temporarily disabled · resets in 4m",
        "Fast mode overloaded and is temporarily unavailable · resets in 8m",
        "Fast mode disabled · usage credit limit reached",
        "Approaching usage limit · resets at 7pm",
        "✔ Update installed · Restart to update",
        "API Error: Request was aborted.",
        "you've hit your memory limit (512 MiB)",
        "Error: API rate limit exceeded for user ID 1234.",
        "Monthly limit reached",
        "Checked the weekly limit handling in the parser",
        "The parser now reads the Fable limit notice too, and the session limit one.",
        "Switched to Sonnet due to high demand for Opus",
        "Switched to claude-sonnet-4-5 due to high demand for claude-opus-4-1",
    ] {
        assert_eq!(classify_wall(text), None, "{text}");
    }
}

#[test]
fn a_cloud_gateway_sign_in_is_the_lost_login() {
    for text in [
        "API Error: Not signed in to the Cloud gateway — run /login.",
        "API Error: Cloud gateway session expired — run /login to reconnect.",
        "API Error: Cloud gateway token expired — refresh ANTHROPIC_AUTH_TOKEN and restart.",
    ] {
        assert_eq!(classify_wall(text), Some(WallKind::Auth), "{text}");
    }
    assert_eq!(
        classify_wall("API Error: terminated"),
        Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Server,
        })
    );
}

use crate::phase::{Phase, limit_notice, worker_phase};
use crate::prompt::fixtures::{
    API_ERROR_ENOTFOUND_80_MEASURED, API_ERROR_ENOTFOUND_MEASURED, API_ERROR_QUEUED_SENT_MEASURED,
    API_ERROR_TWICE_MEASURED, COMPACT_ERROR_500_MEASURED, COMPACT_FAILED_3_MEASURED,
    COMPACT_FAILED_FIRST_MEASURED, COMPACT_NOT_ENOUGH_MEASURED, COMPACT_SUCCEEDED_MEASURED,
    CONTEXT_WALL_COMPACT_MENU_MEASURED, CONTEXT_WALL_MEASURED, composer, rows, screen,
};

fn framed(body: &[&str], footer: &str) -> Vec<String> {
    let mut r = rows(body);
    r.extend(composer(footer));
    r
}

const FOOTER: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
const API_529: &str = "API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.";

#[test]
fn the_owner_walk_reads_a_wall_but_never_in_a_calls_first_row() {
    const WALLS: [(&str, WallKind); 4] = [
        (API_529, WallKind::Overloaded),
        ("Not logged in · Please run /login", WallKind::Auth),
        (
            "Context limit reached · /compact or /clear to continue",
            WallKind::Context,
        ),
        (
            "You've hit your weekly limit · resets Oct 30 at 7pm (America/Los_Angeles)",
            WallKind::UsageWeekly,
        ),
    ];
    let screen_of = |body: &[&str]| {
        let mut rows: Vec<&str> = body.to_vec();
        rows.extend(["", "✻ Worked for 4s · done 5:47 AM", ""]);
        framed(&rows, FOOTER)
    };
    let read = |r: &Vec<String>| {
        let reading = crate::reader::read(Some("claude"), r, None);
        super::wall(r)
            .or(reading.wall)
            .map(|w| (w.kind, w.placement, w.under_call))
    };
    let calls = [
        "⏺ Bash(tail -1 build.log)",
        "⏺ Web Search(\"aterm release notes\")",
        "⏺ Update(notes.txt)",
        "! cat notes.txt",
        "❯ !cat notes.txt",
        "⏺ github - get_issue (MCP)(owner: \"acme\", issue_number: 7)",
        "⏺ github - ping (MCP)",
        "⏺ ticket desk - get_issue (MCP)(owner: \"acme\", issue_number: 8)",
        "⏺ github - Fetch Page (MCP)(url: \"http://example.invalid/x\")",
        "⏺ claude.ai Gmail - search_threads (MCP)(query: \"from:billing\")",
        "⏺ mcp__github__get_issue (MCP)(issue_number: 7)",
        "⏺ mcp__github__get_issue(issue_number: 7)",
        "⏺ Claude in Chrome[get_page_text](tabId: 1)",
        "⏺ Claude in Chrome[tabs_context_mcp]",
    ];
    let mut wrong = Vec::new();
    for (wall, kind) in WALLS {
        let gutter = format!("  ⎿  {wall}");
        let under = Some((kind, Placement::Gutter, true));
        let want = Some((kind, Placement::Gutter, false));
        for owner in calls {
            assert!(crate::phase::is_tool_call(owner), "{owner}");
            let first = screen_of(&[owner, &gutter, "     the second row of the output"]);
            if let Some(w) = read(&first) {
                wrong.push(format!("first row under {owner}: {w:?}"));
            }
            let later = screen_of(&[owner, "  ⎿  stamped", "     a second row", &gutter]);
            if read(&later) != under {
                wrong.push(format!("later row under {owner}: {:?}", read(&later)));
            }
            let hooked = screen_of(&[
                owner,
                "  ⎿  PreToolUse:Bash says: remember to run the tests",
                &gutter,
            ]);
            if read(&hooked) != under {
                wrong.push(format!("hooked under {owner}: {:?}", read(&hooked)));
            }
            let blank = screen_of(&[
                owner,
                "  ⎿  Issue 7: the parser drops a row",
                "",
                "     Steps to reproduce: run it",
                &gutter,
            ]);
            if read(&blank) != under {
                wrong.push(format!(
                    "behind a blank row under {owner}: {:?}",
                    read(&blank)
                ));
            }
            let ended = screen_of(&[
                owner,
                "  ⎿  Issue 7: the parser drops a row",
                "",
                "",
                &gutter,
            ]);
            if read(&ended) != under {
                wrong.push(format!(
                    "behind blank rows ending a block under {owner}: {:?}",
                    read(&ended)
                ));
            }
        }
        for wrapped in [
            screen_of(&[
                "⏺ Bash(cargo test --workspace --no-fail-fast -- --nocapture",
                "  2>&1 | tail -40)",
                &gutter,
            ]),
            screen_of(&[
                "⏺ github - get_issue (MCP)(owner: \"acme\", repo: \"app\",",
                "  issue_number: 7)",
                &gutter,
            ]),
            screen_of(&[
                "⏺ Bash(python3 - <<'EOF'",
                "      print('first line')",
                "",
                "      print('third line')",
                "      EOF)",
                &gutter,
            ]),
        ] {
            if let Some(w) = read(&wrapped) {
                wrong.push(format!("first row under a wrapped call: {w:?}"));
            }
        }
        for (name, r) in [
            (
                "column 5, after a blank row",
                screen_of(&[
                    "⏺ Bash(cat issue.txt)",
                    "  ⎿  Issue 7: the parser drops a row",
                    "",
                    &format!("     ⎿  {wall}"),
                ]),
            ),
            (
                "column 5",
                screen_of(&[
                    "⏺ Bash(cat issue.txt)",
                    "  ⎿  Issue 7: the parser drops a row",
                    &format!("     ⎿  {wall}"),
                ]),
            ),
            (
                "column 3, under no call",
                screen_of(&["⏺ The runs are queued.", &format!("   ⎿  {wall}")]),
            ),
        ] {
            if let Some(w) = read(&r) {
                wrong.push(format!("{name}: {w:?}"));
            }
        }
        for (name, r) in [
            (
                "scrolled off",
                framed(&[&gutter, "     more output", ""], FOOTER),
            ),
            (
                "under a long output whose call scrolled off",
                screen_of(&["     78", "     79", "     80", &gutter]),
            ),
            (
                "under a prompt's second paragraph",
                screen_of(&[
                    "❯ Please fix the parser.",
                    "",
                    "  The second paragraph says what done looks like.",
                    &gutter,
                ]),
            ),
            (
                "under a wrapped group row",
                screen_of(&[
                    "❯ toolstart",
                    "",
                    "  Searched for 2 patterns, read 1 file (ctrl+o",
                    "  to expand)",
                    &gutter,
                ]),
            ),
            (
                "under a group row behind a call's block",
                screen_of(&[
                    "⏺ Bash(ls)",
                    "  ⎿  out",
                    "",
                    "  Read 1 file (ctrl+o to expand)",
                    &gutter,
                ]),
            ),
        ] {
            if read(&r) != want {
                wrong.push(format!("{name}: {:?}", read(&r)));
            }
        }
        for owner in [
            &["❯ fourth turn: the stub says the prompt is too long"][..],
            &["⏺ Suites are running; I'll report the number when it lands."],
            &["⏺ Suites are running (all three)."],
            &["⏺ Wired the github server in (MCP), and it answers."],
            &["⏺ Fixed rows[i] so the parser keeps the last one (it dropped it)."],
            &["⏺ Here is the plan:"],
            &["⏺ Done:", "  - parser fixed", "  - tests added"],
            &["⏺ Dynamic workflow \"Second review pass\" completed · 5s"],
            &["⏺ Background command \"Run the cache tests\" completed (exit code 0)"],
            &["⏺ Goal check-in: background work still running"],
            &["⏺ The runs are queued.", ""],
            &["⏺ Monitor event: worker s-1 screen changed"],
            &["❯ toolstart", "", "  Read 1 file (ctrl+o to expand)"],
            &[
                "❯ toolstart",
                "",
                "  Called github 2 times (ctrl+o to expand)",
            ],
            &[
                "⏺ Ran 1 stop hook (ctrl+o to expand)",
                "  ⎿  Stop hook error: check the tests",
            ],
        ] {
            let mut body = owner.to_vec();
            body.push(&gutter);
            if read(&screen_of(&body)) != want {
                wrong.push(format!("control {owner:?}: {:?}", read(&screen_of(&body))));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_owner_walk_on_its_measured_screens() {
    use crate::prompt::fixtures::{
        BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED, BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED,
        COLLAPSED_GROUP_CONTEXT_WALL_MEASURED, GUTTER_PHRASE_IN_OUTPUT_MEASURED,
        LONG_OUTPUT_CONTEXT_WALL_MEASURED, MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
        MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED, MCP_MULTI_BLOCK_OUTPUT_MEASURED,
        MCP_OUTPUT_ENDS_TURN_MEASURED, MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED,
        MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
        MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED, MULTILINE_COMMAND_OUTPUT_MEASURED,
        POST_HOOK_THEN_CONTEXT_WALL_MEASURED, PRE_HOOK_ABOVE_OUTPUT_MEASURED,
        PRE_HOOK_ERROR_ABOVE_OUTPUT_MEASURED, SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
        STOP_HOOK_CONTEXT_WALL_MEASURED, WRAPPED_GROUP_ROW_CONTEXT_WALL,
    };
    let read = |capture: &str| {
        let r = screen(capture);
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(
            wall(&r).map(|w| w.kind),
            reading.wall.as_ref().map(|w| w.kind),
            "the reader and `wall` agree"
        );
        (wall(&r), reading)
    };
    const CONTEXT: &str = "Context limit reached · /compact or /clear to continue";
    for (name, capture, under_call) in [
        (
            "collapsed group",
            COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
            false,
        ),
        ("stop hook", STOP_HOOK_CONTEXT_WALL_MEASURED, false),
        ("bash output", BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED, true),
        (
            "bash output, a blank row",
            BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED,
            true,
        ),
        ("post hook", POST_HOOK_THEN_CONTEXT_WALL_MEASURED, true),
        (
            "a prompt's second paragraph",
            MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED,
            false,
        ),
        ("mcp output", MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED, true),
        ("long output", LONG_OUTPUT_CONTEXT_WALL_MEASURED, false),
    ] {
        let (w, reading) = read(capture);
        let w = w.unwrap_or_else(|| panic!("{name}: the wall"));
        assert_eq!(
            (w.kind, w.placement, w.message.as_str(), w.under_call),
            (WallKind::Context, Placement::Gutter, CONTEXT, under_call),
            "{name}"
        );
        assert_eq!(
            reading.wall.map(|w| w.under_call),
            Some(under_call),
            "{name}"
        );
    }
    let (w, _) = read(WRAPPED_GROUP_ROW_CONTEXT_WALL);
    assert_eq!(
        w.map(|w| (w.kind, w.placement, w.under_call)),
        Some((WallKind::Context, Placement::Gutter, false)),
        "a wrapped group row"
    );
    for (name, capture) in [
        ("mcp", MCP_OUTPUT_ENDS_TURN_MEASURED),
        ("shell mode", SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED),
        ("column 5", GUTTER_PHRASE_IN_OUTPUT_MEASURED),
        ("heredoc", MULTILINE_COMMAND_OUTPUT_MEASURED),
    ] {
        let (w, reading) = read(capture);
        assert_eq!(w, None, "{name}");
        assert_eq!(reading.phase, Phase::Idle, "{name}");
        assert!(reading.phase_authoritative, "{name}");
    }
    for (name, capture, under_call) in [
        ("mcp, three blocks", MCP_MULTI_BLOCK_OUTPUT_MEASURED, true),
        (
            "mcp, a blank row in the first block",
            MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
            true,
        ),
        (
            "mcp, the first block ending on a blank line",
            MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
            true,
        ),
        (
            "mcp, a long block then another",
            MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED,
            false,
        ),
        ("pre hook", PRE_HOOK_ABOVE_OUTPUT_MEASURED, true),
        ("pre hook error", PRE_HOOK_ERROR_ABOVE_OUTPUT_MEASURED, true),
    ] {
        let (w, _) = read(capture);
        assert_eq!(
            w.map(|w| (w.kind, w.placement, w.under_call)),
            Some((WallKind::Auth, Placement::Gutter, under_call)),
            "{name}: read, for the transcript to refuse"
        );
    }
}

#[test]
fn the_login_expired_error_row_is_the_auth_wall() {
    use crate::prompt::fixtures::LOGIN_EXPIRED;
    let r = screen(LOGIN_EXPIRED);
    let w = wall(&r).expect("the login wall");
    assert_eq!(w.kind, WallKind::Auth);
    assert_eq!(w.placement, Placement::ErrorRow);
    assert_eq!(w.message, "Login expired · Please run /login");
    assert_eq!(w.reset, None);
    assert_eq!(r[w.row], "⏺ Login expired · Please run /login");
    assert_eq!(worker_phase(&r), Phase::Idle);
    assert_eq!(limit_notice(&r), None);
    let reading = crate::reader::read(Some("claude"), &r, None);
    assert_eq!(reading.phase, Phase::Idle);
    assert!(reading.phase_authoritative);
    assert_eq!(reading.wall.map(|w| w.kind), Some(WallKind::Auth));
    assert!(!login_restored(&r));

    let at = w.row;
    let with = |tail: &[&str]| {
        let mut v = r.clone();
        v.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
        v
    };
    for (name, v, kind) in [
        (
            "over a done row",
            with(&[
                "⏺ Login expired · Please run /login",
                "",
                "✻ Worked for 0s · done 5:00 AM",
            ]),
            WallKind::Auth,
        ),
        (
            "wrapped",
            with(&["⏺ Login expired · Please run", "  /login"]),
            WallKind::Auth,
        ),
        (
            "the profile wording",
            with(&[
                "⏺ Login expired · Run /login to sign in again, or re-authenticate your \
                 Anthropic profile",
            ]),
            WallKind::Auth,
        ),
        (
            "an API error in the same form",
            with(&[&format!("⏺ {API_529}")]),
            WallKind::Overloaded,
        ),
    ] {
        let w = wall(&v).unwrap_or_else(|| panic!("{name}"));
        assert_eq!((w.kind, w.placement), (kind, Placement::ErrorRow), "{name}");
    }
}

#[test]
fn the_workers_words_and_a_finished_login_are_no_login_wall() {
    use crate::prompt::fixtures::LOGIN_EXPIRED;
    let r = screen(LOGIN_EXPIRED);
    let at = wall(&r).expect("the control").row;
    let with = |tail: &[&str]| {
        let mut v = r.clone();
        v.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
        v
    };
    let login_back = with(&[
        "⏺ Login expired · Please run /login",
        "",
        "❯ /login",
        "  ⎿  Login successful",
    ]);
    assert_eq!(wall(&login_back), None, "the login is back");
    assert!(login_restored(&login_back));
    assert!(login_restored(&with(&[
        "⏺ Login expired · Please run /login",
        "",
        "❯ /login",
        "  ⎿  Login successful · account or organization changed",
    ])));
    for (name, v) in [
        (
            "a sentence with no remedy joined on",
            with(&["⏺ Not logged in to gh, so nothing was pushed."]),
        ),
        (
            "the notice quoted deep in a sentence",
            with(&["⏺ Pushed the branch; the other tab said Login expired · Please run /login"]),
        ),
        (
            "a block longer than a notice",
            with(&[
                "⏺ Login expired · Please run /login is what the other tab said,",
                "  and it said it again after the retry,",
                "  and a third time after the second,",
                "  so I stopped there.",
            ]),
        ),
        (
            "a tool call that echoes it",
            with(&["⏺ Bash(echo 'Login expired · Please run /login')"]),
        ),
        (
            "a tool's output",
            with(&[
                "⏺ Bash(tail -1 peer.log)",
                "  ⎿  Login expired · Please run /login",
            ]),
        ),
        (
            "answered after it",
            with(&[
                "⏺ Login expired · Please run /login",
                "",
                "❯ continue",
                "",
                "⏺ Back at it: the suites are running.",
            ]),
        ),
    ] {
        assert_eq!(wall(&v), None, "{name}");
        assert!(!login_restored(&v), "{name}");
    }
    for (name, v) in [
        (
            "an interrupted login",
            with(&[
                "⏺ Login expired · Please run /login",
                "",
                "❯ /login",
                "  ⎿  Login interrupted",
            ]),
        ),
        (
            "the words in a tool's output",
            with(&["⏺ Bash(grep -c Login auth.log)", "  ⎿  Login successful"]),
        ),
    ] {
        assert!(!login_restored(&v), "{name}");
    }
}

#[test]
fn a_login_back_wrapped_in_a_narrow_pane_is_read() {
    let rule = "─".repeat(40);
    let screen_with = |call: &str| {
        let mut v = rows(&[
            "⏺ Login expired · Please run /login",
            "",
            call,
            "  ⎿  Login successful · account or",
            "     organization changed",
            "",
        ]);
        v.extend(rows(&[&rule, "❯", &rule, "  ? for shortcuts"]));
        v
    };
    let back = screen_with("❯ /login");
    assert_eq!(wall(&back), None);
    assert!(login_restored(&back));
    assert!(!login_restored(&screen_with("⏺ Bash(tail -2 login.log)")));
}

/// A LOGIN BACK BESIDE THE SIDE PANEL IS READ: its rows are read up to the panel's rule
/// ([`crate::phase::Transcript`]). Read whole before (KI-1101-3), the person's `❯ /login` row
/// ended in its padding and the rule, so it was no `/login` and the login read as never back.
/// No capture draws a login beside the panel, so these screens are CONSTRUCTED: the measured
/// 2.1.316 panel capture's rows above its last waiting row, then the login rows of
/// [`the_workers_words_and_a_finished_login_are_no_login_wall`] and a done row, each padded to
/// the measured rule with no panel text beside it or the panel's measured heading, and the
/// capture's composer, emptied, and its footer, bare. CONTROL: the same rows with no panel
/// (each cut at the rule) read the same, and a tool's call under the same rows is no `/login`.
#[test]
fn a_login_back_beside_the_side_panel_is_read() {
    use crate::prompt::fixtures::WAITING_AGENTS_WORKFLOWS_2_1_316;
    let panel = screen(WAITING_AGENTS_WORKFLOWS_2_1_316);
    let at = panel
        .iter()
        .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
        .expect("the waiting row");
    let (head, _) = panel[at].split_once('│').expect("the panel's rule");
    let rule = head.chars().count();
    let heading = panel[1].split_once('│').expect("the heading").1;
    let beside = |call: &str, side: &str| {
        let mut v = panel[..at].to_vec();
        for row in [
            "⏺ Login expired · Please run /login",
            "",
            call,
            "  ⎿  Login successful",
            "",
            "✻ Worked for 12s · done 4:57 PM",
            "",
        ] {
            v.push(format!("{row:<rule$}│{side}"));
        }
        let top = at
            + 1
            + panel[at + 1..]
                .iter()
                .position(|r| r.starts_with('─'))
                .expect("the composer's top rule");
        v.extend([
            panel[top].clone(),
            "❯".to_string(),
            panel[top + 2].clone(),
            String::new(),
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string(),
        ]);
        v
    };
    let no_panel = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|r| {
                r.split_once('│')
                    .map_or(r.as_str(), |(left, _)| crate::text::trim_end(left))
                    .to_string()
            })
            .collect()
    };
    for side in ["", heading] {
        let back = beside("❯ /login", side);
        let call = beside("⏺ Bash(tail -2 login.log)", side);
        for (name, back, call) in [
            ("beside the panel", back.clone(), call.clone()),
            ("no panel", no_panel(&back), no_panel(&call)),
        ] {
            assert_eq!(wall(&back), None, "{name} │{side}");
            assert!(login_restored(&back), "{name} │{side}: {back:#?}");
            assert!(!login_restored(&call), "CONTROL {name} │{side}");
        }
    }
}

#[test]
fn the_auto_continues_end_is_read_off_the_vendors_word() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED, WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
        WALL_SESSION_RESUMED_BY_ITSELF_MEASURED, screen,
    };
    let armed = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
    let cancelled = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
    let resumed = screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED);
    assert!(wall(&armed).is_some());
    assert_eq!(auto_continue_end(&armed), Some(AutoContinue::Armed));
    for left in [&cancelled, &resumed] {
        assert_eq!(wall(left), None);
    }
    assert_eq!(auto_continue_end(&cancelled), Some(AutoContinue::Unkept));
    assert_eq!(auto_continue_end(&resumed), Some(AutoContinue::Fired));
    let row = |rows: &[String], opens: &str| {
        rows.iter()
            .position(|r| r.starts_with(opens))
            .unwrap_or_else(|| panic!("no {opens:?} row"))
    };
    let cancel_row = row(&cancelled, "⏺ Automatic continue cancelled");
    let footer = row(&cancelled, "  ⏵⏵ auto mode on");
    let mut only_footer = cancelled.clone();
    only_footer[cancel_row] = String::new();
    assert_eq!(auto_continue_end(&only_footer), Some(AutoContinue::Unkept));
    let mut only_transcript = cancelled.clone();
    only_transcript[footer] = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
    assert_eq!(
        auto_continue_end(&only_transcript),
        Some(AutoContinue::Unkept)
    );
    for (said, want) in [
        (
            "⏺ Usage limit available again · continuing now",
            AutoContinue::Fired,
        ),
        (
            "⏺ Automatic continue stopped · the usage limit now resets more than 24 hours \
             out, so this task will not resume on its own",
            AutoContinue::Unkept,
        ),
        (
            "⏺ Automatic continue was turned off · this task will not resume on its own",
            AutoContinue::Unkept,
        ),
        (
            "⏺ Usage limit has reset · press enter to continue",
            AutoContinue::Unkept,
        ),
        (
            "⏺ Your usage limit has reset · press enter to continue",
            AutoContinue::Unkept,
        ),
        (
            "⚠ Usage limit reset ∙ continuing automatically",
            AutoContinue::Fired,
        ),
    ] {
        let mut v = only_transcript.clone();
        v[cancel_row] = said.to_string();
        assert_eq!(auto_continue_end(&v), Some(want), "{said}");
    }
    let mut later = only_transcript.clone();
    later.splice(
        cancel_row + 1..cancel_row + 1,
        ["", "❯ /usage", "  ⎿  Status dialog dismissed"].map(str::to_string),
    );
    assert_eq!(auto_continue_end(&later), None);
    let fired = row(&resumed, "⏺ Usage limit reset");
    let mut again = resumed.clone();
    again.splice(
        fired + 1..fired + 1,
        [
            "",
            "⏺ Usage limit reached again · continuing automatically at 8:50pm · esc to cancel",
        ]
        .map(str::to_string),
    );
    assert_eq!(auto_continue_end(&again), None);
    let mut countdown = only_transcript.clone();
    countdown[cancel_row] = "  Continuing automatically at 3:50pm · esc to cancel".to_string();
    assert_eq!(auto_continue_end(&countdown), Some(AutoContinue::Armed));
    let top = cancelled
        .iter()
        .rposition(|r| r.starts_with('─'))
        .expect("rule");
    assert_eq!(auto_continue_end(&cancelled[..top]), None);
}

#[test]
fn only_the_footers_notification_is_the_live_processs_toast() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED, WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
        WALL_SESSION_RESUMED_BY_ITSELF_MEASURED, screen,
    };
    let cancelled = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
    let resumed = screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED);
    assert_eq!(auto_continue_toast(&cancelled), Some(AutoContinue::Unkept));
    assert_eq!(auto_continue_end(&resumed), Some(AutoContinue::Fired));
    assert_eq!(auto_continue_toast(&resumed), None);
    let footer = cancelled
        .iter()
        .position(|r| r.starts_with("  ⏵⏵ auto mode on"))
        .expect("the footer");
    let mut toasted = cancelled.clone();
    toasted[footer] = format!(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents{}Usage limit reset · \
         continuing automatically",
        " ".repeat(29)
    );
    assert_eq!(auto_continue_toast(&toasted), Some(AutoContinue::Fired));
    let mut only_transcript = cancelled.clone();
    only_transcript[footer] = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
    assert_eq!(
        auto_continue_end(&only_transcript),
        Some(AutoContinue::Unkept)
    );
    assert_eq!(auto_continue_toast(&only_transcript), None);
    assert_eq!(
        auto_continue_toast(&screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)),
        Some(AutoContinue::Armed)
    );
    let top = cancelled
        .iter()
        .rposition(|r| r.starts_with('─'))
        .expect("rule");
    assert_eq!(auto_continue_toast(&cancelled[..top]), None);
}

#[test]
fn a_resume_at_an_armed_limit_draws_no_countdown() {
    use crate::prompt::fixtures::{
        CLEARED_AT_ARMED_LIMIT_2_1_300, RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
        RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300,
        WALL_SESSION_ARMED_CONVERSATION_B_2_1_300,
    };
    let armed = screen(WALL_SESSION_ARMED_CONVERSATION_B_2_1_300);
    assert_eq!(
        wall(&armed).and_then(|w| w.reset).as_deref(),
        Some("9:19am")
    );
    assert_eq!(auto_continue_end(&armed), Some(AutoContinue::Armed));
    let older = screen(RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300);
    assert_eq!(wall(&older), None);
    assert_eq!(auto_continue_end(&older), Some(AutoContinue::Fired));
    assert_eq!(
        since_limit(&older),
        Some(SinceLimit {
            kind: WallKind::UsageSession,
            reset: Some("8:48am".to_string()),
            served: false,
            fired: true,
            model_set: false,
            chosen_model: None,
            generic: false,
        })
    );
    for dropped in [
        RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
        CLEARED_AT_ARMED_LIMIT_2_1_300,
    ] {
        let rows = screen(dropped);
        assert_eq!(
            (wall(&rows), auto_continue_end(&rows), since_limit(&rows)),
            (None, None, None),
            "{rows:#?}"
        );
    }
}

#[test]
fn a_generic_countdown_row_is_the_turns_own_notices() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
        WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302,
        WALL_SONNET_BUCKET_GENERIC_COUNTDOWN_ARMED_2_1_302,
        WALL_SONNET_BUCKET_GENERIC_COUNTDOWN_GONE_ON_2_1_302, screen,
    };
    let bucket = WallKind::ModelBucket { consent: false };
    let armed = since_limit(&screen(WALL_SONNET_BUCKET_GENERIC_COUNTDOWN_ARMED_2_1_302))
        .expect("the bucket");
    assert_eq!(
        (armed.kind, armed.reset.as_deref(), armed.fired),
        (bucket, Some("9:13am"), false)
    );
    let gone_on = since_limit(&screen(
        WALL_SONNET_BUCKET_GENERIC_COUNTDOWN_GONE_ON_2_1_302,
    ))
    .expect("the bucket");
    assert_eq!((gone_on.kind, gone_on.fired), (bucket, true));
    let left = left_wall(&screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302))
        .expect("the wall the cancel left");
    assert_eq!(left.kind, bucket);
    assert!(
        left.message.starts_with("You've hit your Sonnet limit"),
        "{left:?}"
    );
    let session = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
    let window = since_limit(&session).expect("the window");
    assert_eq!(window.kind, WallKind::UsageSession);
    let elsewhere: Vec<String> = screen(WALL_SONNET_BUCKET_GENERIC_COUNTDOWN_ARMED_2_1_302)
        .into_iter()
        .map(|r| {
            if r.contains("You've hit your Sonnet limit") {
                r.replace("9:13am", "8:13am")
            } else {
                r
            }
        })
        .collect();
    assert_eq!(
        since_limit(&elsewhere).map(|s| s.kind),
        Some(WallKind::UsageSession)
    );
}

#[test]
fn the_newest_limit_and_a_turn_served_since_are_read_off_the_screens_order() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED, WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
        WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED, WALL_SESSION_RESUMED_BY_ITSELF_MEASURED,
        screen,
    };
    let since = |kind: WallKind, reset: &str, served: bool| {
        Some(SinceLimit {
            kind,
            reset: Some(reset.to_string()),
            served,
            fired: false,
            model_set: false,
            chosen_model: None,
            generic: false,
        })
    };
    let turned = screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED);
    assert_eq!(wall(&turned), None);
    let countdown = classify_wall(
        "⏺ Usage limit reached · continuing automatically at 10:38am · esc to cancel",
    )
    .expect("the countdown reads as the limit");
    assert_eq!(since_limit(&turned), since(countdown, "10:38am", true));
    let row = |opens: &str| {
        turned
            .iter()
            .position(|r| r.starts_with(opens))
            .unwrap_or_else(|| panic!("no {opens:?} row"))
    };
    let prompt = row("❯ what is left to do?");
    let mut after_usage = turned.clone();
    after_usage.splice(
        prompt..prompt,
        ["❯ /usage", "  ⎿  Status dialog dismissed", ""].map(str::to_string),
    );
    assert_eq!(since_limit(&after_usage), since(countdown, "10:38am", true));
    for (name, text, reset, fired) in [
        (
            "armed",
            WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
            "3:15pm",
            false,
        ),
        (
            "cancelled",
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
            "3:50pm",
            false,
        ),
        (
            "resumed",
            WALL_SESSION_RESUMED_BY_ITSELF_MEASURED,
            "3:35pm",
            true,
        ),
    ] {
        let read = since_limit(&screen(text)).unwrap_or_else(|| panic!("{name}: no limit"));
        assert_eq!(
            (read.served, read.reset.as_deref(), read.fired),
            (false, Some(reset), fired),
            "{name}"
        );
    }
    let history = rows(&[
        "❯ carry on with the task",
        "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
        "",
        "⏺ Usage limit reset · continuing automatically",
        "",
        "⏺ Picked the task up again.",
        "",
        "✻ Cogitated for 2s · done 3:01 PM",
        "",
        &"─".repeat(100),
        "❯",
        &"─".repeat(100),
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    ]);
    assert_eq!(
        since_limit(&history),
        Some(SinceLimit {
            kind: WallKind::UsageSession,
            reset: Some("3pm (America/Los_Angeles)".to_string()),
            served: false,
            fired: true,
            model_set: false,
            chosen_model: None,
            generic: false,
        })
    );
    let mut met_after = history.clone();
    met_after.splice(
        8..8,
        [
            "❯ keep going",
            "  ⎿  You've hit your session limit · resets 6pm (America/Los_Angeles)",
            "",
        ]
        .map(str::to_string),
    );
    assert_eq!(
        since_limit(&met_after).map(|s| (s.reset, s.fired)),
        Some((Some("6pm (America/Los_Angeles)".to_string()), false))
    );
    let mut local = turned.clone();
    local[prompt] = "❯ /usage".to_string();
    assert_eq!(since_limit(&local), since(countdown, "10:38am", false));
    let reply = row("⏺ Hi! I am the local lab stub, not a model. (reply 3)");
    let mut met_again = turned.clone();
    met_again[reply] =
        "  ⎿  You've hit your session limit · resets 11:38am (America/Los_Angeles)".to_string();
    assert_eq!(
        since_limit(&met_again),
        since(
            WallKind::UsageSession,
            "11:38am (America/Los_Angeles)",
            false
        )
    );
    let mut unanswered = turned.clone();
    unanswered[reply] = "  ⎿  Interrupted · What should Claude do instead?".to_string();
    assert_eq!(since_limit(&unanswered), since(countdown, "10:38am", false));
    let weekly = screen(include_str!(
        "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    ));
    assert_eq!(
        since_limit(&weekly),
        since(WallKind::UsageWeekly, "4pm (America/Los_Angeles)", false)
    );
    let notice = row("❯ carry on with the task");
    let cancel = row("⏺ Automatic continue cancelled");
    let mut unordered = turned.clone();
    unordered.drain(notice..=cancel);
    assert_eq!(since_limit(&unordered), None);
    let top = turned
        .iter()
        .rposition(|r| r.starts_with('─'))
        .expect("rule");
    assert_eq!(since_limit(&turned[..top]), None);
}

#[test]
fn a_model_chosen_since_the_newest_limit_is_read_off_the_screens_order() {
    use crate::prompt::fixtures::{
        IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, screen,
    };
    let measured = screen(IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
    let read = since_limit(&measured).expect("the Fable bucket's notice");
    assert_eq!(
        (read.kind, read.reset, read.served, read.model_set),
        (WallKind::ModelBucket { consent: false }, None, false, true)
    );
    assert_eq!(
        read.chosen_model.as_deref(),
        Some("Opus 5 (1M context) (default) and saved as your default for new sessions")
    );
    let armed = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let reset = "Oct 17 at 4:20pm (America/Los_Angeles)";
    let bucket = |model_set: bool| {
        Some(SinceLimit {
            kind: WallKind::ModelBucket { consent: false },
            reset: Some(reset.to_string()),
            served: false,
            fired: false,
            model_set,
            chosen_model: model_set
                .then(|| "Opus 5.5 and saved as your default for new sessions".to_string()),
            generic: false,
        })
    };
    let set_row = "  ⎿  Set model to Opus 5.5 and saved as your default for new sessions";
    let under = |prompt: &str, result: &str| {
        let mut rows: Vec<String> = armed
            .iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .cloned()
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the frame");
        rows.splice(top..top, [prompt, result, ""].map(str::to_string));
        rows
    };
    assert_eq!(since_limit(&under("❯ /model", set_row)), bucket(true));
    assert_eq!(since_limit(&under("❯ /model opus", set_row)), bucket(true));
    let mut twice = under("❯ /model", set_row);
    let top = twice.iter().position(|r| r.starts_with('─')).unwrap();
    twice.splice(
        top..top,
        ["❯ /model", "  ⎿  Set model to new-default", ""].map(str::to_string),
    );
    assert_eq!(
        since_limit(&twice).and_then(|s| s.chosen_model),
        Some("new-default".to_string()),
        "the newest completed choice stands even when its model is unknown"
    );
    assert_eq!(since_limit(&armed), bucket(false));
    assert_eq!(since_limit(&under("❯ /usage", set_row)), bucket(false));
    assert_eq!(
        since_limit(&under("❯ /models-list", set_row)),
        bucket(false)
    );
    let notice = armed
        .iter()
        .position(|r| r.contains("You've hit your Sonnet limit"))
        .expect("the notice");
    let mut before = armed.clone();
    before.splice(
        notice - 1..notice - 1,
        ["❯ /model", set_row, ""].map(str::to_string),
    );
    assert_eq!(since_limit(&before), bucket(false));
    let quoted = under(
        "❯ what did /model print?",
        "⏺ It printed: Set model to Opus 5.5 and saved as your default.",
    );
    assert_eq!(
        since_limit(&quoted).map(|s| s.model_set),
        Some(false),
        "{quoted:#?}"
    );
}

#[test]
fn the_limit_a_cancel_left_is_read_as_the_wall_it_was() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AFTER_CANCEL_26_2_1_299, WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
        WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
        WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
        WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED, WALL_SESSION_RESUMED_BY_ITSELF_MEASURED,
        screen,
    };
    let cancelled = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
    let countdown = cancelled
        .iter()
        .rposition(|r| r.starts_with("⏺ Usage limit reached"))
        .expect("the countdown row");
    assert_eq!(wall(&cancelled), None);
    assert_eq!(
        left_wall(&cancelled),
        Some(Wall {
            kind: WallKind::UsageSession,
            message: "Usage limit reached · continuing automatically at 3:50pm · esc to cancel"
                .to_string(),
            reset: Some("3:50pm".to_string()),
            row: countdown,
            placement: Placement::ErrorRow,
            under_call: false,
        })
    );
    let narrow = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299);
    assert_eq!(wall(&narrow), None);
    assert_eq!(
        since_limit(&narrow),
        Some(SinceLimit {
            kind: WallKind::UsageSession,
            reset: Some("10:50am".to_string()),
            served: false,
            fired: false,
            model_set: false,
            chosen_model: None,
            generic: false,
        })
    );
    let left = left_wall(&narrow).expect("the narrow cancel's limit");
    assert_eq!(
        (
            left.kind,
            left.reset.as_deref(),
            left.placement,
            narrow[left.row].as_str()
        ),
        (
            WallKind::UsageSession,
            Some("10:50am"),
            Placement::ErrorRow,
            "⏺ Usage limit"
        )
    );
    assert!(
        left.message
            .starts_with("Usage limit reached · continuing automatically at 10:50am"),
        "{left:?}"
    );
    let mut goal = screen(include_str!(
        "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    ));
    let done = goal
        .iter()
        .position(|r| r.starts_with("✻ Sautéed"))
        .expect("the done row");
    goal.splice(
        done..done,
        [
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            "",
        ]
        .map(str::to_string),
    );
    let notice = goal
        .iter()
        .position(|r| r.contains("You've hit your weekly limit"))
        .expect("the notice");
    let left = left_wall(&goal).expect("the goal's limit");
    assert_eq!(
        (left.kind, left.reset.as_deref(), left.placement, left.row),
        (
            WallKind::UsageWeekly,
            Some("4pm (America/Los_Angeles)"),
            Placement::Gutter,
            notice
        ),
        "{left:?}"
    );
    for (name, rows) in [
        ("armed", screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)),
        ("fired", screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED)),
        (
            "served",
            screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED),
        ),
        ("met again", screen(WALL_SESSION_AFTER_CANCEL_26_2_1_299)),
    ] {
        assert_eq!(left_wall(&rows), None, "{name}");
    }
    let cancel = cancelled
        .iter()
        .position(|r| r.starts_with("⏺ Automatic continue cancelled"))
        .expect("the cancel row");
    let mut local = cancelled.clone();
    local.splice(
        cancel + 1..cancel + 1,
        ["", "❯ /usage", "  ⎿  Status dialog dismissed"].map(str::to_string),
    );
    let footer = local
        .iter()
        .position(|r| r.starts_with("  ⏵⏵ auto mode on"))
        .expect("the footer");
    local[footer] = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
    assert_eq!(left_wall(&local), None, "a local command after the cancel");
    let mut no_reset = cancelled.clone();
    no_reset[countdown] = "⏺ Usage limit reached · esc to cancel".to_string();
    for r in &mut no_reset {
        *r = r.replace("resets 3:50pm (America/Los_Angeles)", "limit reached");
    }
    assert_eq!(left_wall(&no_reset), None, "no reset named");
    let asked = cancelled
        .iter()
        .rposition(|r| r == "❯ carry on with the task")
        .expect("the prompt the limit met");
    let quoting = |reply: bool| {
        let mut q = cancelled.clone();
        let mut rows = vec![
            "❯ read the notes",
            "",
            "⏺ Bash(cat notes.txt)",
            "  ⎿  You've hit your session limit · resets 3:50pm (America/Los_Angeles)",
            "     ⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
        ];
        if reply {
            rows.extend(["", "⏺ The notes quote a limit notice."]);
        }
        q.splice(asked..=cancel, rows.into_iter().map(str::to_string));
        q
    };
    for reply in [true, false] {
        let q = quoting(reply);
        assert_eq!(
            auto_continue_end(&q),
            Some(AutoContinue::Unkept),
            "the footer still says it"
        );
        assert_eq!(left_wall(&q), None, "quoted by a tool (reply {reply})");
    }
    let top = cancelled
        .iter()
        .rposition(|r| r.starts_with('─'))
        .expect("rule");
    assert_eq!(left_wall(&cancelled[..top]), None, "no composer");
}

#[test]
fn a_walls_line_ink_wrapped_is_read_as_one() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AFTER_CANCEL_26_2_1_299, WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
        WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
        WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
        WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299,
    };
    let footer = |text: &str, banner: &str, countdown: &str, reset: &str| {
        let r = screen(text);
        let w = wall(&r).expect("the wall");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::UsageSession, Placement::Footer)
        );
        assert_eq!(w.message, format!("{banner} \u{2014} {countdown}"));
        assert_eq!(w.reset.as_deref(), Some(reset));
        assert_eq!(limit_notice(&r), Some((w.message, w.reset)));
    };
    footer(
        WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
        "⚠ Usage limit reached · limit resets 10:25am",
        "Continuing automatically at 10:25am · esc to cancel",
        "10:25am",
    );
    let r = screen(WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299);
    let w = wall(&r).expect("the weekly wall");
    assert_eq!(
        (w.kind, w.placement, w.reset.as_deref()),
        (
            WallKind::UsageWeekly,
            Placement::Footer,
            Some("Oct 15 at 12:33pm")
        )
    );
    assert_eq!(
        w.message,
        "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
         \u{2014} ⚠ Usage limit reached · limit resets Oct 15 at 12:33pm \u{2014} Continuing \
         automatically at Oct 15 at 12:33pm · esc to cancel"
    );
    assert_eq!(limit_notice(&r), Some((w.message, w.reset)));
    footer(
        WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
        "⚠ Usage limit reached · limit resets 10:51am",
        "Continuing automatically at 10:51am · esc to cancel",
        "10:51am",
    );

    let r = screen(WALL_SESSION_AFTER_CANCEL_26_2_1_299);
    let w = wall(&r).expect("the turn's notice");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::UsageSession, Placement::Gutter)
    );
    assert_eq!(
        w.message,
        "You've hit your session limit · resets 10:51am (America/Los_Angeles) /upgrade to \
         increase your usage limit."
    );
    assert_eq!(w.reset.as_deref(), Some("10:51am (America/Los_Angeles)"));
    assert_eq!(
        worker_phase(&r),
        Phase::Limited {
            message: w.message,
            reset: w.reset,
        }
    );

    let r = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299);
    assert_eq!(wall(&r), None);
    assert_eq!(auto_continue_end(&r), Some(AutoContinue::Unkept));

    let mut r = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299);
    let at = |r: &[String], opens: &str| r.iter().position(|x| x.starts_with(opens));
    let banner = at(&r, "  ⚠ Usage limit").expect("the banner");
    let mode = at(&r, "  ⏵⏵").expect("the mode line");
    r.drain(banner..mode);
    let w = wall(&r).expect("the transcript's notice");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::UsageSession, Placement::ErrorRow)
    );
    assert_eq!(
        w.message,
        "Usage limit reached · continuing automatically at 10:25am · esc to cancel"
    );
    assert_eq!(w.reset.as_deref(), Some("10:25am"));
}

/// A MARK THE GRID KEEPS IN A CELL WIDENS NO PANE: the readers that take the pane's width from
/// its widest row ([`crate::question::screen_width`]) read the `⎿` notice the last turn ended
/// on in its box to the pane's edge with no slack ([`crate::wrap::transcript_room`]), so one cell
/// of the pane decides a wrap. Counted in chars before (KI-1101-3), a row drawn to the pane's
/// edge holding characters the grid stores in the cell of the one before them (the two
/// combining acutes of a file name macOS spells decomposed) measured the pane two cells wider
/// than it is: the measured 26-column 2.1.299 notice's `You've hit your` read as a line of its
/// own (15 + 1 + 7 cells fit 23), so no wall and the phase idle, and a `Login successful ·
/// account or organization changed` Ink wrapped at that width read as no login back
/// (`account or` + 1 + `organization` fit 23). No capture draws such a row, so these screens
/// are CONSTRUCTED: the measured 26-column screen after the cancel with a call and its output
/// row (read back as aterm's grid stores it) above its first prompt; for the login, that
/// screen's last prompt and notice replaced by `❯ /login` and that notice, Ink-wrapped in the
/// gutter's room. They read as the screens without the row: the wall (its kind, placement,
/// message and reset), the limit notice and the phase; the login back. NEGATIVE CONTROL: an
/// output row really two cells past the edge (a pane 28 wide) reads the pane 28 wide, whose
/// notice rows are no wrap: no wall.
#[test]
fn a_mark_the_grid_keeps_in_a_cell_widens_no_pane() {
    use crate::prompt::fixtures::{WALL_SESSION_AFTER_CANCEL_26_2_1_299, ink_wrap};
    use crate::question::{cells, read_back, screen_width};
    const WIDTH: usize = 26;
    let measured = screen(WALL_SESSION_AFTER_CANCEL_26_2_1_299);
    assert_eq!(screen_width(&measured), WIDTH, "PRECONDITION: the pane");
    let marked = read_back("  ⎿  Re\u{301}sume\u{301}.pdf  notes.txt");
    let marked = crate::text::trim_end(&marked).to_string();
    assert_eq!(
        (cells(&marked), marked.chars().count()),
        (WIDTH, WIDTH + 2),
        "PRECONDITION: the row runs to the pane's edge, the grid keeping both marks: {marked:?}"
    );
    let first = measured
        .iter()
        .position(|r| r.starts_with("❯ "))
        .expect("the first prompt");
    let with = |screen: &[String], output: &str| {
        let mut v = screen.to_vec();
        v.splice(
            first..first,
            [
                "❯ list my documents",
                "",
                "⏺ Bash(ls ~/Documents)",
                output,
                "",
            ]
            .map(String::from),
        );
        v
    };
    let read = |r: &[String]| {
        (
            wall(r).map(|w| (w.kind, w.placement, w.message, w.reset)),
            limit_notice(r),
            worker_phase(r),
        )
    };
    let wanted = read(&measured);
    assert!(
        matches!(
            wanted.0,
            Some((WallKind::UsageSession, Placement::Gutter, _, Some(_)))
        ),
        "PRECONDITION: the measured wall: {wanted:?}"
    );
    assert_eq!(read(&with(&measured, &marked)), wanted, "the wall");
    let wider = "  ⎿  Resume.pdf  notes-2.txt";
    assert_eq!(
        cells(wider),
        WIDTH + 2,
        "PRECONDITION: two cells past the edge"
    );
    assert_eq!(
        read(&with(&measured, wider)).0,
        None,
        "NEGATIVE CONTROL: a row two cells past the edge"
    );
    let last = measured
        .iter()
        .rposition(|r| r.starts_with("❯ keep going"))
        .expect("the last prompt");
    let done = measured
        .iter()
        .rposition(|r| r.starts_with("✻ "))
        .expect("the done row");
    let mut login = measured.clone();
    let notice = ink_wrap(
        "Login successful · account or organization changed",
        crate::wrap::transcript_room(5, WIDTH),
    );
    let turn = std::iter::once("❯ /login".to_string())
        .chain(
            notice
                .iter()
                .enumerate()
                .map(|(k, r)| format!("{}{r}", if k == 0 { "  ⎿  " } else { "     " })),
        )
        .chain([String::new()]);
    login.splice(last..done, turn);
    assert!(
        notice.len() == 3 && login_restored(&login) && wall(&login).is_none(),
        "PRECONDITION: the login back: {login:#?}"
    );
    assert!(login_restored(&with(&login, &marked)), "the login back");
}

#[test]
fn a_status_lines_words_are_no_footer_notice() {
    use crate::prompt::fixtures::{
        FOOTER_NOT_LOGGED_IN_26COL_MEASURED, FOOTER_NOT_LOGGED_IN_AND_STATUS_LINE_26COL_MEASURED,
        FOOTER_NOT_LOGGED_IN_AND_STATUS_LINE_MEASURED, FOOTER_NOT_LOGGED_IN_MEASURED,
        STATUS_LINE_LOGIN_WORDS_MEASURED, WALL_SESSION_ARMED_STATUS_LINE_LOGIN_WORDS_MEASURED,
        WALL_SESSION_CANCELLED_STATUS_LINE_LOGIN_WORDS_MEASURED,
    };
    let r = screen(STATUS_LINE_LOGIN_WORDS_MEASURED);
    assert_eq!(wall(&r), None, "a status line's words after a turn");
    assert_eq!(worker_phase(&r), Phase::Idle);
    let status_row = r
        .iter()
        .position(|x| x == "  Not logged in · Run /login")
        .expect("the status line");
    let with = |line: &str| {
        let mut s = r.clone();
        s[status_row] = line.to_string();
        s
    };
    for line in [
        "  opus-5.5  ~/proj/aterm  Not logged in · Run /login",
        "  ~/Developer/aterm  main  You've hit your session limit · resets 3pm",
    ] {
        let s = with(line);
        assert_eq!(wall(&s), None, "{line}");
        assert_eq!(worker_phase(&s), Phase::Idle, "{line}");
    }
    let (left, notice) = (
        "  opus-5.5  ~/proj/aterm",
        "You've hit your session limit · resets 3pm",
    );
    let edge = with(&format!(
        "{left}{}{notice}",
        " ".repeat(142 - left.chars().count() - notice.chars().count())
    ));
    assert_eq!(edge[status_row].chars().count(), 142);
    let w = wall(&edge).expect("a notification against the edge");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::UsageSession, Placement::Footer)
    );
    let armed = screen(WALL_SESSION_ARMED_STATUS_LINE_LOGIN_WORDS_MEASURED);
    let w = wall(&armed).expect("the limit under the banner");
    assert_eq!(w.kind, WallKind::UsageSession, "{w:#?}");
    assert_eq!(auto_continue_end(&armed), Some(AutoContinue::Armed));
    let cancelled = screen(WALL_SESSION_CANCELLED_STATUS_LINE_LOGIN_WORDS_MEASURED);
    assert_eq!(wall(&cancelled), None);
    assert_eq!(auto_continue_end(&cancelled), Some(AutoContinue::Unkept));
    for (name, capture, row) in [
        ("right-aligned", FOOTER_NOT_LOGGED_IN_MEASURED, "  ⏵⏵"),
        (
            "beside the status line",
            FOOTER_NOT_LOGGED_IN_AND_STATUS_LINE_MEASURED,
            "  Not logged in",
        ),
        (
            "stacked",
            FOOTER_NOT_LOGGED_IN_26COL_MEASURED,
            "  Not logged in",
        ),
        (
            "stacked under the status line",
            FOOTER_NOT_LOGGED_IN_AND_STATUS_LINE_26COL_MEASURED,
            "  Not logged in",
        ),
    ] {
        let r = screen(capture);
        let w = wall(&r).expect(name);
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::Auth, Placement::Footer),
            "{name}: {w:#?}"
        );
        let mode = r
            .iter()
            .rposition(|x| x.starts_with("  ⏵⏵"))
            .expect("the mode row");
        let at = r
            .iter()
            .rposition(|x| x.starts_with(row))
            .expect("the notice's row");
        assert_eq!(w.row, at, "{name}");
        assert!(at >= mode || name.starts_with("beside"), "{name}");
    }
}

#[test]
fn a_generic_banner_is_the_limit_the_turns_own_notice_names() {
    use crate::prompt::fixtures::{
        WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
        WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299,
    };
    let sonnet = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let w = wall(&sonnet).expect("the Sonnet bucket");
    assert_eq!(
        (w.kind, w.placement, w.reset.as_deref()),
        (
            WallKind::ModelBucket { consent: false },
            Placement::Footer,
            Some("Oct 17 at 4:20pm")
        ),
        "{w:#?}"
    );
    assert_eq!(
        w.message,
        "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles) \u{2014} \
         ⚠ Usage limit reached · limit resets Oct 17 at 4:20pm \u{2014} Continuing \
         automatically at Oct 17 at 4:20pm · esc to cancel"
    );
    assert_eq!(classify_wall(&w.message), Some(w.kind));
    assert_eq!(limit_notice(&sonnet), Some((w.message, w.reset)));
    let weekly = screen(WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299);
    let w = wall(&weekly).expect("the weekly limit");
    assert_eq!(
        (w.kind, w.placement, w.reset.as_deref()),
        (
            WallKind::UsageWeekly,
            Placement::Footer,
            Some("Oct 15 at 12:33pm")
        ),
        "{w:#?}"
    );
    assert!(
        w.message.starts_with(
            "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
             \u{2014} ⚠ Usage limit reached"
        ),
        "{w:#?}"
    );
    for (rows, kind, prefix) in [
        (
            &sonnet,
            WallKind::ModelBucket { consent: false },
            "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles) \u{2014} ⚠ Usage limit reached",
        ),
        (
            &weekly,
            WallKind::UsageWeekly,
            "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \u{2014} ⚠ Usage limit reached",
        ),
    ] {
        let rule = rows
            .iter()
            .position(|r| r.starts_with("────"))
            .expect("the composer's top rule");
        let mut paused = rows.clone();
        paused.splice(
            rule..rule,
            [
                "",
                "⏺ Goal paused · usage limit reached · send a message after it resets to continue",
                "",
            ]
            .map(str::to_string),
        );
        let w = wall(&paused).expect("the turn's limit under Goal paused");
        assert_eq!(w.kind, kind, "{w:#?}");
        assert!(w.message.starts_with(prefix), "{w:#?}");
    }

    let session = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299);
    let w = wall(&session).expect("the session limit");
    assert_eq!(
        (w.kind, w.message.as_str()),
        (
            WallKind::UsageSession,
            "⚠ Usage limit reached · limit resets 10:25am \u{2014} Continuing automatically at \
             10:25am · esc to cancel"
        )
    );
    let banner_only = |rows: &[String]| {
        let w = wall(rows).expect("the banner");
        (w.kind, w.message.starts_with("⚠ Usage limit reached"))
    };
    let older: Vec<String> = sonnet
        .iter()
        .map(|r| {
            r.replace(
                "resets Oct 17 at 4:20pm (America",
                "resets Oct 12 at 9:05am (America",
            )
        })
        .collect();
    assert_eq!(banner_only(&older), (WallKind::UsageSession, true));
    let notice = sonnet
        .iter()
        .position(|r| r.contains("You've hit your Sonnet limit"))
        .expect("the notice");
    let mut history = sonnet.clone();
    history.splice(
        notice + 1..notice + 1,
        ["", "❯ and the tests?", "", "⏺ Running them."].map(str::to_string),
    );
    assert_eq!(banner_only(&history), (WallKind::UsageSession, true));
}

#[test]
fn every_notices_line_runs_on_over_its_wrapped_rows() {
    let framed_at = |width: usize, body: &[&str], footer: &[&str]| {
        let rule = "─".repeat(width);
        let mut r = rows(body);
        r.extend([rule.clone(), "❯".to_string(), rule]);
        r.extend(rows(footer));
        r
    };
    let r = framed_at(
        34,
        &[
            "❯ carry on with the task",
            "  ⎿  You've hit your session limit",
            "     · resets 8:15pm (UTC)",
            "",
            "✻ Cogitated for 0s · done 8:05 PM",
            "",
        ],
        &["  ⏵⏵ auto mode on (shift+tab to"],
    );
    let w = wall(&r).expect("the turn's notice");
    assert_eq!(
        (w.kind, w.placement, w.reset.as_deref()),
        (
            WallKind::UsageSession,
            Placement::Gutter,
            Some("8:15pm (UTC)")
        )
    );
    assert_eq!(
        limit_notice(&r),
        Some((
            "You've hit your session limit · resets 8:15pm (UTC)".to_string(),
            Some("8:15pm (UTC)".to_string())
        ))
    );

    let r = framed_at(
        26,
        &[
            "❯ carry on with the task",
            "  ⎿  You've hit your",
            "     session limit ·",
            "     resets 10:51am",
            "     (America/Los_Angeles)",
            "",
            "✻ Worked for 0s · done",
            "  10:36 AM",
            "",
        ],
        &[
            "  ⏵⏵ auto mode on      ·",
            "  Automatic continue",
            "  cancelled ·",
            "  /rate-limit-options to",
            "  re-arm",
        ],
    );
    assert_eq!(auto_continue_end(&r), Some(AutoContinue::Unkept));

    let r = framed_at(
        26,
        &[
            "❯ carry on with the task",
            "",
            "⏺ On it.",
            "",
            "  ⚠ Usage limit reached",
            "  · limit resets 10:51am",
            "",
        ],
        &["  ? for shortcuts"],
    );
    let w = wall(&r).expect("the banner");
    assert_eq!(
        (w.kind, w.placement, w.reset.as_deref()),
        (WallKind::UsageSession, Placement::Banner, Some("10:51am"))
    );
    assert_eq!(w.message, "⚠ Usage limit reached · limit resets 10:51am");
}

fn vendor_box(text: &str, width: usize) -> Vec<String> {
    let room = width - 10;
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split(' ') {
        let n = word.chars().count();
        assert!(n <= room, "{word:?} is wider than {room} cells");
        let line = lines.last_mut().expect("a line");
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + n <= room {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(word.to_string());
        }
    }
    let mut r = rows(&["❯ carry on with the task", ""]);
    for (k, line) in lines.iter().enumerate() {
        r.push(format!("{}{line}", if k == 0 { "⏺ " } else { "  " }));
    }
    r.push(String::new());
    let rule = "─".repeat(width);
    r.extend([
        rule.clone(),
        "❯".to_string(),
        rule,
        "  ? for shortcuts".to_string(),
    ]);
    r
}

#[test]
fn the_workers_prose_is_no_error_row_in_a_pane_of_any_width() {
    use crate::prompt::fixtures::{PROSE_LIMIT_WORDS_34_2_1_300, PROSE_LIMIT_WORDS_80_2_1_300};
    for (label, text) in [
        ("34 columns", PROSE_LIMIT_WORDS_34_2_1_300),
        ("80 columns", PROSE_LIMIT_WORDS_80_2_1_300),
    ] {
        let r = screen(text);
        assert_eq!(wall(&r), None, "{label}");
        assert_eq!(worker_phase(&r), Phase::Idle, "{label}");
    }
    let rule = "─".repeat(80);
    let mut r = rows(&[
        "❯ status?",
        "",
        "⏺ Usage limit reached · the parser is done and the lexer is next",
        "  today.",
        "  Then the migration, the docs and the review of every one of the",
        "  notes.",
        "",
    ]);
    r.extend([
        rule.clone(),
        "❯".to_string(),
        rule,
        "  ? for shortcuts".to_string(),
    ]);
    assert_eq!(wall(&r), None);
    let notice = "Usage limit reached · continuing automatically at 10:25am · esc to cancel";
    let long = "Usage limit reached · the next step edits the files named \
                crates/aterm-agent/src/supervise/limit.rs and then \
                crates/aterm-agent/src/supervise/run.rs and also \
                crates/aterm-phase/src/wall.rs.";
    for width in [26, 34, 52, 60, 70, 79, 80] {
        let w = wall(&vendor_box(notice, width)).expect("the vendor's notice");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (WallKind::UsageSession, Placement::ErrorRow, Some("10:25am")),
            "{width} columns"
        );
        if width >= 52 {
            assert_eq!(wall(&vendor_box(long, width)), None, "{width} columns");
        }
    }
}

#[test]
fn a_programs_words_behind_a_vendor_label_are_no_wall() {
    use crate::prompt::fixtures::{
        MONITOR_EVENT_CONTEXT_PHRASE_MEASURED, MONITOR_EVENT_LOGIN_PHRASE_DETAILED_MEASURED,
        MONITOR_EVENT_LOGIN_PHRASE_MEASURED, MONITOR_EVENT_THEN_CONTEXT_WALL_MEASURED,
        POST_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED, PROMPT_HOOK_MESSAGE_CONTEXT_PHRASE_MEASURED,
        STOP_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED, STOP_HOOK_STDERR_LOGIN_PHRASE_MEASURED,
    };
    let mut read = Vec::new();
    for (name, capture, phase) in [
        (
            "monitor event, login",
            MONITOR_EVENT_LOGIN_PHRASE_MEASURED,
            Phase::Busy,
        ),
        (
            "monitor event, context",
            MONITOR_EVENT_CONTEXT_PHRASE_MEASURED,
            Phase::Busy,
        ),
        (
            "monitor event, detailed",
            MONITOR_EVENT_LOGIN_PHRASE_DETAILED_MEASURED,
            Phase::Idle,
        ),
        (
            "stop hook stderr",
            STOP_HOOK_STDERR_LOGIN_PHRASE_MEASURED,
            Phase::Idle,
        ),
        (
            "stop hook message",
            STOP_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED,
            Phase::Idle,
        ),
        (
            "prompt hook message",
            PROMPT_HOOK_MESSAGE_CONTEXT_PHRASE_MEASURED,
            Phase::Idle,
        ),
        (
            "post-tool hook message",
            POST_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED,
            Phase::Idle,
        ),
    ] {
        let r = screen(capture);
        if let Some(w) = wall(&r) {
            read.push(format!(
                "{name}: {:?} {:?} {}",
                w.kind, w.placement, w.message
            ));
        }
        assert_eq!(limit_notice(&r), None, "{name}");
        assert_eq!(worker_phase(&r), phase, "{name}");
    }
    assert!(read.is_empty(), "{read:#?}");
    let w = wall(&screen(MONITOR_EVENT_THEN_CONTEXT_WALL_MEASURED)).expect("the real wall");
    assert_eq!(
        (w.kind, w.placement, w.under_call, w.message.as_str()),
        (
            WallKind::Context,
            Placement::Gutter,
            false,
            "Context limit reached · /compact or /clear to continue"
        )
    );
}

#[test]
fn a_stop_reasons_later_line_at_column_two_names_no_limit() {
    let capture = crate::prompt::fixtures::STOP_HOOK_STOP_REASON_TWO_LINES_MEASURED;
    let second = "\n  Not logged in · Please run /login\n";
    assert_eq!(
        capture.matches(second).count(),
        1,
        "the capture's second line"
    );
    let respelled = capture.replace(second, "\n  Usage limit reached · resets 3pm\n");
    let r = screen(&respelled);
    assert!(wall(&r).is_none(), "{:?}", wall(&r).map(|w| w.message));
    assert!(since_limit(&r).is_none(), "{:?}", since_limit(&r));
    assert!(
        auto_continue_end(&r).is_none(),
        "{:?}",
        auto_continue_end(&r)
    );
}

#[test]
fn background_works_delivered_output_is_not_drawn_under_its_notice() {
    use crate::prompt::fixtures::{
        VERBOSE_BACKGROUND_AGENT_LOGIN_WORDS_MEASURED,
        VERBOSE_BACKGROUND_COMMAND_LOGIN_WORDS_MEASURED,
    };
    for (name, capture, notice) in [
        (
            "command",
            VERBOSE_BACKGROUND_COMMAND_LOGIN_WORDS_MEASURED,
            "⏺ Background command \"Run the login check\" completed (exit code 0)",
        ),
        (
            "agent",
            VERBOSE_BACKGROUND_AGENT_LOGIN_WORDS_MEASURED,
            "⏺ Agent \"Login check\" finished · 0s",
        ),
    ] {
        let r = screen(capture);
        let at = r.iter().position(|x| x == notice).expect(name);
        assert!(
            !r[at + 1].starts_with("  "),
            "{name}: nothing drawn under the notice: {:?}",
            r[at + 1]
        );
        let worded: Vec<&String> = r.iter().filter(|x| x.contains("Not logged in")).collect();
        assert!(
            worded.iter().all(|x| x.starts_with("⏺ Bash(")),
            "{name}: {worded:?}"
        );
        assert_eq!(wall(&r), None, "{name}");
        assert_eq!(since_limit(&r), None, "{name}");
    }
}

#[test]
fn a_wall_whose_call_scrolled_off_stands_under_it_with_the_history() {
    use crate::phase::owner_above_rows;
    use crate::prompt::fixtures::{
        VERBOSE_LONG_OUTPUT_CONTEXT_WALL_MEASURED,
        VERBOSE_LONG_OUTPUT_CONTEXT_WALL_SCROLLBACK_MEASURED,
    };
    let r = screen(VERBOSE_LONG_OUTPUT_CONTEXT_WALL_MEASURED);
    let w = wall(&r).expect("the wall");
    assert_eq!((w.kind, w.under_call), (WallKind::Context, false));
    assert!(owner_above_rows(&r, w.row));
    let mut joined = screen(VERBOSE_LONG_OUTPUT_CONTEXT_WALL_SCROLLBACK_MEASURED);
    joined.extend(r);
    let w = wall(&joined).expect("the wall, the history over it");
    assert_eq!((w.kind, w.under_call), (WallKind::Context, true));
    assert!(!owner_above_rows(&joined, w.row));
    let on_screen = screen(CONTEXT_WALL_MEASURED);
    let w = wall(&on_screen).expect("the wall");
    assert!(!owner_above_rows(&on_screen, w.row));
}

#[test]
fn a_labelled_messages_wrapped_row_names_no_limit() {
    let monitor = screen(crate::prompt::fixtures::MONITOR_EVENT_LONG_LINE_MEASURED);
    let event = monitor
        .iter()
        .position(|r| r.starts_with("⏺ Monitor event: "))
        .expect("the event row");
    assert!(monitor[event].trim_end().ends_with('…'));
    assert!(monitor[event + 1].trim().is_empty(), "one row");
    assert_eq!(since_limit(&monitor), None);
    assert_eq!(auto_continue_end(&monitor), None);
    let capture = crate::prompt::fixtures::PROMPT_HOOK_STOPPED_WRAPPED_LIMIT_PHRASE_MEASURED;
    let r = screen(capture);
    assert_eq!(since_limit(&r), None);
    assert_eq!(auto_continue_end(&r), None);
    assert_eq!(wall(&r), None);
    let label = "⏺ Operation stopped by hook: the nightly";
    assert_eq!(capture.matches(label).count(), 1, "the capture's label");
    let unlabelled = screen(&capture.replace(label, "⏺ The nightly"));
    assert_eq!(
        since_limit(&unlabelled).map(|s| (s.kind, s.reset)),
        Some((WallKind::UsageSession, Some("3pm".to_string())))
    );
}

#[test]
fn a_stop_reason_taller_than_the_rows_read_is_read_with_the_rows_above() {
    use crate::phase::{WordsOf, words_of};
    use crate::prompt::fixtures::{
        PROMPT_HOOK_STOPPED_WRAPPED_LIMIT_PHRASE_MEASURED,
        STOP_HOOK_STOP_REASON_35_LINES_LIMIT_PHRASE_MEASURED,
        STOP_HOOK_STOP_REASON_60_LINES_LIMIT_PHRASE_MEASURED,
        STOP_HOOK_STOP_REASON_60_LINES_SCROLLBACK_MEASURED,
        WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
    };
    let session = Some((WallKind::UsageSession, Some("3pm".to_string())));
    let read = |r: &[String]| since_limit(r).map(|s| (s.kind, s.reset));
    let phrase = |r: &[String]| {
        r.iter()
            .rposition(|x| x == "  Usage limit reached · resets 3pm")
            .expect("the limit's words")
    };
    let mut whole = screen(STOP_HOOK_STOP_REASON_35_LINES_LIMIT_PHRASE_MEASURED);
    assert_eq!(whole.len(), 49, "the capture's rows");
    whole.push(String::new());
    let first = whole.len() - 40;
    let tail = whole[first..].to_vec();
    assert!(
        whole[..first]
            .iter()
            .any(|r| r.starts_with("⏺ Ran 1 stop hook")),
        "the summary above the tail"
    );
    assert_eq!(read(&whole), None);
    assert_eq!(limit_words_above(&whole), None);
    assert_eq!(
        read(&tail),
        session,
        "the tail alone reads the hook's words"
    );
    let at = phrase(&tail);
    assert_eq!(
        limit_words_above(&tail),
        Some(WordsAbove { row: at, end: at })
    );
    assert_eq!(words_of(&tail, at), WordsOf::Above);
    assert_eq!(words_of(&whole, first + at), WordsOf::Program);
    let screen60 = screen(STOP_HOOK_STOP_REASON_60_LINES_LIMIT_PHRASE_MEASURED);
    let history = screen(STOP_HOOK_STOP_REASON_60_LINES_SCROLLBACK_MEASURED);
    assert_eq!(read(&screen60), session, "the screen alone reads it");
    let at = phrase(&screen60);
    assert_eq!(
        limit_words_above(&screen60),
        Some(WordsAbove { row: at, end: at })
    );
    let owner = history
        .iter()
        .rposition(|r| !r.is_empty() && !r.starts_with(' '))
        .expect("an owner");
    let joined: Vec<String> = history[owner..].iter().chain(&screen60).cloned().collect();
    let above = history.len() - owner;
    assert_eq!(words_of(&joined, above + at), WordsOf::Program);
    assert_eq!(read(&joined), None);
    assert_eq!(limit_words_above(&joined), None);
    let mut worker = joined.clone();
    worker[0] = "⏺ The nightly notes:".to_string();
    assert_eq!(words_of(&worker, above + at), WordsOf::Vendor);
    assert_eq!(read(&worker), session);
    assert_eq!(limit_words_above(&worker), None);
    let wrapped = screen(PROMPT_HOOK_STOPPED_WRAPPED_LIMIT_PHRASE_MEASURED);
    let at = phrase(&wrapped);
    let cut = wrapped[at..].to_vec();
    assert_eq!(read(&cut), session);
    assert_eq!(limit_words_above(&cut), Some(WordsAbove { row: 0, end: 0 }));
    assert_eq!(words_of(&wrapped, at), WordsOf::Program);
    let real = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
    assert!(since_limit(&real).is_some());
    assert_eq!(limit_words_above(&real), None);
}

#[test]
fn a_hooks_words_under_its_own_rows_are_no_wall() {
    use crate::prompt::fixtures::{
        POST_HOOK_BLANK_REASON_THEN_CONTEXT_WALL_MEASURED,
        POST_HOOK_BLOCK_NO_REASON_THEN_CONTEXT_WALL_MEASURED,
        POST_HOOK_BLOCKING_REASON_LOGIN_PHRASE_MEASURED, POST_HOOK_REASON_CONTEXT_PHRASE_MEASURED,
        PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED,
        PROMPT_HOOK_ERROR_NO_STDERR_THEN_TOO_LONG_WALL_MEASURED,
        PROMPT_HOOK_ERROR_STDERR_LOGIN_PHRASE_MEASURED, PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED,
        PROMPT_HOOK_STOPPED_WRAPPED_LIMIT_PHRASE_MEASURED,
        STOP_HOOK_DECISION_BLOCK_THEN_CONTEXT_WALL_MEASURED,
        STOP_HOOK_ERROR_THEN_CONTEXT_WALL_302_MEASURED,
        STOP_HOOK_STOP_REASON_CONTEXT_PHRASE_MEASURED, STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
        STOP_HOOK_STOP_REASON_LOGIN_PHRASE_MEASURED, STOP_HOOK_STOP_REASON_TWO_LINES_MEASURED,
        STOP_HOOK_WORDS_DOUBLE_SPACE_LIMIT_PHRASE_MEASURED, STOP_HOOK_WORDS_SPELL_A_BOX_MEASURED,
        STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_LAST_MEASURED,
        STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_MEASURED, STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED,
    };
    for capture in [
        POST_HOOK_BLANK_REASON_THEN_CONTEXT_WALL_MEASURED,
        POST_HOOK_REASON_CONTEXT_PHRASE_MEASURED,
        POST_HOOK_BLOCKING_REASON_LOGIN_PHRASE_MEASURED,
    ] {
        let said = crate::phase::last_said_notice(&screen(capture)).expect("the hook's place");
        assert!(said.hook_words, "{said:?}");
        assert_eq!(said.placement, Placement::Gutter);
    }
    let said =
        crate::phase::last_said_notice(&screen(STOP_HOOK_ERROR_THEN_CONTEXT_WALL_302_MEASURED))
            .expect("the wall after the hook's rows");
    assert!(!said.hook_words, "{said:?}");
    let mut read = Vec::new();
    for (name, capture) in [
        (
            "post-tool hook, a blank reason, the real wall",
            POST_HOOK_BLANK_REASON_THEN_CONTEXT_WALL_MEASURED,
        ),
        (
            "post-tool hook, the wall's words as its reason",
            POST_HOOK_REASON_CONTEXT_PHRASE_MEASURED,
        ),
        (
            "prompt hook stopped, wrapped",
            PROMPT_HOOK_STOPPED_WRAPPED_LIMIT_PHRASE_MEASURED,
        ),
        ("verbose stop hooks", STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED),
        (
            "verbose stop hooks, a labelled prompt row",
            STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_MEASURED,
        ),
        (
            "verbose stop hooks, a labelled prompt row, the stop reason last",
            STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_LAST_MEASURED,
        ),
        (
            "stop reason, login",
            STOP_HOOK_STOP_REASON_LOGIN_PHRASE_MEASURED,
        ),
        (
            "stop reason, context",
            STOP_HOOK_STOP_REASON_CONTEXT_PHRASE_MEASURED,
        ),
        (
            "stop reason, detailed",
            STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
        ),
        (
            "stop reason, two lines",
            STOP_HOOK_STOP_REASON_TWO_LINES_MEASURED,
        ),
        (
            "hook words, double space",
            STOP_HOOK_WORDS_DOUBLE_SPACE_LIMIT_PHRASE_MEASURED,
        ),
        (
            "post-tool hook reason",
            POST_HOOK_BLOCKING_REASON_LOGIN_PHRASE_MEASURED,
        ),
        (
            "hook words spell a box",
            STOP_HOOK_WORDS_SPELL_A_BOX_MEASURED,
        ),
        (
            "prompt hook stderr",
            PROMPT_HOOK_ERROR_STDERR_LOGIN_PHRASE_MEASURED,
        ),
        (
            "prompt hook blocked",
            PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED,
        ),
        (
            "prompt hook stopped",
            PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED,
        ),
    ] {
        let r = screen(capture);
        if let Some(w) = wall(&r) {
            read.push(format!(
                "{name}: wall {:?} {:?} under_call={} {}",
                w.kind, w.placement, w.under_call, w.message
            ));
        }
        if let Some(s) = since_limit(&r) {
            read.push(format!("{name}: since_limit {s:?}"));
        }
        if let Some(a) = auto_continue_end(&r) {
            read.push(format!("{name}: auto_continue_end {a:?}"));
        }
        if let Some(p) = crate::prompt::parse_prompt(&r) {
            read.push(format!("{name}: a box {p:?}"));
        }
        let phase = worker_phase(&r);
        if phase != Phase::Idle {
            read.push(format!("{name}: {phase:?}"));
        }
    }
    assert!(read.is_empty(), "{read:#?}");
    for (name, capture, under_call, opens) in [
        (
            "after the stop hook's error row",
            STOP_HOOK_ERROR_THEN_CONTEXT_WALL_302_MEASURED,
            false,
            "Context limit reached",
        ),
        (
            "after a stop hook's decision to block, no blank row",
            STOP_HOOK_DECISION_BLOCK_THEN_CONTEXT_WALL_MEASURED,
            false,
            "Context limit reached",
        ),
        (
            "after a stop hook's feedback row, no blank row (2.1.304)",
            crate::prompt::fixtures::STOP_HOOK_FEEDBACK_THEN_CONTEXT_WALL_MEASURED,
            false,
            "Context limit reached",
        ),
        (
            "after a reasonless block's default reason",
            POST_HOOK_BLOCK_NO_REASON_THEN_CONTEXT_WALL_MEASURED,
            true,
            "Context limit reached",
        ),
        (
            "after a non-blocking error's lead row",
            PROMPT_HOOK_ERROR_NO_STDERR_THEN_TOO_LONG_WALL_MEASURED,
            false,
            "Prompt is too long",
        ),
    ] {
        let w = wall(&screen(capture)).unwrap_or_else(|| panic!("the real wall {name}"));
        assert_eq!(
            (w.kind, w.placement, w.under_call),
            (WallKind::Context, Placement::Gutter, under_call),
            "{name}"
        );
        assert!(w.message.starts_with(opens), "{name}: {}", w.message);
    }
}

#[test]
fn a_programs_words_name_no_limit_and_no_continue_however_they_split() {
    use crate::prompt::fixtures::{
        STOP_HOOK_WORDS_DOUBLE_SPACE_LIMIT_PHRASE_MEASURED,
        WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    };
    let measured = screen(STOP_HOOK_WORDS_DOUBLE_SPACE_LIMIT_PHRASE_MEASURED);
    let labelled: Vec<String> = measured
        .iter()
        .filter(|r| {
            !r.starts_with("⏺ Ran 1 stop hook")
                && crate::text::trim(r) != "⎿ \u{a0}note  Usage limit reached · resets 3pm"
        })
        .cloned()
        .collect();
    assert_eq!(labelled.len() + 2, measured.len());
    let cancelled: Vec<String> = labelled
        .iter()
        .map(|r| {
            r.replace(
                "Usage limit reached · resets 3pm",
                "Automatic continue cancelled · /rate-limit-options to re-arm",
            )
        })
        .collect();
    let sonnet = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
    let notice =
        "  ⎿ \u{a0}You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";
    let weekly = notice.replace("Sonnet", "weekly");
    let output: Vec<String> = sonnet
        .iter()
        .flat_map(|r| {
            if crate::text::trim_end(r) == notice {
                vec!["⏺ Bash(cat notes.txt)".to_string(), weekly.clone()]
            } else {
                vec![r.clone()]
            }
        })
        .collect();
    assert_eq!(output.len(), sonnet.len() + 1);

    let mut read = Vec::new();
    if let Some(s) = since_limit(&labelled) {
        read.push(format!("a labelled limit: since_limit {s:?}"));
    }
    if let Some(w) = wall(&labelled) {
        read.push(format!("a labelled limit: wall {:?} {}", w.kind, w.message));
    }
    if let Some(a) = auto_continue_end(&cancelled) {
        read.push(format!("a labelled cancel: auto_continue_end {a:?}"));
    }
    let banner = wall(&output).expect("the generic banner");
    if banner.kind != WallKind::UsageSession {
        read.push(format!(
            "a tool's output names the banner: {:?} {}",
            banner.kind, banner.message
        ));
    }
    if let Some(s) = since_limit(&output) {
        read.push(format!("a tool's output: since_limit {s:?}"));
    }
    assert!(read.is_empty(), "{read:#?}");

    let vendors: Vec<String> = labelled
        .iter()
        .map(|r| r.replace("⎿ \u{a0}Stop says: note  ", "⎿ \u{a0}"))
        .collect();
    let s = since_limit(&vendors).expect("Claude Code's own limit");
    assert_eq!(
        (s.kind, s.reset.as_deref()),
        (WallKind::UsageSession, Some("3pm"))
    );
    let named: Vec<String> = sonnet
        .iter()
        .map(|r| {
            if crate::text::trim_end(r) == notice {
                weekly.clone()
            } else {
                r.clone()
            }
        })
        .collect();
    assert_eq!(wall(&named).map(|w| w.kind), Some(WallKind::UsageWeekly));
}

#[test]
fn the_2_1_301_fable_walls_are_the_model_bucket_with_and_without_a_reset() {
    use crate::prompt::fixtures::{
        FABLE_WARNING_2_1_301, WALL_FABLE_NO_RESET_2_1_301, WALL_FABLE_RESET_2_1_301, screen,
    };
    let bucket = WallKind::ModelBucket { consent: false };
    let w = wall(&screen(WALL_FABLE_RESET_2_1_301)).expect("the Fable wall");
    assert_eq!(
        (w.kind, w.reset.as_deref()),
        (bucket, Some("1:36am (America/Los_Angeles)"))
    );
    let w = wall(&screen(WALL_FABLE_NO_RESET_2_1_301)).expect("the Fable wall, no reset");
    assert_eq!((w.kind, w.reset), (bucket, None));
    let warning = screen(FABLE_WARNING_2_1_301);
    assert_eq!(wall(&warning), None);
    assert_eq!(worker_phase(&warning), Phase::Idle);
}

#[test]
fn context_is_its_own_wall_and_the_quota_notices_are_none() {
    const CONTEXT: &str = "Context limit reached · /compact or /clear to continue";
    let gutter = framed(
        &["⏺ Summarising the run.", &format!("  ⎿  {CONTEXT}"), ""],
        FOOTER,
    );
    let footer = framed(
        &["⏺ Summarising the run.", ""],
        &crate::prompt::fixtures::right_aligned(FOOTER, CONTEXT),
    );
    for r in [&gutter, &footer] {
        let w = wall(r).expect("the context wall");
        assert_eq!(w.kind, WallKind::Context);
        assert_eq!(limit_notice(r), None);
        assert_eq!(worker_phase(r), Phase::Idle);
    }
    const WEEKLY: &str =
        "You've hit your weekly limit · resets Oct 30 at 7pm (America/Los_Angeles)";
    const AUTH: &str = "Not logged in · Please run /login";
    const MEMORY: &str =
        "Critical memory usage (140.4GB) \u{2014} restart and resume with claude --continue";
    for (name, higher, kind) in [
        (
            "footer context over gutter limit",
            CONTEXT,
            WallKind::Context,
        ),
        ("footer auth over gutter limit", AUTH, WallKind::Auth),
    ] {
        let r = framed(
            &[
                "⏺ Summarising the run.",
                &format!("  ⎿  {WEEKLY}"),
                "",
                "✻ Worked for 4s · done 5:47 AM",
                "",
            ],
            &crate::prompt::fixtures::right_aligned(FOOTER, higher),
        );
        let w = wall(&r).expect(name);
        assert_eq!((w.kind, w.placement), (kind, Placement::Footer), "{name}");
        assert_eq!(limit_notice(&r), None, "{name}");
        assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Idle, "{name}");
        assert_eq!(reading.wall.map(|w| w.kind), Some(kind), "{name}");
    }
    let mut with_memory = framed(
        &[
            "⏺ Summarising the run.",
            &format!("  ⎿  {WEEKLY}"),
            "",
            "✻ Worked for 4s · done 5:47 AM",
            "",
        ],
        FOOTER,
    );
    let top = with_memory
        .iter()
        .position(|row| row.starts_with('─') || row.starts_with('╭'))
        .expect("the top rule");
    let rule_width = with_memory[top].chars().count();
    with_memory.insert(top, format!("{MEMORY:>w$}", w = rule_width - 1));
    assert_eq!(wall(&with_memory).map(|w| w.kind), Some(WallKind::Memory));
    assert_eq!(limit_notice(&with_memory), None);
    assert_eq!(worker_phase(&with_memory), Phase::Idle);
    let mut with_shell = with_memory.clone();
    let mode_row = with_shell
        .iter()
        .rposition(|row| row.contains("bypass permissions"))
        .expect("the footer mode row");
    with_shell[mode_row].push_str(" · 1 shell running");
    let shell_reading = crate::reader::read(Some("claude"), &with_shell, None);
    assert_eq!(shell_reading.phase, Phase::Idle);
    assert_eq!(shell_reading.wall.map(|w| w.kind), Some(WallKind::Memory));
    let limit_over_context = framed(
        &[
            "⏺ Summarising the run.",
            &format!("  ⎿  {CONTEXT}"),
            "",
            "✻ Worked for 4s · done 5:47 AM",
            "",
        ],
        &crate::prompt::fixtures::right_aligned(FOOTER, WEEKLY),
    );
    let w = wall(&limit_over_context).expect("footer limit over gutter context");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::UsageWeekly, Placement::Footer)
    );
    assert_eq!(
        limit_notice(&limit_over_context),
        Some((w.message.clone(), w.reset.clone()))
    );
    assert_eq!(
        worker_phase(&limit_over_context),
        Phase::Limited {
            message: w.message,
            reset: w.reset,
        }
    );
    for quota in [
        "Concurrent subagent limit reached. You can run 5 subagents at once. Do not retry.",
        "Budget limit reached ($5.01 spent of the $5.00 maximum). New agents cannot be started.",
        "Fast limit reached and temporarily disabled · resets in 4m",
    ] {
        let r = framed(&["⏺ Fanning out.", &format!("  ⎿  {quota}"), ""], FOOTER);
        assert_eq!(wall(&r), None, "{quota}");
        assert_eq!(worker_phase(&r), Phase::Idle, "{quota}");
    }
    for (name, capture, notice) in [
        (
            "the wall",
            CONTEXT_WALL_MEASURED,
            Some("Context limit reached · /compact or /clear to continue"),
        ),
        (
            "a failed /compact",
            COMPACT_FAILED_FIRST_MEASURED,
            Some("Compaction failed · conversation could not be reduced below the context limit"),
        ),
        (
            "the third failure in a row",
            COMPACT_FAILED_3_MEASURED,
            Some("Compaction failed · conversation could not be reduced below the context limit"),
        ),
        (
            "a /compact whose request failed, wrapped",
            COMPACT_ERROR_500_MEASURED,
            Some("Error during compaction: API Error: 500 Internal server error."),
        ),
        ("a compaction that worked", COMPACT_SUCCEEDED_MEASURED, None),
        ("nothing to compact", COMPACT_NOT_ENOUGH_MEASURED, None),
        (
            "the wall with `/compact` typed and the slash menu open (2.1.292)",
            CONTEXT_WALL_COMPACT_MENU_MEASURED,
            Some("Context limit reached · /compact or /clear to continue"),
        ),
    ] {
        let r = screen(capture);
        let found = wall(&r);
        match (notice, found) {
            (Some(head), Some(w)) => {
                assert_eq!(
                    (w.kind, w.placement),
                    (WallKind::Context, Placement::Gutter),
                    "{name}"
                );
                assert!(w.message.starts_with(head), "{name}: {}", w.message);
            }
            (None, None) => {}
            (want, got) => panic!("{name}: wanted {want:?}, read {got:?}"),
        }
        assert_eq!(limit_notice(&r), None, "{name}");
        assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
    }
    for (text, kind) in [
        (
            "Error during compaction: Invalid API key · Fix external API key",
            WallKind::Auth,
        ),
        (
            "Error during compaction: Credit balance is too low",
            WallKind::Spend,
        ),
        (
            "Error during compaction: API Error: Connection to the API was lost (ECONNRESET).",
            WallKind::Context,
        ),
    ] {
        assert_eq!(classify_wall(text), Some(kind), "{text}");
    }
}

#[test]
fn the_memory_banner_is_read_by_its_place() {
    use crate::prompt::fixtures::{MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE};
    let r = screen(MEMORY_BANNER_BUSY);
    let w = memory_wall(&r).expect("the banner");
    assert_eq!(w.kind, WallKind::Memory);
    assert_eq!(w.placement, Placement::Banner);
    assert_eq!(w.reset, None);
    assert!(w.message.starts_with(crate::anchor!("wall.memory")));
    assert!(w.message.ends_with("claude --continue"), "{}", w.message);
    let mut shared = r.clone();
    let spinner = w.row - 1;
    let pad = 142 - shared[spinner].chars().count() - w.message.chars().count();
    shared[spinner] = format!("{}{}{}", shared[spinner], " ".repeat(pad), w.message);
    shared[w.row] = String::new();
    let s = memory_wall(&shared).expect("beside the spinner");
    assert_eq!((s.row, s.message.as_str()), (spinner, w.message.as_str()));
    let idle = memory_wall(&screen(MEMORY_BANNER_IDLE)).expect("idle");
    assert_eq!(idle.message, w.message);
    assert_eq!(wall(&r).map(|w| w.kind), Some(WallKind::Memory));
}

fn narrowed(rows: &[String], at: usize, width: usize, banner: &str) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            if i == at {
                format!("{banner:>w$}", w = width - 2)
            } else if row.starts_with('─') {
                "─".repeat(width)
            } else {
                row.chars().take(width).collect()
            }
        })
        .collect()
}

fn to_edge(text: &str, width: usize) -> String {
    let mut row = format!("{text}, it said, so the run stops here");
    while row.chars().count() < width - 2 {
        row.push_str(" and");
    }
    row.chars().take(width - 2).collect()
}

#[test]
fn the_memory_banner_reads_at_every_width_under_a_spinner() {
    use crate::prompt::fixtures::MEMORY_BANNER_BUSY;
    let wide = screen(MEMORY_BANNER_BUSY);
    let at = memory_wall(&wide).expect("the control").row;
    let banner = wide[at].trim_start().to_string();
    for width in 77..=144 {
        let r = narrowed(&wide, at, width, &banner);
        let w = memory_wall(&r).unwrap_or_else(|| panic!("{width} columns"));
        assert_eq!((w.row, w.message.as_str()), (at, banner.as_str()));
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Busy, "{width} columns");
        assert_eq!(
            reading.wall.map(|w| w.kind),
            Some(WallKind::Memory),
            "{width} columns"
        );
    }
}

#[test]
fn a_queued_message_quoting_the_memory_banner_is_not_a_wall() {
    use crate::prompt::fixtures::MEMORY_BANNER_BUSY;
    let wide = screen(MEMORY_BANNER_BUSY);
    let at = memory_wall(&wide).expect("the control").row;
    let banner = wide[at].trim_start().to_string();
    for width in [80, 144] {
        let r = narrowed(&wide, at, width, &banner);
        let quote = to_edge(&format!("  {banner}"), width);
        for message in [
            vec!["❯ the tab above says".to_string(), quote.clone()],
            vec![
                "❯ the tab above has said for an hour now that it will not".to_string(),
                "  read a key, and the banner under its spinner says".to_string(),
                quote.clone(),
            ],
        ] {
            let mut queued = r.clone();
            queued.splice(at..=at, message.iter().cloned());
            assert_eq!(memory_wall(&queued), None, "{width}: {message:?}");
            let reading = crate::reader::read(Some("claude"), &queued, None);
            assert_eq!(reading.phase, Phase::Busy, "{width}");
            assert_eq!(reading.wall, None, "{width}: {message:?}");
            let mut both = r.clone();
            both.splice(at + 1..at + 1, message.iter().cloned());
            assert_eq!(
                memory_wall(&both).map(|w| w.row),
                Some(at),
                "{width}: the banner over it"
            );
        }
    }
}

#[test]
fn a_quoted_memory_banner_is_not_a_wall() {
    use crate::prompt::fixtures::{MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE};
    let r = screen(MEMORY_BANNER_BUSY);
    let at = memory_wall(&r).expect("the control").row;
    let banner = r[at].trim_start().to_string();
    let with = |row: String| {
        let mut v = r.clone();
        v[at] = row;
        v
    };
    let right_aligned = |text: &str| format!("{text:>142}");
    let no_size = format!("{} is back to normal", crate::anchor!("wall.memory"));
    assert_eq!(
        memory_wall(&with(right_aligned(&banner))).map(|w| w.row),
        Some(at)
    );
    let blank = with(String::new());
    let idle = screen(MEMORY_BANNER_IDLE);
    let at_idle = memory_wall(&idle).expect("the idle control").row;
    let ending = |tail: &[&str]| {
        let mut v = idle.clone();
        v.splice(at_idle..=at_idle, tail.iter().map(|row| row.to_string()));
        v
    };
    let edge = |text: String| to_edge(&text, 144);
    let (message, paragraph, output, continued) = (
        edge(format!("⏺ {banner}")),
        edge(format!("  {banner}")),
        edge(format!("  ⎿  {banner}")),
        edge(format!("     {banner}")),
    );
    let cases = [
        ("blanked: only the draft's quote", blank),
        (
            "indented, ending short of the edge",
            with(format!("{}{banner}", " ".repeat(40))),
        ),
        ("the words with no size", with(right_aligned(&no_size))),
        ("idle: the worker's message", ending(&[&message])),
        (
            "idle: a paragraph of the worker's message",
            ending(&["⏺ The banner said:", "", &paragraph]),
        ),
        (
            "idle: a tool's output",
            ending(&["⏺ Bash(tail -1 run.log)", &output]),
        ),
        (
            "idle: a tool output's continuation row",
            ending(&[
                "⏺ Bash(tail -2 run.log)",
                "  ⎿  the run stopped:",
                &continued,
            ]),
        ),
    ];
    for (name, v) in &cases {
        assert_eq!(memory_wall(v), None, "{name}");
        assert_eq!(wall(v), None, "{name}");
    }
    let mut above = r.clone();
    above[at] = String::new();
    above.insert(1, r[at].clone());
    assert_eq!(memory_wall(&above), None, "above the status row");
    let unframed: Vec<String> = r
        .iter()
        .filter(|row| !row.starts_with('─'))
        .cloned()
        .collect();
    assert_eq!(memory_wall(&unframed), None, "no frame");
}

#[test]
fn only_the_usage_kinds_and_a_rate_limit_read_limited() {
    assert!(WallKind::UsageSession.reads_limited());
    assert!(WallKind::ModelBucket { consent: true }.reads_limited());
    assert!(
        WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        }
        .reads_limited()
    );
    assert!(!WallKind::Overloaded.reads_limited());
    assert!(!WallKind::Context.reads_limited());
    assert!(!WallKind::Auth.reads_limited());
    assert!(!WallKind::Memory.reads_limited());
    assert!(
        !WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server,
        }
        .reads_limited()
    );
}

const ENOTFOUND: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
const UNREACHABLE: WallKind = WallKind::ApiError {
    code: None,
    retryable: true,
    cause: ApiCause::Unreachable,
};

#[test]
fn a_283_api_error_message_row_is_a_wall_at_every_width() {
    for (name, r) in [
        ("144 measured", screen(API_ERROR_ENOTFOUND_MEASURED)),
        ("80 measured", screen(API_ERROR_ENOTFOUND_80_MEASURED)),
        ("two errors measured", screen(API_ERROR_TWICE_MEASURED)),
    ] {
        assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
        let w = wall(&r).unwrap_or_else(|| panic!("{name}: the wall"));
        assert_eq!(w.kind, UNREACHABLE, "{name}");
        assert_eq!(w.placement, Placement::ErrorRow, "{name}");
        assert_eq!(w.message, ENOTFOUND, "{name}");
        assert!(r[w.row].starts_with("⏺ API Error"), "{name}");
    }
    let twice = screen(API_ERROR_TWICE_MEASURED);
    let last = twice
        .iter()
        .rposition(|r| r.starts_with("⏺ API Error"))
        .expect("the second error");
    assert_eq!(wall(&twice).map(|w| w.row), Some(last));
    let sent = screen(API_ERROR_QUEUED_SENT_MEASURED);
    assert_eq!(worker_phase(&sent), Phase::Busy);
    assert_eq!(wall(&sent), None);
    let variants = [
        framed(&["⏺ Working.", "", &format!("⏺ {ENOTFOUND}"), ""], FOOTER),
        framed(&["● Working.", "", &format!("● {ENOTFOUND}"), ""], FOOTER),
        framed(
            &[
                "⏺ Working.",
                "",
                &format!("⏺ {ENOTFOUND}"),
                "  (ctrl+o to expand)",
                "",
            ],
            FOOTER,
        ),
    ];
    for r in &variants {
        let w = wall(r).expect("the wall");
        assert_eq!((w.kind, w.message.as_str()), (UNREACHABLE, ENOTFOUND));
    }
}

#[test]
fn the_workers_prose_and_history_are_not_a_message_wall() {
    let error = format!("⏺ {ENOTFOUND}");
    let none: Vec<(&str, Vec<String>)> = vec![
        (
            "overloaded prose",
            framed(
                &["⏺ The server was overloaded, so I retried the push.", ""],
                FOOTER,
            ),
        ),
        (
            "high load prose",
            framed(
                &["⏺ Fixed the high load path in the scheduler.", ""],
                FOOTER,
            ),
        ),
        (
            "login prose",
            framed(&["⏺ Please run /login when you are back.", ""], FOOTER),
        ),
        (
            "context prose",
            framed(
                &["⏺ Context limit reached in the fixture was the bug.", ""],
                FOOTER,
            ),
        ),
        (
            "lowercase",
            framed(&["⏺ api error: can't reach the api server", ""], FOOTER),
        ),
        ("user row", framed(&[&format!("❯ {ENOTFOUND}"), ""], FOOTER)),
        (
            "indented",
            framed(
                &["⏺ Bash(cat log)", &format!("  ⏺ {ENOTFOUND}"), ""],
                FOOTER,
            ),
        ),
        (
            "keep going after it",
            framed(&[&error, "", "❯ keep going", ""], FOOTER),
        ),
        (
            "later words",
            framed(&[&error, "", "⏺ Back online; carrying on.", ""], FOOTER),
        ),
        (
            "a tool call after it",
            framed(
                &[&error, "", "⏺ Bash(git status)", "  ⎿  clean", ""],
                FOOTER,
            ),
        ),
        (
            "two paragraphs",
            framed(
                &[
                    "⏺ API Error: 529 Overloaded is what CI hit last night.",
                    "",
                    "  I re-ran it and it passed.",
                    "",
                ],
                FOOTER,
            ),
        ),
        (
            "aborted",
            framed(&["⏺ API Error: Request was aborted.", ""], FOOTER),
        ),
    ];
    for (name, r) in none {
        assert_eq!(wall(&r), None, "{name}");
    }
    let under = framed(
        &[
            &error,
            "  ⎿  Context limit reached · /compact or /clear to continue",
            "",
        ],
        FOOTER,
    );
    let w = wall(&under).expect("the gutter's notice");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::Context, Placement::Gutter)
    );
}

#[test]
fn every_api_error_family_of_the_283_catalog_classifies() {
    let unreachable = [
        "Can't reach the API server — check your internet or DNS (ENOTFOUND)",
        "Can't reach the API server — check your internet or DNS (EAI_AGAIN)",
        "Can't reach the API server — check your internet or DNS (FailedToOpenSocket)",
        "No internet route — check your connection or VPN (ENETUNREACH)",
        "No internet route — check your connection or VPN (EHOSTDOWN)",
        "Unable to connect to API. Check your internet connection",
        "Unable to connect to API (ECONNABORTED)",
        "Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED)",
        "Connection dropped (ECONNRESET)",
        "Connection dropped (UND_ERR_SOCKET)",
        "Request timed out. Check your internet connection and proxy settings",
        "No response from API",
        "Connection lost while your computer was asleep",
        "Connection closed before the response finished",
        "Unable to connect to API: SSL error (ERR_TLS_HANDSHAKE_TIMEOUT)",
        "Unable to connect to API: SSL error (ERR_SSL_WRONG_VERSION_NUMBER)",
    ];
    let cut_off = [
        "The response stopped arriving. The response above may be incomplete.",
        "Server error mid-response. The response above may be incomplete.",
        "The response stream was malformed. The response above may be incomplete.",
        "Part of the response never arrived. The response above may be incomplete.",
        "Your computer went to sleep mid-response. The response above may be incomplete.",
        "Connection lost mid-response. The response above may be incomplete.",
        "The response stalled before a response was produced. Try again.",
        "The response stream was malformed and no response was produced. Try again.",
        "Part of the response never arrived and no response was produced. Try again.",
        "Your computer went to sleep before a response was produced. Try again.",
        "Connection lost before a response was produced. Try again.",
        "An image in the conversation could not be processed and was removed. Re-read the file with a different approach if you still need it.",
    ];
    let overlong = [
        "Claude's response exceeded the 32000 output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.",
        "Claude's response exceeded the 32000 output token maximum for a request sent without streaming. The CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable cannot raise this limit.",
    ];
    let config = [
        "Unable to connect to API: SSL certificate verification failed",
        "Unable to connect to API: SSL certificate has expired",
        "Unable to connect to API: SSL certificate has been revoked",
        "Unable to connect to API: SSL certificate hostname mismatch",
        "Unable to connect to API: SSL certificate is not yet valid",
        "Unable to connect to API: Self-signed certificate detected",
        "Unable to connect to API (SELF_SIGNED_CERT_IN_CHAIN). The certificate comes from an authority Claude Code doesn't trust",
        "Couldn't connect through your proxy (ERR_PROXY_TUNNEL) — the proxy refused the tunnel: check its credentials and that it allows this host",
    ];
    for (texts, cause, retryable) in [
        (&unreachable[..], ApiCause::Unreachable, true),
        (&cut_off[..], ApiCause::CutOff, true),
        (&overlong[..], ApiCause::Overlong, true),
        (&config[..], ApiCause::Config, false),
    ] {
        for text in texts {
            let text = format!("API Error: {text}");
            assert_eq!(
                classify_wall(&text),
                Some(WallKind::ApiError {
                    code: None,
                    retryable,
                    cause,
                }),
                "{text}"
            );
        }
    }
    assert_eq!(
        classify_wall("API Error: 503 Service unavailable. Connection refused upstream"),
        Some(WallKind::ApiError {
            code: Some(503),
            retryable: true,
            cause: ApiCause::Server,
        }),
        "a status is the server's, whatever words follow"
    );
    assert_eq!(
        classify_wall(
            "API Error: Request rejected (429) · this may be a temporary capacity issue."
        ),
        Some(WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        }),
        "2.1.283's own 429"
    );
    assert_eq!(
        classify_wall("API Error: The model has reached its context window limit."),
        Some(WallKind::Context)
    );
    for text in [
        "API Error: Claude Opus 5.5 can't help with this. Start a new session to continue.",
        "API Error (claude-x-1): The provided model identifier is invalid. Run /model to pick a different model.",
        "API Error: Effort 'max' isn't available with thinking turned off on this model · run /effort high to continue",
    ] {
        assert_eq!(
            classify_wall(text),
            Some(WallKind::ApiError {
                code: None,
                retryable: false,
                cause: ApiCause::Request,
            }),
            "{text}"
        );
    }
    for text in [
        "API Error (claude-x-1): The model claude-x-1 is not available.",
        "API Error: Effort 'max' isn't available for this model.",
    ] {
        assert_eq!(
            classify_wall(text),
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            }),
            "{text}"
        );
    }
}

#[test]
fn a_refused_request_is_its_own_wall_and_an_unnamed_api_error_the_servers() {
    let refused = |code| {
        Some(WallKind::ApiError {
            code,
            retryable: false,
            cause: ApiCause::Request,
        })
    };
    for text in [
        "API Error: Claude can't help with this. Start a new session to continue. Send feedback with /feedback or learn more: https://www.anthropic.com/legal/aup",
        "API Error: Opus 4.1's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.",
        "API Error: Claude Opus 5.5's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.",
        "API Error: Effort 'max' isn't available with thinking turned off on this model · run /effort high to continue, or turn thinking back on (unset MAX_THINKING_TOKENS=0)",
        "API Error: advisor tool is not supported · The configured advisor model is not compatible with this request model — run /advisor to change or disable the advisor",
        "API Error: Verify your identity to keep using Claude: https://claude.ai/verify",
        "API Error: Claude Code 2.1.200 does not support this model; version 2.1.250 or newer is required. Run 'claude update', or update the Claude desktop app, then try again.",
        "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.",
        "The model claude-opus-9 is not available on your Bedrock deployment. Try switching to opus, or ask your admin to enable this model.",
        "Your account is on hold and can't use Claude Code. View details or appeal: https://claude.ai/restricted",
        "Claude Opus is not available with the Claude Pro plan. If you have updated your subscription plan recently, run /logout and /login for the plan to take effect.",
    ] {
        assert_eq!(classify_wall(text), refused(None), "{text}");
    }
    for (text, code) in [
        (
            "API Error: 400 due to tool use concurrency issues. Run /rewind to recover the conversation.",
            400,
        ),
        (
            "API Error: 400 orphaned tool_result in conversation history. Run /rewind to recover the conversation.",
            400,
        ),
        (
            "API Error: 400 Request blocked by your organization's data loss prevention policy.",
            400,
        ),
        ("API Error: 400 This organization has been disabled.", 400),
        ("API Error: 403 Forbidden", 403),
        ("API Error: 404 Not Found", 404),
        ("API Error: 413 Request too large", 413),
        ("API Error: 422 Unprocessable Entity", 422),
    ] {
        assert_eq!(classify_wall(text), refused(Some(code)), "{text}");
    }
    let server = |code, retryable| {
        Some(WallKind::ApiError {
            code,
            retryable,
            cause: ApiCause::Server,
        })
    };
    for text in [
        "API Error: Please wait a moment and try again.",
        "API Error: terminated",
        "API Error: fetch failed",
        "API Error",
    ] {
        assert_eq!(classify_wall(text), server(None, true), "{text}");
    }
    for (text, code, retryable) in [
        ("API Error: 401 Unauthorized", 401, false),
        ("API Error: 408 Request Timeout", 408, true),
        ("API Error: 409 Conflict", 409, true),
        ("API Error: 451 Unavailable For Legal Reasons", 451, false),
        ("API Error: 500 Internal server error", 500, true),
    ] {
        assert_eq!(classify_wall(text), server(Some(code), retryable), "{text}");
    }
    for text in [
        "API Error: Request was aborted.",
        "I hit API Error: 400 earlier in the logs, so I patched the retry",
        "The tests check that the advisor model is not compatible with Haiku.",
        "Fixed the parser so it reads an account that is on hold as a wall.",
        "Invalid advisor model: claude-haiku-9",
    ] {
        assert_eq!(classify_wall(text), None, "{text}");
    }
}

#[test]
fn a_reading_by_the_status_alone_is_told_from_one_the_words_make() {
    for text in [
        "API Error: 403 Forbidden",
        "API Error: 401 Unauthorized",
        "API Error: 404 Not Found",
        "API Error: 500 Internal server error",
        r#"API Error: 403 {"type":"error","error":{"type":"permission_error","message":"OAuth token has been revoked. Please obtain a new token."}}"#,
    ] {
        assert!(named_by_status_alone(text), "{text}");
    }
    for text in [
        "API Error: 400 due to tool use concurrency issues. Run /rewind to recover the conversation.",
        "API Error: 403 Request blocked by your organization's data loss prevention policy.",
        "API Error: 401 Invalid API key · Please run /login",
        "API Error: 429 The request was rate limited",
        "API Error: terminated",
        "API Error: Claude can't help with this. Start a new session to continue.",
        "You've hit your session limit · resets 3pm (America/Los_Angeles)",
        "Failed to authenticate. API Error: 403 Forbidden",
    ] {
        assert!(!named_by_status_alone(text), "{text}");
    }
}

#[test]
fn the_refusals_and_the_unnamed_api_errors_read_on_the_screen() {
    let row = |text: &str| framed(&["⏺ Working.", "", &format!("⏺ {text}"), ""], FOOTER);
    for (text, kind) in [
        (
            "API Error: Claude can't help with this. Start a new session to continue.",
            WallKind::ApiError {
                code: None,
                retryable: false,
                cause: ApiCause::Request,
            },
        ),
        (
            "API Error: 400 due to tool use concurrency issues. Run /rewind to recover the conversation.",
            WallKind::ApiError {
                code: Some(400),
                retryable: false,
                cause: ApiCause::Request,
            },
        ),
        (
            "API Error: Please wait a moment and try again.",
            WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            },
        ),
        (
            "API Error: Usage credits required for 1M context · run /model to switch to standard context",
            WallKind::Spend,
        ),
    ] {
        let w = wall(&row(text)).unwrap_or_else(|| panic!("{text}: the wall"));
        assert_eq!((w.kind, w.placement), (kind, Placement::ErrorRow), "{text}");
        assert_eq!(w.message, text);
    }
    let high = framed(
        &[
            "⏺ Running the suites.",
            "  ⎿  We are experiencing high demand for Opus 4.",
            "",
        ],
        FOOTER,
    );
    let w = wall(&high).expect("the high demand row");
    assert_eq!(
        (w.kind, w.placement),
        (WallKind::Overloaded, Placement::Gutter)
    );
    for r in [
        framed(
            &[
                "⏺ I hit API Error: 400 earlier in the logs, so I patched the retry.",
                "",
            ],
            FOOTER,
        ),
        framed(
            &[
                "⏺ Running the suites.",
                "  ⎿  Switched to Sonnet due to high demand for Opus",
                "",
            ],
            FOOTER,
        ),
    ] {
        assert_eq!(wall(&r), None, "{r:?}");
    }
}

#[test]
fn the_last_thing_said_is_given_where_the_vendor_draws_a_notice() {
    use crate::phase::last_said_notice;
    let hold = "Your account is on hold and can't use Claude Code. View details or appeal: https://claude.ai/restricted";
    let r = framed(&["⏺ Working.", "", &format!("⏺ {hold}"), ""], FOOTER);
    let said = last_said_notice(&r).expect("the hold row");
    assert_eq!((said.text.as_str(), said.shaped), (hold, false));
    let wrapped = framed(
        &[
            "⏺ Working.",
            "",
            "⏺ Your account is on hold and can't use",
            "  Claude Code. View details or appeal:",
            "  https://claude.ai/restricted",
            "",
        ],
        FOOTER,
    );
    assert_eq!(
        last_said_notice(&wrapped).map(|s| s.text),
        Some(hold.to_string())
    );
    let api = framed(&["⏺ Working.", "", "⏺ API Error: terminated", ""], FOOTER);
    let said = last_said_notice(&api).expect("the error row");
    assert_eq!(
        (said.text.as_str(), said.shaped),
        ("API Error: terminated", true)
    );
    let gutter = framed(
        &["❯ /compact", "  ⎿  Not enough messages to compact.", ""],
        FOOTER,
    );
    let said = last_said_notice(&gutter).expect("the gutter row");
    assert_eq!(
        (said.text.as_str(), said.shaped),
        ("Not enough messages to compact.", false)
    );
    for (name, r) in [
        (
            "a call's output",
            framed(&["⏺ Bash(cat x)", "  ⎿  API Error: terminated", ""], FOOTER),
        ),
        ("a call", framed(&["⏺ Bash(ls)", ""], FOOTER)),
        (
            "the person's row",
            framed(&["❯ API Error: terminated", ""], FOOTER),
        ),
        (
            "said before the last",
            framed(&["⏺ API Error: terminated", "", "❯ keep going", ""], FOOTER),
        ),
    ] {
        assert_eq!(last_said_notice(&r), None, "{name}");
    }
}

#[test]
fn request_timed_out_under_the_gutter_is_unreachable() {
    let r = framed(
        &["⏺ Checking the suite.", "  ⎿  Request timed out", ""],
        FOOTER,
    );
    let w = wall(&r).expect("the wall");
    assert_eq!((w.kind, w.placement), (UNREACHABLE, Placement::Gutter));
    let tool = framed(
        &["⏺ Bash(curl -m1 x)", "  ⎿  Request timed out", ""],
        FOOTER,
    );
    assert_eq!(wall(&tool), None);
    let longer = framed(
        &[
            "⏺ Checking the suite.",
            "  ⎿  Request timed out. Retrying the fetch.",
            "",
        ],
        FOOTER,
    );
    assert_eq!(wall(&longer), None, "only the whole row");
}

#[test]
fn usage_credits_and_the_vendors_warnings_are_no_wall() {
    for notice in [
        "You're now using usage credits",
        "You're now using usage credits · Your session limit resets 9:43am (America/Los_Angeles)",
        "Now using usage credits",
        "Now using usage credits for Opus · Your Opus limit resets 4pm",
        "You're now using your usage allocation",
        "Now using your usage allocation",
        "You're now using extra usage",
        "Now using extra usage",
        "Fable 5 is drawing from usage credits",
        "Opus is now using usage credits instead of your plan limits",
        "You've used 92% of your session limit · resets 4:12am (America/Los_Angeles)",
        "You've used 80% of your weekly limit · resets Oct 18 at 8:50pm (America/Los_Angeles)",
        "You're close to your usage credit limit",
    ] {
        assert_eq!(classify_wall(notice), None, "{notice}");
    }
    assert_eq!(
        classify_wall("Opus 4.1 now uses usage credits"),
        Some(WallKind::ModelBucket { consent: false })
    );
    assert_eq!(
        classify_wall("You're out of usage credits. Switch to another model"),
        Some(WallKind::Spend)
    );
}

#[test]
fn off_macos_a_line_of_the_models_drawn_with_a_dot_is_its_reply() {
    use crate::prompt::fixtures::{WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED, screen};
    let dotted = |rows: Vec<String>| {
        rows.into_iter()
            .map(|r| r.replace('⏺', "●"))
            .collect::<Vec<_>>()
    };
    let turned = dotted(screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED));
    let read = since_limit(&turned).expect("the limit");
    assert_eq!(
        (read.served, read.reset.as_deref()),
        (true, Some("10:38am"))
    );
    let unanswered: Vec<String> = turned
        .iter()
        .filter(|r| !r.starts_with("● Hi!"))
        .cloned()
        .collect();
    let read = since_limit(&unanswered).expect("the limit");
    assert_eq!(
        (read.served, read.reset.as_deref()),
        (false, Some("10:38am"))
    );
}

#[test]
fn the_login_expiry_notice_is_read_where_a_notification_is_drawn_and_only_there() {
    const CAPTURE: &str = include_str!("fixtures/idle-after-limit-and-model-switch.txt");
    let rows: Vec<String> = CAPTURE.lines().map(str::to_string).collect();
    let at = rows
        .iter()
        .position(|r| r.contains("Update installed"))
        .expect("the measured notification row");
    let notice = |words: &str| {
        let mut r = rows.clone();
        r[at] = format!("{:>136}", words);
        r
    };
    assert_eq!(login_expires_in(&rows), None, "the capture as measured");
    for (words, days) in [
        (
            "\u{26a0} Your login expires in 2 days \u{b7} run /login to renew",
            2,
        ),
        ("Your login expires in 1 day \u{b7} run /login to renew", 1),
        ("\u{26a0} Your login expires in 3 days", 3),
    ] {
        assert_eq!(login_expires_in(&notice(words)), Some(days), "{words}");
    }
    let mut footer = rows.clone();
    let last = footer.len() - 1;
    footer[last] = format!(
        "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle){:>90}",
        "Your login expires in 1 day \u{b7} run /login to renew"
    );
    assert_eq!(login_expires_in(&footer), Some(1));
    let quoted = |row: String| {
        let mut r = rows.clone();
        r.insert(at, row);
        r
    };
    for row in [
        "\u{23fa} Your login expires in 2 days \u{b7} run /login to renew".to_string(),
        "  Your login expires in 2 days \u{b7} run /login to renew".to_string(),
        "  \u{23bf}  Your login expires in 2 days \u{b7} run /login to renew".to_string(),
        "\u{23fa} Bash(echo 'Your login expires in 2 days \u{b7} run /login to renew')".to_string(),
    ] {
        assert_eq!(login_expires_in(&quoted(row.clone())), None, "{row}");
    }
    for words in [
        "Note: Your login expires in 2 days \u{b7} run /login to renew",
        "Your login expires in two days \u{b7} run /login to renew",
        "Your login expires in 2 weeks",
        "Your login expires in 2 days, they say",
    ] {
        assert_eq!(login_expires_in(&notice(words)), None, "{words}");
    }
    let mut drafted = rows.clone();
    let caret = drafted
        .iter()
        .rposition(|r| r.starts_with('\u{276f}'))
        .expect("the caret row");
    drafted[caret] = "\u{276f} Your login expires in 2 days \u{b7} run /login to renew".to_string();
    assert_eq!(login_expires_in(&drafted), None, "a draft is the person's");
}

#[test]
fn a_generic_row_with_no_notice_of_its_turn_names_no_limit() {
    use crate::prompt::fixtures::{
        SONNET_BUCKET_UNDER_24H_CANCELLED_THEN_MODEL_18_ROWS_2_1_302,
        WALL_SESSION_RESUMED_BY_ITSELF_MEASURED,
        WALL_SONNET_BUCKET_ARMED_UNDER_24H_15_ROWS_2_1_302,
        WALL_SONNET_BUCKET_RESUMED_BY_ITSELF_UNDER_24H_2_1_302,
        WALL_SONNET_BUCKET_RESUMED_BY_ITSELF_UNDER_24H_15_ROWS_2_1_302, screen,
    };
    let bucket = WallKind::ModelBucket { consent: false };
    let fired = screen(WALL_SONNET_BUCKET_RESUMED_BY_ITSELF_UNDER_24H_15_ROWS_2_1_302);
    assert!(
        fired[0].starts_with("⏺ Usage limit reached")
            && !fired.iter().any(|r| r.contains("Sonnet limit")),
        "the generic row on top, the bucket's notice scrolled off: {fired:#?}"
    );
    assert_eq!(auto_continue_end(&fired), Some(AutoContinue::Fired));
    assert_eq!(
        since_limit(&fired),
        Some(SinceLimit {
            kind: WallKind::UsageSession,
            reset: Some("1:56am".to_string()),
            served: false,
            fired: true,
            model_set: false,
            chosen_model: None,
            generic: true,
        })
    );
    let armed = screen(WALL_SONNET_BUCKET_ARMED_UNDER_24H_15_ROWS_2_1_302);
    let read = since_limit(&armed).expect("the armed bucket's notice");
    assert_eq!(
        (read.kind, read.reset.as_deref(), read.generic),
        (bucket, Some("1:56am"), false)
    );
    let tall = screen(WALL_SONNET_BUCKET_RESUMED_BY_ITSELF_UNDER_24H_2_1_302);
    let read = since_limit(&tall).expect("the taller pane's notice");
    assert_eq!((read.kind, read.generic), (bucket, false));
    let session = screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED);
    let read = since_limit(&session).expect("the session limit's notice");
    assert_eq!((read.kind, read.generic), (WallKind::UsageSession, false));
    let chosen = screen(SONNET_BUCKET_UNDER_24H_CANCELLED_THEN_MODEL_18_ROWS_2_1_302);
    assert!(
        chosen[0].starts_with("⏺ Usage limit reached")
            && !chosen.iter().any(|r| r.contains("Sonnet limit")),
        "the generic row on top, the bucket's notice scrolled off: {chosen:#?}"
    );
    assert_eq!(
        since_limit(&chosen),
        Some(SinceLimit {
            kind: WallKind::UsageSession,
            reset: Some("2:25am".to_string()),
            served: false,
            fired: false,
            model_set: true,
            chosen_model: Some("Opus 5.5 and saved as your default for new sessions".to_string()),
            generic: true,
        })
    );
}

#[test]
fn the_limit_a_cancel_left_under_the_limit_dialog_is_that_limit() {
    use crate::prompt::fixtures::{WALL_SONNET_BUCKET_CANCELLED_OVER_24H_2_1_302, screen};
    let bucket = WallKind::ModelBucket { consent: false };
    let cancelled = screen(WALL_SONNET_BUCKET_CANCELLED_OVER_24H_2_1_302);
    let notice = cancelled
        .iter()
        .position(|r| r.contains("You've hit your Sonnet limit"))
        .expect("the bucket's notice");
    let dialog = cancelled
        .iter()
        .position(|r| r == "❯ /rate-limit-options")
        .expect("the dialog's own command");
    let cancel = cancelled
        .iter()
        .position(|r| r.starts_with("⏺ Automatic continue cancelled"))
        .expect("the cancel");
    assert!(notice < dialog && dialog < cancel, "{cancelled:#?}");
    assert_eq!(wall(&cancelled), None);
    assert_eq!(auto_continue_end(&cancelled), Some(AutoContinue::Unkept));
    let since = since_limit(&cancelled).expect("the bucket's notice");
    assert_eq!(
        (since.kind, since.served, since.generic),
        (bucket, false, false)
    );
    let left = left_wall(&cancelled).expect("the cancel's limit");
    assert_eq!(
        (left.kind, left.reset.as_deref(), left.row, left.placement),
        (
            bucket,
            Some("Oct 19 at 2:25am (America/Los_Angeles)"),
            notice,
            Placement::Gutter
        ),
        "{left:?}"
    );
    let mut usage = cancelled.clone();
    usage.splice(
        dialog..cancel - 1,
        ["❯ /usage", "  ⎿  Status dialog dismissed"].map(str::to_string),
    );
    assert_eq!(left_wall(&usage), None, "a person's /usage in its place");
    let mut after = cancelled.clone();
    let rows: Vec<String> = after.drain(dialog..cancel).collect();
    let cancel = after
        .iter()
        .position(|r| r.starts_with("⏺ Automatic continue cancelled"))
        .expect("the cancel");
    after.splice(
        cancel + 1..cancel + 1,
        std::iter::once(String::new()).chain(rows),
    );
    assert_eq!(left_wall(&after), None, "the dialog after the cancel");
}

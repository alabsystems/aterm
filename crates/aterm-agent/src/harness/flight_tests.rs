// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the flight fold and mapping ([`super`]).

use super::*;
use aterm_phase::agents_panel::parse_row;
use std::path::Path;

/// A measured transcript of `testdata/flight/`, its rows (the header left out).
fn rows(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/testdata/flight")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A measured screen of `aterm-phase`'s fixtures, row by row.
fn screen(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The fold of `rows`.
fn fold(rows: &[String]) -> FlightFold {
    let mut fold = FlightFold::new();
    for row in rows {
        fold.fold_bytes(row.as_bytes());
    }
    fold
}

/// Unix ms of an RFC 3339 stamp.
fn ms(stamp: &str) -> i64 {
    crate::harness::codex_rollout::stamp_ms(stamp).unwrap()
}

/// MEASURED: `long probe`'s one call, `sleep 300` with `timeout: 400000`, is
/// in flight from its row to its result's, then the model's, then done; its
/// start is its first row's. Control: before its row, nothing is known.
#[test]
fn the_measured_long_call_is_in_flight_until_its_result() {
    let rows = rows("claude-2.1.304-long-bash.jsonl");
    let first = fold(&rows[..1]);
    assert_eq!(first.start_ms(), Some(ms("2026-10-20T15:58:20.682Z")));
    assert_eq!(first.flight(), FileFlight::Unknown, "no call row yet");
    let FileFlight::Tool(call) = fold(&rows[..2]).flight() else {
        panic!("in flight: {:?}", fold(&rows[..2]).flight());
    };
    assert_eq!(call.tool.as_str(), "Bash");
    assert_eq!(call.at_ms, ms("2026-10-20T15:58:20.732Z"));
    assert_eq!(call.budget_ms, 400_000, "its declared timeout");
    assert_eq!(call.until_ms(), call.at_ms + 430_000, "and the slack");
    assert!(ms("2026-10-20T16:03:20.995Z") < call.until_ms());
    assert_eq!(fold(&rows[..3]).flight(), FileFlight::Model, "answered");
    assert_eq!(fold(&rows).flight(), FileFlight::Done, "its turn ended");
}

/// MEASURED: `bg probe`'s Bash went to the background and is never in
/// flight. Control: the same row without `run_in_background` is.
#[test]
fn a_background_call_is_never_in_flight() {
    let rows = rows("claude-2.1.304-bg-bash.jsonl");
    assert!(rows[1].contains("\"run_in_background\":true"));
    assert_eq!(fold(&rows[..2]).flight(), FileFlight::Model);
    let foreground = [
        rows[0].clone(),
        rows[1].replace(",\"run_in_background\":true", ""),
    ];
    assert!(
        matches!(fold(&foreground).flight(), FileFlight::Tool(c) if c.budget_ms == DEFAULT_BUDGET_MS)
    );
    assert_eq!(fold(&rows).flight(), FileFlight::Done);
}

/// A call row of message `msg`, call `id`, tool `name`, input `input`, at second `s` past 10:00.
fn call_row(msg: &str, id: &str, name: &str, input: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{msg}\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{input}}}]}},\"timestamp\":\"2026-10-20T10:{:02}:{:02}.000Z\"}}",
        s / 60,
        s % 60
    )
}

/// The result row of call `id`.
fn result_row(id: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"tool_use_id\":\"{id}\",\"type\":\"tool_result\",\"content\":\"\"}}]}},\"timestamp\":\"2026-10-20T10:59:00.000Z\"}}"
    )
}

const PROMPT: &str = "{\"type\":\"user\",\"message\":{\"content\":\"[stand-in prompt]\"},\"timestamp\":\"2026-10-20T10:00:00.000Z\"}";

/// Builds a text row for message `msg` with optional `stop` reason.
fn text_row(msg: &str, stop: Option<&str>, s: u32) -> String {
    let stop = stop.map_or_else(|| "null".to_owned(), |stop| format!("\"{stop}\""));
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{msg}\",\"stop_reason\":{stop},\"content\":[{{\"type\":\"text\",\"text\":\"[stand-in]\"}}]}},\"timestamp\":\"2026-10-20T10:{:02}:{:02}.000Z\"}}",
        s / 60,
        s % 60
    )
}

/// THE WAYS A WORKFLOW'S AGENT ENDS (the shapes MEASURED on one run of 22
/// agents, module docs): on its answered [`FINAL_TOOL`] call with no
/// assistant row after it, or on a text row whose turn stops on
/// `stop_sequence` (or `max_tokens`, `refusal`): each is done, so a run
/// whose other agent sits in a long Bash call waits on that call, not on
/// the model. An assistant row read for its kind alone says the model's
/// turn. CONTROLS: an errored output's result is the model's turn again, as
/// any other answered call is.
#[test]
fn an_agent_ends_on_its_answered_output_or_its_stop() {
    let output = [
        PROMPT.to_owned(),
        call_row("m1", "o1", FINAL_TOOL, "{}", 1),
        result_row("o1"),
    ];
    assert_eq!(
        fold(&output).flight(),
        FileFlight::Done,
        "its output answered"
    );
    let bash = [
        PROMPT.to_owned(),
        call_row("m1", "b1", "Bash", "{\"timeout\":300000}", 2),
    ];
    let FileFlight::Tool(call) = fold(&bash).flight() else {
        panic!("in flight: {:?}", fold(&bash).flight());
    };
    assert_eq!(
        combine([fold(&output).flight(), fold(&bash).flight()]),
        FileFlight::Tool(call),
        "the run waits on its other agent's long call"
    );
    for stop in ["stop_sequence", "max_tokens", "refusal"] {
        let rows = [
            PROMPT.to_owned(),
            call_row("m1", "c1", "Bash", "{}", 1),
            result_row("c1"),
            text_row("m2", Some(stop), 3),
        ];
        assert_eq!(fold(&rows).flight(), FileFlight::Done, "{stop}");
    }
    let talking = [PROMPT.to_owned(), text_row("m1", None, 1)];
    assert_eq!(
        fold(&talking).flight(),
        FileFlight::Model,
        "a turn going on"
    );
    let errored = [
        PROMPT.to_owned(),
        call_row("m1", "o1", FINAL_TOOL, "{}", 1),
        result_row("o1").replace("\"tool_result\"", "\"tool_result\",\"is_error\":true"),
    ];
    assert_eq!(
        fold(&errored).flight(),
        FileFlight::Model,
        "an errored output"
    );
    let other = [
        PROMPT.to_owned(),
        call_row("m1", "r1", "Read", "{}", 1),
        result_row("r1"),
    ];
    assert_eq!(
        fold(&other).flight(),
        FileFlight::Model,
        "another tool answered"
    );
}

fn budget_of(rows: &[String]) -> u64 {
    match fold(rows).flight() {
        FileFlight::Tool(call) => call.budget_ms,
        other => panic!("in flight: {other:?}"),
    }
}

/// A Bash call's budget is its declared timeout, at most Bash's maximum, or
/// Bash's default; every other tool's is the default, whatever it declares
/// (control: the same `timeout` on Bash buys it).
#[test]
fn a_calls_budget_is_its_own_declared_timeout() {
    let with =
        |name: &str, input: &str| vec![PROMPT.to_owned(), call_row("m1", "c1", name, input, 1)];
    assert_eq!(budget_of(&with("Bash", "{\"timeout\":300000}")), 300_000);
    assert_eq!(
        budget_of(&with("Bash", "{\"timeout\":9000000}")),
        MAX_BUDGET_MS
    );
    assert_eq!(budget_of(&with("Bash", "{}")), DEFAULT_BUDGET_MS);
    assert_eq!(
        budget_of(&with("Bash", "{\"timeout\":0}")),
        DEFAULT_BUDGET_MS
    );
    assert_eq!(
        budget_of(&with("WebFetch", "{\"timeout\":300000}")),
        DEFAULT_BUDGET_MS
    );
    assert_eq!(budget_of(&with("Read", "{}")), DEFAULT_BUDGET_MS);
}

/// Of parallel calls, the first to outlive its budget is the one shown;
/// answering it shows the next. A new message answers every older call.
#[test]
fn parallel_calls_show_the_first_due_and_a_new_message_answers_them() {
    let rows = vec![
        PROMPT.to_owned(),
        call_row("m1", "long", "Bash", "{\"timeout\":500000}", 1),
        call_row("m1", "short", "Bash", "{\"timeout\":60000}", 2),
    ];
    let FileFlight::Tool(call) = fold(&rows).flight() else {
        panic!("in flight");
    };
    assert_eq!(call.budget_ms, 60_000, "the short one is due first");
    let mut answered = rows.clone();
    answered.push(result_row("short"));
    assert!(matches!(fold(&answered).flight(), FileFlight::Tool(c) if c.budget_ms == 500_000));
    let mut next = rows;
    next.push(call_row("m2", "other", "Read", "{}", 9));
    let FileFlight::Tool(call) = fold(&next).flight() else {
        panic!("in flight");
    };
    assert_eq!(call.tool.as_str(), "Read", "m1's calls were answered");
}

/// A message with more calls than kept, or a row too long to read, leaves
/// the file unknown until its next message; [`FLIGHT_MAX`] calls are kept.
#[test]
fn what_cannot_be_kept_leaves_the_file_unknown_until_the_next_message() {
    let many = |n: usize| {
        let mut rows = vec![PROMPT.to_owned()];
        rows.extend((0..n).map(|i| call_row("m1", &format!("c{i}"), "Bash", "{}", 1)));
        rows
    };
    assert!(matches!(
        fold(&many(FLIGHT_MAX)).flight(),
        FileFlight::Tool(_)
    ));
    let mut over = many(FLIGHT_MAX + 1);
    assert_eq!(fold(&over).flight(), FileFlight::Unknown);
    over.push(call_row("m2", "next", "Bash", "{}", 5));
    assert!(
        matches!(fold(&over).flight(), FileFlight::Tool(_)),
        "the next message"
    );

    let mut long = fold(&many(1));
    long.long_line();
    assert_eq!(long.flight(), FileFlight::Unknown);
    long.fold_bytes(call_row("m2", "next", "Bash", "{}", 5).as_bytes());
    assert!(matches!(long.flight(), FileFlight::Tool(_)));
}

/// A row is read only where its bytes may hold a call, a result or a turn's
/// end — a `\u` escape spelling a key included (control: the same row with
/// its key spelt out); a restart forgets everything and moves on.
#[test]
fn escaped_keys_are_read_and_a_restart_forgets() {
    let row = call_row("m1", "c1", "Bash", "{}", 1);
    let escaped = row.replace("\"tool_use\",\"id\"", "\"tool\\u005fuse\",\"id\"");
    assert_ne!(row, escaped);
    for row in [row, escaped] {
        assert!(matches!(
            fold(&[PROMPT.to_owned(), row]).flight(),
            FileFlight::Tool(_)
        ));
    }
    let mut f = fold(&[PROMPT.to_owned(), call_row("m1", "c1", "Bash", "{}", 1)]);
    let generation = f.generation();
    f.restart();
    assert_eq!(f.flight(), FileFlight::Unknown);
    assert_eq!(f.start_ms(), None);
    assert!(f.generation() > generation);
}

/// Claude Code's own normalisations, as 2.1.304 writes them.
#[test]
fn panel_text_and_slugs_are_claude_codes() {
    assert_eq!(panel_text("  long\tprobe \n now "), "long probe now");
    assert_eq!(panel_text("a\u{7}b"), "ab");
    assert_eq!(workflow_slug("stall-probe"), "stall-probe");
    assert_eq!(workflow_slug("  Stall  Probe!! v2 "), "stall-probe-v2");
    assert_eq!(workflow_slug("!!"), "workflow");
    assert_eq!(cells("a中b"), "a中 b");
}

/// The panel rows of a measured screen, each with its key.
fn keyed(screen: &[String]) -> (Vec<String>, Vec<Option<RowKey>>) {
    let bottom = aterm_phase::phase::composer_bottom(screen).expect("a composer");
    let panel = crate::harness::footer::agents_panel_under(screen, bottom).expect("a panel");
    let mut names = Vec::new();
    let mut keys = Vec::new();
    for row in panel.rows() {
        let text = &screen[usize::from(row.row)];
        names.push(text.clone());
        keys.push(RowKey::of(text, row));
    }
    (names, keys)
}

/// Writes measured `long probe` and `bg probe` fixture files up to `rows_read`.
fn run1_table(rows_read: usize) -> FlightTable {
    let long = rows("claude-2.1.304-long-bash.jsonl");
    let bg = rows("claude-2.1.304-bg-bash.jsonl");
    let long = fold(&long[..rows_read]);
    let bg = fold(&bg);
    FlightTable {
        flyers: vec![
            Flyer::agent(
                "general-purpose",
                Some("long probe"),
                long.start_ms().unwrap(),
                long.flight(),
            ),
            Flyer::agent(
                "general-purpose",
                Some("bg probe"),
                bg.start_ms().unwrap(),
                bg.flight(),
            ),
        ],
    }
}

/// The measured panel's row whose text holds `name`.
fn row_of(names: &[String], name: &str) -> usize {
    names.iter().position(|r| r.contains(name)).unwrap()
}

/// MEASURED: at `2m 37s` `long probe`'s row is its file's, waiting in Bash
/// 157 s into a call that has 273 s of its budget left. `bg probe`'s clock
/// stands at `0s` (its turn is done; Claude draws end − start), so it implies
/// no start its file agrees with: unknown, the screen's rules alone.
#[test]
fn the_measured_rows_map_to_their_own_files() {
    let screen = screen("claude-2.1.304-agents-panel-long-bash-2m37s-120col.txt");
    let (names, keys) = keyed(&screen);
    let start = ms("2026-10-20T15:58:20.682Z");
    let now = start + 157_500;
    let table = run1_table(2);
    let flights = place(&keys, &table, now);
    let long = row_of(&names, "long probe");
    let Flight::Tool {
        tool,
        age_ms,
        left_ms,
        ..
    } = flights[long]
    else {
        panic!("long probe waits: {:?}", flights[long]);
    };
    assert_eq!(tool.as_str(), "Bash");
    assert_eq!(age_ms, 157_450);
    assert_eq!(left_ms, 430_000 - 157_450);
    assert_eq!(flights[row_of(&names, "bg probe")], Flight::Unknown);
    assert_eq!(flights[row_of(&names, "main")], Flight::Unknown, "no task");
    assert_eq!(place(&keys, &run1_table(3), now)[long], Flight::Model);
}

/// A row maps only by exact agreement: another activity, another name, a
/// start outside the clock's resolution and the slack, two files that both
/// agree, or two rows that both take one file — each leaves it unknown.
/// Control: the measured agreement maps.
#[test]
fn a_row_maps_only_by_exact_agreement() {
    let screen = screen("claude-2.1.304-agents-panel-long-bash-2m37s-120col.txt");
    let (names, keys) = keyed(&screen);
    let long = row_of(&names, "long probe");
    let start = ms("2026-10-20T15:58:20.682Z");
    let now = start + 157_500;
    let table = run1_table(2);
    let maps = |table: &FlightTable, now: i64| {
        matches!(place(&keys, table, now)[long], Flight::Tool { .. })
    };
    assert!(maps(&table, now), "control");
    let with = |f: &dyn Fn(&mut Flyer)| {
        let mut t = table.clone();
        f(&mut t.flyers[0]);
        t
    };
    assert!(!maps(
        &with(&|f| *f = Flyer::agent("general-purpose", Some("long probes"), f.start_ms, f.flight)),
        now
    ));
    assert!(!maps(
        &with(&|f| *f = Flyer::agent("Explore", Some("long probe"), f.start_ms, f.flight)),
        now
    ));
    assert!(!maps(
        &with(&|f| *f = Flyer::agent("general-purpose", None, f.start_ms, f.flight)),
        now
    ));
    let slack = i64::try_from(1_000 + START_SLACK_MS).unwrap();
    assert!(maps(&with(&|f| f.start_ms -= slack - 500), now));
    assert!(!maps(&with(&|f| f.start_ms -= slack + 500), now));
    assert!(!maps(&with(&|f| f.start_ms += slack + 1_000), now));
    let mut twins = table.clone();
    twins.flyers.push(twins.flyers[0].clone());
    assert!(!maps(&twins, now));
    let doubled = [keys[long], keys[long]];
    assert_eq!(place(&doubled, &table, now)[..2], [Flight::Unknown; 2]);
    assert!(matches!(
        place(&doubled[..1], &table, now)[0],
        Flight::Tool { .. }
    ));
    let mut clockless = keys[long].unwrap();
    clockless.clock = None;
    assert_eq!(place(&[Some(clockless)], &table, now)[0], Flight::Unknown);
    assert_eq!(
        place(&keys, &with(&|f| f.flight = FileFlight::Unknown), now)[long],
        Flight::Unknown
    );
}

/// A row whose activity Claude cut with `…` maps by its prefix (control: a
/// prefix the description does not start with), and a wide glyph's right
/// half is a space of the pane's text.
#[test]
fn a_cut_activity_maps_by_its_prefix_in_cells() {
    let start = 1_000_000_000_000i64;
    let row = |activity: &str| format!("  ◯ general-purpose  {activity}   2m 37s · ↓ 6.2k tokens");
    let table = FlightTable {
        flyers: vec![Flyer::agent(
            "general-purpose",
            Some("sweep the 中文 corpus"),
            start,
            FileFlight::Model,
        )],
    };
    let flight = |text: &str| {
        let parsed = parse_row(text, aterm_phase::agents_panel::one_cell).unwrap();
        place(&[RowKey::of(text, &parsed)], &table, start + 157_000)[0]
    };
    assert_eq!(
        flight(&row("sweep the 中 文  corpus")),
        Flight::Model,
        "whole, in cells"
    );
    assert_eq!(flight(&row("sweep the 中 文 …")), Flight::Model, "cut");
    assert_eq!(flight(&row("sweep the…")), Flight::Model, "cut at a space");
    assert_eq!(flight(&row("sweep tha…")), Flight::Unknown, "control");
    assert_eq!(
        flight(&row("sweep the 中文 corpus")),
        Flight::Unknown,
        "not in cells"
    );
}

/// MEASURED: the workflow's row is its run's by its slug and its script's
/// start; its run waits on `long`'s call while `quick` is done (control:
/// with `long` answered, the run is done and its row the model's).
#[test]
fn the_measured_workflow_row_maps_to_its_run() {
    let screen = screen("claude-2.1.304-agents-panel-workflow-long-bash-2m46s-120col.txt");
    let (names, keys) = keyed(&screen);
    let wf = row_of(&names, "stall-probe");
    let long = rows("claude-2.1.304-workflow-long.jsonl");
    let quick = fold(&rows("claude-2.1.304-workflow-quick.jsonl"));
    assert_eq!(quick.flight(), FileFlight::Done);
    let start = 1_792_512_327_967i64;
    let now = start + 166_500;
    let table = |long_read: usize| FlightTable {
        flyers: vec![Flyer::workflow(
            "stall-probe",
            start,
            combine([fold(&long[..long_read]).flight(), quick.flight()]),
        )],
    };
    let Flight::Tool { tool, left_ms, .. } = place(&keys, &table(3), now)[wf] else {
        panic!("waits: {:?}", place(&keys, &table(3), now)[wf]);
    };
    assert_eq!(tool.as_str(), "Bash");
    assert!(left_ms > 100_000, "300 s budget, 166 s in: {left_ms}");
    assert_eq!(place(&keys, &table(4), now)[wf], Flight::Model);
}

/// A run is unknown if any of its agents is, the model's while any is in
/// its turn, else waiting on the first call due, else done.
#[test]
fn a_runs_flight_combines_its_agents() {
    let call = |budget_ms| Call {
        id: budget_ms,
        tool: ToolName::of("Bash"),
        at_ms: 0,
        budget_ms,
    };
    use FileFlight::*;
    assert_eq!(combine([Tool(call(9)), Unknown, Model]), Unknown);
    assert_eq!(combine([Tool(call(9)), Model, Done]), Model);
    assert_eq!(combine([Tool(call(9)), Tool(call(5)), Done]), Tool(call(5)));
    assert_eq!(combine([Done, Done]), Done);
    assert_eq!(combine([]), Done);
}

/// A tool's name is kept to its bound: whole where it fits (its bound
/// exactly, too), else cut at a char boundary and ended with `…`, so a cut
/// name is never shown or compared as a whole one. NEGATIVE CONTROL: the
/// name cut bare at the bound is not what is kept.
#[test]
fn a_tool_name_is_kept_inline_and_a_cut_one_says_so() {
    assert_eq!(ToolName::of("Bash").as_str(), "Bash");
    let whole = "x".repeat(TOOL_NAME_MAX);
    assert_eq!(
        ToolName::of(&whole).as_str(),
        whole,
        "the bound itself fits"
    );
    let long = "mcp__claude_ai_Claude_Docs__batch";
    let kept = ToolName::of(long);
    assert!(kept.as_str().len() <= TOOL_NAME_MAX);
    assert_eq!(kept.as_str(), "mcp__claude_ai_Claude_Docs__\u{2026}");
    assert_ne!(
        kept.as_str(),
        &long[..TOOL_NAME_MAX],
        "CONTROL: never cut bare"
    );
    let wide = "é".repeat(20);
    assert_eq!(
        ToolName::of(&wide).as_str(),
        format!("{}\u{2026}", "é".repeat(14))
    );
    let output = format!("{FINAL_TOOL}{}", "x".repeat(TOOL_NAME_MAX));
    assert_ne!(ToolName::of(&output).as_str(), FINAL_TOOL);
}

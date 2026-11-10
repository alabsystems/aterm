// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! libtest's JSON events (`-Z unstable-options --format json`), read and rendered back into the
//! log libtest's pretty formatter writes, so every reader of a test binary's output
//! ([`crate::libtest`], [`crate::findings`]) reads a JSON run as it reads a human one.
//!
//! A JSON run gives each case's captured block in its `failed` event, as the case ends, where the
//! human format prints it only in its report at the binary's end. That is what lets a halted run
//! end a binary at its first decisive red case (`exec`'s `LiveCases`) and still keep the case's
//! panic: the event is in the child's log, and [`render`] writes it into the block the readers
//! take a red row's "why" from.
//!
//! Measured on Trust build 16387's libtest (2026-11-06): `--format json` alone is refused (exit
//! 101, "only accepted on the nightly compiler with -Z unstable-options"); with
//! `-Z unstable-options` it is accepted; a `failed` event carries the case's whole captured block
//! in `stdout`, and a `should_panic` mismatch's note in `message`; a failure exits 101; `--list`
//! prints a suite `discovery` event. A `should_panic` case's name carries no ` - should panic`
//! there, so a rendered `test … FAILED` line names it bare; no reader takes a name from that line.

use std::fmt::Write as _;

/// The value of an event's `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Suite,
    Test,
    Bench,
}

/// One libtest JSON event.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub(crate) kind: Kind,
    /// `started`, `ok`, `failed`, `ignored`, `timeout`, `discovery`, `discovered`, `completed`, …
    pub(crate) event: String,
    pub(crate) name: Option<String>,
    pub(crate) stdout: Option<String>,
    pub(crate) message: Option<String>,
    /// Why a case libtest timed failed (`time limit exceeded` under `--ensure-time`).
    pub(crate) reason: Option<String>,
    pub(crate) source_path: Option<String>,
    pub(crate) start_line: Option<u64>,
    pub(crate) start_col: Option<u64>,
    pub(crate) test_count: Option<u64>,
    pub(crate) passed: Option<u64>,
    pub(crate) failed: Option<u64>,
    pub(crate) ignored: Option<u64>,
    pub(crate) measured: Option<u64>,
    pub(crate) filtered_out: Option<u64>,
    pub(crate) exec_time: Option<f64>,
}

/// Every key libtest's JSON formatter writes, on any event.
const KEYS: &[&str] = &[
    "type",
    "event",
    "name",
    "stdout",
    "message",
    "reason",
    "test_count",
    "shuffle_seed",
    "passed",
    "failed",
    "ignored",
    "measured",
    "filtered_out",
    "exec_time",
    "median",
    "deviation",
    "mib_per_second",
    "ignore",
    "ignore_message",
    "source_path",
    "start_line",
    "start_col",
    "end_line",
    "end_col",
    "tests",
    "benchmarks",
    "total",
];

/// The longest completed cases libtest timed, bounded diagnostic data rather than a verdict.
pub(crate) fn slow_cases(timings: &[crate::exec::CaseTiming]) -> Vec<aterm_json::Value> {
    let mut cases: Vec<_> = timings.iter().collect();
    cases.sort_by(|a, b| b.execution.cmp(&a.execution).then(a.case.cmp(&b.case)));
    cases
        .into_iter()
        .take(10)
        .map(|timing| {
            aterm_json::json!({
                "case": crate::report::clip_json(&timing.case, crate::report::EVENT_ROW_BYTES),
                "execution_s": timing.execution.as_secs_f64(),
            })
        })
        .collect()
}

/// The event `line` is, if it is one whole: an object whose `type` is `suite`, `test` or `bench`,
/// with a string `event` and no key libtest does not write. A test's own output that only looks
/// like JSON, or a line cut short, is none.
#[must_use]
pub(crate) fn parse_event(line: &str) -> Option<Event> {
    let line = line.trim_end_matches('\r');
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }
    let value = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
    let object = value.as_object()?;
    if object.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return None;
    }
    let kind = match object.get("type")?.as_str()? {
        "suite" => Kind::Suite,
        "test" => Kind::Test,
        "bench" => Kind::Bench,
        _ => return None,
    };
    let text = |k: &str| {
        object
            .get(k)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned)
    };
    let count = |k: &str| object.get(k).and_then(aterm_json::Value::as_u64);
    let event = text("event")?;
    let name = text("name");
    if kind != Kind::Suite && name.is_none() {
        return None;
    }
    Some(Event {
        kind,
        event,
        name,
        stdout: text("stdout"),
        message: text("message"),
        reason: text("reason"),
        source_path: text("source_path"),
        start_line: count("start_line"),
        start_col: count("start_col"),
        test_count: count("test_count"),
        passed: count("passed"),
        failed: count("failed"),
        ignored: count("ignored"),
        measured: count("measured"),
        filtered_out: count("filtered_out"),
        exec_time: object.get("exec_time").and_then(aterm_json::Value::as_f64),
    })
}

/// The event `line` ends with and the text before it: the whole line when it is one
/// ([`parse_event`]), else the last `{"type":` (libtest writes `{ "type": `) that starts one,
/// where output a test wrote past capture with no newline (a child on the same stdout, a `print!`)
/// ran into libtest's event. Inside an event's strings every quote is escaped, so no later
/// `{ "type":` sits in the event itself.
#[must_use]
pub(crate) fn line_event(line: &str) -> Option<(&str, Event)> {
    if let Some(ev) = parse_event(line) {
        return Some(("", ev));
    }
    let at = ["{ \"type\":", "{\"type\":"]
        .iter()
        .filter_map(|p| line.rfind(p))
        .max()
        .filter(|&at| at > 0)?;
    parse_event(&line[at..]).map(|ev| (&line[..at], ev))
}

/// A failed case's block as the readers take it: its captured `stdout`, then `note: <message>`
/// when the event has a message (a `should_panic` mismatch), as libtest's report prints it, or
/// `note: <reason>` for a timed failure.
#[must_use]
pub(crate) fn case_block(ev: &Event) -> String {
    let mut block = report_block(ev);
    if ev.message.is_some() || ev.reason.is_some() {
        block.push('\n');
    }
    block
}

/// The block exactly as libtest's report writes it under its `---- <name> stdout ----` line:
/// [`case_block`] with the note unterminated, then the report's own newline.
fn report_block(ev: &Event) -> String {
    let mut block = ev.stdout.clone().unwrap_or_default();
    if let Some(note) = ev.message.as_ref().or(ev.reason.as_ref()) {
        let _ = write!(block, "note: {note}");
    }
    block
}

/// A binary verify ended at its first decisive red case (`exec`'s `LiveCases`), before libtest's
/// own report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stopped {
    /// cargo's re-run spec for the binary.
    pub spec: String,
    /// The case whose failure ended it.
    pub case: String,
}

/// What the binary did, read from its events, for its stopped line.
#[derive(Default)]
struct Tally {
    passed: u64,
    failed: u64,
    ignored: u64,
    total: Option<u64>,
    /// Started and not yet ended, in start order.
    running: Vec<String>,
}

/// The log libtest's pretty formatter would have written for this JSON run's `raw` output. Lines
/// that are no event (cargo's `Running` preamble, a test's uncaptured output) stay in place. A
/// run verify `stopped` gets no suite end: its finished cases' blocks and their list, a
/// `test result: FAILED.` line that says verify stopped it, and, outside every block, the line
/// that says which cases had not finished. A run that ended with no suite event otherwise (a
/// crash) keeps its failed cases' blocks and list, with no result line.
#[must_use]
pub fn render(raw: &str, stopped: Option<&Stopped>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut tally = Tally::default();
    let mut ended = false;
    for line in raw.split_inclusive('\n') {
        let bare = line.trim_end_matches('\n');
        let Some((before, ev)) = line_event(bare) else {
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
            continue;
        };
        if !before.is_empty() {
            out.push_str(before);
            out.push('\n');
        }
        let name = ev.name.clone().unwrap_or_default();
        match (ev.kind, ev.event.as_str()) {
            (Kind::Suite, "started") => {
                let n = ev.test_count.unwrap_or(0);
                tally.total = Some(n);
                let _ = writeln!(out, "\nrunning {n} test{}", if n == 1 { "" } else { "s" });
            }
            (Kind::Suite, "ok" | "failed") => {
                write_failures(&mut out, &failures);
                let _ = writeln!(
                    out,
                    "\ntest result: {}. {} passed; {} failed; {} ignored; {} measured; {} \
                     filtered out; finished in {:.2}s\n",
                    if ev.event == "ok" { "ok" } else { "FAILED" },
                    ev.passed.unwrap_or(0),
                    ev.failed.unwrap_or(0),
                    ev.ignored.unwrap_or(0),
                    ev.measured.unwrap_or(0),
                    ev.filtered_out.unwrap_or(0),
                    ev.exec_time.unwrap_or(0.0),
                );
                failures.clear();
                ended = true;
            }
            (Kind::Test | Kind::Bench, "started") => tally.running.push(name),
            (Kind::Test | Kind::Bench, "ok") => {
                tally.running.retain(|r| *r != name);
                tally.passed += 1;
                let _ = writeln!(out, "test {name} ... ok");
            }
            (Kind::Test | Kind::Bench, "failed") => {
                tally.running.retain(|r| *r != name);
                tally.failed += 1;
                let _ = writeln!(out, "test {name} ... FAILED");
                failures.push((name, report_block(&ev)));
            }
            (Kind::Test | Kind::Bench, "ignored") => {
                tally.running.retain(|r| *r != name);
                tally.ignored += 1;
                match &ev.message {
                    Some(why) => {
                        let _ = writeln!(out, "test {name} ... ignored, {why}");
                    }
                    None => {
                        let _ = writeln!(out, "test {name} ... ignored");
                    }
                }
            }
            (Kind::Test | Kind::Bench, "timeout") => {
                let _ = writeln!(out, "test {name} has been running for over 60 seconds");
            }
            _ => {}
        }
    }
    if ended {
        return out;
    }
    write_failures(&mut out, &failures);
    if let Some(stop) = stopped {
        let _ = writeln!(
            out,
            "\ntest result: FAILED. {} passed; {} failed; {} ignored; 0 measured; 0 filtered out; \
             stopped by verify at its first red case `{}`\n",
            tally.passed, tally.failed, tally.ignored, stop.case
        );
        let finished = tally.passed + tally.failed + tally.ignored;
        let total = tally.total.unwrap_or(finished).max(finished);
        let left = total - finished;
        let running = u64::try_from(tally.running.len()).unwrap_or(u64::MAX);
        let mut which = tally.running.join(", ");
        if left > running {
            if !which.is_empty() {
                which.push_str("; ");
            }
            let _ = write!(which, "{} not started", left - running);
        }
        if which.is_empty() {
            which.push_str("none");
        }
        let _ = writeln!(
            out,
            "aterm-verify: stopped {} at its first red case `{}`; {left} of {total} cases had not \
             finished: {which}",
            stop.spec, stop.case
        );
    }
    out
}

/// libtest's report of its failed cases: each block in the order the cases ended, then their
/// sorted list; nothing when none failed.
fn write_failures(out: &mut String, failures: &[(String, String)]) {
    if failures.is_empty() {
        return;
    }
    out.push_str("\nfailures:\n\n");
    for (name, block) in failures {
        if block.is_empty() {
            continue;
        }
        let _ = writeln!(out, "---- {name} stdout ----\n{block}");
    }
    out.push_str("\nfailures:\n");
    let mut names: Vec<&str> = failures.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    for name in names {
        let _ = writeln!(out, "    {name}");
    }
}

#[cfg(test)]
#[path = "libtest_json_tests.rs"]
mod tests;

// An actual nested GUI failure block from the 8ad verifier incident. The child
// summary must remain diagnostic text, never replace the outer case identity.
#[cfg(test)]
pub(crate) const NESTED_GUI_FAILURE: &str = r#"thread 'seamless::carry_tests::fixture_generator::generate_handoff_fixtures' (155504210) panicked at crates/aterm-gui/src/seamless_fixture_generator_tests.rs:39:5:
`seamless::carry_tests::fixture_generator::generate_handoff_fixtures`, alone in its own process, failed (exit status: 101)
stdout:

running 1 test
test seamless::carry_tests::fixture_generator::generate_handoff_fixtures ... FAILED

failures:

failures:
    seamless::carry_tests::fixture_generator::generate_handoff_fixtures

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 15194 filtered out; finished in 1.20s


stderr:

thread 'seamless::carry_tests::fixture_generator::generate_handoff_fixtures' (155504231) panicked at crates/aterm-gui/src/seamless_fixture_generator_tests.rs:4723:5:
carried fields and variants no desk here writes (held since v0.170.0); give each a value in a desk, as a real session would carry it, or name one no handoff writes in NO_HANDOFF_WRITES with why (tests/fixtures/handoff/README.md):
  meta OutputStateRepr.program_state_began (listed, but no desk reaches it)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"#;

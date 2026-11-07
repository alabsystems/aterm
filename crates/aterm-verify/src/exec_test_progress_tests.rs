// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::libtest::COULD_NOT_RUN_SENTINEL;
use std::io::Write;

fn started(name: &str) -> String {
    format!(
        "{}\n",
        aterm_json::json!({"type": "test", "event": "started", "name": name})
    )
}

fn failed(name: &str, stdout: &str) -> String {
    format!(
        "{}\n",
        aterm_json::json!({"type": "test", "name": name, "event": "failed", "stdout": stdout})
    )
}

fn passed(name: &str, stdout: &str) -> String {
    format!(
        "{}\n",
        aterm_json::json!({"type": "test", "name": name, "event": "ok", "stdout": stdout})
    )
}

const PLAIN: &str = "\nthread 'x' panicked at src/a.rs:1:1:\nassertion failed\n";

#[test]
fn a_long_running_case_is_reported_once_without_deciding_or_ending_it() {
    let timeout = |name| {
        format!(
            "{}\n",
            aterm_json::json!({"type": "test", "event": "timeout", "name": name})
        )
    };
    assert_eq!(
        table(
            true,
            &[
                timeout("orphan"),
                started("slow"),
                timeout("slow"),
                timeout("slow"),
                passed("slow", ""),
                timeout("slow"),
                started("red"),
                failed("red", PLAIN),
            ]
        ),
        [
            vec![],
            vec![],
            vec![Seen::RunningLong {
                case: "slow".into()
            }],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![red("red")],
        ]
    );
}

fn refused() -> String {
    format!("\nthread 'x' panicked at src/a.rs:1:1:\n{COULD_NOT_RUN_SENTINEL} — no fish\n")
}

fn disk() -> String {
    format!(
        "\nthread 'x' panicked at src/a.rs:1:1:\n{} (os error 28)\n",
        crate::exec::ENOSPC_SENTENCE
    )
}

/// Write `chunks` one at a time to a log, polling after each: what each poll saw.
fn table(json: bool, chunks: &[String]) -> Vec<Vec<Seen>> {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("running.log");
    let mut writer = std::fs::File::create(&path).unwrap();
    let mut live = LiveCases::open(&path, json).unwrap();
    chunks
        .iter()
        .map(|chunk| {
            writer.write_all(chunk.as_bytes()).unwrap();
            live.poll()
        })
        .collect()
}

fn red(case: &str) -> Seen {
    Seen::Red {
        case: case.into(),
        marks: Marks::default(),
    }
}

fn red_with(case: &str, refused: bool, disk: bool) -> Seen {
    Seen::Red {
        case: case.into(),
        marks: Marks { refused, disk },
    }
}

fn spared(case: &str, refused: bool, disk: bool) -> Seen {
    Seen::Spared {
        case: case.into(),
        marks: Marks { refused, disk },
    }
}

/// THE LIVE VIEW OF A BINARY'S CASES, ROW BY ROW, each chunk appended to the log before a poll:
/// a plain block is red at once; a refusal, a full disk, a coloured refusal and a block with both
/// are spared; a refusal then a full disk is red at the second (neither every-refused nor
/// every-out-of-disk can hold), while two refusals never are. A partial line is nothing until its
/// newline; a failed event for a case that never started, a second end of one case and an event
/// inside a passing case's output are nothing. NEGATIVE CONTROL: the plain row is red, so a table
/// that read nothing would fail it.
#[test]
fn each_failed_case_is_red_spared_or_nothing_by_its_own_block() {
    let coloured = format!("\x1b[31m{}\x1b[0m", refused());
    let both = format!("{}{}", refused(), disk());
    assert_eq!(
        table(true, &[started("a"), failed("a", PLAIN)]),
        [vec![], vec![red("a")]]
    );
    for (block, refused, disk) in [
        (refused(), true, false),
        (disk(), false, true),
        (coloured, true, false),
        (both, true, true),
    ] {
        assert_eq!(
            table(true, &[started("a"), failed("a", &block)]),
            [vec![], vec![spared("a", refused, disk)]],
            "{block:?}"
        );
    }
    assert_eq!(
        table(
            true,
            &[
                started("a"),
                started("b"),
                failed("a", &refused()),
                failed("b", &disk())
            ]
        ),
        [
            vec![],
            vec![],
            vec![spared("a", true, false)],
            vec![red_with("b", false, true)]
        ]
    );
    assert_eq!(
        table(
            true,
            &[
                started("a"),
                started("b"),
                failed("a", &refused()),
                failed("b", &refused())
            ]
        ),
        [
            vec![],
            vec![],
            vec![spared("a", true, false)],
            vec![spared("b", true, false)]
        ]
    );
    let whole = failed("a", PLAIN);
    let (head, tail) = whole.split_at(whole.len() / 2);
    assert_eq!(
        table(true, &[started("a"), head.into(), tail.into()]),
        [vec![], vec![], vec![red("a")]],
        "a partial line is nothing until its newline"
    );
    assert_eq!(
        table(true, &[failed("ghost", PLAIN)]),
        [vec![]],
        "a failed event for a case that never started"
    );
    assert_eq!(
        table(true, &[started("a"), passed("a", ""), failed("a", PLAIN)]),
        [vec![], vec![], vec![]],
        "a case's second end"
    );
    let inside = passed("a", &failed("a", PLAIN));
    assert_eq!(
        table(true, &[started("a"), inside]),
        [vec![], vec![]],
        "an event inside a passing case's output"
    );
}

/// A LINE OVER THE CAP, AND A HUMAN LOG, ARE DECIDED AT THE BINARY'S END: a failed event over
/// [`LINE_CAP`] is Undecided, and the next case is read as before; a human `test … FAILED` line,
/// coloured or not, is Undecided under its name. NEGATIVE CONTROL: the case after the long line is
/// red, so the cap did not end reading.
#[test]
fn an_oversized_or_human_failure_is_undecided() {
    let long = "x".repeat(LINE_CAP + 10);
    assert_eq!(
        table(
            true,
            &[
                started("a"),
                started("b"),
                failed("a", &long),
                failed("b", PLAIN)
            ]
        ),
        [
            vec![],
            vec![],
            vec![Seen::Undecided { case: None }],
            vec![red("b")]
        ]
    );
    assert_eq!(
        table(
            false,
            &[
                "test first ... ok\n".into(),
                "test bad ... \x1b[31mFAILED\x1b[0m\r\n".into(),
            ]
        ),
        [
            vec![],
            vec![Seen::Undecided {
                case: Some("bad".into())
            }]
        ]
    );
}

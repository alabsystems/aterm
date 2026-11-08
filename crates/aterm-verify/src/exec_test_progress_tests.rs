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
fn structured_nested_failure_keeps_outer_identity_and_full_block_classification() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("cases");
    let noise = failed("fake", PLAIN);
    std::fs::write(
        &path,
        format!(
            "{}{}{}{}",
            started("passing"),
            passed("passing", &noise),
            started("outer"),
            failed("outer", crate::libtest_json::NESTED_GUI_FAILURE)
        ),
    )
    .unwrap();
    let mut cases = LiveCases::open(&path, true).unwrap();
    assert_eq!(cases.poll(), vec![red("outer")]);
    assert_eq!(cases.observations().read_end, ReadEnd::Eof);
    let failure = &cases.observations().failures[0];
    assert_eq!(failure.case, "outer");
    assert!(
        failure
            .output
            .contains("seamless_fixture_generator_tests.rs:4723:5")
    );
    assert_eq!(failure.omitted_bytes, 0);
    // Classification must see the full block even when the diagnostic omits its middle.
    let full = format!(
        "{}{}{}",
        "a".repeat(4096),
        COULD_NOT_RUN_SENTINEL,
        "界".repeat(4096)
    );
    let clipped = diagnostic("bounded".into(), &full, crate::libtest::marks(&full));
    assert!(clipped.marks.refused);
    assert!(clipped.omitted_bytes > 0);
    assert_eq!(
        full.len() - clipped.omitted_bytes,
        clipped.output.split("\n…\n").map(str::len).sum::<usize>()
    );
}

#[test]
fn an_empty_bounded_batch_is_not_a_claim_to_have_read_the_log() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("cases");
    // More than one poll's 32MiB, with no complete case event in the first batch.
    std::fs::write(&path, vec![b'x'; (512 << 16) + 1]).unwrap();
    let mut cases = LiveCases::open(&path, true).unwrap();
    assert!(cases.poll().is_empty());
    assert_eq!(cases.observations().read_end, ReadEnd::Budget);
    assert!(cases.poll().is_empty());
    assert_eq!(cases.observations().read_end, ReadEnd::Partial);
    assert!(cases.observations().failures.is_empty());
}

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

/// A complete line is liveness and a partial line is not. The running names are exactly JSON
/// cases with a start and no end, including after the first red (whose later failures are not
/// reported twice). NEGATIVE CONTROLS: `half` leaves the line count unchanged, and ending `a`
/// removes only `a` while `b` remains.
#[test]
fn complete_lines_and_unfinished_json_cases_are_live_evidence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("running.log");
    let mut writer = std::fs::File::create(&path).unwrap();
    let mut live = LiveCases::open(&path, true).unwrap();
    let a = started("a");
    let (half, rest) = a.split_at(a.len() / 2);
    writer.write_all(half.as_bytes()).unwrap();
    assert!(live.poll().is_empty());
    assert_eq!(live.lines(), Some(0));
    assert!(live.running().is_empty());

    writer.write_all(rest.as_bytes()).unwrap();
    writer.write_all(started("b").as_bytes()).unwrap();
    assert!(live.poll().is_empty());
    assert_eq!(live.lines(), Some(2));
    assert_eq!(live.running(), ["a", "b"]);

    writer.write_all(failed("a", PLAIN).as_bytes()).unwrap();
    assert_eq!(live.poll(), [red("a")]);
    assert_eq!(live.running(), ["b"]);
    writer.write_all(passed("b", "").as_bytes()).unwrap();
    writer.write_all(started("c").as_bytes()).unwrap();
    writer.write_all(failed("c", PLAIN).as_bytes()).unwrap();
    assert!(live.poll().is_empty(), "a later red is not emitted twice");
    assert!(live.running().is_empty());
    assert_eq!(live.lines(), Some(6));
}

/// An unread capture cannot prove that no line was written. A later readable file establishes a
/// fresh line generation. NEGATIVE CONTROL: before the injected read error the same reader is
/// available, and after replacement it consumes the line.
#[cfg(unix)]
#[test]
fn an_unread_capture_fails_open_until_a_read_succeeds() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("running.log");
    std::fs::write(&path, started("case")).unwrap();
    let mut live = LiveCases::open(&path, true).unwrap();
    assert_eq!(live.lines(), Some(0));
    live.file = std::fs::File::open(root.path()).expect("a directory descriptor");
    assert!(live.poll().is_empty());
    assert_eq!(live.lines(), None);
    live.file = std::fs::File::open(&path).unwrap();
    assert!(live.poll().is_empty());
    assert_eq!(live.lines(), Some(1));
    assert_eq!(live.running(), ["case"]);
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

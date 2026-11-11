// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::BlockText;
use crate::terminal::{Terminal, TerminalBuilder};

/// Drive one OSC 133 command block (prompt → command → output → done),
/// with `cmd` typed and `output_lines` printed, then return.
fn run_block(term: &mut Terminal, cmd: &str, output_lines: &[&str]) {
    term.process(b"\x1b]133;A\x07");
    term.process(format!("$ {cmd}").as_bytes());
    term.process(b"\x1b]133;B\x07");
    term.process(b"\r\n");
    term.process(b"\x1b]133;C\x07");
    for line in output_lines {
        term.process(line.as_bytes());
        term.process(b"\r\n");
    }
    term.process(b"\x1b]133;D;0\x07");
}

/// With a small scrollback cap, an old block whose rows scrolled away
/// reports evicted, while a still-retained block returns its true output.
#[test]
fn evicted_block_returns_evicted_marker_retained_block_returns_true_output() {
    let mut term = TerminalBuilder::new()
        .size(4, 40)
        .ring_buffer_size(6)
        .build();

    run_block(&mut term, "first", &["EVICT-ME-0", "EVICT-ME-1"]);
    run_block(&mut term, "second", &["KEEP-ME-0", "KEEP-ME-1"]);
    // Next prompt seals `second`'s `end_row` before subsequent output scrolls.
    term.process(b"\x1b]133;A\x07");

    let ids: Vec<u64> = term.all_blocks().map(|b| b.id).collect();
    assert!(ids.len() >= 2, "expected at least two blocks, got {ids:?}");
    let (evict_id, keep_id) = (ids[0], ids[1]);

    for i in 0..6 {
        term.process(format!("filler-{i}\r\n").as_bytes());
    }

    let evict_block = term
        .block_by_id(evict_id)
        .cloned()
        .expect("evicted block id still tracked");
    let keep_block = term
        .block_by_id(keep_id)
        .cloned()
        .expect("retained block id still tracked");

    let evicted = term.block_output_text(&evict_block);
    assert_eq!(
        evicted,
        BlockText::Evicted,
        "block whose rows scrolled past the cap must report Evicted, got {evicted:?}",
    );
    assert_eq!(term.block_output(&evict_block), None);

    assert!(
        term.block_eviction_read_count() >= 1,
        "eviction read must bump the session counter",
    );

    let bogus_id = ids.iter().copied().max().unwrap() + 1_000;
    assert!(
        term.block_by_id(bogus_id).is_none(),
        "a never-existed block id has no block (distinct from Evicted)",
    );

    match term.block_output_text(&keep_block) {
        BlockText::Text(text) => {
            assert!(
                text.contains("KEEP-ME-0") && text.contains("KEEP-ME-1"),
                "retained block must return its TRUE output, got {text:?}",
            );
            assert!(
                !text.contains("filler-"),
                "retained block output must not be shifted into unrelated filler rows: {text:?}",
            );
        }
        BlockText::Evicted => panic!("retained block unexpectedly Evicted"),
        BlockText::NotAvailable => panic!("retained block unexpectedly NotAvailable"),
    }
}

/// The `aterm_log` records (level and message) written on this thread while
/// `body` runs.
fn logged_on_this_thread(body: impl FnOnce()) -> Vec<String> {
    use std::cell::RefCell;
    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }
    struct Capture;
    impl aterm_log::Log for Capture {
        fn enabled(&self, _: &aterm_log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &aterm_log::Record<'_>) {
            LINES.with(|lines| {
                if let Some(lines) = lines.borrow_mut().as_mut() {
                    lines.push(format!("{} {}", record.level(), record.args()));
                }
            });
        }
        fn flush(&self) {}
    }
    static CAPTURE: Capture = Capture;
    let _ = aterm_log::set_logger(&CAPTURE);
    aterm_log::set_max_level(aterm_log::LevelFilter::Trace);
    LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
    body();
    LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default())
}

/// A block read again and again after its eviction logs once per side, as the
/// status observer re-reads a running command with no OSC 633;E up to 4 times a
/// second; every read is still counted.
#[test]
fn a_block_read_again_and_again_after_its_eviction_is_logged_once() {
    let mut term = TerminalBuilder::new()
        .size(4, 40)
        .ring_buffer_size(6)
        .build();
    run_block(&mut term, "first", &["out-a", "out-b"]);
    term.process(b"\x1b]133;A\x07$ long-job\x1b]133;B\x07\r\n\x1b]133;C\x07");
    for i in 0..40 {
        term.process(format!("job-{i}\r\n").as_bytes());
    }
    let older = term.all_blocks().next().cloned().expect("the first block");
    let running = term.current_block().cloned().expect("the running block");
    assert!(running.id > older.id);

    let lines = logged_on_this_thread(|| {
        for _ in 0..50 {
            assert_eq!(term.block_command_text(&running), BlockText::Evicted);
            assert_eq!(term.block_command(&running), None);
        }
        assert_eq!(term.block_command_text(&older), BlockText::Evicted);
        for _ in 0..50 {
            assert_eq!(term.block_output_text(&older), BlockText::Evicted);
        }
    });
    let evicted: Vec<&String> = lines.iter().filter(|l| l.contains("EVICTED")).collect();
    assert_eq!(
        evicted.len(),
        2,
        "one line for the running block's command, one for the older block's \
         output: {evicted:#?}"
    );
    assert_eq!(
        term.block_eviction_read_count(),
        151,
        "every evicted read is still counted"
    );
}

/// A program starting block after block (no nonce guards OSC 133) and scrolling
/// each command out logs evictions by doubling: 64 blocks write 7 lines.
#[test]
fn a_program_starting_block_after_block_logs_their_evictions_by_doubling() {
    let mut term = TerminalBuilder::new()
        .size(4, 40)
        .ring_buffer_size(6)
        .build();
    let lines = logged_on_this_thread(|| {
        for k in 0..64 {
            term.process(
                format!("\x1b]133;A\x07$ job-{k}\x1b]133;B\x07\r\n\x1b]133;C\x07").as_bytes(),
            );
            for i in 0..20 {
                term.process(format!("out-{k}-{i}\r\n").as_bytes());
            }
            let running = term.current_block().cloned().expect("the running block");
            for _ in 0..4 {
                // The status observer's looks at the running command.
                assert_eq!(term.block_command_text(&running), BlockText::Evicted);
            }
            term.process(b"\x1b]133;D;0\x07");
        }
    });
    assert_eq!(
        term.block_eviction_read_count(),
        256,
        "every read is counted"
    );
    let evicted: Vec<&String> = lines.iter().filter(|l| l.contains("EVICTED")).collect();
    assert_eq!(
        evicted.len(),
        7,
        "the 1st, 2nd, 4th … 64th evicted block, not one line per block or per read: \
         {evicted:#?}"
    );
}

/// A block read before any eviction returns its output and counts no eviction.
#[test]
fn retained_block_before_eviction_reads_true_output() {
    let mut term = TerminalBuilder::new()
        .size(4, 40)
        .ring_buffer_size(100)
        .build();
    run_block(&mut term, "echo", &["HELLO-WORLD"]);
    let block = term.all_blocks().next().cloned().expect("one block");
    let out = term.block_output_text(&block);
    assert!(
        matches!(&out, BlockText::Text(t) if t.contains("HELLO-WORLD")),
        "fresh block output must read true text, got {out:?}",
    );
    assert_eq!(
        term.block_eviction_read_count(),
        0,
        "a non-evicted read must not bump the eviction counter",
    );
}

/// `last_completed_block` returns only archived (row-sealed) blocks, never a
/// Complete current one, whose open `end_row` would truncate the read.
#[test]
fn last_completed_block_returns_newest_sealed_block_only() {
    let mut term = TerminalBuilder::new()
        .size(6, 40)
        .ring_buffer_size(100)
        .build();
    assert!(
        term.last_completed_block().is_none(),
        "no shell integration yet: no completed block",
    );

    term.process(b"\x1b]133;A\x07$ ls");
    term.process(b"\x1b]133;B\x07\r\n");
    term.process(b"\x1b]133;C\x07OUT-1\r\nOUT-2\r\n\x1b]133;D;0\x07");
    assert!(
        term.last_completed_block().is_none(),
        "a Complete-but-current block has an open end_row (truncated span)",
    );

    term.process(b"\x1b]133;A\x07");
    let archived = term
        .last_completed_block()
        .cloned()
        .expect("archived block");
    assert_eq!(archived.exit_code, Some(0));
    let out = term.block_output_text(&archived);
    assert!(
        matches!(&out, BlockText::Text(t) if t.contains("OUT-1") && t.contains("OUT-2")),
        "sealed block reads its FULL multi-line output, got {out:?}",
    );
}

/// `text_range` joins soft-wrapped rows (like `selection`/`copy`): a 25-char
/// line wrapped across 3 rows of a 10-col terminal reads back whole.
#[test]
fn text_range_joins_soft_wrapped_rows() {
    let mut term = TerminalBuilder::new()
        .size(6, 10)
        .ring_buffer_size(100)
        .build();
    term.process(b"ABCDEFGHIJKLMNOPQRSTUVWXY");
    match term.text_range(0, 3) {
        BlockText::Text(t) => assert!(
            t.contains("ABCDEFGHIJKLMNOPQRSTUVWXY"),
            "soft-wrapped rows must be joined, not chopped at cols 10/20: {t:?}"
        ),
        other => panic!("expected text, got {other:?}"),
    }
    let mut t2 = TerminalBuilder::new().size(6, 10).build();
    t2.process(b"AAA\r\nBBB");
    match t2.text_range(0, 2) {
        BlockText::Text(t) => assert!(t.contains("AAA\nBBB"), "hard newline preserved: {t:?}"),
        other => panic!("expected text, got {other:?}"),
    }
}

/// A wrapped output line reads as the program printed it, on screen and from
/// history: a space the program printed in the last column of a wrapped row
/// is text that `blocktext` keeps at the join, while a line that ends in
/// blanks loses them, across a wrap too (a padded column wider than the
/// window reads as its text).
#[test]
fn block_output_reads_a_wrapped_line_as_printed() {
    for (lines, want) in [
        (["abcdefghi jklmn", "pad   "], "abcdefghi jklmn\npad"),
        (["foo            ", "bar"], "foo\nbar"),
    ] {
        let mut term = TerminalBuilder::new()
            .size(4, 10)
            .ring_buffer_size(100)
            .build();
        run_block(&mut term, "ls", &lines);
        term.process(b"\x1b]133;A\x07");
        let block = term.last_completed_block().cloned().expect("sealed block");
        let want = BlockText::Text(want.to_string());
        assert_eq!(term.block_output_text(&block), want, "on screen");

        for i in 0..6 {
            term.process(format!("more {i}\r\n").as_bytes());
        }
        assert_eq!(
            term.block_output_text(&block),
            want,
            "the same block read from history"
        );
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for scrollback erase (`ED 3`), full clears (`ED 2`), and prompt/command anchors.

use crate::terminal::{BlockState, BlockText, Terminal, TerminalBuilder};

const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

fn term() -> Terminal {
    TerminalBuilder::new()
        .size(8, 40)
        .ring_buffer_size(512)
        .build()
}

fn start(t: &mut Terminal, command: &str) {
    t.process(command.as_bytes());
    t.process(b"\r\n\x1b]133;C\x07");
}

fn finish(t: &mut Terminal, exit: i32) {
    t.process(format!("\x1b]133;D;{exit}\x07").as_bytes());
    t.process(PROMPT);
}

fn with_history() -> Terminal {
    let mut t = term();
    t.process(PROMPT);
    for i in 0..12 {
        start(&mut t, &format!("echo {i}"));
        t.process(format!("{i}\r\n").as_bytes());
        finish(&mut t, 0);
    }
    assert!(
        t.grid().scrollback_lines() > 0,
        "PRECONDITION: there is history to erase"
    );
    t
}

fn held(t: &Terminal) -> Vec<u64> {
    t.all_blocks().map(|b| b.id).collect()
}

fn probe(t: &Terminal, id: u64) -> Option<(BlockState, Option<i32>)> {
    t.block_by_id(id).map(|b| (b.state, b.exit_code))
}

#[test]
fn a_running_command_keeps_its_block_through_a_scrollback_erase() {
    enum Erase {
        Bytes(&'static [u8]),
        CmdK,
    }
    for (case, erase, takes_history, moves) in [
        ("ED 3", Erase::Bytes(b"\x1b[3J"), true, false),
        ("clear", Erase::Bytes(b"\x1b[H\x1b[2J\x1b[3J"), true, true),
        ("Cmd-K", Erase::CmdK, true, false),
        (
            "ED 2 (control)",
            Erase::Bytes(b"\x1b[H\x1b[2J"),
            false,
            true,
        ),
    ] {
        let mut t = with_history();
        start(&mut t, "sh -c 'exit 6'");
        finish(&mut t, 6);
        let id = t.current_block().expect("a prompt").id;
        let seq = t.completed_command_seq();
        start(&mut t, "sh -c 'printf \"\\033[3J\"; exit 5'");
        let top = t.grid().visible_to_absolute(0);
        let rows = if moves {
            Some((top, Some(top), Some(top)))
        } else {
            t.current_block()
                .map(|b| (b.prompt_start_row, b.command_start_row, b.output_start_row))
        };
        match erase {
            Erase::Bytes(bytes) => t.process(bytes),
            Erase::CmdK => t.clear_scrollback(),
        }
        assert_eq!(
            probe(&t, id),
            Some((BlockState::Executing, None)),
            "{case}: the running command's block outlives the erase"
        );
        finish(&mut t, 5);
        assert_eq!(
            probe(&t, id),
            Some((BlockState::Complete, Some(5))),
            "{case}: the D landed on the block with its exit code"
        );
        assert_eq!(
            t.last_completed_block().map(|b| (b.id, b.exit_code)),
            Some((id, Some(5))),
            "{case}: the next prompt archived it"
        );
        assert_eq!(t.current_block().map(|b| b.id), Some(id + 1), "{case}");
        assert_eq!(
            t.last_completed_command().and_then(|m| m.exit_code),
            Some(5),
            "{case}: the command mark carries the exit too"
        );
        assert_eq!(
            t.last_finished_block().map(|b| (b.id, b.exit_code)),
            Some((id, Some(5))),
            "{case}"
        );
        assert_eq!(t.completed_command_seq(), seq + 1, "{case}");
        assert_eq!(
            t.block_by_id(id).map(|b| (
                b.prompt_start_row,
                b.command_start_row,
                b.output_start_row
            )),
            rows,
            "{case}: an erase renumbers nothing, a full clear moves the rows it blanked"
        );
        let ids = held(&t);
        assert!(
            ids.windows(2).all(|w| w[1] == w[0] + 1),
            "{case}: one run of ids: {ids:?}"
        );
        assert_eq!(
            ids.first() != Some(&0),
            takes_history,
            "{case}: the blocks wholly in the history went with it, and only with an erase: {ids:?}"
        );
    }

    let mut t = with_history();
    let id = t.current_block().expect("a prompt").id;
    let seq = t.completed_command_seq();
    start(&mut t, "reset");
    t.process(b"\x1bc");
    finish(&mut t, 5);
    assert_eq!(probe(&t, id), None, "RIS took the running block");
    assert_eq!(t.completed_command_seq(), seq, "and its D was refused");
}

#[test]
fn a_scrollback_erase_takes_only_what_lay_wholly_in_the_history() {
    let mut t = term();
    t.add_named_mark("old");
    t.add_annotation("old");
    t.process(PROMPT);
    start(&mut t, "true");
    finish(&mut t, 0);
    let gone = t.current_block().expect("a prompt").id - 1;
    start(&mut t, "seq 20");
    let straddling = t.current_block().expect("running").id;
    for i in 1..=20 {
        t.process(format!("{i}\r\n").as_bytes());
    }
    t.add_named_mark("live");
    t.add_annotation("live");
    let before = t.block_by_id(straddling).cloned().expect("running");
    t.process(b"\x1b[3J");
    let oldest = t.grid().oldest_absolute_row();
    assert_eq!(
        probe(&t, gone),
        None,
        "the block wholly in the history went"
    );
    let block = t.block_by_id(straddling).cloned().expect("held");
    assert!(
        before.output_start_row.is_some_and(|row| row < oldest),
        "PRECONDITION: it straddles the erase"
    );
    assert_eq!(
        (block.id, block.state, block.exit_code),
        (before.id, before.state, None)
    );
    assert_eq!(
        rows(&block)[..3],
        [Some(oldest), Some(oldest), Some(oldest)],
        "it starts again at the oldest row left"
    );
    assert!(block.command_text_cleared);
    assert_eq!(t.block_command_text(&block), BlockText::Evicted);
    let (start_row, end_row) = before.output_rows().expect("output rows");
    assert_eq!(
        t.text_range(start_row, end_row),
        BlockText::Evicted,
        "NEGATIVE CONTROL: the rows it had are gone"
    );
    finish(&mut t, 0);
    let block = t.block_by_id(straddling).cloned().expect("held");
    assert!(forward(&block), "{:?}", rows(&block));
    let left: Vec<String> = ((oldest - 1)..=20).map(|i| i.to_string()).collect();
    assert_eq!(
        t.block_output_text(&block),
        BlockText::Text(left.join("\n")),
        "its output reads what is left of it"
    );
    let mark = t.last_completed_command().expect("its mark");
    assert_eq!(
        (mark.prompt_start_row, mark.output_start_row, mark.exit_code),
        (oldest, Some(oldest), Some(0)),
        "its command mark started again with it"
    );
    start(&mut t, "echo on-screen");
    t.process(b"on-screen\r\n");
    finish(&mut t, 0);
    let on_screen = t.last_completed_block().cloned().expect("archived");
    assert_eq!(
        t.block_command(&on_screen).as_deref(),
        Some("$ echo on-screen")
    );
    assert_eq!(t.block_output(&on_screen).as_deref(), Some("on-screen"));
    let names: Vec<_> = t
        .terminal_marks()
        .iter()
        .map(|m| m.name.as_deref())
        .collect();
    assert_eq!(names, [Some("live")], "only the mark on the screen stays");
    let notes: Vec<_> = t.annotations().iter().map(|a| a.message.as_str()).collect();
    assert_eq!(notes, ["live"], "only the annotation on the screen stays");
    assert!(
        t.command_marks()
            .iter()
            .all(|m| m.output_end_row.is_some_and(|r| r >= oldest)),
        "no completed command mark wholly in the history is held"
    );

    start(&mut t, "seq 20");
    let archived = t.current_block().expect("running").id;
    for i in 1..=20 {
        t.process(format!("{i}\r\n").as_bytes());
    }
    finish(&mut t, 0);
    let before = t.block_by_id(archived).cloned().expect("archived");
    assert_eq!(before.state, BlockState::Complete);
    t.process(b"\x1b[3J");
    let oldest = t.grid().oldest_absolute_row();
    assert!(
        before.output_start_row.is_some_and(|row| row < oldest)
            && before.end_row.is_some_and(|row| row > oldest),
        "PRECONDITION: its starts are in the history and its last rows on the screen"
    );
    assert_eq!(
        t.block_by_id(archived),
        Some(&before),
        "the finished straddler, as it was"
    );
    assert_eq!(held(&t), [archived, archived + 1]);
}

#[test]
fn an_erase_on_the_alternate_screen_leaves_every_anchor() {
    let mut t = with_history();
    t.add_named_mark("old");
    let blocks = held(&t);
    let marks = t.command_marks().len();
    start(&mut t, "less");
    let anchored_end = t
        .all_blocks()
        .flat_map(|b| {
            [
                Some(b.prompt_start_row),
                b.command_start_row,
                b.output_start_row,
                b.end_row,
            ]
        })
        .flatten()
        .max()
        .expect("blocks");
    t.process(b"\x1b[?1049h");
    for i in 0..400 {
        t.process(format!("{i}\r\n").as_bytes());
    }
    assert!(
        t.grid().oldest_absolute_row() > anchored_end,
        "PRECONDITION: the alt grid's oldest row is past every main-grid anchor"
    );
    t.process(b"\x1b[3J\x1b[?1049l");
    finish(&mut t, 0);
    let now = held(&t);
    assert!(now.starts_with(&blocks), "every block held: {now:?}");
    assert_eq!(t.command_marks().len(), marks + 1, "every mark held");
    assert_eq!(t.terminal_marks().len(), 1);
}

#[test]
fn the_last_finished_block_outlives_an_erase_of_its_rows() {
    for cmd_k in [true, false] {
        let mut t = term();
        t.process(PROMPT);
        start(&mut t, "sh -c 'exit 6'");
        finish(&mut t, 6);
        let id = t.current_block().expect("a prompt").id;
        start(&mut t, "seq 20; clear");
        t.process(&b"line\r\n".repeat(20));
        t.process(b"\x1b[H\x1b[2J");
        finish(&mut t, 3);
        assert_eq!(
            t.last_completed_block().map(|b| (b.id, b.exit_code)),
            Some((id, Some(3))),
            "CONTROL: the prompt archived it"
        );
        if cmd_k {
            t.clear_scrollback();
        } else {
            t.process(b"\x1b[3J");
        }
        assert_eq!(
            t.last_completed_block().map(|b| b.id),
            None,
            "cmd_k={cmd_k}: PRECONDITION: the erase took it, every row of it in the history"
        );
        assert_eq!(
            t.last_finished_block().map(|b| (b.id, b.exit_code)),
            Some((id, Some(3))),
            "cmd_k={cmd_k}: its exit is still read"
        );
        assert_eq!(t.current_block().map(|b| b.id), Some(id + 1));
    }
}

#[test]
fn a_full_clear_takes_the_text_of_the_blocks_it_blanked() {
    for (case, clear, blanks) in [
        ("clear", &b"\x1b[H\x1b[2J\x1b[3J"[..], true),
        ("Ctrl-L", b"\x1b[H\x1b[2J", true),
        ("ED 3 alone (control)", b"\x1b[3J", false),
        (
            "alt-screen ED 2 (control)",
            b"\x1b[?1049h\x1b[H\x1b[2J\x1b[?1049l",
            false,
        ),
    ] {
        let mut t = term();
        t.process(PROMPT);
        start(&mut t, "echo old");
        t.process(b"old-output\r\n");
        finish(&mut t, 7);
        let old = t.last_completed_block().expect("archived").id;
        start(&mut t, "clear");
        let clearing = t.current_block().expect("running").id;
        t.process(clear);
        finish(&mut t, 0);
        start(&mut t, "echo new");
        t.process(b"NEW-OUTPUT\r\n");
        finish(&mut t, 0);
        let new = t.last_completed_block().cloned().expect("archived");

        let block = t.block_by_id(old).cloned().expect("held");
        assert_eq!(
            (block.state, block.exit_code, block.text_cleared),
            (BlockState::Complete, Some(7), blanks),
            "{case}: the block and its exit stay"
        );
        if blanks {
            assert_eq!(
                block.output_rows().map(|(s, e)| t.text_range(s, e)),
                Some(BlockText::Text("NEW-OUTPUT".into())),
                "{case}: NEGATIVE CONTROL: its rows hold the next command's output"
            );
            assert_eq!(t.block_output_text(&block), BlockText::Evicted, "{case}");
            assert_eq!(t.block_command_text(&block), BlockText::Evicted, "{case}");
            assert_eq!(
                t.block_by_id(clearing).map(|b| b.text_cleared),
                Some(false),
                "{case}: the block being built is not marked"
            );
            assert_eq!(
                t.block_command(&new).as_deref(),
                Some("$ echo new"),
                "{case}"
            );
            assert_eq!(
                t.block_output(&new).as_deref(),
                Some("NEW-OUTPUT"),
                "{case}"
            );
            let mut carried = term();
            carried.restore_checkpoint(&t.checkpoint());
            let block = carried.block_by_id(old).cloned().expect("carried");
            assert!(block.text_cleared, "{case}: the checkpoint carries it");
            assert_eq!(carried.block_output_text(&block), BlockText::Evicted);
        } else {
            assert_eq!(
                t.block_output(&block).as_deref(),
                Some("old-output"),
                "{case}"
            );
            assert_eq!(
                t.block_command(&block).as_deref(),
                Some("$ echo old"),
                "{case}"
            );
        }
        let ids = held(&t);
        assert_eq!(ids, (old..=new.id + 1).collect::<Vec<_>>(), "{case}");
    }
}

fn start_named(t: &mut Terminal, command: &str) {
    t.process(command.as_bytes());
    let named = command.replace('\\', "\\\\").replace(';', "\\x3b");
    t.process(format!("\r\n\x1b]633;E;{named}\x07\x1b]133;C\x07").as_bytes());
}

fn rows(block: &crate::terminal::OutputBlock) -> [Option<u64>; 4] {
    [
        Some(block.prompt_start_row),
        block.command_start_row,
        block.output_start_row,
        block.end_row,
    ]
}

fn forward(block: &crate::terminal::OutputBlock) -> bool {
    let named: Vec<u64> = rows(block).into_iter().flatten().collect();
    named.windows(2).all(|w| w[0] <= w[1])
}

const FULL_CLEARS: [(&str, &[u8]); 5] = [
    ("clear", b"\x1b[H\x1b[2J\x1b[3J"),
    ("clear, erase first", b"\x1b[3J\x1b[H\x1b[2J"),
    ("home ED 2", b"\x1b[H\x1b[2J"),
    ("home ED 0", b"\x1b[H\x1b[J"),
    ("ED 2 then home", b"\x1b[2J\x1b[H"),
];

fn erases(clear: &[u8]) -> bool {
    clear.windows(4).any(|w| w == b"\x1b[3J")
}

#[test]
fn a_full_clear_moves_a_running_block_on_the_screen_to_its_home_row() {
    for (case, clear) in FULL_CLEARS {
        for (history, named) in [(false, false), (true, false), (true, true)] {
            let case = format!("{case} history={history} 633;E={named}");
            let mut t = if history { with_history() } else { term() };
            if !history {
                t.process(PROMPT);
            }
            let command = "clear; sh -c 'exit 4'";
            if named {
                start_named(&mut t, command);
            } else {
                start(&mut t, command);
            }
            let id = t.current_block().expect("running").id;
            t.process(b"before-1\r\nbefore-2\r\n");
            let before = t.current_block().cloned().expect("running");
            let top = t.grid().visible_to_absolute(0);
            assert!(
                before.prompt_start_row >= top,
                "{case}: PRECONDITION: the whole block is on the screen"
            );
            t.process(clear);
            t.process(b"after-1\r\nafter-2\r\n");
            finish(&mut t, 4);

            let block = t.block_by_id(id).cloned().expect("held");
            assert_eq!(
                (block.state, block.exit_code),
                (BlockState::Complete, Some(4)),
                "{case}: the D landed on the block"
            );
            assert_eq!(
                t.last_completed_block().map(|b| b.id),
                Some(id),
                "{case}: the next prompt archived it"
            );
            assert_eq!(
                rows(&block)[..3],
                [Some(top), Some(top), Some(top)],
                "{case}: it moved to the clear's home row"
            );
            assert!(forward(&block), "{case}: {:?}", rows(&block));
            assert_eq!(
                t.block_output_text(&block),
                BlockText::Text("after-1\nafter-2".into()),
                "{case}: its output is what followed the clear"
            );
            assert_eq!(
                t.block_command_text(&block),
                if named {
                    BlockText::Text(command.into())
                } else {
                    BlockText::Evicted
                },
                "{case}"
            );
            assert_eq!(
                (block.text_cleared, block.command_text_cleared),
                (false, true),
                "{case}"
            );
            let mark = t.last_completed_command().expect("the command's mark");
            assert_eq!(mark.exit_code, Some(4), "{case}");
            assert!(
                mark.output_start_row <= mark.output_end_row && mark.output_start_row >= Some(top),
                "{case}: the mark moved with it: {mark:?}"
            );
            start(&mut t, "echo next");
            t.process(b"next\r\n");
            finish(&mut t, 0);
            let next = t.last_completed_block().cloned().expect("archived");
            assert_eq!(next.id, id + 1, "{case}");
            assert_eq!(t.block_command(&next).as_deref(), Some("$ echo next"));
            assert_eq!(t.block_output(&next).as_deref(), Some("next"), "{case}");
            if !history {
                let (start, end) = before.command_rows().expect("command rows");
                assert_eq!(
                    t.text_range(start, end),
                    BlockText::Text("after-1".into()),
                    "{case}: PRECONDITION: the rows it had read another line"
                );
            }
        }
    }
}

#[test]
fn a_full_clear_keeps_the_running_blocks_rows_in_the_history() {
    for (case, clear) in FULL_CLEARS {
        let mut t = term();
        t.process(PROMPT);
        let command = "seq 40; tput clear; echo done";
        start(&mut t, command);
        let id = t.current_block().expect("running").id;
        for i in 1..=40 {
            t.process(format!("L{i}\r\n").as_bytes());
        }
        let before = t.current_block().cloned().expect("running");
        let top = t.grid().visible_to_absolute(0);
        assert!(
            before.output_start_row.is_some_and(|row| row + 1 < top),
            "{case}: PRECONDITION: its first lines are in the history"
        );
        t.process(clear);
        t.process(b"done\r\n");
        finish(&mut t, 0);
        let block = t.block_by_id(id).cloned().expect("held");
        assert_eq!(
            (block.state, block.exit_code),
            (BlockState::Complete, Some(0)),
            "{case}"
        );
        assert!(forward(&block), "{case}: {:?}", rows(&block));
        assert!(!block.text_cleared, "{case}");
        let mark = t.last_completed_command().cloned().expect("its mark");
        assert_eq!(mark.exit_code, Some(0), "{case}");
        let output = t.block_output(&block).expect("its output");
        assert!(!output.contains("L34"), "{case}: {output:?}");
        if erases(clear) {
            assert_eq!(
                rows(&block)[..3],
                [Some(top), Some(top), Some(top)],
                "{case}: the erase took its rows"
            );
            assert_eq!(mark.prompt_start_row, top, "{case}");
            assert_eq!(output, "done", "{case}");
            assert_eq!(t.block_command_text(&block), BlockText::Evicted, "{case}");
        } else {
            assert_eq!(
                rows(&block)[..3],
                rows(&before)[..3],
                "{case}: its rows in the history stay"
            );
            assert_eq!(
                mark.prompt_start_row, before.prompt_start_row,
                "{case}: the prompt jump still reaches its prompt"
            );
            let mut lines: Vec<String> = (1..top).map(|i| format!("L{i}")).collect();
            lines.push("done".into());
            assert_eq!(output, lines.join("\n"), "{case}");
            assert_eq!(
                t.block_command(&block),
                Some(format!("$ {command}")),
                "{case}"
            );
            assert!(!block.command_text_cleared, "{case}");
        }
        assert_eq!(
            t.text_range(top, top + 1),
            BlockText::Text("done".into()),
            "{case}: NEGATIVE CONTROL: the clear blanked the screen's rows"
        );
    }
}

const ZSH_PROMPT: &[u8] = b"\x1b]133;A\x07\r\x1b[J$ \x1b[K\x1b]133;B\x07";

#[test]
fn clear_then_exit_4_leaves_an_empty_output_range_not_a_backward_one() {
    for history in [false, true] {
        let mut t = if history { with_history() } else { term() };
        if !history {
            t.process(PROMPT);
            for i in 0..3 {
                start(&mut t, &format!("echo {i}"));
                t.process(format!("{i}\r\n").as_bytes());
                finish(&mut t, 0);
            }
        }
        let earlier = t.last_completed_block().cloned().expect("archived");
        assert_eq!(
            t.block_output(&earlier).as_deref(),
            Some(if history { "11" } else { "2" }),
            "PRECONDITION: the block before it has text on the screen"
        );
        start(&mut t, "clear; sh -c 'exit 4'");
        let before = t.current_block().cloned().expect("running");
        let top = t.grid().visible_to_absolute(0);
        t.process(b"\x1b[3J\x1b[H\x1b[2J");
        t.process(b"\x1b]133;D;4\x07");
        t.process(ZSH_PROMPT);
        let block = t.last_completed_block().cloned().expect("archived");
        assert_eq!(
            (block.id, block.exit_code),
            (before.id, Some(4)),
            "history={history}"
        );
        assert_eq!(block.output_rows(), Some((top, top)), "history={history}");
        assert!(forward(&block), "history={history}: {:?}", rows(&block));
        assert_eq!(t.block_output_text(&block), BlockText::Text(String::new()));
        assert!(!block.text_cleared, "history={history}");
        assert_eq!(
            t.block_by_id(earlier.id).map(|b| b.text_cleared),
            Some(true),
            "history={history}: the block before it had text on the cleared screen"
        );
        assert!(
            before.output_start_row > block.end_row,
            "history={history}: NEGATIVE CONTROL: its old output start {:?} is past its end {:?}",
            before.output_start_row,
            block.end_row
        );
        assert!(
            super::block_rows_end(&block) > top,
            "history={history}: NEGATIVE CONTROL: its rows reach the cleared screen"
        );
    }
}

#[test]
fn a_full_clear_at_the_prompt_starts_it_where_the_shell_redraws_it() {
    for (case, prompt) in [("one line", "$ "), ("two lines", "~/dir on main\r\n$ ")] {
        let mut t = with_history();
        t.process(format!("\r\n\x1b]133;A\x07{prompt}\x1b]133;B\x07").as_bytes());
        let before = t.current_block().cloned().expect("the prompt");
        t.process(b"echo h");
        let top = t.grid().visible_to_absolute(0);
        t.process(format!("\x1b[H\x1b[2J{prompt}echo h").as_bytes());
        start(&mut t, "i");
        t.process(b"hi\r\n");
        finish(&mut t, 0);
        let block = t.block_by_id(before.id).cloned().expect("held");
        let below = if prompt.contains('\n') { 1 } else { 0 };
        assert_eq!(
            rows(&block)[..3],
            [Some(top), Some(top + below), Some(top + below + 1)],
            "{case}"
        );
        assert!(forward(&block), "{case}: {:?}", rows(&block));
        assert_eq!(
            t.block_command(&block).as_deref(),
            Some("$ echo hi"),
            "{case}"
        );
        assert_eq!(t.block_output(&block).as_deref(), Some("hi"), "{case}");
        assert_eq!(
            (block.text_cleared, block.command_text_cleared),
            (false, false),
            "{case}"
        );
        let mark = t.last_completed_command().expect("its mark");
        assert_eq!(
            (mark.prompt_start_row, mark.command_start_row),
            (top, Some(top + below)),
            "{case}: the mark started again with it"
        );
        assert!(
            before.command_start_row > block.output_start_row,
            "{case}: NEGATIVE CONTROL: its old command row {:?} is past its output start {:?}",
            before.command_start_row,
            block.output_start_row
        );
        if below > 0 {
            assert_eq!(
                t.text_range(top, top + 1),
                BlockText::Text("~/dir on main".into()),
                "{case}: NEGATIVE CONTROL: the top row is the prompt's"
            );
        }
    }
}

fn mark_forward(mark: &crate::terminal::CommandMark) -> bool {
    let named: Vec<u64> = [
        Some(mark.prompt_start_row),
        mark.command_start_row,
        mark.output_start_row,
        mark.output_end_row,
    ]
    .into_iter()
    .flatten()
    .collect();
    named.windows(2).all(|w| w[0] <= w[1])
}

#[test]
fn a_full_clear_at_the_prompt_keeps_its_rows_in_order_however_it_is_redrawn() {
    for hook_lines in [1_u64, 2] {
        let case = format!("{hook_lines} hook lines");
        let mut t = with_history();
        let hook = "hook\r\n".repeat(usize::try_from(hook_lines).expect("small"));
        t.process(format!("\r\n\x1b]133;A\x07{hook}$ \x1b]133;B\x07").as_bytes());
        let before = t.current_block().cloned().expect("the prompt");
        t.process(b"echo h");
        let top = t.grid().visible_to_absolute(0);
        t.process(b"\x1b[H\x1b[2J$ echo h");
        t.process(b"i\r\n\x1b]633;E;echo hi\x07\x1b]133;C\x07hi\r\n");
        finish(&mut t, 0);
        let block = t.block_by_id(before.id).cloned().expect("held");
        assert!(forward(&block), "{case}: {:?}", rows(&block));
        assert_eq!(
            rows(&block)[..3],
            [Some(top), Some(top + 1), Some(top + 1)],
            "{case}"
        );
        assert_eq!(
            t.block_command_text(&block),
            BlockText::Text("echo hi".into()),
            "{case}"
        );
        assert_eq!(t.block_output(&block).as_deref(), Some("hi"), "{case}");
        let mark = t.last_completed_command().expect("its mark");
        assert!(mark_forward(mark), "{case}: {mark:?}");
        assert_eq!(
            (mark.prompt_start_row, mark.command_start_row),
            (top, Some(top + 1)),
            "{case}"
        );
        assert_eq!(
            before.command_start_row,
            Some(before.prompt_start_row + hook_lines),
            "{case}: NEGATIVE CONTROL: the B was as far below the A as the hook's lines, \
             so a command row that far below the redrawn prompt is at or past the C's row"
        );
    }

    let prompt = "~/dir on main\r\n$ ";
    let mut t = with_history();
    t.process(format!("\r\n\x1b]133;A\x07{prompt}\x1b]133;B\x07").as_bytes());
    let edited = t.current_block().cloned().expect("the prompt");
    t.process(b"echo h");
    let top = t.grid().visible_to_absolute(0);
    t.process(format!("\x1b[H\x1b[2J\x1b]133;A\x07{prompt}\x1b]133;B\x07echo h").as_bytes());
    let archived = t.block_by_id(edited.id).cloned().expect("held");
    assert_eq!(archived.state, BlockState::EnteringCommand, "fish");
    assert!(forward(&archived), "fish: {:?}", rows(&archived));
    assert_eq!(
        rows(&archived),
        [Some(top), Some(top), None, Some(top)],
        "fish"
    );
    start(&mut t, "i");
    t.process(b"hi\r\n");
    finish(&mut t, 0);
    let block = t
        .block_by_id(edited.id + 1)
        .cloned()
        .expect("the redrawn one");
    assert_eq!(
        rows(&block)[..3],
        [Some(top), Some(top + 1), Some(top + 2)],
        "fish"
    );
    assert_eq!(
        t.block_command(&block).as_deref(),
        Some("$ echo hi"),
        "fish"
    );
    assert_eq!(t.block_output(&block).as_deref(), Some("hi"), "fish");
    assert!(
        top + 1 > archived.end_row.expect("archived"),
        "fish: NEGATIVE CONTROL: the restart's command row is past the redraw's A"
    );

    for (case, clear) in [
        ("ED 2 in place", &b"\x1b[2J"[..]),
        ("ED 2 then home", b"\x1b[2J\x1b[H"),
    ] {
        let mut t = with_history();
        t.process(b"\r\n");
        t.process(PROMPT);
        let before = t.current_block().cloned().expect("the prompt");
        let row = before.command_start_row.expect("its B");
        t.process(b"echo h");
        let top = t.grid().visible_to_absolute(0);
        t.process(clear);
        start(&mut t, "i");
        t.process(b"hi\r\n");
        finish(&mut t, 0);
        let block = t.block_by_id(before.id).cloned().expect("held");
        assert!(forward(&block), "{case}: {:?}", rows(&block));
        let mark = t.last_completed_command().expect("its mark");
        assert!(mark_forward(mark), "{case}: {mark:?}");
        assert!(!block.text_cleared && !block.command_text_cleared, "{case}");
        if clear.len() == 4 {
            assert_eq!(
                rows(&block)[..3],
                [Some(before.prompt_start_row), Some(row), Some(row + 1)],
                "{case}: the prompt keeps its rows"
            );
            assert_eq!(
                (mark.prompt_start_row, mark.command_start_row),
                (before.prompt_start_row, Some(row)),
                "{case}"
            );
            assert_eq!(
                t.block_command_text(&block),
                BlockText::Text("        i".into()),
                "{case}"
            );
            let restarted = t.text_range(top, row + 1);
            assert_eq!(
                restarted,
                BlockText::Text(format!(
                    "{}        i",
                    "\n".repeat(usize::try_from(row - top).expect("small"))
                )),
                "{case}: NEGATIVE CONTROL: restarted at the top row it reads blank lines"
            );
            assert!(
                row > top,
                "{case}: PRECONDITION: its row is not the top one"
            );
        } else {
            assert_eq!(
                rows(&block)[..3],
                [Some(top + 1), Some(top + 1), Some(top + 1)],
                "{case}: the C brought its rows up to its own"
            );
            assert!(
                before.prompt_start_row > top + 1,
                "{case}: NEGATIVE CONTROL: the row it had is past where the C came"
            );
        }
    }
}

#[test]
fn ctrl_l_at_a_prompt_taller_than_the_screen_finds_its_command_row() {
    let prompt: String = (1..=9).map(|i| format!("p{i}\r\n")).collect::<String>() + "$ ";
    let mut t = with_history();
    t.process(format!("\r\n\x1b]133;A\x07{prompt}\x1b]133;B\x07").as_bytes());
    let before = t.current_block().cloned().expect("the prompt");
    t.process(b"echo h");
    let top = t.grid().visible_to_absolute(0);
    let bottom = t.grid().visible_to_absolute(t.grid().rows() - 1);
    t.process(format!("\x1b[H\x1b[2J{prompt}echo h").as_bytes());
    start(&mut t, "i");
    t.process(b"hi\r\n");
    finish(&mut t, 0);
    let block = t.block_by_id(before.id).cloned().expect("held");
    assert_eq!(
        rows(&block)[..3],
        [Some(top), Some(top + 9), Some(top + 10)]
    );
    assert!(forward(&block), "{:?}", rows(&block));
    assert_eq!(t.block_command(&block).as_deref(), Some("$ echo hi"));
    assert_eq!(t.block_output(&block).as_deref(), Some("hi"));
    assert!(
        top + 9 > bottom,
        "PRECONDITION: its B is past the screen's last row"
    );
    assert_eq!(
        t.text_range(bottom, bottom + 1),
        BlockText::Text("p8".into()),
        "NEGATIVE CONTROL: the screen's last row at the clear is a prompt line"
    );
}

#[test]
fn an_empty_command_range_loses_no_command_text() {
    for (case, lines, erase) in [
        ("full clear", 2, &b"\x1b[H\x1b[2J"[..]),
        ("ED 3", 20, b"\x1b[3J"),
    ] {
        let mut t = term();
        t.process(b"motd\r\n");
        t.process(PROMPT);
        t.process(b"make\x1b]133;C\x07\r\n");
        let before = t.current_block().cloned().expect("running");
        let row = before.command_start_row.expect("its B");
        assert_eq!(
            before.command_rows(),
            Some((row, row)),
            "{case}: PRECONDITION: B and C on one row"
        );
        t.process(&b"building\r\n".repeat(lines));
        let top = t.grid().visible_to_absolute(0);
        t.process(erase);
        let block = t.current_block().cloned().expect("running");
        assert!(!block.command_text_cleared, "{case}");
        assert_eq!(
            t.block_command_text(&block),
            BlockText::Text(String::new()),
            "{case}"
        );
        if lines == 2 {
            assert!(
                row > top,
                "{case}: NEGATIVE CONTROL: the clear blanked its row"
            );
        } else {
            assert!(
                row < t.grid().oldest_absolute_row(),
                "{case}: NEGATIVE CONTROL: the erase took its row"
            );
        }
    }
}

#[test]
fn an_erase_after_the_d_leaves_the_finished_block_as_it_was() {
    for after_d in [true, false] {
        let mut t = term();
        t.process(PROMPT);
        start(&mut t, "seq 20");
        let id = t.current_block().expect("running").id;
        for i in 1..=20 {
            t.process(format!("L{i}\r\n").as_bytes());
        }
        if after_d {
            t.process(b"\x1b]133;D;0\x07");
        }
        let before = t.current_block().cloned().expect("held");
        t.clear_scrollback();
        let oldest = t.grid().oldest_absolute_row();
        assert!(
            before.prompt_start_row < oldest,
            "after_d={after_d}: PRECONDITION: the erase took its first rows"
        );
        if after_d {
            assert_eq!(
                t.current_block(),
                Some(&before),
                "the erase left the finished block as it was"
            );
        } else {
            t.process(b"\x1b]133;D;0\x07");
        }
        t.process(PROMPT);
        let block = t.block_by_id(id).cloned().expect("held");
        let left: Vec<String> = (oldest..=20).map(|i| format!("L{i}")).collect();
        if after_d {
            assert_eq!(rows(&block)[..3], rows(&before)[..3], "after_d={after_d}");
            assert!(!block.command_text_cleared, "after_d={after_d}");
            assert_eq!(t.block_output_text(&block), BlockText::Evicted);
            assert_eq!(
                t.text_range(oldest, block.end_row.expect("closed")),
                BlockText::Text(left.join("\n")),
                "its last lines are still on the screen"
            );
        } else {
            assert_eq!(rows(&block)[..3], [Some(oldest); 3], "after_d={after_d}");
            assert!(block.command_text_cleared, "after_d={after_d}");
            assert_eq!(t.block_output(&block), Some(left.join("\n")));
            assert_eq!(t.block_command_text(&block), BlockText::Evicted);
        }
    }
}

#[test]
fn a_carry_keeps_the_block_a_clear_moved_above_the_one_before_it() {
    for (case, ctrl_l) in [("clear; seq 300", false), ("Ctrl-L, seq 300", true)] {
        let mut t = with_history();
        start(&mut t, "echo a");
        t.process(b"a\r\n");
        finish(&mut t, 0);
        let echo_a = t.current_block().map(|b| b.id - 1).expect("a prompt");
        let echo_a = t.block_by_id(echo_a).cloned().expect("echo a");
        let prompt_before = t.current_block().expect("the prompt").prompt_start_row;
        if ctrl_l {
            t.process(b"\x1b[H\x1b[2J$ ");
            start(&mut t, "seq 300");
        } else {
            start(&mut t, "clear; seq 300");
            t.process(b"\x1b[H\x1b[2J");
        }
        let id = t.current_block().expect("running").id;
        for i in 1..=300 {
            t.process(format!("{i}\r\n").as_bytes());
        }
        finish(&mut t, 3);
        let cleared = t.block_by_id(id).cloned().expect("held");
        let top = t.grid().visible_to_absolute(0);
        let live = held(&t);
        let history_max = t.grid().scrollback_lines();
        let names_a_line_from = |b: &crate::terminal::OutputBlock, oldest: u64| {
            rows(b)[..3].iter().flatten().any(|&row| row >= oldest)
                || b.end_row.is_some_and(|end| end > oldest)
        };
        let mut in_window = 0;
        for history in 0..=history_max + 1 {
            let oldest = top - u64::try_from(history.min(history_max)).expect("small");
            let carry = t.checkpoint_carry(history).expect("at ground");
            let mut carried = term();
            carried.restore_checkpoint(&carry);
            let from = t
                .all_blocks()
                .position(|b| names_a_line_from(b, oldest))
                .expect("the prompt is on the screen");
            assert_eq!(held(&carried), live[from..], "{case}: history {history}");
            assert_eq!(
                carried.block_by_id(id).map(|b| (b.state, b.exit_code)),
                Some((BlockState::Complete, Some(3))),
                "{case}: history {history}: the cleared block travels with its exit code"
            );
            assert!(
                carried
                    .command_marks()
                    .iter()
                    .any(|m| m.exit_code == Some(3)),
                "{case}: history {history}: and with its command mark"
            );
            for b in t.all_blocks().filter(|b| b.prompt_start_row >= oldest) {
                assert!(
                    carried.block_by_id(b.id).is_some(),
                    "{case}: history {history}: block {} has its prompt row held",
                    b.id
                );
            }
            if oldest > echo_a.prompt_start_row && oldest <= prompt_before {
                in_window += 1;
                assert!(
                    cleared.prompt_start_row < oldest,
                    "{case}: NEGATIVE CONTROL: its prompt row {} is older than the carry's {oldest}",
                    cleared.prompt_start_row
                );
            }
        }
        assert!(
            in_window > 0 && prompt_before > echo_a.prompt_start_row,
            "{case}: PRECONDITION: some bound ends between echo a's prompt and the cleared one's"
        );
    }
}

#[test]
fn a_command_that_homes_the_cursor_keeps_its_rows() {
    let mut t = with_history();
    start(&mut t, "tput cup 2 0; echo hello");
    let before = t.current_block().cloned().expect("running");
    t.process(b"\x1b[3;1Hhello\r\n");
    let a_row = t.grid().visible_to_absolute(t.grid().cursor().row);
    finish(&mut t, 0);
    let block = t.block_by_id(before.id).cloned().expect("held");
    assert_eq!(rows(&block)[..3], rows(&before)[..3], "its rows stay");
    assert_eq!(
        block.end_row,
        Some(a_row),
        "it ends where the next prompt starts"
    );
    assert_eq!(
        t.block_command_text(&block),
        BlockText::Text("$ tput cup 2 0; echo hello".into())
    );
    let mark = t.last_completed_command().expect("its mark");
    assert_eq!(
        (
            mark.prompt_start_row,
            mark.command_start_row,
            mark.output_start_row
        ),
        (
            block.prompt_start_row,
            block.command_start_row,
            block.output_start_row
        ),
        "the block and its command mark name the same rows"
    );
    start(&mut t, "seq 20");
    for i in 1..=20 {
        t.process(format!("{i}\r\n").as_bytes());
    }
    finish(&mut t, 0);
    let top = t.grid().visible_to_absolute(0);
    assert!(
        before.prompt_start_row < top,
        "PRECONDITION: its prompt row is in the history"
    );
    let history = usize::try_from(top - before.prompt_start_row).expect("small");
    let carry = t.checkpoint_carry(history).expect("at ground");
    let mut carried = term();
    carried.restore_checkpoint(&carry);
    assert_eq!(
        carried.block_by_id(block.id).map(rows),
        Some(rows(&block)),
        "a carry that holds its prompt row keeps it"
    );
    assert!(
        a_row < block.command_start_row.expect("its B"),
        "NEGATIVE CONTROL: the next prompt's A {a_row} is above its command row"
    );
}

#[test]
fn a_clear_after_the_d_reads_the_end_off_the_blocks_own_mark() {
    use crate::terminal::{CommandMark, OutputBlock};
    let top = 100;
    let mark = |prompt: u64, end: u64| {
        let mut m = CommandMark::new(prompt, 0);
        m.command_start_row = Some(prompt);
        m.output_start_row = Some(prompt + 1);
        m.output_end_row = Some(end);
        m.exit_code = Some(0);
        m
    };
    for own in [true, false] {
        let mut shell = super::super::ShellIntegrationState::new();
        shell.command_marks.push_back(mark(10, 12));
        if own {
            shell.command_marks.push_back(mark(90, 95));
        }
        let mut block = OutputBlock::new(1, 90, 0);
        block.command_start_row = Some(90);
        block.output_start_row = Some(91);
        block.exit_code = Some(0);
        block.state = BlockState::Complete;
        shell.current_block = Some(block);
        super::note_full_clear(top, true, &mut shell);
        assert_eq!(
            shell.current_block.map(|b| b.text_cleared),
            Some(!own),
            "own mark: {own}"
        );
    }
}

#[test]
fn a_full_clear_after_the_d_takes_the_finished_commands_text() {
    let mut t = with_history();
    start(&mut t, "make");
    t.process(b"built\r\n");
    t.process(b"\x1b]133;D;0\x07");
    let id = t.current_block().expect("finished").id;
    t.process(b"\x1b[H\x1b[2J");
    t.process(b"precmd-line\r\n");
    t.process(PROMPT);
    let block = t.block_by_id(id).cloned().expect("held");
    assert_eq!(
        (block.state, block.exit_code),
        (BlockState::Complete, Some(0))
    );
    assert!(block.text_cleared);
    assert!(forward(&block), "{:?}", rows(&block));
    assert_eq!(t.block_output_text(&block), BlockText::Evicted);
    assert_eq!(t.block_command_text(&block), BlockText::Evicted);
    let (start, end) = block.output_rows().expect("output rows");
    assert_eq!(
        t.text_range(start, end),
        BlockText::Text("precmd-line".into()),
        "NEGATIVE CONTROL: its rows hold the shell's line"
    );
}

#[test]
fn a_full_clear_after_the_d_keeps_a_finished_output_it_never_reached() {
    for reached in [false, true] {
        let mut t = term();
        t.process(PROMPT);
        let lines = if reached { 40 } else { 3 };
        start(&mut t, &format!("seq {lines}"));
        let id = t.current_block().expect("running").id;
        for i in 1..=lines {
            t.process(format!("L{i}\r\n").as_bytes());
        }
        t.process(b"\x1b]133;D;0\x07");
        if !reached {
            t.process(&b"hook\r\n".repeat(10));
        }
        let before = t.current_block().cloned().expect("finished");
        let top = t.grid().visible_to_absolute(0);
        t.process(b"\x1b[H\x1b[2J");
        t.process(PROMPT);
        let block = t.block_by_id(id).cloned().expect("held");
        assert_eq!(
            (block.state, block.exit_code),
            (BlockState::Complete, Some(0)),
            "reached={reached}"
        );
        assert_eq!(
            rows(&block),
            [
                Some(before.prompt_start_row),
                before.command_start_row,
                before.output_start_row,
                Some(top)
            ],
            "reached={reached}: its rows stay, the prompt at the top closes it"
        );
        assert!(forward(&block), "reached={reached}: {:?}", rows(&block));
        assert_eq!(block.text_cleared, reached);
        assert_eq!(
            t.last_completed_command().map(|m| m.prompt_start_row),
            Some(before.prompt_start_row),
            "reached={reached}"
        );
        let (start, end) = block.output_rows().expect("output rows");
        if reached {
            assert_eq!(t.block_output_text(&block), BlockText::Evicted);
            assert_eq!(t.block_command_text(&block), BlockText::Evicted);
            let cut: Vec<String> = (1..top).map(|i| format!("L{i}")).collect();
            assert_eq!(
                t.text_range(start, end),
                BlockText::Text(cut.join("\n")),
                "NEGATIVE CONTROL: its rows read a cut copy"
            );
        } else {
            let output = t.block_output(&block).expect("its output");
            assert!(output.starts_with("L1\nL2\nL3\nhook"), "{output:?}");
            assert_eq!(t.block_command(&block).as_deref(), Some("$ seq 3"));
        }
    }
}

#[test]
fn a_program_that_redraws_reads_only_its_last_frame() {
    for (case, erase) in [
        ("clear", &b"\x1b[H\x1b[2J\x1b[3J"[..]),
        ("clear, erase first", b"\x1b[3J\x1b[H\x1b[2J"),
        ("ED 3 alone (control)", b"\x1b[3J"),
    ] {
        let full = erase.len() > 4;
        let mut t = term();
        t.process(PROMPT);
        start(&mut t, "tui");
        let id = t.current_block().expect("running").id;
        t.process(&b"scroll\r\n".repeat(20));
        t.process(erase);
        t.process(b"frame-1\r\n");
        t.process(&b"scroll\r\n".repeat(20));
        t.process(erase);
        t.process(b"frame-2\r\n");
        let running = t.block_by_id(id).cloned().expect("held");
        assert_eq!(
            t.block_output_text(&running),
            BlockText::Text(if full { "frame-2" } else { "scroll" }.into()),
            "{case}: while it runs, its first row"
        );
        finish(&mut t, 0);
        let block = t.block_by_id(id).cloned().expect("held");
        assert!(forward(&block), "{case}: {:?}", rows(&block));
        let expected = if full {
            "frame-2".to_string()
        } else {
            let mut lines = vec!["scroll"; 7];
            lines.push("frame-2");
            lines.join("\n")
        };
        assert_eq!(t.block_output(&block), Some(expected), "{case}");
        assert!(block.command_text_cleared, "{case}");
        assert_eq!(t.block_command_text(&block), BlockText::Evicted, "{case}");
        let mut carried = term();
        carried.restore_checkpoint(&t.checkpoint());
        let restored = carried.block_by_id(id).cloned().expect("carried");
        assert_eq!(restored, block, "{case}: the checkpoint carries it");
        assert_eq!(carried.block_command_text(&restored), BlockText::Evicted);
    }
}

#[test]
fn a_full_clear_on_the_alternate_screen_leaves_the_running_block() {
    let mut t = term();
    t.process(PROMPT);
    start(&mut t, "vim");
    t.process(b"opening\r\n");
    let before = t.current_block().cloned().expect("running");
    t.process(b"\x1b[?1049h\x1b[H\x1b[2Jbuffer\x1b[?1049l");
    assert_eq!(t.current_block(), Some(&before));
    finish(&mut t, 0);
    let block = t.last_completed_block().cloned().expect("archived");
    assert_eq!(t.block_command(&block).as_deref(), Some("$ vim"));
    assert_eq!(t.block_output(&block).as_deref(), Some("opening"));
}

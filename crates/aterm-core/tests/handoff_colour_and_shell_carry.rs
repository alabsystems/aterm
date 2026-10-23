// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that in-session handoff carries application colours, shell integration, and history numbering.

use aterm_core::config::TerminalConfig;
use aterm_core::terminal::{Rgb, Terminal};

const ROWS: u16 = 12;
const COLS: u16 = 60;
const CARRY: usize = 256;

fn theme(fg: Rgb) -> TerminalConfig {
    TerminalConfig {
        default_foreground: fg,
        allow_palette_reconfigure: true,
        ..TerminalConfig::default()
    }
}

fn outgoing() -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    t.apply_config(&theme(Rgb {
        r: 200,
        g: 200,
        b: 200,
    }));
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07\x1b]11;rgb:10/11/12\x07");
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a b c\r\n\x1b]133;D;0\x07");
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07building\r\n");
    assert_eq!(t.completed_command_seq(), 1);
    t
}

fn successor() -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    t.apply_config(&theme(Rgb {
        r: 90,
        g: 90,
        b: 90,
    }));
    t
}

#[test]
fn colours_the_application_set_survive_over_the_new_theme() {
    let cp = outgoing()
        .checkpoint_carry(CARRY)
        .expect("parser is Ground");
    let mut next = successor();
    next.restore_checkpoint(&cp);

    assert_eq!(
        next.palette_color(4),
        Rgb {
            r: 0x28,
            g: 0x2a,
            b: 0x36
        },
        "the OSC 4 override survives"
    );
    assert_eq!(
        next.default_background(),
        Rgb {
            r: 0x10,
            g: 0x11,
            b: 0x12
        },
        "the OSC 11 background survives"
    );
    assert_eq!(
        next.default_foreground(),
        Rgb {
            r: 90,
            g: 90,
            b: 90
        },
        "an un-overridden slot keeps the SUCCESSOR's theme, not the old one"
    );
    assert_eq!(
        next.palette_color(5),
        successor().palette_color(5),
        "an un-overridden palette entry is the successor's"
    );

    let mut cells_only = cp.clone();
    cells_only.color = Default::default();
    let mut old = successor();
    old.restore_checkpoint(&cells_only);
    assert_ne!(old.palette_color(4), next.palette_color(4));
}

#[test]
fn a_command_running_across_the_handoff_completes_after_it() {
    let cp = outgoing()
        .checkpoint_carry(CARRY)
        .expect("parser is Ground");
    let mut next = successor();
    next.restore_checkpoint(&cp);

    let finished: Vec<_> = next.all_blocks().filter(|b| b.is_complete()).collect();
    assert_eq!(finished.len(), 1, "the finished block survives");
    assert_eq!(next.command_marks().len(), 1, "its prompt mark survives");
    let prompt_row = next.command_marks()[0].prompt_start_row;
    let top = next.grid().absolute_row_counter() - u64::from(ROWS);
    let visible = usize::try_from(prompt_row - top).expect("prompt row is on screen");
    assert!(
        next.row_text(visible)
            .unwrap_or_default()
            .starts_with("$ ls"),
        "the carried absolute row names the same line on the successor"
    );

    next.process(b"done\r\n\x1b]133;D;0\x07");
    assert_eq!(
        next.completed_command_seq(),
        2,
        "the running command completed"
    );
    assert!(next.current_block().is_some_and(|b| b.is_complete()));

    let mut cells_only = cp.clone();
    cells_only.shell = Default::default();
    let mut old = successor();
    old.restore_checkpoint(&cells_only);
    old.process(b"done\r\n\x1b]133;D;0\x07");
    assert_eq!(
        old.completed_command_seq(),
        0,
        "the old carry lost the command"
    );
}

use aterm_core::grid::OlderHistory;
use aterm_core::scrollback::Line;

fn outgoing_with_history() -> Terminal {
    let mut t = Terminal::new(5, 40);
    for i in 0..30 {
        t.process(format!("old{i}\r\n").as_bytes());
    }
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07pwd\r\n\x1b]133;C\x07SECOND-OUT\r\n\x1b]133;D;0\x07");
    t.process(b"tail0\r\ntail1\r\n");
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07building\r\n");
    t
}

fn exported(t: &Terminal) -> Vec<Line> {
    let fence = t.history_fence();
    t.history_lines_since_fence(&fence, fence.oldest, usize::MAX)
        .expect("the fence holds")
}

fn history_text(t: &Terminal) -> Vec<String> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn finished_output(t: &Terminal, command: &str) -> Option<String> {
    t.all_blocks()
        .filter(|b| b.is_complete())
        .find(|b| t.block_command(b).is_some_and(|c| c.ends_with(command)))
        .and_then(|b| t.block_output(b))
}

#[test]
fn carried_blocks_name_the_same_lines_after_the_history_import() {
    const CARRIED: usize = 6;
    let mut source = outgoing_with_history();
    let lines = exported(&source);
    let cp = source.checkpoint_carry(CARRIED).expect("parser is Ground");
    let take = lines.len() - CARRIED;
    assert!(take > 0, "the sidecar holds what the checkpoint does not");
    let source_finished = finished_output(&source, "pwd");
    assert_eq!(source_finished.as_deref(), Some("SECOND-OUT\ntail0\ntail1"));

    let mut next = Terminal::new(5, 40);
    next.restore_checkpoint(&cp);
    let claim = next.reserve_older_history_keys(take as u64);
    assert_eq!(
        finished_output(&next, "pwd"),
        source_finished,
        "the reserve leaves every carried mark on its line"
    );
    let older = OlderHistory::build(&lines[..take], 40, next.history_cols());
    assert_eq!(
        next.attach_older_history(older, claim).expect("lands"),
        take
    );
    assert_eq!(history_text(&next), history_text(&source), "the history");
    assert_eq!(
        finished_output(&next, "pwd"),
        source_finished,
        "a finished block reads its own output after the import"
    );

    for t in [&mut source, &mut next] {
        t.process(b"done\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    }
    assert_eq!(
        finished_output(&source, "make").as_deref(),
        Some("building\ndone")
    );
    assert_eq!(
        finished_output(&next, "make"),
        finished_output(&source, "make")
    );
}

#[test]
fn a_fresh_numbered_restore_still_reserves_the_whole_import() {
    const CARRIED: usize = 6;
    let source = outgoing_with_history();
    let lines = exported(&source);
    let mut cp = source.checkpoint_carry(CARRIED).expect("parser is Ground");
    cp.absolute_row_counter = 0;
    cp.alt_absolute_row_counter = 0;
    cp.shell = Default::default();
    let take = lines.len() - CARRIED;

    let mut next = Terminal::new(5, 40);
    next.restore_checkpoint(&cp);
    let before = next.grid().absolute_row_counter();
    assert_eq!(next.grid().oldest_absolute_row(), 0, "numbered from zero");
    let claim = next.reserve_older_history_keys(take as u64);
    assert_eq!(next.grid().absolute_row_counter(), before + take as u64);
    let top = next.grid().visible_to_absolute(0);
    let older = OlderHistory::build(&lines[..take], 40, next.history_cols());
    next.attach_older_history(older, claim).expect("lands");
    assert_eq!(next.grid().visible_to_absolute(0), top, "no live key moved");
    assert_eq!(next.grid().oldest_absolute_row(), 0);
    assert_eq!(history_text(&next), history_text(&source));
}

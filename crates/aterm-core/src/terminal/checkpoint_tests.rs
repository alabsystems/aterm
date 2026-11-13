// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scrollback::serialize_lines;

#[test]
fn borrowed_checkpoint_projection_preserves_main_and_alt_wire_bytes() {
    let mut source = Terminal::new(4, 96);
    for row in 0..1208 {
        source.process(
            format!(
                "\x1b[1;38;5;{}m\x1b]8;;https://example.test/{row}\x07\
                 row-{row:04} é界e\u{301} abcdefghijklmnopqrstuvwxyz\
                 \x1b]8;;\x07\x1b[m\r\n",
                row % 6 + 1
            )
            .as_bytes(),
        );
    }
    for on_alt in [false, true] {
        if on_alt {
            source.process(b"\x1b[?1049halt-one\r\nalt-two");
        }
        for bound in [0, 1, 1000, 2048, usize::MAX] {
            let checkpoint = source
                .checkpoint_continuing_bounded(bound)
                .expect("complete parser input");
            assert_eq!(
                checkpoint.grid,
                serialize_lines(&source.grid.checkpoint_lines_bounded(bound)),
                "active grid: alt={on_alt}, bound={bound}"
            );
            assert_eq!(
                checkpoint.alt_grid,
                source
                    .alt_grid
                    .as_ref()
                    .map(|grid| { serialize_lines(&grid.checkpoint_lines_bounded(bound)) }),
                "inactive grid: alt={on_alt}, bound={bound}"
            );
            assert_eq!(
                checkpoint.history_lines as usize,
                source.grid.scrollback_lines().min(bound)
            );
            let restored = Terminal::from_checkpoint(&checkpoint);
            assert_eq!(source.visible_content(), restored.visible_content());
        }
    }
}

#[test]
fn restoring_deep_history_preserves_store_ring_visible_and_counter_boundaries() {
    const ROWS: u16 = 4;
    const COLS: u16 = 96;
    let mut source = Terminal::new(ROWS, COLS);
    for row in 0..1208 {
        source.process(
            format!(
                "\x1b[1;38;5;{}m\x1b]8;;https://example.com/{row}\x07\
                 row-{row:04} é界 abcdefghijklmnopqrstuvwxyz\
                 \x1b]8;;\x07\x1b[m\r\n",
                row % 6 + 1
            )
            .as_bytes(),
        );
    }
    for on_alt in [false, true] {
        if on_alt {
            source.process(b"\x1b[?1049halt-one\r\nalt-two");
        }
        let checkpoint = source.checkpoint();
        let primary = if on_alt {
            checkpoint.alt_grid.as_ref().expect("saved main grid")
        } else {
            &checkpoint.grid
        };
        let primary_lines = deserialize_lines(primary);
        let history = primary_lines.len() - usize::from(ROWS);
        assert!(history > 1000, "both the store and ring are populated");

        for limit in [None, Some(0), Some(7)] {
            let restored = match limit {
                None => Terminal::replay_from_checkpoint(&checkpoint),
                Some(limit) => Terminal::replay_from_checkpoint_bounded(&checkpoint, limit),
            };
            let again = restored.checkpoint();
            let kept = limit.map_or(history, |limit| history.min(1000 + limit));
            let expected = serialize_lines(&primary_lines[history - kept..]);
            if on_alt {
                assert_eq!(again.alt_grid.as_ref(), Some(&expected), "limit {limit:?}");
                assert_eq!(again.grid, checkpoint.grid, "the alternate screen");
                assert_eq!(restored.grid().scrollback_lines(), 0);
            } else {
                assert_eq!(again.grid, expected, "limit {limit:?}");
            }
            assert_eq!(again.cursor, checkpoint.cursor);
            assert_eq!(again.alt_cursor, checkpoint.alt_cursor);
            assert_eq!(again.absolute_row_counter, checkpoint.absolute_row_counter);
            assert_eq!(
                again.alt_absolute_row_counter,
                checkpoint.alt_absolute_row_counter
            );
        }
    }
}

#[test]
fn restoring_short_and_alternate_grids_keeps_the_visible_rows() {
    use crate::scrollback::Line;

    let cursor = GridCursorRepr::capture(Terminal::new(4, 20).grid());
    for count in [0usize, 1, 3, 6] {
        let lines: Vec<Line> = (0..count)
            .map(|row| Line::from(format!("row-{row}").as_str()))
            .collect();
        let bytes = serialize_lines(&lines);
        let first_visible = count.saturating_sub(4);
        for is_alt in [false, true] {
            let restored = restore_grid(4, 20, &bytes, &cursor, is_alt, None);
            let visible = restored.checkpoint_visible_lines();
            for (row, actual) in visible.iter().enumerate() {
                let expected = lines
                    .get(first_visible + row)
                    .map_or_else(String::new, ToString::to_string);
                assert_eq!(
                    actual.to_string(),
                    expected,
                    "count {count}, alternate {is_alt}"
                );
            }
            assert_eq!(
                restored.scrollback_lines(),
                if is_alt { 0 } else { first_visible }
            );
            assert_eq!(GridCursorRepr::capture(&restored), cursor);
        }
    }
}

/// `lift_screen` moves the screen's top lines into its history (on the alternate screen, a history of just those; under a history limit too small for them, one that holds them) and the cursor up with its line, hidden once its line is history.
#[test]
fn a_lifted_screen_keeps_its_top_lines_as_history_and_its_cursor_on_its_line() {
    for (alt, limit) in [
        (false, None),
        (true, None),
        (false, Some(0)),
        (false, Some(1)),
    ] {
        let mut t = Terminal::new(5, 10);
        t.set_scrollback_line_limit(limit);
        if alt {
            t.process(b"\x1b[?1049h");
        }
        t.process(b"a\r\nb\r\nc\r\nd\r\ne");
        t.lift_screen(2);
        let alt = format!("{alt} limit={limit:?}");
        let line = |t: &Terminal, r: i32| t.get_line_text(r, None).unwrap_or_default();
        assert_eq!(
            (line(&t, -2), line(&t, -1), line(&t, 0), line(&t, 2)),
            ("a".into(), "b".into(), "c".into(), "e".into()),
            "alt={alt}"
        );
        assert_eq!(
            line(&t, 3).trim_end(),
            "",
            "alt={alt}: blank rows join the bottom"
        );
        assert_eq!(t.grid().scrollback_lines(), 2, "alt={alt}");
        assert_eq!((t.cursor().row, t.cursor().col), (2, 1), "alt={alt}");
        assert!(t.cursor_visible(), "alt={alt}");
    }
    let mut t = Terminal::new(5, 10);
    t.process(b"top\x1b[H");
    t.lift_screen(1);
    assert!(
        !t.cursor_visible(),
        "a cursor whose line is history is hidden"
    );
    let mut t = Terminal::new(3, 10);
    t.process(b"a\r\nb\r\nc");
    t.lift_screen(9);
    assert_eq!(t.grid().scrollback_lines(), 2, "at most every row but one");
    assert_eq!(t.get_line_text(0, None).as_deref(), Some("c"));
}

#[cfg(feature = "serde")]
#[test]
fn the_carry_meta_is_the_visible_carrys_meta_and_projects_no_grid() {
    let mut alt = build_rich_terminal(6, 24);
    alt.process(b"\x1b[?1049hon the alt screen\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
    let mut partial = build_rich_terminal(6, 24);
    partial.process(b"\x1b]0;an unterminated title");
    for (label, terminal) in [
        ("plain", Terminal::new(4, 20)),
        ("rich", build_rich_terminal(6, 24)),
        ("alt screen", alt),
        ("mid-sequence", partial),
    ] {
        let (carry, _) = terminal.checkpoint_carry_abandoning_partial(0);
        let meta = crate::fault::with_armed("checkpoint.carry_projection", || {
            terminal.carry_meta_abandoning_partial()
        });
        assert_eq!(meta, CheckpointMeta::from_checkpoint(&carry), "{label}");
        let projected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::fault::with_armed("checkpoint.carry_projection", || {
                terminal.checkpoint_carry_abandoning_partial(0)
            })
        }));
        assert!(projected.is_err(), "{label}: the carry does take it");
    }
}

fn build_rich_terminal(rows: u16, cols: u16) -> Terminal {
    let mut t = Terminal::new(rows, cols);

    t.process(b"\x1b[1;4;38;5;202;48;2;10;20;30mstyled\x1b[0m\r\n");

    for i in 0..(rows as usize + 6) {
        t.process(format!("line{i}\r\n").as_bytes());
    }

    t.process(format!("\x1b[2;{}r", rows - 1).as_bytes());

    t.process(b"\x1b[3;1H"); // row 3
    let last_col_fill: String = std::iter::repeat('Z').take(cols as usize).collect();
    t.process(last_col_fill.as_bytes()); // fills to last col -> pending_wrap set

    t.process(b"\x1b[4;1H"); // move somewhere safe
    t.process(b"\x1b[3g"); // TBC 3: clear all tab stops
    t.process(b"\x1b[1;5H\x1bH"); // move to col 5, HTS sets a tab stop here

    t.process(b"\x1b(0");

    t.process(b"\x1b[>5u");

    t.process(b"\x1b[>4;2m");

    t.process(b"\x1b]9;4;1;42\x07");

    t.set_secure_keyboard_entry(true);

    t.process(b"\x1b]7;file://host/tmp/work\x07");

    t.modes.allow_palette_reconfigure = true;
    t.process(b"\x1b]4;1;rgb:12/34/56\x07\x1b]10;rgb:aa/bb/cc\x07\x1b]12;rgb:01/02/03\x07");
    t.process(b"\x1b]30001\x07");
    t.process(b"\x1b]4;2;rgb:65/43/21\x07");

    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07");

    t.process(b"\x1b[?1049h");
    t.process(b"ALT-SCREEN-CONTENT\r\n");

    assert!(
        t.parser_is_ground(),
        "test stream must leave parser in Ground state"
    );
    t
}

fn lineage_taken(c: TerminalCheckpoint) -> TerminalCheckpoint {
    assert!(
        c.holder.seed_untrusted,
        "a restore output reached before adoption carries its lineage"
    );
    c.without_seed_lineage()
}

fn cell_signature(t: &Terminal, row: u16, col: u16) -> (char, CellFlags, PackedColor, PackedColor) {
    let cell = *t
        .grid()
        .row(row)
        .and_then(|r| r.get(col))
        .expect("cell in range");
    (
        cell.char(),
        cell.flags(),
        cell.fg_color().unwrap_or(PackedColor::DEFAULT_FG),
        cell.bg_color().unwrap_or(PackedColor::DEFAULT_BG),
    )
}

#[test]
fn checkpoint_roundtrip_full_projection() {
    let (rows, cols) = (12u16, 40u16);
    let t = build_rich_terminal(rows, cols);

    assert!(t.modes.alternate_screen, "alt screen active at capture");
    assert!(t.secure_keyboard_entry, "secure input captured");
    assert_eq!(
        t.taskbar_progress,
        Some(TaskbarProgress::Normal(42)),
        "taskbar captured"
    );
    assert!(t.current_working_directory.is_some(), "cwd captured");
    assert!(
        t.alt_grid.is_some(),
        "alt grid present (main saved under alt)"
    );

    let c0 = t.checkpoint();
    assert_eq!(c0.color.palette.len(), 2, "two OSC 4 overrides");
    assert!(c0.color.foreground.is_some() && c0.color.cursor.is_some());
    assert_eq!(c0.color.stack_overrides.len(), 1, "one XTPUSHCOLORS entry");
    assert_eq!(c0.shell.command_marks.len(), 1, "one finished command");
    assert!(
        c0.shell.current_block.is_some(),
        "a running command's block"
    );
    assert_eq!(c0.shell.completed_seq, 1);
    let h = Terminal::from_checkpoint(&c0);
    let c1 = h.checkpoint();

    assert_eq!(c0, c1, "re-checkpoint equality (c0 == c1)");

    assert_eq!(
        t.visible_content(),
        h.visible_content(),
        "visible_content equal"
    );
    for r in 0..rows as usize {
        assert_eq!(t.row_text(r), h.row_text(r), "row_text equal for row {r}");
    }

    assert_eq!(t.cursor(), h.cursor(), "cursor equal");

    assert_eq!(
        t.grid().scroll_region(),
        h.grid().scroll_region(),
        "scroll region equal"
    );
    assert_eq!(t.modes, h.modes, "modes equal");
    assert_eq!(t.charset, h.charset, "charset equal");
    assert_eq!(t.taskbar_progress, h.taskbar_progress, "taskbar equal");
    assert_eq!(
        t.secure_keyboard_entry, h.secure_keyboard_entry,
        "secure input equal"
    );
    assert_eq!(
        t.current_working_directory, h.current_working_directory,
        "cwd equal"
    );
    assert_eq!(t.xterm_keyboard, h.xterm_keyboard, "xterm keyboard equal");
    assert_eq!(
        t.kitty_keyboard.snapshot(),
        h.kitty_keyboard.snapshot(),
        "kitty keyboard equal"
    );
    assert_eq!(
        t.grid().tab_stops(),
        h.grid().tab_stops(),
        "tab stops equal"
    );
    assert_eq!(
        t.grid().pending_wrap(),
        h.grid().pending_wrap(),
        "pending wrap equal"
    );
}

#[test]
fn visible_checkpoint_restores_screen_without_carrying_history() {
    let (rows, cols) = (12u16, 40u16);
    let t = build_rich_terminal(rows, cols);
    let full = t.checkpoint();
    let visible = t.checkpoint_visible().expect("parser is Ground");

    assert_eq!(deserialize_lines(&visible.grid).len(), rows as usize);
    let full_bytes = full.grid.len() + full.alt_grid.as_ref().map_or(0, Vec::len);
    let visible_bytes = visible.grid.len() + visible.alt_grid.as_ref().map_or(0, Vec::len);
    assert!(
        visible_bytes < full_bytes,
        "deep saved/main-grid history is excluded from handoff carry"
    );
    if let Some(alt) = visible.alt_grid.as_ref() {
        assert_eq!(deserialize_lines(alt).len(), rows as usize);
    }

    let restored = Terminal::from_checkpoint(&visible);
    assert_eq!(t.visible_content(), restored.visible_content());
    assert_eq!(t.cursor(), restored.cursor());
    assert_eq!(
        Some(visible),
        restored.checkpoint_visible(),
        "restored parser remains Ground"
    );
}

#[test]
fn visible_checkpoint_carries_a_partial_parser_sequence() {
    let mut terminal = Terminal::new(4, 20);
    terminal.process(b"prefix\x1b[");
    assert!(!terminal.parser_is_ground());
    let cp = terminal
        .checkpoint_visible()
        .expect("a split CSI is carried");
    assert!(!cp.parser_ground);
    assert_eq!(
        cp.parser.as_ref().map(|carry| carry.state.name()),
        Some("CsiEntry")
    );
    let mut restored = Terminal::from_checkpoint(&cp);
    restored.process(b"0m!");
    assert_eq!(
        restored.row_text(0).as_deref().map(str::trim_end),
        Some("prefix!"),
        "the tail finished the sequence instead of printing"
    );
    terminal.process(b"0m");
    assert!(terminal.parser_is_ground());
    assert!(terminal.checkpoint_visible().is_some());

    terminal.process(b"\x1bP$qm");
    assert_eq!(
        terminal.partial_sequence_uncarried(),
        Some("DcsPassthrough")
    );
    assert!(
        terminal.checkpoint_visible().is_none(),
        "a hooked DCS string fails closed"
    );
}

#[test]
fn a_bounded_continuing_checkpoint_is_the_full_one_under_that_scrollback_limit() {
    const BOUND: usize = 10;
    let engine = |alt: bool| {
        let mut t = Terminal::new(4, 20);
        for i in 0..30 {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
        if alt {
            t.process(b"\x1b[?1049hon the alt screen");
        }
        t.process(b"\x1b[1;3");
        t
    };
    for alt in [false, true] {
        let bounded = engine(alt)
            .checkpoint_continuing_bounded(BOUND)
            .expect("a split CSI is carried");
        let mut limited = engine(alt);
        limited.set_scrollback_line_limit(Some(BOUND));
        assert_eq!(
            Some(&bounded),
            limited.checkpoint_continuing().as_ref(),
            "alt: {alt}"
        );
        let primary = if alt {
            bounded.alt_grid.as_deref().expect("the saved primary")
        } else {
            &bounded.grid[..]
        };
        assert_eq!(deserialize_lines(primary).len(), BOUND + 4, "alt: {alt}");
        let full = engine(alt).checkpoint_continuing().expect("carried");
        let full_primary = if alt {
            full.alt_grid.as_deref().expect("the saved primary")
        } else {
            &full.grid[..]
        };
        assert!(
            deserialize_lines(full_primary).len() > BOUND + 4,
            "alt: {alt}"
        );

        let mut restored = Terminal::from_checkpoint(&bounded);
        let mut live = engine(alt);
        restored.process(b"1mX");
        live.process(b"1mX");
        assert!(restored.parser_is_ground(), "the tail ended the CSI");
        assert_eq!(restored.visible_content(), live.visible_content());
    }
}

#[test]
fn visible_checkpoint_is_live_frame_not_scrolled_viewport_and_continues() {
    let mut terminal = Terminal::new(4, 20);
    for line in 0..10 {
        terminal.process(format!("line-{line}\r\n").as_bytes());
    }
    let live = terminal
        .checkpoint_visible()
        .expect("Ground at live bottom");
    terminal.scroll_display(3);
    assert!(terminal.grid().display_offset() > 0);
    let scrolled = terminal
        .checkpoint_visible()
        .expect("Ground while scrolled");
    assert_eq!(
        live, scrolled,
        "viewport navigation cannot replace the PTY's live continuation frame"
    );

    let mut restored = Terminal::from_checkpoint(&scrolled);
    terminal.process(b"tail");
    restored.process(b"tail");
    assert_eq!(
        terminal.checkpoint_visible(),
        restored.checkpoint_visible().map(lineage_taken),
        "restored live frame continues identically after new PTY output"
    );
}

#[test]
fn decsc_saved_cursor_survives_handoff_and_decrc_continuation() {
    let mut terminal = Terminal::new(6, 24);
    terminal.process(b"\x1b[3;5H\x1b[1;31m\x1b7");
    terminal.process(b"\x1b[H\x1b[0mchanged");
    let checkpoint = terminal.checkpoint_visible().expect("Ground after DECSC");
    assert!(checkpoint.saved_cursor_main.is_some());
    let mut restored = Terminal::from_checkpoint(&checkpoint);

    let continuation = b"\x1b8X";
    terminal.process(continuation);
    restored.process(continuation);
    assert_eq!(terminal.cursor(), restored.cursor());
    assert_eq!(
        cell_signature(&terminal, 2, 4),
        cell_signature(&restored, 2, 4)
    );
    assert_eq!(
        terminal.checkpoint_visible(),
        restored.checkpoint_visible().map(lineage_taken),
        "DECRC and subsequent styled output must remain equivalent"
    );
}

#[test]
fn checkpoint_post_hydration_styled_write_matches() {
    let (rows, cols) = (10u16, 30u16);
    let mut t = build_rich_terminal(rows, cols);
    let c0 = t.checkpoint();
    let mut h = Terminal::from_checkpoint(&c0);

    let seq = b"\x1b[H\x1b[1;3;38;2;1;2;3;48;5;9mQ\x1b[0m";
    t.process(seq);
    h.process(seq);

    assert!(t.parser_is_ground() && h.parser_is_ground());

    let ts = cell_signature(&t, 0, 0);
    let hs = cell_signature(&h, 0, 0);
    assert_eq!(
        ts, hs,
        "post-hydration styled cell identical (char, flags, fg, bg)"
    );

    assert_eq!(
        t.checkpoint(),
        lineage_taken(h.checkpoint()),
        "post-write re-checkpoint equality"
    );
}

#[test]
fn checkpoint_alt_screen_toggle_on_hydrated() {
    let (rows, cols) = (10u16, 30u16);
    let mut t = build_rich_terminal(rows, cols);
    let c0 = t.checkpoint();
    let mut h = Terminal::from_checkpoint(&c0);

    t.process(b"\x1b[?1049l");
    h.process(b"\x1b[?1049l");

    assert!(!t.modes.alternate_screen && !h.modes.alternate_screen);
    assert_eq!(
        t.visible_content(),
        h.visible_content(),
        "main screen restored identically after alt toggle"
    );
    for r in 0..rows as usize {
        assert_eq!(
            t.row_text(r),
            h.row_text(r),
            "row {r} equal after alt toggle"
        );
    }
    assert_eq!(
        t.checkpoint(),
        lineage_taken(h.checkpoint()),
        "re-checkpoint equal after alt toggle"
    );
}

#[test]
fn checkpoint_roundtrips_decslrm_horizontal_margins() {
    let (rows, cols) = (10u16, 40u16);
    let mut t = Terminal::new(rows, cols);
    t.process(b"\x1b[?69h");
    t.process(b"\x1b[5;30s");
    assert!(t.parser_is_ground());

    let live = t.grid().horizontal_margins();
    assert_eq!((live.left, live.right), (4, 29), "margins set on live grid");

    let c0 = t.checkpoint();
    assert_eq!(
        (c0.cursor.margin_left, c0.cursor.margin_right),
        (4, 29),
        "margins captured into the checkpoint projection"
    );

    let h = Terminal::from_checkpoint(&c0);
    let restored = h.grid().horizontal_margins();
    assert_eq!(
        (restored.left, restored.right),
        (4, 29),
        "margins restored onto the hydrated grid"
    );
    assert_eq!(
        c0,
        h.checkpoint(),
        "re-checkpoint identity holds with horizontal margins"
    );
}

#[test]
fn checkpoint_roundtrips_on_main_with_retained_alt_buffer() {
    let (rows, cols) = (8u16, 24u16);
    let mut t = Terminal::new(rows, cols);
    for i in 0..(rows as usize + 5) {
        t.process(format!("main{i}\r\n").as_bytes());
    }
    t.process(b"\x1b[?47h");
    t.process(b"ALT-LINE\r\n");
    t.process(b"\x1b[?47l");
    assert!(t.parser_is_ground());
    assert!(!t.modes.alternate_screen, "back on the main screen");
    assert!(
        t.alt_grid.is_some(),
        "alt buffer retained in alt_grid after mode-47 exit"
    );

    let c0 = t.checkpoint();
    assert!(c0.alt_grid.is_some(), "retained alt buffer captured");
    let h = Terminal::from_checkpoint(&c0);
    assert!(!h.modes.alternate_screen, "hydrated stays on main");
    assert!(h.alt_grid.is_some(), "hydrated retains the alt buffer");
    assert_eq!(
        c0,
        h.checkpoint(),
        "re-checkpoint identity (on main, alt buffer retained)"
    );
    assert_eq!(
        t.visible_content(),
        h.visible_content(),
        "main content equal after restore"
    );
}

#[test]
fn restored_alt_screen_accrues_no_scrollback_when_scrolled() {
    let (rows, cols) = (6u16, 16u16);
    let mut t = Terminal::new(rows, cols);
    t.process(b"\x1b[?1049h"); // enter alt screen (active grid = alt, 0-ring)
    assert!(t.parser_is_ground() && t.modes.alternate_screen);

    let c0 = t.checkpoint();
    let mut h = Terminal::from_checkpoint(&c0);
    assert!(h.modes.alternate_screen, "hydrated is on the alt screen");

    for i in 0..(rows as usize * 3) {
        let line = format!("s{i}\r\n");
        t.process(line.as_bytes());
        h.process(line.as_bytes());
    }
    assert!(t.parser_is_ground() && h.parser_is_ground());

    assert_eq!(
        t.grid().scrollback_lines(),
        0,
        "live alt screen accrues no scrollback"
    );
    assert_eq!(
        h.grid().scrollback_lines(),
        0,
        "restored alt screen must ALSO accrue no scrollback (#5: 0-ring, not 1000)"
    );
    assert_eq!(
        t.checkpoint(),
        lineage_taken(h.checkpoint()),
        "restored alt screen stays in lockstep with the live engine after scrolling"
    );
}

#[test]
fn in_place_restore_advances_content_scroll_epoch_once() {
    let mut source = Terminal::new(3, 12);
    source.process(b"restored");
    let checkpoint = source.checkpoint();

    let mut live = Terminal::new(3, 12);
    live.process(b"\x1b[3;1H\n");
    let before = live.content_scroll_state();
    live.restore_checkpoint(&checkpoint);
    let after = live.content_scroll_state();
    assert_eq!(after.uniform_up_rows, before.uniform_up_rows);
    assert_eq!(
        after.invalidation_epoch,
        before.invalidation_epoch + 1,
        "wholesale in-place grid replacement invalidates existing host coordinates"
    );

    live.process(b"");
    assert_eq!(
        live.content_scroll_state(),
        after,
        "the restored grid carries no latent scroll sentinel to double-report"
    );
}
#[test]
fn a_resize_moves_the_decsc_slot_with_its_text_and_the_wire_carries_it() {
    let mut t = Terminal::new(56, 149);
    t.process(b"\x1b[56;1H\x1b[?1049h");
    assert_eq!(t.cursor_save.main.map(|saved| saved.cursor.row), Some(55));
    t.resize(55, 149);
    assert_eq!(
        t.cursor_save.main.map(|saved| saved.cursor.row),
        Some(54),
        "the demote moved the slot's row up one"
    );
    let cp = t.checkpoint_carry(0).expect("Ground");
    assert_eq!(cp.rows, 55);
    assert_eq!(
        cp.saved_cursor_main.map(|saved| saved.cursor_row),
        Some(54),
        "the wire carries the slot as the engine holds it"
    );
    t.resize(56, 149);
    t.process(b"\x1b[?1049l");
    assert_eq!(t.cursor().row, 55, "the reveal moved it back");
}

/// The wire carries whether a resize changed the size since the save, so a hydrated terminal
/// restores a saved wrap the reflow left mid-line as the one it came from does.
#[test]
fn the_wire_carries_a_resize_since_the_save() {
    let mut t = Terminal::new(3, 5);
    t.process(b"abcde\x1b7");
    assert_eq!(t.cursor_save.main.map(|saved| saved.resized), Some(false));
    t.resize(3, 10);
    assert_eq!(t.cursor_save.main.map(|saved| saved.resized), Some(true));
    let cp = t.checkpoint_carry(0).expect("Ground");
    assert_eq!(
        cp.saved_cursor_main.map(|saved| saved.resized.get()),
        Some(true)
    );
    #[cfg(feature = "serde")]
    let cp = CheckpointMeta::from_checkpoint(&cp).into_checkpoint(cp.grid.clone(), None);
    let mut adopted = Terminal::new(3, 10);
    adopted.restore_checkpoint(&cp);
    for (at, mut restored) in [
        ("from_checkpoint", Terminal::from_checkpoint(&cp)),
        ("restore", adopted),
        ("source", t),
    ] {
        restored.process(b"\x1b8X");
        assert_eq!(
            restored.row_text(0).unwrap_or_default().trim_end(),
            "abcdeX",
            "{at}: X follows the e"
        );
        restored.process(b"\x1b7");
        assert_eq!(
            restored.cursor_save.main.map(|saved| saved.resized),
            Some(false),
            "{at}: a new save starts unresized"
        );
    }
}

#[test]
fn a_flap_across_a_restore_hands_back_what_it_owes() {
    fn screen(t: &Terminal) -> Vec<String> {
        (0..usize::from(t.rows()))
            .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
            .collect()
    }
    let fresh = || {
        let mut t = Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n");
        t
    };
    let drawn = screen(&fresh());
    let paths: [(&[u16], i32, u16); 3] = [(&[9], -1, 0), (&[7], 1, 0), (&[30], -13, 9)];
    for (path, owed, above) in paths {
        let mut t = fresh();
        for &rows in path {
            t.resize(rows, 20);
        }
        let carry = t.checkpoint_carry(usize::MAX).expect("Ground");
        assert_eq!(
            (carry.cursor.handback_rows, carry.cursor.handback_above),
            (owed, above),
            "{path:?}"
        );
        #[cfg(feature = "serde")]
        let carry =
            CheckpointMeta::from_checkpoint(&carry).into_checkpoint(carry.grid.clone(), None);
        let mut restored = Terminal::from_checkpoint(&carry);
        assert_eq!(
            (
                restored.grid().handback_rows(),
                restored.grid().handback_above()
            ),
            (owed, above),
            "{path:?}: seeded"
        );
        restored.resize(8, 20);
        assert_eq!(
            screen(&restored),
            drawn,
            "{path:?}: every row where it was drawn"
        );
    }

    let mut t = fresh();
    t.resize(2, 20);
    t.resize(1, 20);
    let restored = Terminal::from_checkpoint(&t.checkpoint_carry(usize::MAX).expect("Ground"));
    assert_eq!(
        (
            restored.grid().handback_rows(),
            restored.grid().handback_above()
        ),
        (6, 1),
        "trims then a demote"
    );

    let mut t = fresh();
    t.resize(9, 20);
    let mut restored = Terminal::from_checkpoint(&t.checkpoint_carry(usize::MAX).expect("Ground"));
    restored.forget_handback();
    restored.resize(8, 20);
    assert_eq!(
        screen(&restored)[..2],
        ["h12", "top"],
        "forgotten, every row stays down"
    );
}

#[test]
fn a_flap_across_a_restore_brings_a_demoted_row_back() {
    fn picture(t: &Terminal) -> (Vec<String>, u16, u16) {
        let rows = (0..usize::from(t.rows()))
            .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
            .collect();
        (rows, t.cursor().row, t.cursor().col)
    }
    let full = || {
        let mut t = Terminal::new(8, 20);
        for i in 0..30 {
            t.process(format!("n{i}\r\n").as_bytes());
        }
        t.process(b"prompt$ ");
        t
    };
    let blank_tailed = || {
        let mut t = Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n");
        t
    };
    let cases: [(&str, &dyn Fn() -> Terminal, &[u16]); 3] = [
        ("full screen, 7", &full, &[7]),
        ("full screen, 5", &full, &[5]),
        ("blank tail, 2 then 1", &blank_tailed, &[2, 1]),
    ];
    for (label, build, half) in cases {
        let drawn = picture(&build());
        let mut live = build();
        for &rows in half {
            live.resize(rows, 20);
        }
        let carry = live.checkpoint_carry(usize::MAX).expect("Ground");
        let mut restored = Terminal::from_checkpoint(&carry);
        live.resize(8, 20);
        restored.resize(8, 20);
        assert_eq!(picture(&live), drawn, "{label}: live");
        assert_eq!(picture(&restored), drawn, "{label}: across the restore");
    }
}

#[test]
fn a_carried_synchronized_update_window_still_times_out() {
    let mut t = Terminal::new(24, 80);
    t.process(b"$ vim\r\n\x1b[?2026h");
    let cp = t.checkpoint_carry(0).expect("carried");
    assert!(
        cp.modes.synchronized_output,
        "PRECONDITION: the window is open"
    );

    let mut fresh = Terminal::from_checkpoint(&cp);
    let mut adopted = Terminal::new(24, 80);
    adopted.restore_checkpoint(&cp);
    for (at, restored) in [("from_checkpoint", &mut fresh), ("restore", &mut adopted)] {
        assert!(restored.modes().synchronized_output(), "{at}: carried open");
        restored.backdate_sync_start(std::time::Duration::from_secs(120));
        restored.process(b"x");
        assert!(
            !restored.modes().synchronized_output(),
            "{at}: the carried window timed out"
        );
    }

    let mut closed = Terminal::from_checkpoint(&cp);
    closed.process(b"frame\x1b[?2026l");
    assert!(!closed.modes().synchronized_output());
}

/// A CODEX AT A BOX STAYS AT IT ACROSS A HANDOFF: the program's own report standing (OSC
/// 7501) is carried, through the carry's meta too, and the adopting engine holds the same
/// report, which begins no turn and keeps no turn edges; codex's re-send of it is the same
/// box (where it began is kept). CONTROL: an engine with no report carries none, and the
/// live engine drew one turn.
#[test]
fn the_programs_own_report_is_carried() {
    let mut t = Terminal::new(4, 20);
    assert_eq!(
        t.checkpoint_carry(0)
            .expect("Ground")
            .output_state
            .program_state,
        None,
        "CONTROL"
    );
    t.process(
        b"\x1b]7501;state=working:app=codex\x1b\\\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\",
    );
    let cp = t.checkpoint_carry(0).expect("Ground");
    #[cfg(feature = "serde")]
    let cp = CheckpointMeta::from_checkpoint(&cp).into_checkpoint(cp.grid.clone(), None);
    let mut adopted = Terminal::new(4, 20);
    adopted.restore_checkpoint(&cp);
    for (at, mut restored) in [
        ("from_checkpoint", Terminal::from_checkpoint(&cp)),
        ("restore", adopted),
    ] {
        let report = restored.program_state();
        assert_eq!(report.state, t.program_state().state, "{at}");
        assert_eq!(report.turns, 0, "{at}: a carried report begins no turn");
        assert_eq!(t.program_state().turn_edges().count(), 1, "CONTROL");
        assert_eq!(report.turn_edges().count(), 0, "{at}: and carries no edge");
        restored.process(b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\");
        assert_eq!(
            restored.program_state().began,
            report.began,
            "{at}: the box re-sent is the same box"
        );
    }
}

#[test]
fn output_state_in_flight_names_an_open_link_and_a_pending_vt52_address() {
    let mut t = Terminal::new(4, 20);
    assert_eq!(t.output_state_in_flight(), None);

    t.process(b"\x1b]8;id=x;https://example.com\x07lin");
    assert_eq!(t.output_state_in_flight(), Some("Osc8HyperlinkOpen"));
    let cp = t.checkpoint_carry(0).expect("carried");
    let mut adopted = Terminal::new(4, 20);
    adopted.restore_checkpoint(&cp);
    for (at, restored) in [
        ("from_checkpoint", Terminal::from_checkpoint(&cp)),
        ("restore", adopted),
    ] {
        assert_eq!(
            restored.current_hyperlink().map(AsRef::as_ref),
            Some("https://example.com"),
            "{at}: the restore keeps the open link"
        );
        assert_eq!(
            restored.output_state_in_flight(),
            Some("Osc8HyperlinkOpen"),
            "{at}"
        );
    }
    t.process(b"k\x1b]8;;\x07");
    assert_eq!(t.output_state_in_flight(), None, "the link is closed");

    t.process(b"\x1b[?2l\x1bY");
    assert_eq!(t.output_state_in_flight(), Some("Vt52CursorAddress"));
    t.process(b"\x22");
    assert_eq!(
        t.output_state_in_flight(),
        Some("Vt52CursorAddress"),
        "the row is in, the column still to come"
    );
    let mut half = Terminal::from_checkpoint(&t.checkpoint_carry(0).expect("carried"));
    t.process(b"\x24");
    assert_eq!(t.output_state_in_flight(), None, "the address is complete");
    assert_eq!((t.cursor().row, t.cursor().col), (2, 4));
    half.process(b"\x24");
    assert_eq!(
        (half.cursor().row, half.cursor().col),
        (2, 4),
        "the restored engine took the column the address was waiting for"
    );
    t.process(b"\x1b<");
    assert_eq!(t.output_state_in_flight(), None);
}

#[test]
fn a_restored_output_state_goes_through_the_engines_own_doors() {
    use crate::terminal::{SavedModeRepr, SgrPushRepr};
    let pushed = |fg: u32, mask: u16| SgrPushRepr {
        fg,
        bg: 0,
        flags: 0,
        protected: false,
        underline_color: None,
        mask,
    };
    let depth = u32::try_from(crate::terminal::SGR_STACK_MAX_DEPTH).expect("small");
    let mut cp = Terminal::new(4, 20).checkpoint_carry(0).expect("Ground");
    cp.output_state = OutputStateRepr {
        rep: Some('\u{7}'),
        underline_color: Some(0x01_ff_00_00),
        hyperlink: Some(crate::terminal::HyperlinkRepr {
            uri: "javascript:alert(1)".into(),
            id: None,
        }),
        vt52_address: None,
        zwj_pending: true,
        sgr_stack: (0..depth + 1)
            .map(|n| pushed(n, 0x3FF))
            .chain([pushed(depth + 1, 0xFFFF)])
            .collect(),
        saved_modes: vec![
            SavedModeRepr {
                mode: 7,
                set: false,
            },
            SavedModeRepr {
                mode: 65000,
                set: true,
            },
        ],
        program_state: Some("state=blocked:app=codex:kind=coffee".into()),
    };
    let mut restored = Terminal::new(4, 20);
    restored.process(b"Y\x1b]8;;https://example.com\x07\x1b[#{\x1b[?25s");
    restored.restore_checkpoint(&cp);
    assert_eq!(
        restored.current_hyperlink(),
        None,
        "the refused link is not installed"
    );
    assert_eq!(
        restored.capture_output_state_repr(),
        OutputStateRepr {
            underline_color: Some(0x01_ff_00_00),
            zwj_pending: true,
            sgr_stack: (2..depth + 1)
                .map(|n| pushed(n, 0x3FF))
                .chain([pushed(depth + 1, 0x3FF)])
                .collect(),
            saved_modes: vec![SavedModeRepr {
                mode: 7,
                set: false,
            }],
            ..OutputStateRepr::default()
        },
        "the newest pushes, the mask's own bits, the known mode, no report the program \
         state reader refuses; none of the engine's own"
    );
    restored.process(b"\x1b[3b");
    assert_eq!(
        restored.row_text(0).unwrap_or_default().trim_end(),
        "",
        "no REP character: the control was dropped, and the engine's own `Y` cleared"
    );
    cp.output_state.rep = Some('Y');
    cp.output_state.hyperlink = Some(crate::terminal::HyperlinkRepr {
        uri: "https://example.com".into(),
        id: Some("x".into()),
    });
    cp.output_state.sgr_stack = (0..depth).map(|n| pushed(n, 0x21)).collect();
    cp.output_state.saved_modes.truncate(1);
    cp.output_state.program_state = Some("state=blocked:app=codex:kind=question".into());
    restored.restore_checkpoint(&cp);
    assert_eq!(
        restored.program_state().began,
        1,
        "the restore assigns its own local episode"
    );
    assert_eq!(restored.capture_output_state_repr(), cp.output_state);
}

#[test]
fn an_unterminated_osc_is_carried_whole() {
    let mut t = Terminal::new(4, 20);
    t.process(b"hello\x1b]0;x");
    assert!(!t.parser_is_ground());
    let (cp, abandoned) = t.checkpoint_carry_abandoning_partial(0);
    assert_eq!(abandoned, None, "nothing is abandoned");
    assert_eq!(Some(&cp), t.checkpoint_carry(0).as_ref());
    assert!(!cp.parser_ground, "the restore continues the OSC");
    assert_eq!(cp.history_lines, 0);

    assert!(!t.parser_is_ground(), "the live parser is not mutated");
    t.process(b"\x07");
    assert_eq!(t.title(), "x", "the live partial OSC survived the capture");

    let mut restored = Terminal::from_checkpoint(&cp);
    assert!(!restored.parser_is_ground());
    restored.process(b"\x07 world");
    assert_eq!(restored.title(), "x", "the carried OSC completed");
    assert_eq!(
        restored.row_text(0).as_deref().map(str::trim_end),
        Some("hello world")
    );
}

#[test]
fn hooked_dcs_carries_consumed_strings_but_refuses_stateful_operations() {
    let consumed: [(&[u8], &[u8]); 3] = [
        (
            b"\x1bP1000p%begin 1 1 0\n",
            "%output %1 caf\u{e9} \u{276f} ls\n%end\x1b\\after".as_bytes(),
        ),
        (
            b"\x1bP@kitty-cmd{\"t\":\"",
            "h\u{e9}llo \u{276f}\"}\x1b\\after".as_bytes(),
        ),
        (b"\x1bP@kitty-cmd{", b"\"t\":1}\x9cafter"),
    ];
    for (head, tail) in consumed {
        let mut live = Terminal::new(4, 30);
        live.process(b"one\r\n");
        live.process(head);
        assert_eq!(live.partial_sequence_state(), Some("DcsPassthrough"));
        assert_eq!(live.partial_sequence_uncarried(), None);
        assert_eq!(live.partial_sequence_swallowed(), None, "nothing is lost");
        assert!(!live.partial_sequence_swallows_picture());
        let cp = live.checkpoint_carry(0).expect("carried");
        assert_eq!(
            cp.parser.as_ref().map(|carry| carry.state),
            Some(aterm_parser::HOOKED_STRING_CARRY)
        );
        let mut restored = Terminal::from_checkpoint(&cp);
        restored.process(tail);
        live.process(tail);
        assert_eq!(
            restored.visible_content(),
            live.visible_content(),
            "{:?}",
            String::from_utf8_lossy(head)
        );
        assert_eq!(
            restored.row_text(1).as_deref().map(str::trim_end),
            Some("after"),
            "{:?}",
            String::from_utf8_lossy(head)
        );
        assert_eq!(restored.title(), live.title(), "the inner sequence lands");
    }

    for tail in [&b"#0!10~-\x1b\\after"[..], &b"#0!10~-\x9cafter"[..]] {
        let mut sixel = Terminal::new(4, 30);
        sixel.process(b"one\r\n\x1bPq#0;2;0;0;0");
        assert_eq!(sixel.partial_sequence_uncarried(), None);
        assert!(
            sixel.partial_sequence_swallowed().is_some(),
            "a sixel the engine decodes is a picture the carry swallows"
        );
        assert!(sixel.partial_sequence_swallows_picture());
        let cp = sixel.checkpoint_carry(0).expect("carried");
        let mut restored = Terminal::from_checkpoint(&cp);
        restored.process(tail);
        assert_eq!(
            restored.row_text(1).as_deref().map(str::trim_end),
            Some("after"),
            "{tail:?}: the tail is swallowed, never printed, and what follows prints"
        );
        assert_eq!(
            sixel.parser.state().name(),
            "DcsPassthrough",
            "live parser untouched"
        );
    }

    let mut query = Terminal::new(4, 30);
    query.process(b"\x1bP+q");
    assert_eq!(query.partial_sequence_uncarried(), Some("DcsPassthrough"));
    assert!(query.checkpoint_carry(0).is_none());
    let mut restore = Terminal::new(4, 30);
    restore.process(b"\x1bP1$t");
    assert_eq!(restore.partial_sequence_uncarried(), Some("DcsPassthrough"));
    assert!(restore.checkpoint_carry(0).is_none());
}

#[test]
fn hooked_dcs_escape_keeps_consumed_strings_but_refuses_stateful_operations() {
    for head in [
        &b"\x1bP$qm\x1b"[..],
        b"\x1bP+q544E\x1b",
        b"\x1bP1$t1;1;1;@;@;H;0;2;@;BBBB\x1b",
    ] {
        let mut operation = Terminal::new(4, 30);
        operation.process(head);
        assert_eq!(operation.take_response(), None, "{head:?}");
        assert!(operation.partial_sequence_uncarried().is_some(), "{head:?}");
        assert!(operation.checkpoint_carry(0).is_none(), "{head:?}");
    }
    for head in [&b"\x1bP1000p%end\x1b"[..], b"\x1bPq#0;2;0;0;0#0~\x1b"] {
        for tail in [&b"\\after"[..], b"]2;inner\x07after"] {
            let mut live = Terminal::new(4, 30);
            live.process(b"one\r\n");
            live.process(head);
            assert_eq!(live.partial_sequence_uncarried(), None, "{head:?}");
            let cp = live.checkpoint_carry(0).expect("carried");
            assert_eq!(
                cp.parser.as_ref().map(|carry| carry.state),
                Some(aterm_parser::State::Escape),
                "{head:?}: the ESC alone"
            );
            let mut restored = Terminal::from_checkpoint(&cp);
            restored.process(tail);
            live.process(tail);
            assert_eq!(
                restored.row_text(1).as_deref().map(str::trim_end),
                Some("after"),
                "{head:?} then {tail:?}"
            );
            assert_eq!(restored.title(), live.title(), "{head:?} then {tail:?}");
        }
    }
}

#[test]
fn a_string_with_a_0x9c_inside_a_character_is_swallowed_live_and_carried() {
    let strings: [(&[u8], &str); 3] = [
        (b"\x1bP1000p", "%output %1 \u{672c}\u{d71c}\u{1f71c}\n%end"),
        (b"\x1bP@kitty-cmd", "{\"t\":\"\u{672c}\"}"),
        (b"\x1b_G", "a=q,\u{672c}"),
    ];
    for (head, payload) in strings {
        let stream = [head, payload.as_bytes(), b"\x1b\\after"].concat();
        let mut live = Terminal::new(4, 30);
        live.process(b"one\r\n");
        live.process(&stream);
        assert_eq!(
            live.row_text(1).as_deref().map(str::trim_end),
            Some("after"),
            "{payload:?}: swallowed whole, live"
        );

        let split = head.len() + payload.find('\u{672c}').expect("holds 本") + 1;
        let (before, after) = stream.split_at(split);
        let mut outgoing = Terminal::new(4, 30);
        outgoing.process(b"one\r\n");
        outgoing.process(before);
        let cp = outgoing.checkpoint_carry(0).expect("carried");
        assert_eq!(
            cp.parser
                .as_ref()
                .map(|carry| carry.string_utf8_tail.as_slice()),
            Some(&[0xE6][..]),
            "{payload:?}: the carry holds the character's head"
        );
        #[cfg(feature = "serde")]
        let cp = CheckpointMeta::from_checkpoint(&cp)
            .into_checkpoint(cp.grid.clone(), cp.alt_grid.clone());
        let mut restored = Terminal::from_checkpoint(&cp);
        restored.process(after);
        assert_eq!(
            restored.row_text(1).as_deref().map(str::trim_end),
            Some("after"),
            "{payload:?}: swallowed whole, across the handoff"
        );
    }
}

#[test]
fn every_partial_parser_state_is_carried_and_named() {
    let partials: [(&[u8], &str, Option<&str>); 13] = [
        (b"\x1b", "Escape", Some("Escape")),
        (b"\x1b(", "EscapeIntermediate", Some("EscapeIntermediate")),
        (b"\x1b[", "CsiEntry", Some("CsiEntry")),
        (b"\x1b[1;", "CsiParam", Some("CsiParam")),
        (b"\x1b[?1$", "CsiIntermediate", Some("CsiIntermediate")),
        (b"\x1bP", "DcsEntry", Some("DcsEntry")),
        (b"\x1bP1;2", "DcsParam", Some("DcsParam")),
        (b"\x1bP1$", "DcsIntermediate", Some("DcsIntermediate")),
        (
            b"\x1bPq#0;2;0;0;0",
            "DcsPassthrough",
            Some("SosPmApcString"),
        ),
        (b"\x1bPtmu", "DcsIgnore", Some("DcsIgnore")),
        (b"\x1bP$qm", "DcsPassthrough", None),
        (b"\x1b]2;half a title", "OscString", Some("OscString")),
        (b"\x1b_Gf=100;", "SosPmApcString", Some("SosPmApcString")),
    ];
    for (partial, expected, carried) in partials {
        let mut t = Terminal::new(6, 30);
        t.process(b"\x1b[1mbold\x1b[0m\r\n");
        for i in 0..12 {
            t.process(format!("history {i}\r\n").as_bytes());
        }
        t.process(partial);
        let live_state = t.parser.state();
        assert_eq!(live_state.name(), expected, "{partial:?}");

        let (cp, abandoned) = t.checkpoint_carry_abandoning_partial(5);
        assert_eq!(
            abandoned,
            carried.is_none().then_some(expected),
            "{partial:?}"
        );
        assert_eq!(
            cp.parser.as_ref().map(|carry| carry.state.name()),
            carried,
            "{partial:?}"
        );
        assert_eq!(cp.parser_ground, carried.is_none(), "{partial:?}");
        assert_eq!(cp.history_lines, 5, "the history bound still applies");
        assert_eq!(t.parser.state(), live_state, "live parser untouched");

        let restored = Terminal::from_checkpoint(&cp);
        assert_eq!(
            restored.parser.state().name(),
            carried.unwrap_or("Ground"),
            "{partial:?}"
        );
        assert_eq!(restored.visible_content(), t.visible_content());
        assert_eq!(
            restored.checkpoint_carry_abandoning_partial(5).0.parser,
            cp.parser,
            "{partial:?}: a restored carry re-projects to itself"
        );
    }
}

#[test]
fn the_abandoning_carry_of_a_ground_parser_is_the_ordinary_carry() {
    let t = build_rich_terminal(12, 40);
    for max_history in [0, 3, usize::MAX] {
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(max_history),
            (t.checkpoint_carry(max_history).expect("Ground"), None)
        );
    }
}

#[test]
fn the_pre_projection_reads_agree_with_the_projection() {
    let mut t = Terminal::new(6, 30);
    assert_eq!(t.partial_sequence_state(), None);
    assert!(!t.has_inactive_grid());
    assert!(t.checkpoint_carry(0).expect("Ground").alt_grid.is_none());

    t.process(b"\x1b[?1049h");
    assert!(t.has_inactive_grid());
    assert!(t.checkpoint_carry(0).expect("Ground").alt_grid.is_some());

    t.process(b"\x1b[?1049l");
    assert!(!t.is_alternate_screen());
    assert_eq!(
        t.has_inactive_grid(),
        t.checkpoint_carry(0).expect("Ground").alt_grid.is_some(),
        "after 1049 exit the read still matches the projection"
    );

    t.process(b"\x1b[38;5;19");
    assert_eq!(t.partial_sequence_state(), Some("CsiParam"));
    assert_eq!(
        t.partial_sequence_uncarried(),
        None,
        "a split CSI is carried"
    );
    assert_eq!(
        t.checkpoint_carry_abandoning_partial(0).1,
        t.partial_sequence_uncarried()
    );
    t.process(b"6m");
    assert_eq!(t.partial_sequence_state(), None);

    t.process(b"\x1bP$qm");
    assert_eq!(t.partial_sequence_uncarried(), Some("DcsPassthrough"));
    assert_eq!(
        t.checkpoint_carry_abandoning_partial(0).1,
        t.partial_sequence_uncarried()
    );
}

#[test]
fn the_abandoning_carry_carries_the_shell_integration_nonce() {
    let nonce = [0x5Au8; 32];
    let mut t = Terminal::new(4, 20);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    let (ground, _) = t.checkpoint_carry_abandoning_partial(0);
    assert_eq!(
        ground.shell_integration_nonce,
        Some(ShellIntegrationNonce(nonce))
    );
    assert_eq!(Some(ground), t.checkpoint_carry(0));

    t.process(b"\x1b]0;half");
    let (partial, abandoned) = t.checkpoint_carry_abandoning_partial(0);
    assert_eq!(abandoned, None, "the partial OSC is carried");
    assert!(partial.parser.is_some());
    assert_eq!(
        partial.shell_integration_nonce,
        Some(ShellIntegrationNonce(nonce)),
        "a mid-sequence tab keeps its nonce across the handoff"
    );
}

#[test]
fn only_the_carry_projection_takes_the_shell_integration_nonce() {
    let nonce = [0xA5u8; 32];
    let mut t = Terminal::new(4, 20);
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::Off);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

    assert_eq!(t.checkpoint().shell_integration_nonce, None);
    let carry = t.checkpoint_carry(0).expect("Ground");
    assert_eq!(
        carry.shell_integration_nonce,
        Some(ShellIntegrationNonce(nonce))
    );
    assert!(!format!("{carry:?}").contains("a5a5"), "Debug redacts it");

    #[cfg(feature = "serde")]
    {
        let meta = CheckpointMeta::from_checkpoint(&carry);
        assert_eq!(
            meta.shell_integration_nonce.as_deref(),
            Some("a5".repeat(32).as_str())
        );
        let back = meta.clone().into_checkpoint(carry.grid.clone(), None);
        assert_eq!(back.shell_integration_nonce, carry.shell_integration_nonce);
        let mut bad = meta;
        bad.shell_integration_nonce = Some("a5".repeat(31));
        assert_eq!(
            bad.into_checkpoint(carry.grid.clone(), None)
                .shell_integration_nonce,
            None,
            "a short nonce is no nonce"
        );
    }

    let mut adopted = Terminal::new(4, 20);
    adopted.restore_checkpoint(&carry);
    assert_eq!(
        adopted.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    adopted.authorize_shell_integration(carry.shell_integration_nonce.expect("carried").0);
    assert_eq!(
        adopted.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
}

#[test]
fn a_signed_integration_rev_is_recorded_and_carried() {
    let key = [0x5Au8; 32];
    let id = format!(";id={}", ShellIntegrationNonce(key).to_hex());
    let mark = |rev: &str, id: &str| format!("\x1b]633;P;AtermIntegration={rev}{id}\x07");
    let (rev_a, rev_b) = ("0123456789abcdef", "fedcba9876543210");

    let mut t = Terminal::new(4, 20);
    t.process(mark(rev_a, "").as_bytes());
    assert_eq!(t.shell_integration_rev(), None, "unsigned: not recorded");

    t.authorize_shell_integration(key);
    t.set_require_shell_integration_nonce(true);
    t.process(mark(rev_a, ";id=00").as_bytes());
    assert_eq!(t.shell_integration_rev(), None, "a wrong id is dropped");
    for bad in ["0123456789ABCDEF", "0123456789abcde", "../../etc", ""] {
        t.process(mark(bad, &id).as_bytes());
        assert_eq!(t.shell_integration_rev(), None, "{bad:?} is no address");
    }
    t.process(mark(rev_a, &id).as_bytes());
    assert_eq!(t.shell_integration_rev(), Some(rev_a));
    t.process(mark(rev_b, &id).as_bytes());
    assert_eq!(t.shell_integration_rev(), Some(rev_b), "the latest wins");
    t.process(b"\x1bc");
    assert_eq!(t.shell_integration_rev(), Some(rev_b), "RIS keeps it");

    assert_eq!(t.checkpoint().shell_integration_rev, None);
    let carry = t.checkpoint_carry(0).expect("Ground");
    assert_eq!(carry.shell_integration_rev.as_deref(), Some(rev_b));
    assert_eq!(
        t.checkpoint_carry_abandoning_partial(0)
            .0
            .shell_integration_rev
            .as_deref(),
        Some(rev_b)
    );
    #[cfg(feature = "serde")]
    {
        let meta = CheckpointMeta::from_checkpoint(&carry);
        assert_eq!(meta.shell_integration_rev.as_deref(), Some(rev_b));
        let back = meta.clone().into_checkpoint(carry.grid.clone(), None);
        assert_eq!(back.shell_integration_rev.as_deref(), Some(rev_b));
        let mut bad = meta;
        bad.shell_integration_rev = Some("not-an-address".into());
        assert_eq!(
            bad.into_checkpoint(carry.grid.clone(), None)
                .shell_integration_rev,
            None
        );
    }

    let mut adopted = Terminal::new(4, 20);
    assert_eq!(adopted.shell_integration_rev(), None);
    adopted.restore_checkpoint(&carry);
    assert_eq!(adopted.shell_integration_rev(), Some(rev_b), "installed");
    let mut plain = carry.clone();
    plain.shell_integration_rev = None;
    adopted.restore_checkpoint(&plain);
    assert_eq!(adopted.shell_integration_rev(), Some(rev_b));

    assert_eq!(
        Terminal::from_checkpoint(&carry).shell_integration_rev(),
        Some(rev_b),
        "from_checkpoint installs the carried rev"
    );
    assert_eq!(
        Terminal::from_checkpoint(&plain).shell_integration_rev(),
        None,
        "from_checkpoint of a carry naming none has none"
    );
}

#[test]
fn a_rekey_is_in_use_only_from_the_first_mark_signed_with_it() {
    let key = [0x3Cu8; 32];
    let mut t = Terminal::new(4, 20);
    t.set_require_shell_integration_nonce(true);
    t.authorize_shell_integration_on_first_mark(key);
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "handed over, not yet taken"
    );
    assert_eq!(
        t.checkpoint_carry(0)
            .expect("Ground")
            .shell_integration_nonce,
        None,
        "an untaken key is not carried"
    );
    t.process(format!("\x1b]133;A;id={}\x07", "1".repeat(64)).as_bytes());
    t.process(b"\x1b]133;A\x07");
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "a wrong or missing id is not the shell taking the key"
    );
    t.process(format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(key).to_hex()).as_bytes());
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);
    assert_eq!(
        t.checkpoint_carry(0)
            .expect("Ground")
            .shell_integration_nonce,
        Some(ShellIntegrationNonce(key)),
        "in use now, so carried"
    );

    let mut spawned = Terminal::new(4, 20);
    spawned.set_require_shell_integration_nonce(true);
    spawned.authorize_shell_integration(key);
    assert_eq!(
        spawned.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
    let mut both = Terminal::new(4, 20);
    both.set_require_shell_integration_nonce(true);
    both.authorize_shell_integration_on_first_mark(key);
    both.authorize_shell_integration(key);
    assert_eq!(
        both.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
}

#[test]
fn a_typed_rekey_is_taken_back_until_the_shell_reads_it() {
    let key = [0x4Du8; 32];
    let signed = |k: &[u8; 32]| {
        format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(*k).to_hex()).into_bytes()
    };
    let lost = || {
        let mut t = Terminal::new(4, 20);
        t.set_require_shell_integration_nonce(true);
        t
    };

    let mut t = lost();
    t.authorize_shell_integration_rekey(key);
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    assert!(t.withdraw_shell_integration_rekey(&key));
    assert!(!t.withdraw_shell_integration_rekey(&key), "once");
    let dropped = t.shell_integration_dropped_count();
    t.process(&signed(&key));
    assert_eq!(t.shell_integration_dropped_count(), dropped + 1);
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );

    let mut t = lost();
    t.authorize_shell_integration_rekey(key);
    assert!(
        !t.settle_shell_integration_rekey(&[0x11; 32]),
        "not that key"
    );
    assert!(t.settle_shell_integration_rekey(&key));
    assert!(!t.withdraw_shell_integration_rekey(&key), "settled");
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "the file was read, no mark yet"
    );
    t.process(&signed(&key));
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

    let mut t = lost();
    t.authorize_shell_integration_rekey(key);
    t.process(&signed(&key));
    assert!(!t.withdraw_shell_integration_rekey(&key));
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

    let channel = [0x5Eu8; 32];
    let mut t = lost();
    t.authorize_shell_integration_on_first_mark(channel);
    t.authorize_shell_integration_rekey([0x6F; 32]);
    t.authorize_shell_integration_rekey(key);
    assert!(!t.withdraw_shell_integration_rekey(&[0x6F; 32]), "replaced");
    assert!(t.withdraw_shell_integration_rekey(&key));
    t.process(&signed(&channel));
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

    let mut t = lost();
    t.authorize_shell_integration_rekey(key);
    t.authorize_shell_integration(key);
    assert!(!t.withdraw_shell_integration_rekey(&key));
    assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);
}

#[test]
fn shell_integration_nonce_hex_round_trips_and_refuses_malformed() {
    let n = ShellIntegrationNonce(core::array::from_fn(|i| i as u8));
    assert_eq!(ShellIntegrationNonce::from_hex(&n.to_hex()), Some(n));
    assert_eq!(
        ShellIntegrationNonce::from_hex(&n.to_hex().to_uppercase()),
        Some(n)
    );
    assert_eq!(ShellIntegrationNonce::from_hex(""), None);
    assert_eq!(ShellIntegrationNonce::from_hex(&"g".repeat(64)), None);
    assert_eq!(ShellIntegrationNonce::from_hex(&"0".repeat(66)), None);
}

#[test]
fn carry_keeps_title_icon_and_stack() {
    let mut t = Terminal::new(4, 20);
    t.process(b"\x1b]1;the-icon\x07\x1b]2;the caller\x07");
    t.process(b"\x1b[22;0t\x1b]2;vim main.rs\x07");
    let expected = TitleRepr {
        window: "vim main.rs".to_string(),
        icon: "the-icon".to_string(),
        stack: vec![("the-icon".to_string(), "the caller".to_string())],
        ..TitleRepr::default()
    };
    let carry = t.checkpoint_carry(0).expect("Ground");
    assert_eq!(carry.title.as_ref(), Some(&expected), "projected whole");
    assert_eq!(
        t.checkpoint_carry_abandoning_partial(0).0.title.as_ref(),
        Some(&expected),
        "the mid-sequence projection takes it too"
    );
    #[cfg(feature = "serde")]
    let carry = {
        let meta = CheckpointMeta::from_checkpoint(&carry);
        assert_eq!(meta.title.as_ref(), Some(&expected), "the meta names it");
        assert_eq!(
            t.carry_meta_abandoning_partial().title.as_ref(),
            Some(&expected),
            "so does the Repaint rung's scalar-only meta"
        );
        let back = meta.into_checkpoint(carry.grid.clone(), carry.alt_grid.clone());
        assert_eq!(back, carry, "the meta reassembles the carry exactly");
        back
    };

    let mut adopted = Terminal::new(4, 20);
    let epoch = adopted.title_epoch();
    adopted.restore_checkpoint(&carry);
    assert_eq!(adopted.title(), "vim main.rs", "the tab keeps its label");
    assert_eq!(adopted.icon_name(), "the-icon");
    assert!(
        adopted.title_epoch() > epoch,
        "a restored title is a change the host must see"
    );
    adopted.process(b"\x1b[23;0t");
    assert_eq!(
        adopted.title(),
        "the caller",
        "the program's exit puts its caller's title back, across the update"
    );

    let rebuilt = Terminal::from_checkpoint(&t.checkpoint());
    assert_eq!(rebuilt.checkpoint(), t.checkpoint());
    assert_eq!(rebuilt.title(), "vim main.rs");

    let mut untitled = carry.clone();
    untitled.title = None;
    let mut seeded = Terminal::new(4, 20);
    seeded.restore_title(&TitleRepr {
        window: "from the record".to_string(),
        ..TitleRepr::default()
    });
    seeded.restore_checkpoint(&untitled);
    assert_eq!(seeded.title(), "from the record");
    let mut blank = carry;
    blank.title = Some(TitleRepr::default());
    seeded.restore_checkpoint(&blank);
    assert_eq!(seeded.title(), "", "an untitled program stays untitled");
}

#[test]
fn a_checkpoint_preserves_saved_empty_title_slots() {
    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for projection in ["memory", "carry", "journal"] {
        for (label, prefix, tail, window, icon) in [
            (
                "nonempty",
                &b"\x1b]0;caller\x07\x1b[22t\x1b]0;program\x07"[..],
                &b"\x1b[23t"[..],
                "caller",
                "caller",
            ),
            (
                "empty window, unsaved icon",
                b"\x1b]1;icon\x07\x1b[22;2t\x1b]0;program\x07",
                b"\x1b[23;2t",
                "",
                "program",
            ),
            (
                "empty icon, unsaved window",
                b"\x1b]2;window\x07\x1b[22;1t\x1b]0;program\x07",
                b"\x1b[23;1t",
                "program",
                "",
            ),
            (
                "both saved empty",
                b"\x1b[22t\x1b]0;program\x07",
                b"\x1b[23t",
                "",
                "",
            ),
            (
                "older saved empty window",
                b"\x1b]1;icon\x07\x1b[22;2t\x1b[22;1t\x1b]0;program\x07",
                b"\x1b[23;2t",
                "",
                "program",
            ),
        ] {
            let mut live = Terminal::new(4, 20);
            live.process(prefix);
            let checkpoint = match projection {
                "memory" => live.checkpoint(),
                "carry" => live.checkpoint_carry(0).expect("Ground carry"),
                "journal" => live.checkpoint_journal(0).expect("Ground journal"),
                _ => unreachable!(),
            };
            #[cfg(feature = "serde")]
            let checkpoint = {
                let meta = CheckpointMeta::from_checkpoint(&checkpoint);
                let back =
                    meta.into_checkpoint(checkpoint.grid.clone(), checkpoint.alt_grid.clone());
                assert_eq!(back, checkpoint, "{projection}/{label}: meta reassembly");
                back
            };
            let mut adopted = Terminal::from_checkpoint(&checkpoint);
            live.process(tail);
            adopted.process(tail);
            assert_eq!(
                (live.title(), live.icon_name()),
                (window, icon),
                "{projection}/{label}: live premise"
            );
            if label == "nonempty" {
                assert_eq!(
                    (adopted.title(), adopted.icon_name()),
                    (window, icon),
                    "{projection}: positive handoff"
                );
            }
            observed.push((
                projection,
                label,
                adopted.title().to_string(),
                adopted.icon_name().to_string(),
            ));
            expected.push((projection, label, window.to_string(), icon.to_string()));
        }
    }
    assert_eq!(
        observed, expected,
        "a checkpoint must distinguish saved empty slots from slots a push did not save"
    );
}

#[test]
fn a_carried_title_is_sanitized_and_capped() {
    let hostile = format!(
        "{}\x1b]2;owned\x07\u{202E}moc.evil\u{9b}\r\n{}",
        "x".repeat(5),
        "y".repeat(10 * 1024)
    );
    let repr = TitleRepr {
        window: hostile.clone(),
        icon: hostile.clone(),
        stack: (0..15)
            .map(|n| (format!("icon {n}\x07"), format!("window {n}\x1b")))
            .collect(),
        ..TitleRepr::default()
    };
    let mut t = Terminal::new(4, 20);
    t.restore_title(&repr);
    let clean = |s: &str| {
        s.len() <= 1024
            && !s.chars().any(|c| {
                (c < ' ' && c != '\t')
                    || ('\u{80}'..='\u{9f}').contains(&c)
                    || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
            })
    };
    assert!(clean(t.title()), "window title: {:?}", t.title());
    assert!(clean(t.icon_name()), "icon name: {:?}", t.icon_name());
    assert!(t.title().starts_with("xxxxx]2;owned"), "{:?}", t.title());
    let stack = t.capture_title_repr().stack;
    assert_eq!(
        stack.len(),
        super::super::TITLE_STACK_MAX_DEPTH,
        "the stack is bounded"
    );
    assert_eq!(
        stack.first(),
        Some(&("icon 0".to_string(), "window 0".to_string())),
        "the bottom entries are the ones kept, controls stripped"
    );
    assert!(
        stack
            .iter()
            .all(|(icon, window)| clean(icon) && clean(window))
    );
    let again = t.capture_title_repr();
    let mut twin = Terminal::new(4, 20);
    twin.restore_title(&again);
    assert_eq!(twin.capture_title_repr(), again, "restoring is idempotent");
}

#[test]
fn the_journal_keyframe_continues_the_stream_it_was_cut_from() {
    let tail: &[u8] = b"lor\x1b[0m after\r\nnext line\r\n";
    for (label, cut) in [
        ("ground", &b"\x1b[31mco"[..]),
        ("mid-csi", &b"\x1b[31mco\x1b[3"[..]),
        ("mid-osc", &b"\x1b]0;a title being wri"[..]),
        ("mid-utf8", &b"caf\xc3"[..]),
    ] {
        let mut live = build_rich_terminal(6, 24);
        live.process(cut);
        let keyframe = live.checkpoint_journal(512).expect("carried");
        assert_eq!(
            (keyframe.parser.is_some(), keyframe.parser_ground),
            (label != "ground", label == "ground"),
            "{label}: a split sequence rides the keyframe's parser carry"
        );
        let mut restored = Terminal::from_checkpoint(&keyframe);
        assert_eq!(
            restored.checkpoint_journal(512),
            Some(keyframe.clone()),
            "{label}: the keyframe round-trips"
        );
        live.process(tail);
        restored.process(tail);
        assert_eq!(
            restored.checkpoint_continuing().map(lineage_taken),
            live.checkpoint_continuing(),
            "{label}: the replayed tail lands where the live one did"
        );
    }

    let mut deep = Terminal::new(6, 24);
    for i in 0..40 {
        deep.process(format!("history {i}\r\n").as_bytes());
    }
    let history = deep.grid.scrollback_lines();
    assert!(history > 8, "{history}");
    assert_eq!(deep.checkpoint_journal(8).expect("ground").history_lines, 8);
    assert_eq!(
        deep.checkpoint_journal(0).expect("ground").history_lines,
        0,
        "a zero bridge is the visible screen"
    );
    assert_eq!(
        deep.checkpoint_journal(usize::MAX)
            .expect("ground")
            .history_lines as usize,
        history,
        "a bridge past the history carries all of it"
    );

    let mut query = Terminal::new(4, 20);
    query.process(b"hello\x1bP$qm");
    assert!(
        query.checkpoint_journal(512).is_none(),
        "a hooked DCS query defers the keyframe"
    );
    query.process(b"\x1b\\");
    assert!(
        query.checkpoint_journal(512).is_some(),
        "and its end allows it"
    );
}

#[test]
fn the_marked_keyframe_carries_the_marks_its_bridge_does_not_reach() {
    let mut t = Terminal::new(4, 30);
    for command in 0..5u8 {
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07");
        for _ in 0..9 {
            t.process(b"out\r\n");
        }
        t.process(format!("\x1b]133;D;{command}\x07").as_bytes());
    }
    let top = t.grid.visible_to_absolute(0);
    let marks: Vec<(u64, u64)> = t
        .command_marks()
        .iter()
        .map(|m| (m.prompt_start_row, m.output_end_row.expect("finished") + 1))
        .collect();
    let rows: Vec<u64> = marks.iter().map(|&(prompt, _)| prompt).collect();
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|&r| r < top), "premise: all in the history");
    let reaching = |floor: u64| -> Vec<u64> {
        marks
            .iter()
            .filter(|&&(_, end)| end > floor)
            .map(|&(prompt, _)| prompt)
            .collect()
    };
    let within = |c: &TerminalCheckpoint| -> Vec<u64> {
        c.shell
            .command_marks
            .iter()
            .map(|m| m.prompt_start_row)
            .collect()
    };
    let plain = t.checkpoint_journal(0).expect("ground");
    let on_screen = reaching(top);
    assert_eq!(
        within(&plain),
        on_screen,
        "the bridge's own floor: the last command's output reaches the screen"
    );
    assert_eq!(on_screen.len(), 1, "premise: one reaches the screen");
    let horizon = 25;
    let floor = top - horizon as u64;
    let marked = t.checkpoint_journal_marked(0, horizon).expect("ground");
    let want = reaching(floor);
    assert!(
        want.len() > on_screen.len() && want.len() < rows.len(),
        "premise: the horizon splits them: {want:?} of {rows:?}"
    );
    assert!(
        want.iter().any(|&prompt| prompt < floor),
        "premise: one reaches the horizon from a prompt row older than it: {want:?}"
    );
    assert_eq!(within(&marked), want, "the horizon, and no further");
    let blocks = |c: &TerminalCheckpoint| -> Vec<u64> {
        c.shell
            .output_blocks
            .iter()
            .chain(c.shell.current_block.as_ref())
            .map(|b| b.span.prompt_start_row)
            .collect()
    };
    let want_blocks: Vec<u64> = t
        .all_blocks()
        .filter(|b| b.end_row.is_none_or(|end| end > floor))
        .map(|b| b.prompt_start_row)
        .collect();
    assert!(
        want_blocks.len() > 1,
        "premise: archived blocks inside the horizon"
    );
    assert_eq!(blocks(&marked), want_blocks, "their blocks too");
    assert_eq!(
        (marked.grid.clone(), marked.history_lines),
        (plain.grid.clone(), plain.history_lines),
        "the grids are the bridge's"
    );
    assert_eq!(
        within(&t.checkpoint_journal_marked(8, 0).expect("ground")),
        within(&t.checkpoint_journal(8).expect("ground")),
        "a bridge reaching further than the horizon keeps its own"
    );

    let restored = Terminal::from_checkpoint(&marked);
    assert_eq!(
        within(&restored.checkpoint_continuing_swallowing()),
        want,
        "the recovery's projection keeps them"
    );
    assert_eq!(
        within(&restored.checkpoint_continuing().expect("ground")),
        on_screen,
        "control: the carry's floor drops what its history does not reach"
    );
}

#[test]
fn the_journal_bridge_rides_the_saved_primary_on_the_alternate_screen() {
    let mut live = Terminal::new(5, 20);
    for i in 0..30 {
        live.process(format!("shell {i}\r\n").as_bytes());
    }
    live.process(b"\x1b[?1049hfull screen app");
    let keyframe = live.checkpoint_journal(10).expect("ground");
    assert_eq!(
        keyframe.history_lines, 0,
        "the alternate grid has no history"
    );
    assert_eq!(deserialize_lines(&keyframe.grid).len(), 5);
    let primary = keyframe.alt_grid.as_deref().expect("the saved primary");
    assert_eq!(
        deserialize_lines(primary).len(),
        5 + 10,
        "the bridge rides the saved primary"
    );
    let mut restored = Terminal::from_checkpoint(&keyframe);
    restored.process(b"\x1b[?1049l");
    live.process(b"\x1b[?1049l");
    assert_eq!(restored.visible_content(), live.visible_content());
    assert_eq!(restored.grid.scrollback_lines(), 10);
}

#[test]
fn the_journal_keyframe_carries_no_nonce_and_marks_are_counted() {
    let key = [7u8; 32];
    let mut t = Terminal::new(4, 40);
    t.set_require_shell_integration_nonce(true);
    t.authorize_shell_integration(key);
    assert_eq!(t.shell_marks_verified(), 0);
    t.process(
        format!(
            "\x1b]133;A;id={}\x07$ ",
            ShellIntegrationNonce(key).to_hex()
        )
        .as_bytes(),
    );
    assert_eq!(t.shell_marks_verified(), 1, "a verified mark is counted");
    t.process(format!("\x1b]133;B;id={}\x07", "1".repeat(64)).as_bytes());
    assert_eq!(
        t.shell_marks_verified(),
        1,
        "a mark with the wrong nonce is dropped, not counted"
    );
    assert_eq!(t.shell_integration_dropped_count(), 1);
    assert!(
        t.checkpoint_carry(0)
            .expect("ground")
            .shell_integration_nonce
            .is_some(),
        "control: the seamless carry takes the nonce"
    );
    let keyframe = t.checkpoint_journal(512).expect("ground");
    assert_eq!(keyframe.shell_integration_nonce, None);
    assert_eq!(keyframe.shell_integration_rev, None);

    let mut open = Terminal::new(4, 40);
    open.process(b"\x1b]133;A\x07");
    assert_eq!(
        open.shell_marks_verified(),
        0,
        "with no nonce required nothing is verified (and the replay applies the mark)"
    );
}

#[test]
fn the_journal_defers_sequences_a_carry_would_only_swallow() {
    for (label, head, tail) in [
        ("ground", &b"plain"[..], &b" tail"[..]),
        ("split-csi", &b"\x1b[3"[..], &b"1mred"[..]),
        ("split-osc", &b"\x1b]0;short "[..], &b"title\x07"[..]),
        ("split-glyph", &b"caf\xc3"[..], &b"\xa9"[..]),
        (
            "ignored-dcs",
            &b"\x1bPtmux;"[..],
            &b"ignored\x1b\\after"[..],
        ),
    ] {
        let mut live = Terminal::new(4, 20);
        live.process(head);
        assert_eq!(live.partial_sequence_uncarried(), None, "{label}");
        assert_eq!(live.partial_sequence_swallowed(), None, "{label}");
        let keyframe = live.checkpoint_journal(0).expect(label);
        let mut restored = Terminal::from_checkpoint(&keyframe);
        live.process(tail);
        restored.process(tail);
        assert_eq!(
            restored.checkpoint_continuing().map(lineage_taken),
            live.checkpoint_continuing(),
            "{label}: lossless parser state remains admitted"
        );
    }

    let mut long_title = b"\x1b]0;".to_vec();
    long_title.extend(std::iter::repeat_n(b'A', aterm_parser::OSC_CARRY_CAP + 1));
    let mut long_image = b"\x1b]1337;File=inline=1:".to_vec();
    long_image.extend(std::iter::repeat_n(b'A', aterm_parser::OSC_CARRY_CAP + 1));
    let mut refusals = Vec::new();
    for (label, head) in [
        ("oversize-title", &long_title[..]),
        ("oversize-image", &long_image[..]),
        ("active-apc", &b"\x1b_Ga=T,f=100;"[..]),
        ("hooked-sixel", &b"\x1bPq#0;2;0;0;0"[..]),
    ] {
        let mut live = Terminal::new(4, 20);
        live.process(head);
        assert_eq!(live.partial_sequence_uncarried(), None, "{label}");
        assert!(
            live.partial_sequence_swallowed().is_some(),
            "{label}: the actual engine has the intended carry state"
        );
        let before = live.checkpoint_continuing();
        let refused = live.checkpoint_journal(0).is_none();
        assert_eq!(
            live.checkpoint_continuing(),
            before,
            "{label}: the capture does not mutate the live parser"
        );
        refusals.push((label, refused));
        live.process(b"\x1b\\");
        assert!(live.checkpoint_journal(0).is_some(), "{label}: ended");
    }
    assert!(
        refusals.iter().all(|(_, refused)| *refused),
        "lossy parser states must defer the journal keyframe: {refusals:?}"
    );
}

#[test]
fn the_swallowing_projection_carries_a_hooked_query_as_a_string_to_swallow() {
    let mut query = Terminal::new(4, 20);
    query.process(b"before\r\n\x1bP+q544e");
    assert_eq!(query.partial_sequence_uncarried(), Some("DcsPassthrough"));
    assert_eq!(
        query.checkpoint_continuing(),
        None,
        "control: refused there"
    );
    let c = query.checkpoint_continuing_swallowing();
    assert!(!c.parser_ground);
    assert_eq!(c.shell_integration_nonce, None);
    let mut restored = Terminal::from_checkpoint(&c);
    restored.process(b"\x1b\\after");
    query.process(b"\x1b\\after");
    assert_eq!(
        restored.row_text(1),
        query.row_text(1),
        "the tail is swallowed"
    );
    assert!(
        query.take_response().is_some(),
        "control: the live engine answered the query"
    );
    assert!(
        restored.take_response().is_none(),
        "the restored one cannot: its reply lived in the handler"
    );

    for head in [&b"plain"[..], b"\x1b[3", b"\x1b]0;short ", b"\x1bPtmux;"] {
        let mut t = Terminal::new(4, 20);
        t.process(head);
        assert_eq!(
            Some(t.checkpoint_continuing_swallowing()),
            t.checkpoint_continuing(),
            "{head:?}: equal wherever checkpoint_continuing answers"
        );
    }
}

#[test]
fn the_journal_is_never_cut_inside_a_shell_mark() {
    let hex = "5a".repeat(32);
    for (payload, mark) in [
        (format!("133;A;id={hex}"), true),
        ("133;".to_owned(), true),
        ("0133;B".to_owned(), true),
        (format!("633;E;ls;id={hex}"), true),
        ("133".to_owned(), false),
        ("13".to_owned(), false),
        ("0;a title".to_owned(), false),
        ("1337;SetMark".to_owned(), false),
        ("7;file:///tmp".to_owned(), false),
    ] {
        let mut t = Terminal::new(4, 80);
        t.process(format!("$ \x1b]{payload}").as_bytes());
        assert_eq!(t.partial_sequence_is_shell_mark(), mark, "{payload}");
        assert_eq!(t.checkpoint_journal(8).is_none(), mark, "{payload}");
        let carried = t.checkpoint_continuing().expect("an OSC is carried");
        assert_eq!(carried.parser_holds_shell_mark(), mark, "{payload}");
        t.process(b"\x07");
        assert!(!t.partial_sequence_is_shell_mark(), "{payload}: ended");
        assert!(t.checkpoint_journal(8).is_some(), "{payload}: ended");
    }
}

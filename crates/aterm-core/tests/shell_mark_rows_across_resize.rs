// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shell's `133;B` prompt mark reads back as its prompt row across height changes.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

const COLS: u16 = 137;
const PROMPT: &str = "me@a-studio-of-128gb aterm % ";

fn live(rows: u16) -> Terminal {
    Terminal::with_scrollback(rows, COLS, 1000, Scrollback::with_defaults())
}

fn prompt(t: &mut Terminal) {
    let mut v = Vec::new();
    v.extend_from_slice(b"\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m");
    v.extend_from_slice(" ".repeat(usize::from(COLS) - 1).as_bytes());
    v.extend_from_slice(b"\r \r\x1b]133;D;0\x07\x1b]133;A\x07");
    v.extend_from_slice(b"\r\x1b[0m\x1b[27m\x1b[24m\x1b[J");
    v.extend_from_slice(PROMPT.as_bytes());
    v.extend_from_slice(b"\x1b[K\x1b]133;B\x07\x1b[?2004h");
    t.process(&v);
}

fn run(t: &mut Terminal, line: &str) {
    t.process(line.as_bytes());
    t.process(b"\r\n\x1b]133;C\x07");
}

fn fullscreen(t: &mut Terminal) {
    t.process(b"\x1b[?1049h\x1b[2J\x1b[H");
    for r in 1..=t.rows() {
        t.process(format!("\x1b[{r};1Hconversation row {r}\x1b[K").as_bytes());
    }
}

fn sigterm_exit(t: &mut Terminal) {
    t.process(b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l");
    t.process(b"\x1b[?2026h\x1b[?25h\x1b[?2026l");
    t.process(
        b"\x1b[?1004l\x1b[<u\x1b[?1049l\x1b[>4m\x1b[>4m\x1b[<u\x1b[?2031l\x1b[?2004l\x1b(B\x0f\
          \x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b7\x1b[r\x1b8",
    );
    t.process(b"\x1b[?25h");
    t.process(
        b"\x1b(B\x0f\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[>4m\x1b[?1004l\
          \x1b[?2031l\x1b[?2004l\x1b[<u\x1b[?25h\x1b7\x1b[r\x1b8\x1b]0;\x07",
    );
    t.process(
        b"\x1b[2m\x1b[22m\r\n\x1b[2mResume this session with:\x1b[22m\r\n\
          \x1b[2mclaude --resume 253a40df-d85e-420b-85f5-0e322e9e663b\x1b[22m\r\n\x1b[2m\x1b[22m",
    );
}

fn assert_mark_is_the_prompt(t: &Terminal, label: &str) {
    let block = t.all_blocks().last().expect("a block");
    let cmd = block.command_start_row.expect("133;B recorded");
    let cursor = t.cursor();
    let at_mark = t.abs_row_text(cmd).unwrap_or_default();
    let at_cursor = t
        .get_line_text(i32::from(cursor.row), None)
        .unwrap_or_default();
    assert_eq!(
        at_mark.trim_end(),
        at_cursor.trim_end(),
        "{label}: the mark (row {cmd}) must read back as the prompt row"
    );
    assert_eq!(at_cursor.trim_end(), PROMPT.trim_end(), "{label}");
    assert_eq!(block.command_start_col, Some(cursor.col), "{label}");
}

#[test]
fn a_tab_that_grew_before_it_had_history_reads_its_marks_back() {
    let mut t = live(30);
    t.resize(31, COLS);
    for i in 1..=40 {
        t.process(format!("{i}\r\n").as_bytes());
    }
    prompt(&mut t);
    assert_mark_is_the_prompt(&t, "after a grow of a fresh tab");
}

#[test]
fn the_prompt_after_a_fullscreen_agents_exit_reads_back_as_the_prompt() {
    for (adopt_rows, parked) in [(52u16, [53u16, 53]), (52, [53, 52]), (51, [53, 54])] {
        let mut old = live(adopt_rows);
        for i in 0..400 {
            old.process(format!("line {i}\r\n").as_bytes());
        }
        prompt(&mut old);
        run(
            &mut old,
            "claude --dangerously-skip-permissions --resume 253a40df",
        );
        fullscreen(&mut old);
        let mut carry = old.checkpoint_visible().expect("parser at ground");
        carry.absolute_row_counter = 0;
        carry.alt_absolute_row_counter = 0;
        let mut t = Terminal::with_scrollback(carry.rows, COLS, 1000, Scrollback::with_defaults());
        t.restore_checkpoint(&carry);
        t.resize(parked[0], COLS);
        sigterm_exit(&mut t);
        prompt(&mut t);
        assert_mark_is_the_prompt(&t, "the first prompt after the adopt");
        run(
            &mut t,
            " '/Users//me/Library/Application Support/aterm/pkg/agents/claude' '--resume' '253a40df-d85e-420b-85f5-0e322e9e663b'",
        );
        fullscreen(&mut t);
        t.resize(parked[1], COLS);
        sigterm_exit(&mut t);
        prompt(&mut t);
        assert_mark_is_the_prompt(&t, "the prompt the relaunch reads");
        for rows in [54u16, 53, 52] {
            t.resize(rows, COLS);
            t.process(b"\r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J");
            t.process(PROMPT.as_bytes());
            assert_mark_is_the_prompt(&t, &format!("after a resize to {rows} rows"));
        }
    }
}

#[test]
fn the_counter_covers_every_retained_row() {
    let mut state: u64 = 0x2026_1001;
    let mut next = |n: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % n
    };
    for script in 0..300 {
        let mut rows: u16 = 24;
        let mut t = live(rows);
        let mut steps = Vec::new();
        for _ in 0..40 {
            let step = match next(8) {
                0 => {
                    let n = next(40);
                    for i in 0..n {
                        t.process(format!("out {i}\r\n").as_bytes());
                    }
                    format!("out{n}")
                }
                1 => {
                    t.process(b"\x1b[?1049h\x1b[2J\x1b[Hfullscreen");
                    "1049h".to_string()
                }
                2 => {
                    t.process(b"\x1b[?1049l\r\nbye\r\n");
                    "1049l".to_string()
                }
                3 => {
                    t.process(b"\x1b[3J");
                    "3J".to_string()
                }
                4 | 5 => {
                    rows = (i64::from(rows) + i64::try_from(next(5)).unwrap_or(0) - 2)
                        .clamp(8, 40)
                        .try_into()
                        .unwrap_or(24);
                    let cols = match next(4) {
                        0 | 1 => t.cols(),
                        _ => 60 + u16::try_from(next(90)).unwrap_or(0),
                    };
                    if next(2) == 0 {
                        t.resize(rows, cols);
                    } else {
                        let mut job = t.resize_offloading_scrollback(rows, cols);
                        while let Some(pending) = job {
                            job = t.finish_resize_offload(pending.reflow());
                        }
                    }
                    format!("resize{rows}x{cols}")
                }
                6 => {
                    let carry = t.checkpoint_carry(256).expect("parser at ground");
                    let mut adopted = Terminal::with_scrollback(
                        carry.rows,
                        carry.cols,
                        1000,
                        Scrollback::with_defaults(),
                    );
                    adopted.restore_checkpoint(&carry);
                    t = adopted;
                    "adopt".to_string()
                }
                _ => {
                    prompt(&mut t);
                    "prompt".to_string()
                }
            };
            steps.push(step);
            if t.is_alternate_screen() {
                continue;
            }
            let g = t.grid();
            let retained = u64::from(t.rows()) + g.scrollback_lines() as u64;
            assert!(
                g.absolute_row_counter() >= retained,
                "script {script}: counter {} below the {retained} retained rows after {steps:?}",
                g.absolute_row_counter()
            );
            let base = g.oldest_absolute_row() + g.scrollback_lines() as u64;
            assert_eq!(
                g.visible_to_absolute(0),
                base,
                "script {script}: the maps disagree after {steps:?}"
            );
        }
    }
}

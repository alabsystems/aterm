// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

/// `completed_command_seq` rises once per OSC 133;D, even for two completions in one
/// millisecond, and survives a reset.
#[test]
fn completed_command_seq_is_strictly_per_completion() {
    let mut term = Terminal::new(6, 40);
    assert_eq!(term.completed_command_seq(), 0);
    term.process(
        b"\x1b]133;A\x07$ a\x1b]133;B\x07\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07\
          \x1b]133;A\x07$ b\x1b]133;B\x07\r\n\x1b]133;C\x07no\r\n\x1b]133;D;1\x07",
    );
    assert_eq!(
        term.completed_command_seq(),
        2,
        "two same-instant completions must be two edges"
    );
    let before = term.completed_command_seq();
    term.reset();
    assert_eq!(
        term.completed_command_seq(),
        before,
        "the seq survives reset (monotonic for observers)"
    );
}

/// `started_command_seq` counts each C the A→B→C→D gate accepts, block or
/// no block, and nothing takes one back: RIS, ED 3 and Cmd-K drop the blocks
/// wholly in the history they erase (a running `reset` or `clear` keeps its
/// own, and its D is counted), and a checkpoint round trip carries it.
#[test]
fn started_command_seq_counts_every_accepted_c_and_no_clear_takes_one() {
    let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    let mut term = Terminal::new(6, 40);
    term.process(prompt);
    term.process(b"a\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    assert_eq!(term.started_command_seq(), 1);
    term.process(b"\x1b]133;A\x07\x1b]133;C\x07");
    assert_eq!(term.started_command_seq(), 1, "out of order: no start");
    term.process(prompt);
    term.process(b"reset\r\n\x1b]133;C\x07\x1bc\x1b]133;D;0\x07");
    assert_eq!(term.started_command_seq(), 2);
    assert_eq!(
        term.completed_command_seq(),
        2,
        "the D after the RIS counts"
    );
    assert_eq!(
        term.all_blocks().count(),
        3,
        "the RIS took none of the blocks on the screen (a, the refused C's prompt, reset)"
    );
    term.process(prompt);
    term.process(b"clear\r\n\x1b]133;C\x07\x1b[3J\x1b]133;D;0\x07");
    assert_eq!(term.started_command_seq(), 3);
    term.process(prompt);
    term.process(b"b\r\n\x1b]133;C\x07");
    term.clear_scrollback();
    term.process(b"\x1b]133;D;0\x07");
    assert_eq!(term.started_command_seq(), 4);
    term.reset();
    assert_eq!(term.started_command_seq(), 4, "survives reset");
    let carried = Terminal::from_checkpoint(&term.checkpoint());
    assert_eq!(carried.started_command_seq(), 4, "a checkpoint carries it");
}

/// The capability nonce the input-region tests' shell signs its marks with.
const KEY: [u8; 32] = [0x6B; 32];
/// The key a re-key hands that shell.
const NEW_KEY: [u8; 32] = [0x7C; 32];

/// OSC 133 `mark` signed with `key`, as aterm's integration sends it.
fn signed_with(key: [u8; 32], mark: &str) -> Vec<u8> {
    format!(
        "\x1b]133;{mark};id={}\x07",
        crate::terminal::ShellIntegrationNonce(key).to_hex()
    )
    .into_bytes()
}

/// OSC 133 `mark` signed with [`KEY`].
fn signed(mark: &str) -> Vec<u8> {
    signed_with(KEY, mark)
}

/// The `B` aterm's integration sends where the buffer starts: signed, and
/// saying so (`aterm_input=start`).
fn signed_b() -> Vec<u8> {
    signed("B;aterm_input=start")
}

/// The cell of the input region's start, if any.
fn cell(t: &Terminal) -> Option<(u16, u16)> {
    t.input_region_start().map(|s| (s.row, s.col))
}

/// A 4×20 engine whose host authorized [`KEY`] and turned the nonce gate
/// on, at a signed prompt `$ ` on row 0 with nothing typed.
fn at_signed_prompt() -> Terminal {
    let mut t = Terminal::new(4, 20);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed_b());
    t
}

/// `input_region_start` is the visible cell of the prompt's AUTHENTICATED
/// `B`, and that prompt's id: where the shell said its buffer starts,
/// whatever is typed after it, until the next prompt's own `B`, whose id
/// is another.
#[test]
fn input_region_start_is_the_signed_prompt_s_b() {
    let mut t = at_signed_prompt();
    let first = t.input_region_start().expect("the signed B");
    assert_eq!((first.row, first.col), (0, 2));
    assert_eq!(t.prompt_in_front(), Some(first.prompt));
    t.process(b"echo hi");
    assert_eq!(cell(&t), Some((0, 2)), "typing moves no start");
    t.process(b"\x1b]133;A\x07\x1b]133;B;aterm_input=start\x07");
    assert_eq!(cell(&t), Some((0, 2)), "an unsigned pair moves none");
    t.process(b"\r\n");
    t.process(&signed("C"));
    assert_eq!(t.prompt_in_front(), None, "a command runs");
    t.process(b"hi\r\n");
    t.process(&signed("D;0"));
    t.process(&signed("A"));
    t.process(b"$$ ");
    t.process(&signed_b());
    let next = t.input_region_start().expect("the next prompt's own B");
    assert_eq!((next.row, next.col), (2, 3));
    assert_ne!(next.prompt, first.prompt, "another prompt, another id");

    let mut unsigned = Terminal::new(4, 20);
    unsigned.authorize_shell_integration(KEY);
    unsigned.set_require_shell_integration_nonce(true);
    unsigned.process(b"\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x07");
    assert_eq!(cell(&unsigned), None, "unsigned marks are dropped");

    let mut open = Terminal::new(4, 20);
    open.process(b"\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x07");
    assert_eq!(
        open.current_block().and_then(|b| b.command_start_col),
        Some(2),
        "control: the open gate took the mark"
    );
    assert_eq!(cell(&open), None, "an open gate vouches for nothing");
    assert_eq!(open.prompt_in_front(), None);
}

/// A signed `B` that does not say it stands at the buffer's start names
/// none: zsh sends its `B` from zle-line-init after zle drew a line
/// pre-filled from its buffer stack (accept-and-hold: `P> echo hi` with
/// the mark at column 10, measured on zsh 5.9), and aterm's integration
/// says `aterm_input=start` only over an empty buffer.
#[test]
fn a_b_that_does_not_say_it_starts_the_buffer_names_no_start() {
    let mut t = Terminal::new(4, 20);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    t.process(&signed("A"));
    t.process(b"P> echo hi");
    t.process(&signed("B"));
    assert_eq!(cell(&t), None, "a pre-filled line's B");
    assert!(t.prompt_in_front().is_some(), "its prompt is in front");

    let mut control = Terminal::new(4, 20);
    control.authorize_shell_integration(KEY);
    control.set_require_shell_integration_nonce(true);
    control.process(&signed("A"));
    control.process(b"P> ");
    control.process(&signed_b());
    assert_eq!(cell(&control), Some((0, 3)), "control: an empty line's B");
    control.process(b"\r\n");
    control.process(&signed("A"));
    control.process(b"P> pushed");
    control.process(&signed("B"));
    assert_eq!(cell(&control), None, "a word is the prompt's own");
}

/// A reset takes the input region's start: it named a cell of the screen
/// the reset erased, and only the shell's next `B` names one again, while
/// the prompt it kept stays in front.
/// NEGATIVE CONTROL: with `ShellIntegrationState::reset` keeping
/// `input_start_attested`, `` host reset false: the erased cell is no
/// start `` fails.
#[test]
fn a_reset_takes_the_input_regions_start() {
    for host in [false, true] {
        let mut t = at_signed_prompt();
        assert_eq!(cell(&t), Some((0, 2)), "PRECONDITION: the signed B");
        if host {
            t.reset();
        } else {
            t.process(b"\x1bc");
        }
        assert_eq!(
            cell(&t),
            None,
            "host reset {host}: the erased cell is no start"
        );
        assert!(
            t.prompt_in_front().is_some(),
            "host reset {host}: the prompt is still in front"
        );
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed_b());
        assert_eq!(
            cell(&t),
            Some((0, 2)),
            "host reset {host}: the shell's next B names one"
        );
    }
}

/// A reset leaves the phase as stale as it found it: the prompt it kept is
/// still not the shell's last word until the shell's next prompt.
/// NEGATIVE CONTROL: with `ShellIntegrationState::reset` clearing
/// `phase_stale`, `` still not the shell's last word `` fails.
#[test]
fn a_reset_keeps_a_stale_phase_stale() {
    for host in [false, true] {
        let mut t = at_signed_prompt();
        t.authorize_shell_integration_on_first_mark(NEW_KEY);
        t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
        assert_eq!(
            t.prompt_in_front(),
            None,
            "PRECONDITION: the phase is stale"
        );
        if host {
            t.reset();
        } else {
            t.process(b"\x1bc");
        }
        assert_eq!(
            t.prompt_in_front(),
            None,
            "host reset {host}: still not the shell's last word"
        );
        t.process(&signed_with(NEW_KEY, "A"));
        t.process(b"$ ");
        t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
        assert!(
            t.prompt_in_front().is_some(),
            "host reset {host}: the shell's next prompt is in front"
        );
    }
}

/// In the window of a re-key the shell has not taken, the marks it signs
/// with the new key before then were dropped: the phase, and the `B` the
/// engine holds, may belong to a prompt that is over.
#[test]
fn input_region_start_is_none_while_the_phase_is_not_the_shell_s_last_word() {
    let mut t = at_signed_prompt();
    assert_eq!(cell(&t), Some((0, 2)), "control: before the re-key");
    t.authorize_shell_integration_on_first_mark(NEW_KEY);
    assert_eq!(cell(&t), None, "the new key is not in use yet");
    assert_eq!(t.prompt_in_front(), None);
    t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
    assert_eq!(
        t.shell_integration_nonce_in_use(),
        Some(NEW_KEY),
        "control: the key is in use"
    );
    assert_eq!(cell(&t), None, "the phase is stale");
    assert_eq!(t.prompt_in_front(), None);
    t.process(b"\r\n");
    t.process(&signed_with(NEW_KEY, "A"));
    t.process(b"$ ");
    t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
    assert_eq!(cell(&t), Some((1, 2)), "the new prompt's own B");
}

/// Once the prompt in front is not the one the `B` belongs to, or is not
/// where the grid's coordinates say, there is no input region: a command
/// started (`C`), a program a key binding started holds the foreground, the
/// alternate screen is up, the prompt's row scrolled off the top, or the
/// viewport is scrolled back into history.
#[test]
fn input_region_start_is_none_once_its_prompt_is_not_in_front() {
    let mut t = at_signed_prompt();
    t.process(b"ls\r\n");
    t.process(&signed("C"));
    assert_eq!(cell(&t), None, "a command runs");

    let mut t = at_signed_prompt();
    t.set_foreground_job(true);
    assert_eq!(cell(&t), None, "a binding's program runs");
    assert!(
        t.prompt_in_front().is_some(),
        "its prompt is still the shell's"
    );
    t.set_foreground_job(false);
    assert_eq!(cell(&t), Some((0, 2)));

    let mut t = at_signed_prompt();
    t.process(b"\x1b[?1049h");
    assert_eq!(cell(&t), None, "the alternate screen");
    t.process(b"\x1b[?1049l");
    assert_eq!(cell(&t), Some((0, 2)));

    let mut t = at_signed_prompt();
    t.process(b"\r\n\r\n\r\n");
    assert_eq!(cell(&t), Some((0, 2)), "control: still on screen");
    t.process(b"\r\n");
    assert_eq!(cell(&t), None, "the prompt's row scrolled off");

    let mut t = Terminal::new(4, 20);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    t.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed_b());
    assert_eq!(cell(&t), Some((3, 2)));
    t.scroll_display(1);
    assert_ne!(t.grid().display_offset(), 0, "control: scrolled back");
    assert_eq!(cell(&t), None, "the viewport shows history");
    t.scroll_to_bottom();
    assert_eq!(cell(&t), Some((3, 2)));
}

/// A predecessor's checkpoint carries no `started_seq` (the wire's
/// default reads 0): the restore starts the count at the carried blocks
/// that reached C, finished or running, and a carried count above them
/// (a clear took some) stands.
#[test]
fn a_restore_never_counts_fewer_starts_than_its_blocks_prove() {
    let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    let mut term = Terminal::new(6, 40);
    term.process(prompt);
    term.process(b"a\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    term.process(prompt);
    term.process(b"b\r\n\x1b]133;C\x07");
    let mut cp = term.checkpoint();
    cp.shell.started_seq = 0;
    assert_eq!(
        Terminal::from_checkpoint(&cp).started_command_seq(),
        2,
        "one finished block and the running one each took a C"
    );
    cp.shell.started_seq = 7;
    assert_eq!(
        Terminal::from_checkpoint(&cp).started_command_seq(),
        7,
        "a carried count above the blocks stands"
    );
    let mut idle = Terminal::new(6, 40);
    idle.process(prompt);
    idle.process(prompt);
    idle.process(b"typing");
    let cp = idle.checkpoint();
    assert_eq!(
        cp.shell.output_blocks.len(),
        1,
        "CONTROL: a block is carried"
    );
    assert_eq!(
        Terminal::from_checkpoint(&cp).started_command_seq(),
        0,
        "CONTROL: no carried block reached C"
    );
}

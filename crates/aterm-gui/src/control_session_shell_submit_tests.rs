// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";
const START: &[u8] = b"echo hi\r\n\x1b]133;C\x07";
const END: &[u8] = b"hi\r\n\x1b]133;D;0\x07";

fn entering() -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(PROMPT);
    t
}

#[test]
fn exact_started_block_survives_completion_and_the_next_prompt() {
    let mut t = entering();
    let armed = ShellSubmit::capture(&t);
    assert!(armed.block.is_some());
    assert_eq!(armed.block(&t), None, "a prompt alone is no start");
    t.process(START);
    assert_eq!(armed.block(&t), armed.block);
    t.process(END);
    t.process(PROMPT);
    assert_ne!(t.current_block().unwrap().id, armed.block.unwrap());
    assert_eq!(armed.block(&t), armed.block, "not the new prompt");
    let carried = Terminal::from_checkpoint(&t.checkpoint());
    assert_eq!(armed.block(&carried), armed.block);
}

#[test]
fn multiple_starts_never_guess_the_latest_or_first_block() {
    let mut t = entering();
    let armed = ShellSubmit::capture(&t);
    t.process(START);
    t.process(END);
    t.process(PROMPT);
    t.process(START);
    assert_eq!(armed.block(&t), None);
}

#[test]
fn a_reset_keeps_the_started_block_and_neither_it_nor_eviction_reassociates_a_prompt() {
    let mut t = entering();
    let armed = ShellSubmit::capture(&t);
    t.process(START);
    t.reset();
    assert!(
        armed.block.is_some(),
        "PRECONDITION: the press armed a block"
    );
    assert_eq!(
        armed.block(&t),
        armed.block,
        "the reset kept the block the press started"
    );
    t.process(PROMPT);
    assert_ne!(ShellSubmit::capture(&t).block, armed.block);

    let mut t = entering();
    let armed = ShellSubmit::capture(&t);
    t.process(START);
    t.process(END);
    // Prompt-only blocks evict the witnessed block without adding any starts.
    for _ in 0..10_000 {
        t.process(PROMPT);
    }
    assert!(t.block_by_id(armed.block.unwrap()).is_none());
    assert_eq!(armed.block(&t), None);
}

#[test]
fn a_start_without_an_armed_input_block_has_no_block_claim() {
    let mut t = Terminal::new(24, 80);
    let armed = ShellSubmit::capture(&t);
    t.process(PROMPT);
    t.process(START);
    assert!(t.started_command_seq() > armed.started);
    assert_eq!(armed.block(&t), None);

    // A carried shell phase can retain the accepted-start signal without a block.
    let mut checkpoint = entering().checkpoint();
    checkpoint.shell.current_block = None;
    let mut t = Terminal::from_checkpoint(&checkpoint);
    let armed = ShellSubmit::capture(&t);
    t.process(START);
    assert!(t.started_command_seq() > armed.started);
    assert!(t.current_block().is_none());
    assert_eq!(armed.block(&t), None);
}

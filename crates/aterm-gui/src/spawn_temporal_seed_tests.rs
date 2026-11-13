// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{arm_shell_nonce_then_seed, seed_temporal_keyframe};
use crate::temporal::TemporalRecorder;
use aterm_core::terminal::Terminal;
use std::sync::Mutex;

/// The seed is taken after the session's shell nonce is installed, so a replay from it accepts the shell's signed marks and captures as the live engine does.
#[test]
fn the_seed_carries_the_shell_nonce_its_marks_are_signed_with() {
    let nonce = [0x3Cu8; 32];
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let signed = format!(
        "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07\
         out\r\n\x1b]133;D;0;id={hex}\x07"
    );
    let term = Mutex::new(Terminal::new(6, 40));
    let recorder = Mutex::new(TemporalRecorder::new());
    arm_shell_nonce_then_seed(&term, &recorder, Some((nonce, false)), true, 1);
    assert_eq!(recorder.lock().unwrap().keyframe_count(), 1, "seeded");

    let clock = aterm_core::terminal::ClockReading::now();
    crate::term_lock(&term).process_at(signed.as_bytes(), clock);
    recorder
        .lock()
        .unwrap()
        .apply(crate::temporal::TemporalMsg::RawIn(
            crate::temporal::InputSlice::at(signed.as_bytes(), clock),
        ));
    let live = crate::term_lock(&term);
    assert_eq!(
        live.command_marks().len(),
        1,
        "precondition: the live engine took the mark"
    );
    let replay = recorder
        .lock()
        .unwrap()
        .replay_at(None)
        .expect("the seed replays");
    assert_eq!(replay.command_marks().len(), 1, "the replay took it too");
    assert_eq!(replay.checkpoint_visible(), live.checkpoint_visible());
}

#[test]
fn an_engine_adopted_mid_sequence_seeds_a_keyframe_that_continues_it() {
    let term = Mutex::new(Terminal::new(4, 20));
    crate::term_lock(&term).process(b"$ \x1b[38;5;19");
    assert!(!crate::term_lock(&term).parser_is_ground(), "PRECONDITION");
    let recorder = Mutex::new(TemporalRecorder::new());
    assert!(seed_temporal_keyframe(&term, &recorder, 1), "seeded");
    assert_eq!(recorder.lock().unwrap().keyframe_count(), 1);

    crate::term_lock(&term).process(b"6mX");
    recorder.lock().unwrap().record_raw_in(b"6mX");
    let replay = recorder
        .lock()
        .unwrap()
        .replay_at(None)
        .expect("the keyframe replays");
    let live = crate::term_lock(&term).row_text(0);
    assert_eq!(
        live.as_deref().map(str::trim_end),
        Some("$ X"),
        "PRECONDITION: the live engine finished the sequence"
    );
    assert_eq!(
        replay.row_text(0),
        live,
        "the replay continues the sequence instead of printing its tail"
    );

    let hooked = Mutex::new(Terminal::new(4, 20));
    crate::term_lock(&hooked).process(b"\x1bP$qm");
    let unseeded = Mutex::new(TemporalRecorder::new());
    assert!(!seed_temporal_keyframe(&hooked, &unseeded, 2));
    assert_eq!(unseeded.lock().unwrap().keyframe_count(), 0);
}

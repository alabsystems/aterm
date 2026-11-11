// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::*;
use crate::terminal::ClockReading;

fn at(t: Instant) -> ClockReading {
    ClockReading {
        monotonic: t,
        wall_ms: None,
    }
}

fn moves(term: &Terminal) -> Vec<(Instant, (u16, u16))> {
    term.caret_path().into_iter().flatten().collect()
}

/// Each batch that moves the caret is a move at its own clock; a batch that leaves it where
/// it was is none; a frame the program has not finished moves nothing; the other screen's and
/// another grid's moves are not this screen's.
#[test]
fn the_caret_path_is_each_batchs_move_at_its_clock() {
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut term = Terminal::new(24, 80);
    term.process_at(b"$ ", at(ms(0)));
    term.process_at(b"l", at(ms(110)));
    term.process_at(b"\x1b[0m", at(ms(150)));
    term.process_at(b"s", at(ms(220)));
    assert_eq!(
        moves(&term),
        vec![(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))]
    );
    term.process_at(b"\x1b[?2026h\x1b[10;1Hdrawing", at(ms(300)));
    term.process_at(b"\x1b[1;5H\x1b[?2026l", at(ms(320)));
    assert_eq!(
        moves(&term),
        vec![(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))],
        "the unfinished frame's write position is no move, and its close left the caret back \
         where it was"
    );
    term.process_at(b"\x1b[?1049h\x1b[5;5H", at(ms(400)));
    assert_eq!(
        moves(&term),
        vec![(ms(400), (4, 4))],
        "the other screen's own"
    );
    term.process_at(b"\x1b[?1049l", at(ms(500)));
    assert_eq!(
        moves(&term)[..3],
        [(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))],
        "the main screen's moves again"
    );
    term.resize(30, 100);
    assert!(
        moves(&term).iter().all(|&(t, _)| t > ms(500)),
        "a move on another grid is no move on this one"
    );
    for k in 0..40u64 {
        term.process_at(b"x", at(ms(600 + k)));
    }
    let kept = moves(&term);
    assert_eq!(kept.len(), CARET_PATH_LEN);
    assert_eq!(
        kept.last().map(|m| m.0),
        Some(ms(639)),
        "the latest kept, oldest first"
    );
}

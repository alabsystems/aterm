// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

#[test]
fn mode_2027_is_permanently_reset() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2027$p");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?2027;4$y",
        "power-on"
    );
    term.process(b"\x1b[?2027h\x1b[?2027$p");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?2027;4$y",
        "after CSI ? 2027 h"
    );
    assert!(!term.modes().grapheme_cluster_mode);
    term.process(b"\x1bP$q?2027\x1b\\");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1bP1$r?2027l\x1b\\"
    );
}

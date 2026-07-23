// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Regression: byte-stream RIS (ESC c) must preserve HOST policy flags.
//!
//! RIS (`\x1b c`) is a full terminal reset reachable from the untrusted PTY
//! byte stream. The host can independently arm security policy that lives in
//! `TerminalModes` — most critically the shell-integration nonce requirement
//! (`require_shell_integration_nonce`). If RIS cleared that flag, a rogue
//! program could write `\x1b c` and then forge OSC 133/633 prompt/command
//! marks WITHOUT a nonce (the gate fails open once the flag is false),
//! defeating the anti-spoofing control entirely (#7937 F01-2 / #7960).
//!
//! `Terminal::reset()` (the programmatic path) already preserved this; the
//! byte-stream RIS path in `handler_esc::reset_terminal_state` had drifted and
//! dropped it. This test pins BOTH reset paths to the same contract so they
//! cannot diverge again.

use aterm_core::terminal::Terminal;

/// Byte-stream RIS must NOT clear the host shell-integration nonce requirement.
#[test]
fn byte_stream_ris_preserves_shell_integration_nonce_requirement() {
    let mut t = Terminal::new(24, 80);
    t.set_require_shell_integration_nonce(true);
    assert!(
        t.is_require_shell_integration_nonce(),
        "precondition: host armed the nonce requirement"
    );

    // RIS from the untrusted byte stream.
    t.process(b"\x1bc");

    assert!(
        t.is_require_shell_integration_nonce(),
        "byte-stream RIS (ESC c) must NOT clear the host shell-integration \
         nonce requirement — otherwise a rogue program forges OSC 133/633 \
         marks without a nonce (#7937 F01-2 / #7960)"
    );
}

/// The two reset entry points agree: programmatic `reset()` and byte-stream
/// RIS both preserve the nonce requirement. (Guards against future drift in
/// either direction.)
#[test]
fn reset_paths_agree_on_nonce_preservation() {
    let mut via_api = Terminal::new(24, 80);
    via_api.set_require_shell_integration_nonce(true);
    via_api.reset();

    let mut via_ris = Terminal::new(24, 80);
    via_ris.set_require_shell_integration_nonce(true);
    via_ris.process(b"\x1bc");

    assert_eq!(
        via_api.is_require_shell_integration_nonce(),
        via_ris.is_require_shell_integration_nonce(),
        "Terminal::reset() and byte-stream RIS must treat the nonce requirement identically"
    );
    assert!(
        via_ris.is_require_shell_integration_nonce(),
        "both reset paths must PRESERVE the armed nonce requirement"
    );
}

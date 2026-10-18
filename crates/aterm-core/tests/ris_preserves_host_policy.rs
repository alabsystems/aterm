// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Byte-stream RIS (ESC c) preserves host policy flags.
//!
//! RIS is reachable from the untrusted PTY stream. If it cleared
//! `require_shell_integration_nonce`, a program could send `\x1b c` and then
//! forge OSC 133/633 marks without a nonce. Both reset paths,
//! `Terminal::reset()` and `handler_esc::reset_terminal_state`, keep the flag.

use aterm_core::terminal::Terminal;

/// Programmatic `reset()` and byte-stream RIS both preserve the nonce
/// requirement; RIS from the byte stream clearing it was #7937 F01-2 / #7960.
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

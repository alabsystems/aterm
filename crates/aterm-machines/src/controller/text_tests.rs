// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn host_text_reaches_the_terminal_without_its_control_characters() {
    assert_eq!(
        shown("Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)"),
        "Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)"
    );
    assert_eq!(
        shown("lab\u{1b}]52;c;aGk=\u{7}\r"),
        "lab\\u{1b}]52;c;aGk=\\u{7}\\u{d}"
    );
    assert_eq!(shown("a\u{202e}b"), "a\\u{202e}b");
}

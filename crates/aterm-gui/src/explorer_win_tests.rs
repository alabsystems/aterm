// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn drive_root_expansion_does_not_escape_the_closing_quote() {
    for (_, token) in VERBS {
        let template = verb_command(r"C:\apps\aterm\aterm-gui.exe", token);
        assert!(
            !template.ends_with("\\\""),
            "template ends in backslash-quote: {template}"
        );
        let expanded = template.replace(token, r"C:\");
        assert!(
            !expanded.ends_with("\\\""),
            "expanded verb ends in backslash-quote (operand would parse as `C:\"`): {expanded}"
        );
        assert!(
            expanded.ends_with("\\.\""),
            "expanded verb should end root-dot-quote so the operand stays `C:\\.`: {expanded}"
        );
    }
}

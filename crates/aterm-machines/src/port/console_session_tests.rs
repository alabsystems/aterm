// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::agent_of;
use std::ffi::OsString;

/// An agent is named by its own argv[0] or, under a runtime, by its script; a shell, a
/// runtime running another script, and an empty argv name none.
#[test]
fn an_agent_is_named_by_its_program_or_its_script() {
    let argv = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(agent_of(&argv(&["claude"])), Some("Claude Code"));
    assert_eq!(
        agent_of(&argv(&[
            "/Users//a/Library/Application Support/aterm/pkg/store/claude/1/bin/claude",
            "--resume",
        ])),
        Some("Claude Code")
    );
    assert_eq!(
        agent_of(&argv(&["node", "/opt/homebrew/bin/claude", "--resume"])),
        Some("Claude Code")
    );
    assert!(agent_of(&argv(&["/opt/homebrew/bin/codex"])).is_some());
    assert_eq!(agent_of(&argv(&["-zsh"])), None);
    assert_eq!(agent_of(&argv(&["node", "server.js"])), None);
    assert_eq!(agent_of(&argv(&["node"])), None);
    assert_eq!(agent_of(&[]), None);
}

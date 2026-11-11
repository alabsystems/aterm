// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::prompt::fixtures::rows;

/// codex 0.155.1's trust gate, measured.
pub(crate) fn codex_0_155_trust() -> Vec<String> {
    rows(&[
        "> You are in /Users//x/aterm",
        "",
        "  Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of",
        "  prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.",
        "",
        "› 1. Yes, continue",
        "  2. No, quit",
        "",
        "  Press enter to continue",
    ])
}

/// A turn running over the composer (the reader's sample).
pub(crate) fn working() -> Vec<String> {
    rows(&[
        "• Working (12s • esc to interrupt)",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt-5 · ~/src",
    ])
}

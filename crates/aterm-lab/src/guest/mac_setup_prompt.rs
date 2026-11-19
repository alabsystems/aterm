// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Mac Setup tab's introduction and its unanswered password at the end of a frame.

pub(crate) fn shown(text: &str) -> bool {
    // The real frame can wrap the introduction. Match its stable beginning.
    let mut lines = text.lines().filter(|line| !line.is_empty());
    lines.next().is_some_and(|line| {
        line.starts_with("aterm is installing Apple's Command Line Tools")
            || line.starts_with("aterm is finishing the part of this Mac's setup")
    }) && lines.next_back() == Some(aterm_types::macos_setup::MAC_SETUP_PASSWORD_PROMPT.trim_end())
}

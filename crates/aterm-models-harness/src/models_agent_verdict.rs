// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for the server's agent verdict handshake (`await agent idle`).

use aterm_spec::derive::*;

/// A Claude Code launch and the orchestrator that types on the server's `idle`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_idle_at_composer_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeIdleAtComposer {
            const Buggy = 0;
            var screen = 0;
            var stale = 0;
            var dialog = 0;
            var published = 0;
            var typed = 0;
            var lost = 0;

            action Relaunched when (stale == 0 && dialog == 0 && screen == 0 && published == 0) {
                stale = 1;
            }
            action AlreadyTrusted when (dialog == 0 && screen == 0) {
                dialog = 2;
            }
            action AskTrust when (dialog == 0 && screen == 0) {
                dialog = 1;
                screen = 1;
            }
            action PressTrust when (dialog == 1 && screen == 1) {
                dialog = 2;
                screen = 0;
            }
            action DrawHalf when (dialog == 2 && screen == 0) {
                screen = 2;
            }
            action DrawWhole when (screen == 2) {
                screen = 3;
            }
            action Look when (typed == 0) {
                published = if screen == 1 {
                    2
                } else if screen == 3 {
                    1
                } else if Buggy == 1 {
                    1
                } else if Buggy == 2 && stale == 1 && screen == 0 {
                    1
                } else {
                    0
                };
            }
            action Type when (published == 1 && typed == 0) {
                typed = 1;
                lost = if screen == 3 { 0 } else { 1 };
            }

            invariant IdleIsTheComposer:
                if published == 1 { screen == 3 } else { published <= 2 };
            invariant NoFirstPromptLost: lost == 0;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The server's agent verdict as the handshake an orchestrator types on: `await agent idle`,
//! then the first prompt. Tier 1 is in aterm-gui (`session_status`), which drives the shipping
//! `StatusObserver` over the screens Claude Code draws between launch and REPL (measured on
//! 2.1.283, cursor included); there are no `#[refines]` anchors, as the reader (aterm-phase)
//! takes no dependencies.

use aterm_spec::derive::*;

/// A Claude Code launch and the orchestrator that types on the server's `idle`.
///
/// `screen`: 0 the shell's rows (nothing of Claude Code drawn: before the folder-trust dialog,
/// after its press erased it, or with no dialog), 1 the trust dialog, 2 the REPL half drawn (the
/// composer's top rule and caret, not its bottom rule), 3 the REPL whole (the prompt box between
/// its rules, its caret `❯`, or `!` in shell mode, and the terminal cursor in it). Either
/// renderer draws screens 1-3: the fullscreen one on the alternate screen, prompt box on the
/// last rows; the inline one on the main grid under the launch line (on a pane taller than 40
/// rows, the REPL sits above the grid's last 40). `stale` is 1 for an inline relaunch in the same
/// tab: the previous run's prompt box stays whole on the main grid above the new launch line
/// through every screen, with the cursor under the new launch line, never in the old box.
/// `dialog`: 0 undecided, 1 the dialog up, 2 trusted.
///
/// `Look` is the server's sweep publishing a verdict (`published`: 0 anything but idle, 1
/// `idle`, 2 `prompt`); `Type` is the orchestrator's first prompt, typed on idle, and `lost`
/// records it typed while the new REPL was not there to take it (such text vanishes).
/// `Buggy = 1` reads `idle` for any Claude screen with no box and no busy row, so the shell's
/// rows read idle. `Buggy = 2` reads `idle` wherever the screen's last `❯` caret has a whole box
/// around it, ignoring the cursor, so a same-tab relaunch's old box reads idle while nothing of
/// the new launch is drawn.
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

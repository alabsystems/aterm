// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The server's agent verdict as the handshake an orchestrator types on:
//! `await agent idle`, then the first prompt. Tier-1 bound in aterm-gui
//! (`session_status`), which drives the shipping `StatusObserver` over the
//! screens Claude Code 2.1.283 measurably draws between its launch and its
//! REPL, each with the terminal cursor measured on it; no per-method
//! `#[refines]` anchors (the reader, aterm-phase, takes no dependencies).

use super::*;

/// A Claude Code LAUNCH, as 2.1.283 draws it (measured frame by frame on
/// 2026-09-26 and, with the terminal cursor, 2026-09-27), and the
/// orchestrator that types on the server's `idle`. `screen` is what is on
/// the terminal: 0 the shell's rows (Claude Code started, nothing of it
/// drawn — before the folder-trust dialog, after its press erased it, or
/// with no dialog at all), 1 the trust dialog, 2 the REPL half drawn (the
/// composer's top rule and caret, not its bottom rule), 3 the REPL whole
/// (the prompt box between its two rules, its caret `❯` — or `!` once `!` is
/// typed into it: shell mode, the REPL up and taking keys — and the
/// terminal's cursor IN it). Screens 1-3 are drawn by either renderer: the
/// fullscreen one on the alternate screen, its prompt box on the last rows;
/// the inline one on the main grid under the launch line, blank rows below
/// it — on a pane taller than 40 rows its whole REPL above the grid's last
/// 40. `stale` is 1 when the launch is the inline renderer RELAUNCHED IN THE
/// SAME TAB: the previous run's prompt box stays on the main grid above the
/// new launch line, whole, through every screen of the new launch — with
/// the cursor under the new launch line, never in the old box (measured).
/// `dialog` is the folder's trust: 0 not decided yet, 1 the dialog up, 2
/// trusted (pressed, or trusted before). `Look` is the server's sweep
/// reading the current screen and publishing a verdict (`published`: 0
/// anything but idle — `unknown`, `-` — 1 `idle`, 2 `prompt`); `Type` is the
/// orchestrator's first prompt, typed when the verdict is idle, and `lost`
/// records it typed while the new REPL was not there to take it (measured:
/// such text vanished, 5 of 5 after the dialog in a new tab, 3 of 3 in the
/// same tab). `Buggy=1` is the reader before 2026-09-26: `idle` for any
/// Claude screen that shows no box and no busy row — so the shell's rows
/// read idle. `Buggy=2` is the reader of 2026-09-26: `idle` wherever the
/// screen's last `❯` caret has a whole box around it, the cursor not asked —
/// so the old box a same-tab relaunch leaves reads idle while nothing of the
/// new launch is drawn under it (the new REPL's half-drawn caret, once
/// drawn, is the last one, and has no box yet).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_idle_at_composer_model() -> Model {
    crate::ty_model! {
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

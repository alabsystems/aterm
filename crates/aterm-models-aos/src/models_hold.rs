// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A recording's hold on typed keys that wait for their program's echo (ATERM-KI-1025-1).

use aterm_spec::derive::Model;

/// The momentum glow's recording while typed keys wait for their echo.
///
/// `att` is whether a recording runs; `held` how many typed keys wait for their program's own
/// echo, oldest first; `echoed` how many of those the program has shown in place (the
/// environment's `Echo`); `glow` whether the live glow took a key no recording logged since it
/// last started from rest. The ghosts: `leak` (the log grew while a key was held), `unproven` (a
/// key was logged that was not shown echoed), `gap` (the recording's glow took keys its log does
/// not hold) and `dirty` (a recording started from a glow that took an unlogged key).
///
/// `Key` is a key logged at its press (the tty or an open prompt of aterm's shell echoes it) or,
/// unattached, drawn unrecorded; `Private` a key the recording may not hold, which stops it
/// before the glow sees the key; `Refute` a held run whose echo was not proven, which stops the
/// recording the same way and hands the held keys to the glow unrecorded.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn aos_key_hold_model() -> Model {
    aterm_spec::ty_model! {
        AosKeyHold {
            const Buggy = 0;
            const Cap = 2;
            var att = 0;
            var held = 0;
            var echoed = 0;
            var glow = 0;
            var leak = 0;
            var unproven = 0;
            var gap = 0;
            var dirty = 0;

            action Attach when (att == 0) {
                att = 1;
                dirty = if Buggy == 4 && glow == 1 { 1 } else { dirty };
                glow = if Buggy == 4 { glow } else { 0 };
            }
            action Detach when (att == 1) {
                att = 0;
                glow = if held > 0 { 1 } else { glow };
                held = 0;
                echoed = 0;
            }
            action Key when (held == 0) {
                att = att;
            }
            action Private when (att <= 1) {
                att = 0;
                glow = 1;
                held = 0;
                echoed = 0;
            }
            action Hold when (att == 1 && held + 1 <= Cap) {
                held = held + 1;
            }
            action Echo when (echoed + 1 <= held) {
                echoed = echoed + 1;
            }
            action Tick when (att <= 1) {
                leak = if Buggy == 1 && held > 0 { 1 } else { leak };
            }
            action Release when (att == 1 && held > 0 && (echoed > 0 || Buggy == 2)) {
                unproven = if echoed == 0 { 1 } else { unproven };
                held = held - 1;
                echoed = if echoed > 0 { echoed - 1 } else { 0 };
            }
            action Refute when (att == 1 && held > 0) {
                att = if Buggy == 3 { 1 } else { 0 };
                gap = if Buggy == 3 { 1 } else { gap };
                glow = if Buggy == 3 { glow } else { 1 };
                held = 0;
                echoed = 0;
            }

            invariant NothingLoggedWhileHeld: leak == 0;
            invariant OnlyEchoedKeysLogged: unproven == 0;
            invariant HostedGlowMatchesItsLog: gap == 0;
            invariant AttachStartsFromRest: dirty == 0;
            bound HeldBounded: held <= Cap;
            bound EchoedBounded: echoed <= Cap;
        }
    }
}

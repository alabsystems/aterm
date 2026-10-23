// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the unread-input gate on socket-driven session input.

use aterm_spec::derive::Model;

/// Formal model of a session's input queue and socket-driver unread-input gate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_unread_gate_model() -> Model {
    aterm_spec::ty_model! {
        InputUnreadGate {
            const Buggy = 0;
            const Cap = 2;
            const Warn = 1;
            const Stall = 2;
            const AgeMax = 2;
            var raw = 1;
            var reading = 1;
            var stopped = 0;
            var q = 0;
            var a1 = 0;
            var a2 = 0;
            var published = 0;
            var warned = 0;
            var behind_stalled = 0;
            var refused_live = 0;
            var healthy_published = 0;

            action HumanKey when (q <= Cap - 1) {
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
            }
            action DriverKey when (q <= Cap - 1
                && (Buggy == 1
                    || q == 0
                    || (stopped == 0 && (published == 0 || raw == 0 || a1 <= Stall - 1))))
            {
                behind_stalled = if q > 0
                    && (stopped == 1 || (published == 1 && raw == 1 && Stall <= a1))
                {
                    1
                } else {
                    behind_stalled
                };
                warned = if q > 0 && raw == 1 && Warn <= a1 { 1 } else { 0 };
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
            }
            action DriverRefused when (q > 0
                && (stopped == 1
                    || (published == 1 && raw == 1 && Stall <= a1)
                    || (Buggy == 1 && raw == 1 && Warn <= a1)))
            {
                refused_live = if stopped == 0
                    && (published == 0 || raw == 0 || a1 <= Stall - 1)
                {
                    1
                } else {
                    refused_live
                };
            }
            action Publish when (published == 0 && stopped == 0 && raw == 1 && q > 0
                && Stall <= a1 && (reading == 0 || Buggy == 1))
            {
                healthy_published = if reading == 1 { 1 } else { healthy_published };
                published = 1;
            }
            action Withdraw when (published == 1 && (q == 0 || raw == 0 || a1 <= Stall - 1)) {
                published = 0;
            }
            action Consume when (reading == 1 && stopped == 0 && q > 0) {
                a1 = if q == 2 { a2 } else { 0 };
                a2 = 0;
                q = q - 1;
            }
            action Tick when (q > 0 && a1 <= AgeMax - 1) {
                a1 = a1 + 1;
                a2 = if q == 2 { a2 + 1 } else { a2 };
            }
            action Freeze when (reading == 1) {
                reading = 0;
            }
            action Thaw when (reading == 0) {
                reading = 1;
            }
            action StopJob when (stopped == 0) {
                stopped = 1;
            }
            action ContJob when (stopped == 1) {
                stopped = 0;
            }
            action ModeFlip when (raw <= 1) {
                raw = if raw == 1 { 0 } else { 1 };
            }

            invariant NoDriverKeyBehindStalledInput: behind_stalled == 0;
            invariant NoRefusalWithoutAnEstablishedStall: refused_live == 0;
            invariant NoReadingProgramPublished: healthy_published == 0;
        }
    }
}

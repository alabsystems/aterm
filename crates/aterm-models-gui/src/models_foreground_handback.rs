// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground handback: a program that loses the terminal with program-only modes armed hands them back at the byte where it lost it.

use aterm_spec::derive::Model;

/// One job's life under one shell, the gather thread that reads the PTY and samples the foreground group, and the parse thread that runs the handback.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_model() -> Model {
    aterm_spec::ty_model! {
        ForegroundHandback {
            const Buggy = 0;
            const Split = 1;
            const Alive = 1;
            const Carry = 1;
            const Display = 0;
            var fg = 0;
            var life = 0;
            var wj = 0;
            var qj = 0;
            var ws = 0;
            var qs = 0;
            var tail = 0;
            var rd = 0;
            var run = 0;
            var bj1 = 0;
            var bj2 = 0;
            var bs1 = 0;
            var bs2 = 0;
            var edge = 0;
            var full = 0;
            var dj1 = 0;
            var dj2 = 0;
            var ds1 = 0;
            var ds2 = 0;
            var dedge = 0;
            var leak = 0;
            var paste = 1;
            var prompt = 0;
            var swept = 0;
            var armed = 0;
            var hij = 1;
            var img = 0;
            var kq = 0;
            var qx = 0;
            var xo = 0;
            var xn = 0;
            var nc = 0;
            var xedge = 0;
            var early = 0;
            var mixed = 0;

            action Launch when (life == 0) { life = 1; fg = 1; paste = 0; }
            action OneShot when (life == 0) { life = 1; fg = 1; paste = 0; hij = 0; }
            action Adopt when (life == 0) { life = 1; fg = 1; run = 1; leak = 1; armed = 1; }
            action JobWrite when (life == 1 && wj <= 1) {
                wj = wj + 1;
                qj = qj + 1;
                qx = if kq == 1 { qx + 1 } else { qx };
            }
            action Exec when (life == 1 && img == 0) { img = 1; kq = 1; }
            action Die when (life == 1 && (run == 1 || qj > 0 || armed == 0)) { life = 2; }
            action Stop when (life == 1) { life = 4; }
            action Reclaim when ((life == 2 || life == 4) && rd == 0) {
                life = if life == 2 { 3 } else { 5 };
                fg = 0;
                tail = if qj > 0 { 1 } else { tail };
            }
            action ShellWrite when ((life == 3 || life == 5) && ws == 0) { ws = 1; qs = 1; }
            action Resume when (life == 5 && qs == 0 && full == 0 && bs1 + bs2 == 0) {
                life = 1;
                fg = 1;
                ws = 0;
                prompt = 0;
                paste = 0;
            }
            action Restart when (rd == 0 && full == 0 && bj1 + bj2 + bs1 + bs2 + edge + xedge == 0) {
                run = if Carry == 1 { run } else { fg };
                kq = 0;
                qx = 0;
            }
            action ReadJob when (qj > 0 && edge == 0 && xedge == 0 && (rd == 1 || kq == 0)) {
                qj = qj - 1;
                qx = if kq == 1 && qj <= qx { qx - 1 } else { qx };
                nc = if rd == 0 && xn == 1 { 1 } else { nc };
                xo = if (img == 0 || kq == 1) && qj > qx { 1 } else {
                    if rd == 0 || (img == 1 && kq == 0) { 0 } else { xo }
                };
                xn = if kq == 1 && qj <= qx { 1 } else { if rd == 0 { 0 } else { xn } };
                bj2 = 1;
                rd = 1;
            }
            action ReadShell when (qs == 1 && qj == 0 && edge == 0 && xedge == 0 && (rd == 1 || kq == 0)) {
                qs = 0;
                nc = if rd == 0 && xn == 1 { 1 } else { nc };
                xo = if rd == 0 { 0 } else { xo };
                xn = if rd == 0 { 0 } else { xn };
                bs2 = 1;
                rd = 1;
            }
            action Park when (rd == 1) {
                rd = 0;
                bj1 = if Split == 1 && fg == run && bj2 == 1 { 1 } else { bj1 };
                bj2 = if Split == 1 && fg == run { 0 } else { bj2 };
                bs1 = if Split == 1 && fg == run && bs2 == 1 { 1 } else { bs1 };
                bs2 = if Split == 1 && fg == run { 0 } else { bs2 };
                edge = if Split == 1 { if fg == run { 0 } else { 1 } } else { edge };
                run = if Split == 1 { fg } else { run };
            }
            action ObserveExec when (rd == 0 && kq == 1 && edge == 0 && xedge == 0) {
                kq = 0;
                qx = 0;
                edge = if fg == 1 && run == 0 { 1 } else { 0 };
                run = if fg == 1 { 1 } else { run };
                xedge = if fg == 1 && run == 1 { 1 } else { 0 };
                early = if fg == 1 && run == 1 && (nc == 1 || (Buggy == 2 && xn == 1)) { 1 } else {
                    early
                };
                mixed = if fg == 1 && run == 1 && Buggy == 3 && xo == 1 { 1 } else { mixed };
                xo = 0;
                xn = 0;
                nc = 0;
            }
            action Deliver when (rd == 0 && full == 0 && bj1 + bj2 + bs1 + bs2 + edge + xedge > 0
                && (kq == 0 || xn == 0 || edge == 1 || fg + run == 1)) {
                dj1 = if edge == 0 && (fg == run || xedge == 1) {
                    if bj1 + bj2 > 0 { 1 } else { 0 }
                } else { bj1 };
                dj2 = if edge == 0 && (fg == run || xedge == 1) { 0 } else { bj2 };
                ds1 = if edge == 0 && (fg == run || xedge == 1) {
                    if bs1 + bs2 > 0 { 1 } else { 0 }
                } else { bs1 };
                ds2 = if edge == 0 && (fg == run || xedge == 1) { 0 } else { bs2 };
                dedge = if edge == 1 { 1 } else { if fg == run || xedge == 1 { 0 } else { 1 } };
                run = if edge == 1 || xedge == 1 { run } else { fg };
                kq = if (edge == 1 && run == 1) || (edge == 0 && xedge == 0 && fg + run == 1 && fg == 1) {
                    0
                } else { kq };
                qx = if (edge == 1 && run == 1) || (edge == 0 && xedge == 0 && fg + run == 1 && fg == 1) {
                    0
                } else { qx };
                bj1 = 0;
                bj2 = 0;
                bs1 = 0;
                bs2 = 0;
                edge = 0;
                xedge = 0;
                xo = 0;
                xn = 0;
                nc = if kq == 1 && xn == 1 {
                    if (edge == 1 && run == 1) || (edge == 0 && xedge == 0 && fg + run == 1 && fg == 1) {
                        0
                    } else { 1 }
                } else { 0 };
                full = 1;
            }
            action Parse when (full == 1) {
                leak = if dj2 == 1 { 1 } else {
                    if dedge == 1 && (Buggy == 0 || Buggy > 1) && (dj1 == 1 || leak == 1)
                        && (Alive == 0 || life == 2 || life == 3)
                        && (Display == 1 || hij == 1) { 0 } else {
                        if dj1 == 1 { 1 } else { leak }
                    }
                };
                paste = if ds2 == 1 || (dj2 == 1 && hij == 1) { 1 } else {
                    if dedge == 1 && (Buggy == 0 || Buggy > 1) && (dj1 == 1 || leak == 1)
                        && (Alive == 0 || life == 2 || life == 3)
                        && (Display == 1 || hij == 1) { 0 } else {
                        if ds1 == 1 || (dj1 == 1 && hij == 1) { 1 } else { paste }
                    }
                };
                armed = if dj1 + dj2 > 0 { 1 } else { armed };
                prompt = if ds1 + ds2 > 0 { 1 } else { prompt };
                full = 0;
                dj1 = 0;
                dj2 = 0;
                ds1 = 0;
                ds2 = 0;
                dedge = 0;
            }

            action Sweep when ((life == 3 || life == 5) && swept == 0) {
                swept = 1;
                leak = if Buggy == 1 { 0 } else { leak };
                paste = if Buggy == 1 { 0 } else { paste };
            }

            invariant PromptNotHijacked:
                prompt == 0 || leak == 0 || tail == 1 || life == 5 || hij == 0;
            invariant ShellKeepsItsOwnModes: prompt == 0 || paste == 1;
            invariant JobKeepsItsModesWhileAlive:
                armed == 0 || leak == 1 || life == 2 || life == 3;
            invariant OneShotKeepsItsModes: hij == 1 || armed == 0 || leak == 1;
            invariant ExecCutBlind: early == 0;
            invariant ExecSpanClean: mixed == 0;
        }
    }
}

/// Who owns an input mode the handback may have to hand back (`FgOwners` in `aterm-gui/src/foreground_handback.rs`) across several jobs under one shell, when the reader can miss a job.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_ownership_model() -> Model {
    aterm_spec::ty_model! {
        ForegroundHandbackOwnership {
            const Buggy = 0;
            const Jobs = 2;
            var n = 0;
            var life = 0;
            var seen = 0;
            var bit = 0;
            var owner = 0;
            var obs = 0;
            var done = 0;
            var backs = 0;

            action Launch when (life == 0 && n <= Jobs - 1) {
                life = 1;
                n = n + 1;
                seen = 0;
                obs = 0;
                done = 0;
            }
            action Sample when (life == 1 && seen == 0) { seen = 1; }
            action Arm when (life == 1) {
                owner = if Buggy == 1 && bit == 1 { owner } else {
                    if seen == 1 { 2 } else { 1 }
                };
                bit = 1;
                obs = if seen == 1 { 1 } else { obs };
            }
            action Die when (life == 1) { life = 2; }
            action Reclaim when (life == 2) {
                bit = if seen == 1 && owner == 2 { 0 } else { bit };
                owner = if seen == 1 && owner == 2 { 0 } else { owner };
                backs = if seen == 1 && owner == 2 { backs + 1 } else { backs };
                done = obs;
                life = 0;
            }
            action ShellArm when (life == 0) {
                owner = if Buggy == 1 && bit == 1 { owner } else { 1 };
                bit = 1;
                done = 0;
            }

            invariant ObservedArmIsHandedBack: life > 0 || done == 0 || bit == 0;
        }
    }
}

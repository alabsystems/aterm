// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor Claude Code account rotation, `/usage` probe, and limit-reset models.

use aterm_spec::derive::{Liveness, Model};

/// Supervisor account rotation and rate-limit failover model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_account_rotation_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorAccountRotation {
            const Buggy = 0;
            const Budget = 2;
            var d1 = 0;
            var d2 = 0;
            var sh = 0;
            var act = 0;
            var run = 0;
            var prov = 0;
            var prev = 0;
            var pp = 0;
            var pin = 0;
            var hold = 0;
            var s1 = 1;
            var s2 = 1;
            var jobs = 0;
            var w1 = 0;
            var w2 = 0;
            var n1 = 0;
            var n2 = 0;
            var rot = 0;
            var mv = 0;
            var pa = 0;
            var ph = 0;
            var pw = 0;
            var cx = 0;

            action JobsStart when (jobs == 0) {
                jobs = 1;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action JobsEnd when (jobs == 1) {
                jobs = 0;
                w1 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action WallD1 when (d1 <= 1 && (s1 == 1 || s2 == 1)) {
                d1 = 2;
                run = if act == 1 { 0 } else { run };
                pin = if pin == 1 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action WallD2 when (d2 <= 1 && (s1 == 2 || s2 == 2)) {
                d2 = 2;
                run = if act == 2 { 0 } else { run };
                pin = if pin == 2 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action WallSh when (sh <= 1 && (s1 == 3 || s2 == 3)) {
                sh = 2;
                run = if act == 3 { 0 } else { run };
                pin = if pin == 3 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action LowD1 when (
                d1 == 0 && (s1 == 1 || s2 == 1) && (prov == 1 || act == 0 || 2 <= act)
            ) {
                d1 = 1;
                run = if act == 1 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action LowD2 when (
                d2 == 0 && (s1 == 2 || s2 == 2) && (prov == 1 || act <= 1 || act == 3)
            ) {
                d2 = 1;
                run = if act == 2 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action LowSh when (sh == 0 && (s1 == 3 || s2 == 3) && (prov == 1 || act <= 2)) {
                sh = 1;
                run = if act == 3 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action ResetD1 when (1 <= d1) {
                d1 = 0;
                run = if act == 1 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action ResetD2 when (1 <= d2) {
                d2 = 0;
                run = if act == 2 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action ResetSh when (1 <= sh) {
                sh = 0;
                run = if act == 3 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action UseD1 when (pin == 0 || 2 <= pin) {
                d1 = if d1 == 2 { 0 } else { d1 };
                act = 1;
                run = if d1 == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 1;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 1 && d1 == 2 { 1 } else { 0 };
                n2 = if s2 == 1 && d1 == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action UseD2 when (pin <= 1 || pin == 3) {
                d2 = if d2 == 2 { 0 } else { d2 };
                act = 2;
                run = if d2 == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 2;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 2 && d2 == 2 { 1 } else { 0 };
                n2 = if s2 == 2 && d2 == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action UseSh when (pin <= 2) {
                sh = if sh == 2 { 0 } else { sh };
                act = 3;
                run = if sh == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 3;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 3 && sh == 2 { 1 } else { 0 };
                n2 = if s2 == 3 && sh == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Unpin when (1 <= pin) {
                pin = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Served when (1 <= run && prov == 0 && (s1 == act || s2 == act)) {
                prov = 1;
                prev = 0;
                pp = 0;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Strike when (1 <= run && prov == 0 && (s1 == act || s2 == act)) {
                d1 = if act == 1 { 2 } else { d1 };
                d2 = if act == 2 { 2 } else { d2 };
                sh = if act == 3 { 2 } else { sh };
                act = prev;
                run = if (prev == 1 && d1 == 0) || (prev == 2 && d2 == 0) || (prev == 3 && sh == 0)
                {
                    2
                } else if (prev == 1 && d1 == 1) || (prev == 2 && d2 == 1) || (prev == 3 && sh == 1)
                {
                    1
                } else {
                    0
                };
                prov = pp;
                prev = 0;
                pp = 0;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Lapse when (1 <= act && prov == 0 && hold == 1) {
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            action Move1 when (
                w1 == 0 && jobs == 0 && (
                    ((s1 > act || act > s1) && 1 <= run && prov == 0 && hold == 0
                        && (run == 2
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    || ((s1 > act || act > s1) && 1 <= run && prov == 1
                        && (run == 2 || 1 <= pin
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    || (((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                        && run == 0 && rot <= Budget - 1 && (d1 <= 1 || d2 <= 1 || sh <= 1))
                    || (((s1 == 1 && d1 == 1) || (s1 == 2 && d2 == 1) || (s1 == 3 && sh == 1))
                        && pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s1 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = if (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            action Leave1 when (
                w1 == 0 && jobs == 0
                && ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                && (
                    ((s1 > act || act > s1) && run == 2 && prov == 0 && hold == 0)
                    || ((s1 > act || act > s1) && prov == 1
                        && (run == 2 || (run == 1 && 1 <= pin)))
                    || (pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s1 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            action Wait1 when (
                w1 == 0
                && ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                && (
                    jobs == 1
                    || (1 <= run && prov == 0 && hold == 1)
                    || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                )
            ) {
                w1 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Resume1 when (
                w1 == 0 && n1 == 1
                && ((s1 == 1 && d1 <= 1) || (s1 == 2 && d2 <= 1) || (s1 == 3 && sh <= 1))
            ) {
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            action Resumed1 when (mv == 1 && pw == 1 && cx == 0) {
                cx = 1;
            }

            action Move2 when (
                w2 == 0 && (
                    ((s2 > act || act > s2) && 1 <= run && prov == 0 && hold == 0
                        && (run == 2
                            || (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2)
                            || (s2 == 3 && sh == 2)))
                    || ((s2 > act || act > s2) && 1 <= run && prov == 1
                        && (run == 2 || 1 <= pin
                            || (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2)
                            || (s2 == 3 && sh == 2)))
                    || (((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                        && run == 0 && rot <= Budget - 1 && (d1 <= 1 || d2 <= 1 || sh <= 1))
                    || (((s2 == 1 && d1 == 1) || (s2 == 2 && d2 == 1) || (s2 == 3 && sh == 1))
                        && pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s2 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = if (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            action Leave2 when (
                w2 == 0
                && ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                && (
                    ((s2 > act || act > s2) && run == 2 && prov == 0 && hold == 0)
                    || ((s2 > act || act > s2) && prov == 1
                        && (run == 2 || (run == 1 && 1 <= pin)))
                    || (pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s2 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            action Wait2 when (
                w2 == 0
                && ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                && (
                    (1 <= run && prov == 0 && hold == 1)
                    || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                )
            ) {
                w2 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Resume2 when (
                w2 == 0 && n2 == 1
                && ((s2 == 1 && d1 <= 1) || (s2 == 2 && d2 <= 1) || (s2 == 3 && sh <= 1))
            ) {
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            action Resumed2 when (mv == 2 && pw == 1 && cx == 0) {
                cx = 1;
            }

            action RotatesIntoALimited when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 2
                && (act <= 1 || act == 3) && run == 0 && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 0;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            action WallUnread when (
                Buggy == 1 && act == 2 && 1 <= run && prov == 1 && d2 <= 1
                && (s1 == 2 || s2 == 2)
            ) {
                d2 = 2;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action SharedFirst when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0 && sh == 0
                && run == 0 && rot <= Budget - 1
            ) {
                s1 = 3;
                act = 3;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            action LowFirst when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 1 && sh == 0
                && run == 0 && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 1;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            action FollowsUnproven when (
                Buggy == 1 && w2 == 0 && 1 <= run && prov == 0 && hold == 1
                && (s2 > act || act > s2)
            ) {
                s2 = act;
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = if (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            action WaitsOnTheWall when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0
                && run == 0 && rot <= Budget - 1
            ) {
                w1 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action MovesEarlyUnderAPin when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && pin == 1 && d1 == 1 && d2 == 0
                && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            action RotatesPastTheBreaker when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0
                && run == 0 && rot == Budget
            ) {
                s1 = 2;
                act = 2;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            action WaitsAtTheLiftedWall when (Buggy == 1 && w1 == 0 && jobs == 0 && n1 == 1) {
                w1 = 1;
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action CarriesOnAfterAnEarlyMove when (Buggy == 1 && mv == 1 && pw == 0 && cx == 0) {
                cx = 1;
            }

            invariant NeverIntoALimitedAccount:
                (run == 0 || (act == 1 && d1 <= 1) || (act == 2 && d2 <= 1)
                    || (act == 3 && sh <= 1))
                && (mv == 0
                    || (mv == 1
                        && ((s1 == 1 && d1 <= 1) || (s1 == 2 && d2 <= 1) || (s1 == 3 && sh <= 1)))
                    || (mv == 2
                        && ((s2 == 1 && d1 <= 1) || (s2 == 2 && d2 <= 1) || (s2 == 3 && sh <= 1))));
            invariant DedicatedBeforeShared:
                mv == 0 || act == pa || (mv == 1 && s1 <= 2) || (mv == 2 && s2 <= 2)
                || (1 <= d1 && 1 <= d2);
            invariant FreshBeforeLow:
                mv == 0
                || (mv == 1
                    && ((s1 == 1 && (d1 == 0 || d1 == 2)) || (s1 == 2 && (d2 == 0 || d2 == 2))
                        || (s1 == 3 && (sh == 0 || sh == 2))))
                || (mv == 2
                    && ((s2 == 1 && (d1 == 0 || d1 == 2)) || (s2 == 2 && (d2 == 0 || d2 == 2))
                        || (s2 == 3 && (sh == 0 || sh == 2))))
                || (1 <= d1 && 1 <= d2 && 1 <= sh)
                || (act == pa && (prov == 0 || 1 <= pin || Budget <= rot));
            invariant OnlyAProvenActiveIsFollowed:
                mv == 0 || act > pa || pa > act || prov == 1 || ph == 0;
            invariant NoWaitWhileOneIsUsable:
                (w1 == 0 || (
                    ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                    && (
                        jobs == 1
                        || (1 <= act && prov == 0 && hold == 1)
                        || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                    )
                ))
                && (w2 == 0 || (
                    ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                    && (
                        (1 <= act && prov == 0 && hold == 1)
                        || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                    )
                ));
            invariant TheOwnersWordHolds: pin == 0 || (act == pin && prov == 1);
            invariant NoPingPong: rot <= Budget;
            invariant NothingTypedAfterAnEarlyMove: cx == 0 || pw == 1;
        }
    }
}

/// Liveness spec (`EveryUsableAccountIsTried`) for [`supervisor_account_rotation_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_account_rotation_model)]
pub fn supervisor_account_rotation_liveness() -> Liveness {
    use aterm_spec::derive::{and_, cst, eq, int, le, or_, var};
    let on = |account: i64| eq(var("s1"), int(account));
    let limited = |condition: &'static str| eq(var(condition), int(2));
    let not_at_its_wall = or_(
        or_(
            and_(on(1), le(var("d1"), int(1))),
            and_(on(2), le(var("d2"), int(1))),
        ),
        and_(on(3), le(var("sh"), int(1))),
    );
    let the_canarys_hold_stands = and_(
        and_(le(int(1), var("act")), eq(var("prov"), int(0))),
        eq(var("hold"), int(1)),
    );
    let every_other_is_limited = and_(
        and_(or_(on(1), limited("d1")), or_(on(2), limited("d2"))),
        or_(on(3), limited("sh")),
    );
    let nothing_to_rotate_to = and_(
        eq(var("run"), int(0)),
        or_(le(cst("Budget"), var("rot")), every_other_is_limited),
    );
    Liveness {
        name: "EveryUsableAccountIsTried",
        goal: or_(
            or_(not_at_its_wall, eq(var("jobs"), int(1))),
            or_(the_canarys_hold_stands, nothing_to_rotate_to),
        ),
        weak: vec!["Move1"],
        strong: vec![],
        mutants: vec!["WaitsOnTheWall"],
    }
}

/// Supervisor `/usage` modal open, sample, and dismissal model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_usage_probe_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorUsageProbe {
            const Buggy = 0;
            const Tries = 2;
            var pan = 0;
            var ph = 0;
            var own = 0;
            var hand = 0;
            var hold = 0;
            var turn = 0;
            var tries = 0;
            var told = 0;
            var blind = 0;
            var stale = 0;
            var foreign = 0;
            var handkey = 0;
            var turnkey = 0;
            var stacked = 0;
            var booked = 0;
            var bad = 0;

            action Opens when (ph == 2 && pan == 0) {
                pan = 1;
            }
            action Settles when (pan == 1) {
                pan = 2;
            }
            action Unsettles when (pan == 2) {
                pan = 1;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
                own = 0;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action HoldOn when (hold == 0) {
                hold = 1;
                own = 0;
            }
            action HoldOff when (hold == 1) {
                hold = 0;
            }
            action TurnStarts when (turn == 0) {
                turn = 1;
                own = 0;
            }
            action TurnEnds when (turn == 1) {
                turn = 0;
            }
            action TurnRetiresThePanel when (turn == 1 && pan > 0) {
                turn = 0;
                pan = 0;
                own = 0;
            }
            action PersonCloses when (pan > 0) {
                pan = 0;
                own = 0;
            }
            action LoopRestarts when (ph > 0) {
                ph = if ph > 1 { 2 } else { 0 };
                own = 0;
                tries = 0;
                told = 0;
            }

            action Claim when (ph == 0 && pan == 0 && hand == 0 && hold == 0 && turn == 0) {
                ph = 1;
            }
            action Type when (ph == 1 && pan == 0 && hand == 0 && hold == 0 && turn == 0) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
            }
            action TypeSkipped when (ph == 1) {
                ph = 0;
            }
            action GiveUpNotOpened when (ph == 2 && pan == 0) {
                ph = 0;
                own = 0;
            }
            action Sample when (ph == 2 && pan == 2 && own == 1 && hand == 0) {
                ph = 3;
                own = 0;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
            }
            action NoSample when (ph == 2 && pan > 0 && own == 1) {
                ph = 3;
                own = 0;
            }
            action EscCloses when (
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0
            ) {
                pan = 0;
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
            }
            action EscFails when (
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 3;
                tries = if tries <= Tries - 1 { tries + 1 } else { tries };
                told = if Tries - 1 <= tries { 1 } else { told };
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
            }
            action SeesGone when (ph == 3 && pan == 0) {
                ph = 0;
                own = 0;
                tries = 0;
                told = 0;
            }

            action EscOnTheOpenTimeout when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 0 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            action EscUnderAHand when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && hand == 1 && hold == 0 && turn == 0
            ) {
                pan = 0;
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            action EscIntoATurn when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && hand == 0 && hold == 0 && turn == 1
            ) {
                ph = 3;
                turn = 0;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            action TypeUnderAHand when (
                Buggy == 1 && bad == 0 &&
                ph == 1 && pan == 0 && hand == 1 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                bad = 1;
            }
            action TypeOverAPanel when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                bad = 1;
            }
            action TypeBooksAnAct when (
                Buggy == 1 && bad == 0 &&
                ph == 1 && pan == 0 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                booked = 1;
                bad = 1;
            }
            action SampleUnsettled when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 1 && own == 1 && hand == 0
            ) {
                ph = 3;
                own = 0;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
                bad = 1;
            }
            action SampleNotRead when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 2 && own == 0
            ) {
                ph = 3;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
                bad = 1;
            }
            action EscFailsUntold when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0 && tries == Tries - 1
            ) {
                ph = 3;
                tries = Tries;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            action GoesIdleAtTheLastTry when (
                Buggy == 1 && bad == 0 && ph == 3 && pan > 0 && Tries <= tries
            ) {
                ph = 0;
                own = 0;
                tries = 0;
                told = 0;
                bad = 1;
            }

            invariant EscOnlyAtAPanel: blind == 0;
            invariant OnlyASettledPanelIsASample: stale == 0;
            invariant NoSampleFromAPanelNotRead: foreign == 0;
            invariant NothingWrittenUnderAHand: handkey == 0;
            invariant NoEscapeIntoARunningTurn: turnkey == 0;
            invariant NoSecondUsageOverAPanel: stacked == 0;
            invariant TheProbeIsNoAct: booked == 0;
            invariant AStuckPanelIsTold: tries <= Tries - 1 || told == 1;
            invariant AnOpenPanelIsOwed: pan == 0 || ph > 1;
        }
    }
}

/// Liveness spec (`EveryOpenedPanelCloses`) for [`supervisor_usage_probe_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_usage_probe_model)]
pub fn supervisor_usage_probe_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "EveryOpenedPanelCloses",
        goal: or_(
            or_(eq(var("pan"), int(0)), eq(var("hand"), int(1))),
            or_(eq(var("hold"), int(1)), eq(var("turn"), int(1))),
        ),
        weak: vec!["NoSample", "EscCloses"],
        strong: vec![],
        mutants: vec!["GoesIdleAtTheLastTry"],
    }
}

/// Supervisor extra-usage limit reset claim and confirmation model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_limit_reset_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorLimitReset {
            const Buggy = 0;
            var dlg = 0;
            var guard = 0;
            var focus = 0;
            var claim = 0;
            var ph = 0;
            var own = 0;
            var s2 = 0;
            var hand = 0;
            var hold = 0;
            var line = 0;
            var twice = 0;
            var blind = 0;
            var early = 0;
            var weekly = 0;
            var typed = 0;
            var moved = 0;
            var handkey = 0;
            var falseused = 0;
            var retried = 0;
            var bad = 0;

            action WallOpens when (dlg == 0) {
                dlg = 1;
            }
            action WallOpensWeekly when (dlg == 0) {
                dlg = 2;
            }
            action OpensTheConfirmation when (dlg == 3) {
                dlg = 4;
                guard = 1;
                focus = 1;
            }
            action OpensAnInexactConfirmation when (dlg == 3) {
                dlg = 5;
                guard = 1;
                focus = 1;
            }
            action OpensAnAnswer when (dlg == 3) {
                dlg = 0;
                line = 4;
            }
            action GuardEnds when (guard == 1) {
                guard = 0;
            }
            action FocusMoves when (dlg == 4 || dlg == 5) {
                focus = 1 - focus;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action HoldOn when (hold == 0) {
                hold = 1;
            }
            action HoldOff when (hold == 1) {
                hold = 0;
            }
            action ServerAnswersSpent when (dlg == 6) {
                dlg = 0;
                line = 1;
            }
            action ServerAnswersLifted when (dlg == 6) {
                dlg = 0;
                line = 2;
            }
            action ServerAnswersFailed when (dlg == 6) {
                dlg = 0;
                line = 4;
            }
            action ClaimLapses when (claim == 1 && dlg <= 5 && (line == 0 || 3 <= line)) {
                claim = 2;
            }
            action LoopRestarts when (own == 1) {
                own = 0;
            }
            action S2AtWall when (s2 == 0) {
                s2 = 1;
            }

            action Claim when (claim == 0 && ph == 0 && dlg == 1 && hand == 0 && hold == 0) {
                claim = 1;
                retried = if line == 3 || line == 4 { 1 } else { retried };
            }
            action TakeRow when (claim == 1 && ph == 0 && dlg == 1 && hand == 0 && hold == 0) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action Enter when (
                dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action EnterDropped when (
                dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action Escape when (
                ph > 0 && (dlg == 3 || ((dlg == 4 || dlg == 5) && guard == 0))
                && hand == 0 && hold == 0
            ) {
                dlg = 0;
                guard = 0;
                focus = 0;
                ph = 3;
                line = if dlg == 3 { line } else { 3 };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action EscapeDropped when (
                ph > 0 && (dlg == 4 || dlg == 5) && guard == 0 && hand == 0 && hold == 0
            ) {
                ph = 3;
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action ReadLine when (ph > 0 && dlg <= 2) {
                claim = 2;
                ph = 0;
                own = 0;
            }
            action PressWait when ((dlg == 1 || dlg == 2) && ph == 0 && hand == 0 && hold == 0) {
                dlg = 0;
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action S2Waits when (s2 == 1 && claim == 1) {}
            action S2Resumes when (s2 == 1 && claim == 2 && (line == 1 || line == 2)) {
                s2 = 3;
            }
            action S2Moves when (
                s2 == 1 && (claim == 0 || claim == 2) && (line == 0 || 3 <= line)
            ) {
                s2 = 2;
                moved = if claim == 1 || line == 1 || line == 2 { 1 } else { moved };
            }

            action TakeRowUnclaimed when (
                Buggy == 1 && bad == 0
                && dlg == 1 && (claim == 0 || claim == 2 || ph > 0) && hand == 0 && hold == 0
            ) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action NeverReadsTheLine when (
                Buggy == 1 && bad == 0 && ph == 2 && dlg == 0 && claim == 1
            ) {
                ph = 0;
                own = 0;
                bad = 1;
            }
            action EnterTheInexactConfirmation when (
                Buggy == 1 && bad == 0
                && dlg == 5 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterInsideTheGuard when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 1 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterOffYes when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 0 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 0;
                line = 3;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterAsASuccessor when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 0 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterOnALapsedClaim when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 2
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action TakeRowByItsPlace when (
                Buggy == 1 && bad == 0
                && dlg == 2 && claim == 1 && ph == 0 && hand == 0 && hold == 0
            ) {
                dlg = 6;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action TypesTheCommand when (
                Buggy == 1 && bad == 0 && dlg == 0 && ph == 0 && hand == 0 && hold == 0
            ) {
                typed = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action S2MovesUnderAReset when (
                Buggy == 1 && bad == 0 && s2 == 1 && (claim == 1 || line == 1 || line == 2)
            ) {
                s2 = 2;
                moved = if claim == 1 || line == 1 || line == 2 { 1 } else { moved };
                bad = 1;
            }
            action TakeRowUnderAHand when (
                Buggy == 1 && bad == 0
                && claim == 1 && ph == 0 && dlg == 1 && hand == 1 && hold == 0
            ) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterUnderAHand when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 1 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EscapeUnderAHand when (
                Buggy == 1 && bad == 0
                && ph > 0 && (dlg == 3 || ((dlg == 4 || dlg == 5) && guard == 0))
                && hand == 1 && hold == 0
            ) {
                dlg = 0;
                guard = 0;
                focus = 0;
                ph = 3;
                line = if dlg == 3 { line } else { 3 };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action PressWaitUnderAHand when (
                Buggy == 1 && bad == 0
                && (dlg == 1 || dlg == 2) && ph == 0 && hand == 1 && hold == 0
            ) {
                dlg = 0;
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action UsedRecordedAtEnter when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                claim = 2;
                ph = 0;
                own = 0;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                falseused = if line == 1 { falseused } else { 1 };
                bad = 1;
            }
            action ClaimsAgainAfterAFailure when (
                Buggy == 1 && bad == 0
                && claim == 2 && ph == 0 && dlg == 1 && (line == 3 || line == 4)
                && hand == 0 && hold == 0
            ) {
                claim = 1;
                retried = if line == 3 || line == 4 { 1 } else { retried };
                bad = 1;
            }
            action StopsTryingItsEscape when (
                Buggy == 1 && bad == 0 && ph == 3 && (dlg == 4 || dlg == 5)
            ) {
                ph = 0;
                own = 0;
                bad = 1;
            }

            invariant OneAttemptPerWall: twice == 0;
            invariant EnterOnlyOnTheExactConfirmation: early == 0;
            invariant EnterOnlyPastTheGuardOnYes: blind == 0;
            invariant NeverTheWeeklyRow: weekly == 0;
            invariant NeverTheTypedCommand: typed == 0;
            invariant NoMoveOffAnAccountBeingReset: moved == 0;
            invariant NothingKeyedUnderAHand: handkey == 0;
            invariant OnlyTheLineSaysSpent: falseused == 0;
            invariant AFailureIsNotRetried: retried == 0;
            invariant AnOpenConfirmationIsOwed: dlg <= 3 || dlg > 5 || ph > 0 || claim == 0;
        }
    }
}

/// Liveness spec (`EveryClaimEnds`) for [`supervisor_limit_reset_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_limit_reset_model)]
pub fn supervisor_limit_reset_claim_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "EveryClaimEnds",
        goal: or_(eq(var("claim"), int(0)), eq(var("claim"), int(2))),
        weak: vec!["ReadLine", "ClaimLapses", "ServerAnswersFailed"],
        strong: vec![],
        mutants: vec!["NeverReadsTheLine"],
    }
}

/// Liveness spec (`EveryHarnessConfirmationCloses`) for [`supervisor_limit_reset_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_limit_reset_model)]
pub fn supervisor_limit_reset_confirmation_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, le, or_, var};
    Liveness {
        name: "EveryHarnessConfirmationCloses",
        goal: or_(
            le(var("dlg"), int(3)),
            or_(eq(var("hand"), int(1)), eq(var("hold"), int(1))),
        ),
        weak: vec!["GuardEnds", "Escape", "ServerAnswersFailed"],
        strong: vec![],
        mutants: vec!["StopsTryingItsEscape"],
    }
}

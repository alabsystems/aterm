// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE READER-RESUME LAW (`aterm-link`'s `state.rs`.

use aterm_spec::derive::Model;

/// The reader-resume law across power losses and generation moves (module doc).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_reader_resume_model() -> Model {
    aterm_spec::ty_model! {
        FabricReaderResume {
            const Buggy = 0;
            const MaxH = 3;
            const MaxG = 3;
            const W = 2;
            const Top = 4;
            var g = 0;
            var h = 0;
            var f1 = 4;
            var f2 = 4;
            var c = 0;
            var rg = 0;
            var k = 4;
            var down = 0;
            var pc = 0;
            var r = 0;
            var skip = 0;
            var other = 0;

            action Write when (pc == 0 && down == 0 && h + 1 <= MaxH) {
                h = h + 1;
            }
            action Read when (pc == 0 && down == 0 && rg == g && c + 1 <= h) {
                c = c + 1;
            }
            action Crash when (pc == 0 && down == 0 && g + 1 <= MaxG) {
                down = 1;
            }
            action Lose when (down == 1 && h > 0) {
                h = h - 1;
            }
            action Bump when (down == 1) {
                g = g + 1;
                f1 = h;
                f2 = if f1 == Top { Top } else { if f1 <= h { f1 } else { h } };
                k = if k <= h { k } else { h };
                down = 0;
            }
            action Decide when (pc == 0 && down == 0 && rg + 1 <= g) {
                r = if Buggy == 1 {
                    h
                } else {
                    if g - rg <= W {
                        if Buggy == 2 || g - rg == 1 {
                            if c <= f1 { c } else { f1 }
                        } else {
                            if c <= f1 && c <= f2 { c } else { if f1 <= f2 { f1 } else { f2 } }
                        }
                    } else {
                        if Buggy == 3 {
                            if c <= f2 { c } else { f2 }
                        } else {
                            h
                        }
                    }
                };
                pc = 1;
            }
            action Resume when (pc == 1) {
                c = r;
                rg = g;
                k = Top;
                skip = if g - rg <= W && r > k { 1 } else { skip };
                other = if g - rg > W { if r == h { other } else { 1 } } else { other };
                pc = 0;
            }

            invariant NoSkipInWindow: skip == 0;
            invariant AnotherLogOutsideWindow: other == 0;
            bound GenerationBounded: g <= MaxG;
            bound HeadBounded: h <= MaxH;
            bound CursorBounded: c <= MaxH;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the window chrome regrow floor over the tab strip.

use aterm_spec::derive::*;

/// The tab strip's rows over one window's grid ([`chrome_regrow_floor_liveness`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn chrome_regrow_floor_model() -> Model {
    aterm_spec::ty_model! {
        ChromeRegrowFloor {
            const Buggy = 0;
            const Settle = 1;
            const FlipCap = 4;
            var sw = 0;
            var s = 0;
            var age = 1;
            var since = 1;
            var quick = 0;
            var flips = 0;

            action StripRise when (s == 0 && flips <= FlipCap - 1) {
                sw = 1;
                s = 1;
                age = 0;
                since = 0;
                flips = flips + 1;
            }
            action StripUnwant when (sw == 1 && flips <= FlipCap - 1) {
                sw = 0;
                flips = flips + 1;
            }
            action StripBack when (sw == 0 && s == 1 && flips <= FlipCap - 1) {
                sw = 1;
                flips = flips + 1;
            }
            action StripFall when (sw == 0 && s == 1 && Settle <= since) {
                s = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action Tick when (age <= Settle - 1 || since <= Settle - 1) {
                age = if (age <= Settle - 1) { age + 1 } else { age };
                since = if (since <= Settle - 1) { since + 1 } else { since };
            }
            action StripFallAtOnce when (Buggy == 1 && sw == 0 && s == 1
                && since <= Settle - 1)
            {
                s = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action Restart when (Buggy == 1 && Settle <= since && sw <= s - 1) {
                since = 0;
            }

            invariant NoQuickRegrow: quick == 0;
            invariant TheAnchorIsTheLastRise: since == age;
        }
    }
}

/// The grid regrows: `[]<>(s <= sw)`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(chrome_regrow_floor_model)]
pub fn chrome_regrow_floor_liveness() -> Liveness {
    Liveness {
        name: "TheGridRegrows",
        goal: le(var("s"), var("sw")),
        weak: vec!["Tick", "StripFall"],
        strong: vec![],
        mutants: vec!["Restart"],
    }
}

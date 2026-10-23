// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the window chrome regrow floor across the message band and tab strip.

use aterm_spec::derive::*;

/// Two chrome sources of one window's grid ([`chrome_regrow_floor_liveness`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn chrome_regrow_floor_model() -> Model {
    aterm_spec::ty_model! {
        ChromeRegrowFloor {
            const Buggy = 0;
            const Settle = 1;
            const FlipCap = 4;
            var bw = 0;
            var b = 0;
            var sw = 0;
            var s = 0;
            var bq = 1;
            var age = 1;
            var since = 1;
            var cramped = 0;
            var quick = 0;
            var flips = 0;

            action BandRise when (b == 0 && flips <= FlipCap - 1) {
                bw = 1;
                b = 1;
                age = 0;
                since = 0;
                flips = flips + 1;
            }
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
            action BandBack when (bw == 0 && b == 1 && flips <= FlipCap - 1) {
                bw = 1;
                bq = 1;
                flips = flips + 1;
            }
            action BandUnwant when (bw == 1 && flips <= FlipCap - 1) {
                bw = 0;
                bq = 0;
                flips = flips + 1;
            }
            action BandQuiet when (bw == 0 && b == 1 && bq == 0) {
                bq = 1;
            }
            action Tick when (age <= Settle - 1 || since <= Settle - 1) {
                age = if (age <= Settle - 1) { age + 1 } else { age };
                since = if (since <= Settle - 1) { since + 1 } else { since };
            }
            action BandFall when (bw == 0 && b == 1 && bq == 1 && Settle <= since) {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action Cramp when (cramped == 0 && flips <= FlipCap - 1) {
                cramped = 1;
                flips = flips + 1;
            }
            action Uncramp when (cramped == 1) {
                cramped = 0;
            }
            action BandTaken when (cramped == 1 && bw == 0 && b == 1) {
                b = 0;
                bq = 1;
            }
            action BandFallOwnQuiet when (Buggy == 1 && bw == 0 && b == 1 && bq == 1
                && since <= Settle - 1)
            {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action StripFallAtOnce when (Buggy == 1 && sw == 0 && s == 1
                && since <= Settle - 1)
            {
                s = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action Restart when (Buggy == 1 && Settle <= since && (bw <= b - 1 || sw <= s - 1)) {
                since = 0;
            }

            invariant NoQuickRegrow: quick == 0;
            invariant TheAnchorIsTheLastRise: since == age;
        }
    }
}

/// The grid regrows: `[]<>(b <= bw && s <= sw)`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(chrome_regrow_floor_model)]
pub fn chrome_regrow_floor_liveness() -> Liveness {
    Liveness {
        name: "TheGridRegrows",
        goal: and_(le(var("b"), var("bw")), le(var("s"), var("sw"))),
        weak: vec!["Tick", "BandQuiet", "BandFall", "StripFall"],
        strong: vec![],
        mutants: vec!["Restart"],
    }
}

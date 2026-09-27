// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A short composer wrap needs two independent facts: the exact held caret
//! park and a content follow whose moved ribbon cohort owns the glyph at
//! that park's source edge. The proof belongs to that park, survives its
//! delayed flush, and is consumed by one licensed replay. A same-end return
//! without that proof is still a cancelled park; a follow on another row or
//! in another cohort, or an unlicensed flush, cannot arm a short re-anchor.

use super::Model;

/// The bounded held-park/content-follow handshake. `Buggy = 1` reproduces
/// both failures the real host guards: a same-end return cancelling a proved
/// one-cell wrap, and an unproved short park arming a ribbon relay. Tier-1 in
/// `aterm-effects` drives the real host and checks both decisions against
/// these actions, including false-park and unrelated-cohort negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_short_wrap_park_model() -> Model {
    crate::ty_model! {
        RainbowShortWrapPark {
            const Buggy = 0;
            var parked = 0;
            var content_moved = 0;
            var exact = 0;
            var licensed = 0;
            var armed = 0;
            var relayed = 0;
            var cancelled = 0;

            action Hold when (parked == 0 && armed == 0) {
                parked = 1;
                content_moved = 0;
                exact = 0;
                licensed = 0;
                relayed = 0;
                cancelled = 0;
            }
            // Different row or cohort: content moved, but not this park's.
            action FollowOther when (parked == 1 && exact == 0) {
                content_moved = 1;
            }
            action FollowExact when (parked == 1) {
                content_moved = if Buggy == 1 { 0 } else { 1 };
                exact = 1;
            }
            action CancelSameEnd when (parked == 1 && (exact == 0 || Buggy == 1)) {
                parked = 0;
                cancelled = if exact == 1 { 1 } else { 0 };
            }
            action FlushLicensed when (parked == 1) {
                parked = 0;
                licensed = 1;
                armed = if exact == 1 || Buggy == 1 { 1 } else { 0 };
            }
            action FlushDenied when (parked == 1) {
                parked = 0;
                licensed = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action Replay when (armed == 1) {
                armed = 0;
                relayed = 1;
            }
            action ClearUnused when (armed == 1) {
                armed = 0;
            }

            invariant OnlyExactFollowArms: armed <= exact;
            invariant OnlyExactFollowRelays: relayed <= exact;
            invariant OnlyLicensedFlushArms: armed <= licensed;
            invariant ProvedParkCannotCancel: cancelled + exact <= 1;
            invariant ExactImpliesContent: exact <= content_moved;
        }
    }
}

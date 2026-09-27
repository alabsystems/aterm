// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A seated headless session's index worker and its host handoff. The existing
//! index cooldown model owns the cross-process HEAD bound; this model binds the
//! worker's near/final answers to the real host claim and pass decision.

use super::*;

/// One published build, one session seat and a window which can arrive after
/// the HEAD starts. `Buggy=1` replays three mistakes: another session starts a
/// network worker, a stale claim launches behind a window, and the far answer
/// launches the same build twice. Tier-1 drives `Runner::step` and the real
/// `HostClaim` over these transitions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_session_index_handoff_model() -> Model {
    crate::ty_model! {
        AtpkgSessionIndexHandoff {
            const Buggy = 0;
            // 0 idle, 1 HEAD worker pending, 2 completed.
            var probe = 0;
            var requests = 0;
            var near = 0;
            var window = 0;
            var floor = 0;
            var launches = 0;
            var stale_launch = 0;

            action Start when (probe == 0 && window == 0) {
                probe = 1;
                requests = 1;
            }
            action OtherSessionStarts when (probe == 1) {
                requests = if Buggy == 1 { 2 } else { requests };
            }
            action NearPublished when (probe == 1 && near == 0) {
                near = 1;
            }
            action WindowOpens when (window == 0) {
                window = 1;
            }
            action WindowCloses when (window == 1) {
                window = 0;
            }
            action VerifyFloor when (floor == 0) {
                floor = 1;
            }
            action TryLaunch when (near == 1 && launches == 0) {
                launches = if floor == 0 && (window == 0 || Buggy == 1) { 1 } else { 0 };
                stale_launch = if floor == 0 && window == 1 && Buggy == 1 { 1 } else { 0 };
            }
            action Complete when (probe == 1) {
                probe = 2;
            }
            action FarReplay when (probe == 2 && launches == 1) {
                launches = if Buggy == 1 { 2 } else { 1 };
            }

            invariant OneNetworkWorker: requests <= 1;
            invariant NoWindowOverlap: stale_launch == 0;
            invariant OnePassPerAnswer: launches <= 1;
        }
    }
}

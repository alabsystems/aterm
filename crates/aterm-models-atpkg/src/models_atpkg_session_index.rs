// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A seated headless session's index worker and its host handoff.

use aterm_spec::derive::*;

/// An empty store makes no network request.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_session_index_eligibility_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgSessionIndexEligibility {
            const Buggy = 0;
            const Interval = 6;
            const MaxAge = 7;
            var local_age = 1;
            var network_age = 6;
            var eligible = 0;
            var requests = 0;
            var first_eligible_look = 0;
            var missed = 0;

            action EmptyLook when (eligible == 0 && local_age > 0) {
                local_age = 0;
                network_age = if Buggy == 1 { 0 } else { network_age };
            }
            action Tick when (local_age <= MaxAge - 1 || network_age <= MaxAge - 1) {
                local_age = if local_age <= MaxAge - 1 { local_age + 1 } else { local_age };
                network_age = if network_age <= MaxAge - 1 { network_age + 1 } else { network_age };
            }
            action Enable when (eligible == 0) {
                eligible = 1;
            }
            action FirstEligibleLook when (
                eligible == 1 && local_age > 0 && first_eligible_look == 0
            ) {
                first_eligible_look = 1;
                requests = if network_age > Interval - 1 { 1 } else { 0 };
                missed = if network_age > Interval - 1 { 0 } else { 1 };
            }

            invariant NoArtificialNetworkWarmup: missed == 0;
        }
    }
}

/// One published build.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_session_index_handoff_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgSessionIndexHandoff {
            const Buggy = 0;
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

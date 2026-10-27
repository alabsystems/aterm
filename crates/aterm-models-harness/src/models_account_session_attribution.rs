// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local session replacement fences an older observation at the session writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_session_attribution_model() -> Model {
    aterm_spec::ty_model! {
        AccountSessionAttribution {
            const Buggy = 0;
            var phase = 0; var generation = 1; var process = 1;
            var login = 1; var present = 1; var metadata = 0;
            var applied = 0; var row = 2; var changed = 0; var events = 0;
            action Capture when (phase == 0) { phase = 1; }
            action NewGeneration when (phase == 1 && generation == 1) { generation = 2; }
            action NewProcess when (phase == 1 && process == 1) { process = 2; }
            action NewLogin when (phase == 1 && login == 1) { login = 2; }
            action Gone when (phase == 1 && present == 1) { present = 0; }
            action Enrich when (phase == 1 && metadata == 0) { metadata = 1; }
            action Write when (phase == 1) {
                phase = 2;
                applied = if Buggy == 1 && generation == 2 {1}
                    else {if Buggy == 2 && process == 2 {1}
                    else {if Buggy == 3 && login == 2 {1}
                    else {if Buggy == 4 && present == 0 {1}
                    else {if Buggy == 5 {0}
                    else {if generation == 1 && process == 1 && login == 1 && present == 1 {1} else {0}}}}}};
                row = if Buggy == 6 {2}
                    else {if Buggy == 7 && present == 0 {1}
                    else {if generation == 1 && process == 1 && login == 1 && present == 1 {1} else {2}}};
                changed = if generation == 1 && process == 1 && login == 1 && present == 1 {1} else {0};
                events = if Buggy == 8 {0}
                    else {if generation == 1 && process == 1 && login == 1 && present == 1 {1} else {0}};
            }
            invariant GenerationAdmitted: generation == 1 || applied == 0;
            invariant ProcessAdmitted: process == 1 || applied == 0;
            invariant LoginAdmitted: login == 1 || applied == 0;
            invariant PresentAdmitted: present == 1 || applied == 0;
            invariant CurrentCompletes: phase <= 1 || generation == 2 || process == 2
                || login == 2 || present == 0 || applied == 1;
            invariant WritesObservedRow: applied == 0 || row == 1;
            invariant RefusalPreserves: applied == 1 || (row == 2 && changed == 0 && events == 0);
            invariant RecordsTransition: applied == 0 || events == 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A retained projection is absent, unbound or bound. A read is absent, newly
/// unbound, newly bound, partial or failed. Policy is enabled,
/// disabled or unavailable. One local observation has 45 possible inputs.
/// This proves no transport, membership, credential or provider property.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn lab_source_availability_model() -> Model {
    aterm_spec::ty_model! {
        LabSourceAvailability {
            const Buggy = 0;
            var held = 0; var read = 0; var policy = 0;
            var retained = 0; var visible = 0; var pending = 0;
            var naming = 0; var sheet = 0; var phase = 0;
            var revision = 0; var exposed = 0;

            action Held when (phase == 0 && 2 > held) { held = held + 1; }
            action Read when (phase == 0 && 4 > read) { read = read + 1; }
            action Policy when (phase == 0 && 2 > policy) { policy = policy + 1; }
            action Project when (phase == 0) {
                retained = if Buggy == 2 && read > 2 {0}
                    else if read == 0 {if Buggy == 3 {held} else {0}}
                    else if read == 1 {1} else if read == 2 {2} else {held};
                visible = if (policy == 0 || Buggy == 4)
                    && (read == 1 || read == 2 || (read > 2 && held > 0)) {1} else {0};
                pending = if Buggy == 1 {0}
                    else if policy == 2 || (policy == 0 && read > 2 && held == 0) {1} else {0};
                naming = if Buggy == 6 {0}
                    else if policy == 2 || (policy == 0 && read > 2 && held == 0) {2}
                    else if policy == 0 && (read == 2 || (read > 2 && held == 2)) {1} else {0};
                sheet = if Buggy == 5 {1}
                    else if policy == 1 || (policy == 0
                        && (read == 0 || read == 1 || (read > 2 && held == 1))) {1} else {0};
                revision = if Buggy == 7 && (read == 1 || read == 2) {held}
                    else if read == 0 {0} else if read == 1 {3}
                    else if read == 2 {4} else {held};
                exposed = if Buggy == 8 && policy > 0 {1}
                    else if policy > 0 || read == 0 {0}
                    else if read == 1 {3} else if read == 2 {4} else {held};
                phase = 1;
            }

            invariant RetainedRead: phase == 0
                || (read == 0 && retained == 0) || (read == 1 && retained == 1)
                || (read == 2 && retained == 2) || (read > 2 && retained == held);
            invariant VisiblePolicy: phase == 0
                || (policy == 0 && retained > 0 && visible == 1)
                || ((policy > 0 || retained == 0) && visible == 0);
            invariant PendingRead: phase == 0
                || ((policy == 2 || (policy == 0 && read > 2 && held == 0)) && pending == 1)
                || ((policy == 1 || (policy == 0 && (3 > read || held > 0))) && pending == 0);
            invariant NamingRead: phase == 0
                || ((policy == 2 || (policy == 0 && read > 2 && held == 0)) && naming == 2)
                || (policy == 0 && (read == 2 || (read > 2 && held == 2)) && naming == 1)
                || ((policy == 1 || (policy == 0
                    && (read == 0 || read == 1 || (read > 2 && held == 1)))) && naming == 0);
            invariant SheetAdmission: phase == 0
                || ((policy == 1 || (policy == 0
                    && (read == 0 || read == 1 || (read > 2 && held == 1)))) && sheet == 1)
                || ((policy == 2 || (policy == 0
                    && (read == 2 || (read > 2 && (held == 0 || held == 2))))) && sheet == 0);
            invariant SourceRevision: phase == 0
                || (read == 0 && revision == 0) || (read == 1 && revision == 3)
                || (read == 2 && revision == 4) || (read > 2 && revision == held);
            invariant VisibleRevision: phase == 0
                || (policy == 0 && exposed == revision) || (policy > 0 && exposed == 0);
            bound ObservationBox: held <= 2 && read <= 4 && policy <= 2
                && retained <= 2 && visible <= 1 && pending <= 1
                && naming <= 2 && sheet <= 1 && phase <= 1
                && revision <= 4 && exposed <= 4;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Physical history custody at each finite-limit convergence boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scrollback_handoff_model() -> Model {
    aterm_spec::ty_model! {
        ScrollbackHandoff {
            const RingCap = 20;
            const Buggy = 0;
            var held = 12;
            var total = 32;
            var carried = 20;
            var wanted = 20;
            var boundary = 0;

            action Output when (boundary == 0 && wanted <= 39) {
                carried = carried + 1;
                wanted = wanted + 1;
            }
            action Raise when (boundary == 0 && total <= 44) {
                total = total + 4;
            }
            action Lower when (boundary == 0 && total > 24) {
                total = total - 4;
            }
            action FillRoom when (boundary == 0 && held <= RingCap - 1) {
                held = held + 1;
            }
            action OpenRoom when (boundary == 0 && held > 0) {
                held = held - 1;
            }
            action Handoff when (boundary == 0) {
                carried = if Buggy == 2 { carried } else {
                    if Buggy == 1 {
                        if carried > total - RingCap { total - RingCap } else { carried }
                    } else {
                        if carried > total - held { total - held } else { carried }
                    }
                };
                wanted = if wanted > total - held { total - held } else { wanted };
                boundary = 1;
            }
            action Redetach when (boundary > 0) {
                boundary = 0;
            }
            invariant NewestRowsRetained: boundary == 0 || carried == wanted;
            invariant HandoffBounded: boundary == 0 || carried + held <= total;
        }
    }
}

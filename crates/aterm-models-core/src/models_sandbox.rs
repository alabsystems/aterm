// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sandbox restriction apply model (`aterm_sandbox::Limits::apply`).

use aterm_spec::derive::Model;

/// One restriction slot of the apply step (`aterm_sandbox::Limits::apply`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sandbox_model() -> Model {
    aterm_spec::ty_model! {
        Sandbox {
            const Req = 1;
            const Sup = 1;
            const Buggy = 0;
            scenario Req = 0, Sup = 1;
            scenario Req = 1, Sup = 0;
            scenario Req = 0, Sup = 0;
            var applied = 0;
            var done = 0;

            action Apply when (done == 0) {
                applied = if Buggy == 2 {
                    1
                } else {
                    if Buggy == 0 && Req == 1 && Sup == 1 { 1 } else { applied }
                };
                done = 1;
            }

            invariant AllSupportedApplied: done == 0 || Req + Sup <= 1 || applied == 1;
            invariant NoPhantomApply: applied == 0 || (Req == 1 && Sup == 1);
        }
    }
}

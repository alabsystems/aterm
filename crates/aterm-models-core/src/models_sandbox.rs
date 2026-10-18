// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sandbox apply step that confines the shell child before exec.
//!
//! A restriction the policy requests and the host can enforce must be pushed to the kernel;
//! the macOS regression applied nothing, so the child ran unconfined.

use aterm_spec::derive::Model;

/// One restriction slot of the apply step (`aterm_sandbox::Limits::apply`).
///
/// Every law and the `Apply` step treat the restriction slots independently, so the
/// model is one slot whose host capability surface is a configuration: `Req` (the policy asks for it) and `Sup` (the
/// host can enforce it) are constants, and the four combinations are the committed
/// configuration and its three `scenario`s.
///
/// Mutants: `Buggy = 1` is the macOS no-op (apply nothing), `Buggy = 2` applies a
/// restriction the policy did not request or the host cannot enforce.
///
/// Tier-0: `aterm-models-core/tests/derived_isolation.rs`. The real `Limits::apply`
/// carries the `Apply` anchor.
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

            // Once apply ran, a restriction requested and enforceable is applied.
            invariant AllSupportedApplied: done == 0 || Req + Sup <= 1 || applied == 1;
            // Apply is precise: nothing unrequested or unenforceable is marked applied.
            invariant NoPhantomApply: applied == 0 || (Req == 1 && Sup == 1);
        }
    }
}

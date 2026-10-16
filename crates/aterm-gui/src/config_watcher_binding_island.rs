// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The policy mirror's writers, bound to one arm by the compiler's writer census.
//
// `App.config.harness` is the copy of the running policy that Settings shows and the next reload
// compares against; the policy itself is in `HarnessPolicyCell`, under the seals of
// `harness_policy_cell_island.rs`, which say nothing about this copy. `PolicyWriter` does: the
// compiler finds every site in this crate that may write `config.harness` and fails the build
// unless each is reached only from the `Wake::HarnessPolicyObserved` arm
// (`App::adopt_harness_policy`, the model's `Deliver` in `config_watcher_protocol_island.rs`).
// The one other site is a reload's whole-`Config` swap, which first copies the cell into the new
// value; that it carries the value over is this file's claim, not something the compiler checks.
//
// Cite a pass as evidence beside the Tier-1 test
// (`config_watcher::tests::the_watcher_is_the_policys_one_writer`), never as proof: the census is
// a pointer analysis with known false passes (`docs/DESIGN-trust-temporal-models-2026-09-29.md`
// §5 and open questions 12, 13 and 16).
//
// Mounted only under `cfg(all(clean_islands, trust_seals, trust_binding))` (config_watcher.rs):
// a declared binding runs Trust's spawn census, about 25 minutes on this crate under a stage-1
// compiler.
clean {
    import Trust.Binding
    namespace HarnessPolicy
    open Trust.Protocol

    def PolicyWriter : Trust.Protocol.Binding :=
      binds "config.harness" ["HarnessPolicyObserved"]
        ["App::apply_prepared_config_generation_unfenced"]

    end HarnessPolicy
}

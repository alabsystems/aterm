// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The supervisor policy's protocol, declared and model-checked by the compiler.
//
// `config_watcher_island.rs` proves by hand, for any policy values and any run, that the
// watcher's post agrees with what the window runs. This island only declares the protocol as
// data: the Trust compiler detects the `ScalarModel` by its kernel type, searches its 85
// reachable states, and the Clean kernel checks the result while this crate builds
// (`docs/DESIGN-trust-temporal-models-2026-09-29.md`, P2 and P5):
//   * `OneWriter` is safe: `Agrees` holds in every reachable state
//     (`trust_checked_safe.HarnessPolicy.OneWriter`);
//   * `CatchesUp` is live: under weak fairness of the poll, the delivery and the forget, the
//     window runs the file on disk again and again (`trust_checked_live.HarnessPolicy.CatchesUp`).
// Setting `Buggy` to 1 adds two failure shapes (a second writer; a Commit that raises no forget)
// and stops this crate compiling, printing the run that breaks `Agrees`.
//
// The model's `in_force` is the policy the window runs, in `HarnessPolicyCell`
// (harness_policy_cell.rs), whose seals (`harness_policy_cell_island.rs`) say only `offer`
// changes it, and only with the watcher's stamp. The compiler does not match the model's actions
// to the arms; the Tier-1 tests do.
//
// Mounted only on the seal lane (`--cfg clean_islands --cfg trust_seals`, config_watcher.rs):
// `import Trust.Protocol` needs a Trust newer than the installed seal.
//
// The text is `aterm_spec::derive::harness_policy_one_writer_model` and its liveness obligation
// as `emit_clean_models` writes them, so the model here is the one `ty`, the interpreter and the
// Tier-1 tests check.
clean {
    import Trust.Protocol
    namespace HarnessPolicy
    open Trust.Protocol

    def OneWriter : Trust.Temporal.FiniteModel.ScalarModel :=
      machine "HarnessPolicyOneWriter"
        [param "Buggy" 0,
         param "MaxEdits" 3]
        [var "disk" 0,
         var "edits" 0,
         var "has_posted" 0,
         var "last_posted" 0,
         var "queued" 0,
         var "queued_val" 0,
         var "in_force" 1,
         var "committed" 0,
         var "forget_pending" 0]
        [act "WriteOff" ((v "disk" ≐ n 1) ⋏ ((v "edits" ⊞ n 1) ≼ k "MaxEdits"))
           ["disk" ≔ n 0, "edits" ≔ (v "edits" ⊞ n 1)],
         act "WriteOn" ((v "disk" ≐ n 0) ⋏ ((v "edits" ⊞ n 1) ≼ k "MaxEdits"))
           ["disk" ≔ n 1, "edits" ≔ (v "edits" ⊞ n 1)],
         act "Poll" ((v "queued" ≐ n 0) ⋏ (((v "has_posted" ≐ n 0) ⋎ (v "last_posted" ≻ v "disk")) ⋎ (v "disk" ≻ v "last_posted")))
           ["has_posted" ≔ n 1, "last_posted" ≔ v "disk", "queued" ≔ n 1, "queued_val" ≔ v "disk"],
         act "Deliver" (v "queued" ≐ n 1)
           ["in_force" ≔ (cond (v "committed" ≐ n 1) (v "queued_val") (v "in_force")), "queued" ≔ n 0, "queued_val" ≔ n 0],
         act "AdmitOff" (v "committed" ≐ n 1)
           ["in_force" ≔ v "in_force"],
         act "AdmitOn" (v "committed" ≐ n 1)
           ["in_force" ≔ v "in_force"],
         act "Commit" (v "committed" ≐ n 0)
           ["committed" ≔ n 1, "forget_pending" ≔ n 1],
         act "Forget" (v "forget_pending" ≐ n 1)
           ["has_posted" ≔ n 0, "forget_pending" ≔ n 0],
         act "SecondWriterOff" ((k "Buggy" ≐ n 1) ⋏ (v "committed" ≐ n 1))
           ["in_force" ≔ n 0],
         act "SecondWriterOn" ((k "Buggy" ≐ n 1) ⋏ (v "committed" ≐ n 1))
           ["in_force" ≔ n 1],
         act "CommitWithoutForget" ((k "Buggy" ≐ n 1) ⋏ (v "committed" ≐ n 0))
           ["committed" ≔ n 1]]
        [inv "Agrees" (((((v "committed" ≐ n 0) ⋎ (v "has_posted" ≐ n 0)) ⋎ (v "queued" ≐ n 1)) ⋎ (v "forget_pending" ≐ n 1)) ⋎ (v "in_force" ≐ v "last_posted"))]

    def CatchesUp : Trust.Temporal.FiniteModel.ScalarProgress :=
      progress OneWriter ((v "committed" ≐ n 0) ⋎ (v "in_force" ≐ v "disk"))
        ["Poll", "Deliver", "Forget"] []

    end HarnessPolicy
}

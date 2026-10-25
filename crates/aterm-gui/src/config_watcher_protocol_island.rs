// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Supervisor policy protocol (`aterm_models_harness::harness_policy_one_writer_model`), model-checked by Clean.
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

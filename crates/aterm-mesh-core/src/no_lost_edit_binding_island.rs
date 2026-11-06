// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P3, no lost edit: the binding of the compiled Rust kernels to the spec kernels, and the laws of
// the Worlds that ask them. Each `trust_import_` term is the E6 import of a contract! kernel's
// body, so this island is checked where Trust imports bodies (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: each compiled Rust kernel (the E6 import of its body) equals its
    -- spec kernel, for every argument; so the compiled kernels are Exact, and the
    -- held-draft and waiting-write Worlds that ask them keep every law of
    -- no_lost_edit_island.rs. Mounted last (lib.rs), because an island that names
    -- the imports defers itself and every island after it.

    theorem url_stays_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__url_stays a b = spec_url_stays a b := by
      cases a <;> cases b <;> rfl

    theorem hold_waits_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__hold_waits a b = spec_hold_waits a b := by
      cases a <;> cases b <;> rfl

    theorem answer_refuses_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__answer_refuses a b = spec_answer_refuses a b := by
      cases a <;> cases b <;> rfl

    theorem pending_keeps_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__pending_keeps a b = spec_pending_keeps a b := by
      cases a <;> cases b <;> rfl

    theorem pass_signs_is_exact (a b c : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__pass_signs a b c = spec_pass_signs a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem write_signs_is_exact (a b c : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__write_signs a b c = spec_write_signs a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem write_overtaken_is_exact (a b c : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__write_overtaken a b c = spec_write_overtaken a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    namespace AtermMeshNoLostEdit

    open AtermMeshTemporal

    def compiled : Kernels :=
      Kernels.mk trust_import_aterm_mesh_core__no_lost_edit__url_stays
        trust_import_aterm_mesh_core__no_lost_edit__hold_waits
        trust_import_aterm_mesh_core__no_lost_edit__answer_refuses
        trust_import_aterm_mesh_core__no_lost_edit__pending_keeps
        trust_import_aterm_mesh_core__no_lost_edit__pass_signs
        trust_import_aterm_mesh_core__no_lost_edit__write_signs
        trust_import_aterm_mesh_core__no_lost_edit__write_overtaken

    theorem compiled_exact : Exact compiled :=
      ⟨url_stays_is_exact, hold_waits_is_exact, answer_refuses_is_exact, pending_keeps_is_exact,
       pass_signs_is_exact, write_signs_is_exact, write_overtaken_is_exact⟩

    -- The settle the code runs drops a hold only by descent or on the reach rule's proof.
    theorem compiled_settle_drops_only_on_proof (one : Nat → Bool) (hs : Nat) (ps tk : List Nat)
        (u us : Nat) (r : Reach) (h : settleHold compiled one hs ps tk u us r = none) :
        elem u ps = true ∨ (¬ (hs < us) ∧ tk ≠ [] ∧
          ∃ xs, r = Reach.known xs ∧ ∀ x, elem x xs = true → one x = true ∧ elem x tk = true) :=
      settle_drops_only_on_proof compiled compiled_exact one hs ps tk u us r h

    -- In every run of the held-draft World that asks the Rust kernels, wherever no URL the world
    -- may move reads as one server, the held invariant holds in every state.
    theorem compiled_held_safe (one moves : Nat → Bool) (hf : OneIsFixed one moves) :
        Satisfies (HeldSpec compiled one moves) (Always (Lift (HeldInv one))) :=
      held_safe compiled compiled_exact one moves hf

    -- NO LOST EDIT for the compiled kernels: in every state where each spine carries the text of
    -- the versions it names (the descent rule's obligation on the draft), every edit is safe.
    theorem compiled_no_lost_edit (one moves : Nat → Bool) (hf : OneIsFixed one moves)
        (carries : Nat → Nat → Prop) :
        Satisfies (HeldSpec compiled one moves)
          (Always (Lift (fun w => TextInheritance carries w → EditSafe carries one w))) :=
      no_lost_edit compiled compiled_exact one moves hf carries

    -- The waiting writes, for the compiled kernels: signed only once stored, never lost, dropped
    -- as overtaken only by another Mac's value, and signed only over a base.
    theorem compiled_signed_only_stored : Satisfies (WaitSpec compiled) (Always (Lift SignedOnlyStored)) :=
      signed_only_stored compiled compiled_exact

    theorem compiled_no_lost_write : Satisfies (WaitSpec compiled) (Always (Lift NoLostWrite)) :=
      no_lost_write compiled compiled_exact

    theorem compiled_overtaken_by_another : Satisfies (WaitSpec compiled) (Always (Lift OvertakenByAnother)) :=
      overtaken_by_another compiled compiled_exact

    theorem compiled_cas_step : Satisfies (WaitSpec compiled) (Always (Act CasStep)) :=
      cas_step compiled compiled_exact

    end AtermMeshNoLostEdit
}

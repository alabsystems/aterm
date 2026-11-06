// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P3, no lost edit: the binding of the compiled Rust kernels to the spec kernels, and the laws of
// the Worlds that ask them. Each `trust_import_` term is the E6 import of a contract! kernel's
// body, so this island is checked where Trust imports bodies (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: each compiled Rust kernel (the E6 import of its body) equals its
    -- spec kernel, for every argument; so the compiled kernels are Exact, and the
    -- held-draft, waiting-write and unversioned-hold Worlds that ask them keep every
    -- law of no_lost_edit_island.rs, and the server's compare-and-set the code runs
    -- is the held-draft World's takesOver. Mounted last (lib.rs), because an island
    -- that names the imports defers itself and every island after it.

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

    theorem takes_head_is_exact (a b c d e f g : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__takes_head a b c d e f g = spec_takes_head a b c d e f g := by
      cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> cases f <;> cases g <;> rfl

    theorem holds_version_is_exact (a b c d : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__holds_version a b c d = spec_holds_version a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem refuses_not_newer_is_exact (a b c d : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__refuses_not_newer a b c d = spec_refuses_not_newer a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem refuses_chain_is_exact (a b c d : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__refuses_chain a b c d = spec_refuses_chain a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem refuses_parents_is_exact (a b c d e f g : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__refuses_parents a b c d e f g = spec_refuses_parents a b c d e f g := by
      cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> cases f <;> cases g <;> rfl

    theorem hold_adopts_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__hold_adopts a b = spec_hold_adopts a b := by
      cases a <;> cases b <;> rfl

    theorem unversioned_goes_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__unversioned_goes a b = spec_unversioned_goes a b := by
      cases a <;> cases b <;> rfl

    theorem drafted_keeps_is_exact (a : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__drafted_keeps a = spec_drafted_keeps a := by
      cases a <;> rfl

    theorem spine_settles_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__spine_settles a b = spec_spine_settles a b := by
      cases a <;> cases b <;> rfl

    theorem draft_refuses_is_exact (a b c : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__draft_refuses a b c = spec_draft_refuses a b c := by
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
        trust_import_aterm_mesh_core__no_lost_edit__drafted_keeps
        trust_import_aterm_mesh_core__no_lost_edit__spine_settles
        trust_import_aterm_mesh_core__no_lost_edit__draft_refuses

    theorem compiled_exact : Exact compiled :=
      ⟨url_stays_is_exact, hold_waits_is_exact, answer_refuses_is_exact, pending_keeps_is_exact,
       pass_signs_is_exact, write_signs_is_exact, write_overtaken_is_exact, drafted_keeps_is_exact,
       spine_settles_is_exact, draft_refuses_is_exact⟩

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

    -- And with a server whose pin was replaced (givesWay), judged as the code judges it, while
    -- each such server is still at its line's start (ReplacedOnlyAtTheStart, the premise
    -- compiled_replaced_takes_unchecked names): the same laws.
    theorem compiled_held_safe_replaced (one moves givesWay : Nat → Bool) (hf : OneIsFixed one moves) :
        Satisfies (HeldSpecReplaced compiled one moves givesWay) (Always (Lift (HeldInvR one))) :=
      held_safe_replaced compiled compiled_exact one moves givesWay hf

    theorem compiled_no_lost_edit_replaced (one moves givesWay : Nat → Bool) (hf : OneIsFixed one moves)
        (carries : Nat → Nat → Prop) :
        Satisfies (HeldSpecReplaced compiled one moves givesWay)
          (Always (Lift (fun w => TextInheritance carries w → EditSafe carries one w))) :=
      no_lost_edit_replaced compiled compiled_exact one moves givesWay hf carries

    -- The waiting writes, for the compiled kernels: signed only once stored, never lost, dropped
    -- as overtaken only by another Mac's value or by a value this Mac drafted after them (never
    -- by one of its own earlier writes, waiting or direct, unless a take-back), and signed only
    -- over a base.
    theorem compiled_signed_only_stored : Satisfies (WaitSpec compiled) (Always (Lift SignedOnlyStored)) :=
      signed_only_stored compiled compiled_exact

    theorem compiled_no_lost_write : Satisfies (WaitSpec compiled) (Always (Lift NoLostWrite)) :=
      no_lost_write compiled compiled_exact

    theorem compiled_overtaken_by_another : Satisfies (WaitSpec compiled) (Always (Lift OvertakenByAnother)) :=
      overtaken_by_another compiled compiled_exact

    theorem compiled_later_kept : Satisfies (WaitSpec compiled) (Always (Lift LaterKept)) :=
      later_kept compiled compiled_exact

    theorem compiled_cas_step : Satisfies (WaitSpec compiled) (Always (Act CasStep)) :=
      cas_step compiled compiled_exact

    -- The server's compare-and-set the code runs: its five kernels and a refused pin answer every
    -- envelope exactly once, and on the cases the held-draft World covers, what the server keeps
    -- is that World's takesOver, the environment its reach rule rests on.
    theorem compiled_takeover_partitions (p r n s e a h m : Bool) :
        bitOf (trust_import_aterm_mesh_core__no_lost_edit__takes_head p r n e a h m)
          + bitOf (trust_import_aterm_mesh_core__no_lost_edit__holds_version p r n s) + bitOf (!p)
          + bitOf (trust_import_aterm_mesh_core__no_lost_edit__refuses_not_newer p r n s)
          + bitOf (trust_import_aterm_mesh_core__no_lost_edit__refuses_chain p r n e)
          + bitOf (trust_import_aterm_mesh_core__no_lost_edit__refuses_parents p r n e a h m) = 1 := by
      rw [takes_head_is_exact, holds_version_is_exact, refuses_not_newer_is_exact, refuses_chain_is_exact,
        refuses_parents_is_exact]
      exact takeover_partitions p r n s e a h m

    theorem compiled_takes_over_is_the_kernel (same newer named p e a : Bool)
        (hsame : same = true → newer = false ∧ p = true) :
        takesOver same newer named (p && e) =
          (trust_import_aterm_mesh_core__no_lost_edit__takes_head p false (!newer) e a true named
            || trust_import_aterm_mesh_core__no_lost_edit__holds_version p false (!newer) same) := by
      rw [takes_head_is_exact, holds_version_is_exact]
      exact takes_over_is_the_kernel same newer named p e a hsame

    -- And the replaced pin the code runs takes a version with no sequence, chain or parents
    -- check: the environment premise ReplacedOnlyAtTheStart names.
    theorem compiled_replaced_takes_unchecked (n e a h m : Bool) :
        trust_import_aterm_mesh_core__no_lost_edit__takes_head true true n e a h m = true := by
      rw [takes_head_is_exact]
      exact replaced_takes_unchecked n e a h m

    -- The unversioned hold, for the kernels the code calls at both places it goes.
    def compiledL : LKernels :=
      LKernels.mk trust_import_aterm_mesh_core__no_lost_edit__hold_adopts
        trust_import_aterm_mesh_core__no_lost_edit__unversioned_goes

    theorem compiledL_exact : LExact compiledL := ⟨hold_adopts_is_exact, unversioned_goes_is_exact⟩

    theorem compiled_legacy_kept (orig : List Nat) (st : Nat → Bool) :
        Satisfies (LegacySpec compiledL orig st) (Always (Lift (LegacyKept orig))) :=
      legacy_kept compiledL compiledL_exact orig st

    theorem compiled_goes_only_stored (orig : List Nat) (st : Nat → Bool) :
        Satisfies (LegacySpec compiledL orig st) (Always (Act (GoesOnlyStored orig))) :=
      goes_only_stored compiledL compiledL_exact orig st

    end AtermMeshNoLostEdit
}

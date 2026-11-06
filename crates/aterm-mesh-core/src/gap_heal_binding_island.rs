// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P2, gap heal: the binding of the compiled Rust kernels to the spec kernels, and the laws of the
// World that asks them, P1's compiled kernels included. Each `trust_import_` term is the E6
// import of a contract! kernel's body, so this island is checked where Trust imports bodies
// (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: each compiled Rust kernel (`trust_import_aterm_mesh_core__
    -- gap_heal__<name>`, the E6 import of its body) equals its spec kernel, for every
    -- argument; so the compiled kernels are GExact, and the World that asks them and
    -- P1's compiled kernels heals safely, keeps a holder of every newest epoch, and
    -- satisfies laws 3 and 4. Mounted after P1's binding (lib.rs), whose `compiled`
    -- it reads.

    theorem heals_omission_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__gap_heal__heals_omission a b c = spec_heals_omission a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem gap_is_exact (a b c d e : Bool) : trust_import_aterm_mesh_core__gap_heal__gap a b c d e = spec_gap a b c d e := by
      cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> rfl

    theorem denied_is_exact (a b : Bool) : trust_import_aterm_mesh_core__gap_heal__denied a b = spec_denied a b := by
      cases a <;> cases b <;> rfl

    theorem heal_signs_is_exact (a b : Bool) : trust_import_aterm_mesh_core__gap_heal__heal_signs a b = spec_heal_signs a b := by
      cases a <;> cases b <;> rfl

    theorem carried_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__gap_heal__carried a b c = spec_carried a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem strands_is_exact (a b : Bool) : trust_import_aterm_mesh_core__gap_heal__strands a b = spec_strands a b := by
      cases a <;> cases b <;> rfl


    namespace AtermMeshGapHeal

    open AtermMeshTemporal
    open AtermMeshRevocation (compiled compiled_exact)

    def compiledG : GKernels :=
      GKernels.mk trust_import_aterm_mesh_core__gap_heal__heals_omission trust_import_aterm_mesh_core__gap_heal__gap
        trust_import_aterm_mesh_core__gap_heal__denied trust_import_aterm_mesh_core__gap_heal__heal_signs
        trust_import_aterm_mesh_core__gap_heal__carried trust_import_aterm_mesh_core__gap_heal__strands

    theorem compiledG_exact : GExact compiledG :=
      ⟨heals_omission_is_exact, gap_is_exact, denied_is_exact, heal_signs_is_exact, carried_is_exact,
       strands_is_exact⟩

    -- The compiled heal, carry and mint are safe in every run, guarded or not: each
    -- Wrap a step adds goes to a current seat no roster denies, never to a host list.
    theorem compiled_wraps_safe (g : Bool) :
        Satisfies (LabSpec compiled compiledG g) (Always (Act (WrapsSafe compiled compiledG))) :=
      lab_wraps_safe compiled compiled_exact compiledG compiledG_exact g

    -- The compiled guard keeps a current holder of every newest epoch of a seat
    -- audience in every state of every run.
    theorem compiled_holder_exists :
        Satisfies (LabSpec compiled compiledG true) (Always (Lift (HolderExists compiled))) :=
      lab_holder_exists compiled compiled_exact compiledG compiledG_exact

    -- Law 4 of the compiled decision (epoch convergence, no lockout), under the
    -- premises law4_converges names.
    theorem compiled_law4 (b : Behavior World) (hr : Runs (LabSpec compiled compiledG true) b)
        (a n0 : Nat) (ha : seatAud a = true) (hst : StableFrom compiled compiledG b n0)
        (hh : HealFair compiled compiledG b) (hm : MintFair compiled compiledG b a)
        (hc : ∃ c, c < (b n0).nr ∧ cur compiled (b n0) c = true ∧ den compiledG (b n0) c = false) :
        ∃ k, n0 ≤ k ∧ Converged compiled compiledG (b k) a :=
      law4_converges compiled compiled_exact compiledG compiledG_exact b hr a n0 ha hst hh hm hc

    -- Law 3 of the compiled decision (join liveness), under the premises
    -- law3_joiner_holds names.
    theorem compiled_law3 (b : Behavior World) (hr : Runs (LabSpec compiled compiledG true) b)
        (a n r : Nat) (ha : seatAud a = true) (hadm : AdmitsR compiled (b n) (b (Nat.succ n)) r)
        (hden : den compiledG (b (Nat.succ n)) r = false)
        (hst : StableFrom compiled compiledG b (Nat.succ n)) (hh : HealFair compiled compiledG b)
        (hm : MintFair compiled compiledG b a) :
        ∃ k, Nat.succ n ≤ k ∧ ∀ e, isHead (b k) a e = true → (b k).wr e r = true :=
      law3_joiner_holds compiled compiled_exact compiledG compiledG_exact b hr a n r ha hadm hden hst hh hm

    end AtermMeshGapHeal
}

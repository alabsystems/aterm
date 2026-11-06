// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P5, the relay's aterm half: the binding of the compiled Rust kernels to the spec kernels, and
// the laws of the World that asks them. Each `trust_import_` term is the E6 import of a contract!
// kernel's body, so this island is checked where Trust imports bodies (`cfg(trust_verify)` until
// TG12).
clean {
    -- The binding: each compiled Rust kernel (the E6 import of its body) equals its
    -- spec kernel, for every argument; so the compiled kernels are Exact, and the
    -- World that asks them keeps broker state the verdict of the read, never
    -- revokes an origin and takes one origin per class. Mounted last (lib.rs),
    -- because an island that names the imports defers itself and every island after
    -- it.

    theorem origin_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__relay__origin a b c = spec_origin a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem key_revoked_is_exact (a b c d : Bool) : trust_import_aterm_mesh_core__relay__key_revoked a b c d = spec_key_revoked a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem principal_revoked_is_exact (a b : Bool) : trust_import_aterm_mesh_core__relay__principal_revoked a b = spec_principal_revoked a b := by
      cases a <;> cases b <;> rfl

    namespace AtermMeshRelay

    open AtermMeshTemporal

    def compiled : Kernels :=
      Kernels.mk trust_import_aterm_mesh_core__relay__origin trust_import_aterm_mesh_core__relay__key_revoked
        trust_import_aterm_mesh_core__relay__principal_revoked

    theorem compiled_exact : Exact compiled :=
      ⟨origin_is_exact, key_revoked_is_exact, principal_revoked_is_exact⟩

    -- In every run of the World that asks the Rust kernels, broker state is the
    -- verdict of the read applied.
    theorem compiled_standings_safe : Satisfies (SpecW compiled) (Always (Lift (Inv compiled))) :=
      standings_safe compiled compiled_exact

    -- NoCurrentSeatRevoked of the compiled verdict, in every state of every run.
    theorem compiled_no_origin_revoked :
        Satisfies (SpecW compiled) (Always (Lift (fun w => ∀ k, k < w.view.n → w.origin k = true →
          w.revoked k = false ∧ w.rprin (w.view.prin k) = false))) :=
      fun b runs n k hk ho => no_origin_revoked compiled compiled_exact (b n) (compiled_standings_safe b runs n) k hk ho

    -- One origin per class, of the compiled verdict on any read.
    theorem compiled_one_origin_per_class (v : View) (a b : Nat) (ha : a < v.n) (hb : b < v.n)
        (oa : originW compiled v a = true) (ob : originW compiled v b = true) (hcls : v.cls a = v.cls b) : a = b :=
      one_origin_per_class compiled compiled_exact v a b ha hb oa ob hcls

    end AtermMeshRelay
}

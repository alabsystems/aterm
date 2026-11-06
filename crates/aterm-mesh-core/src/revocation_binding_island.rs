// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P1, revocation: the binding of the compiled Rust kernels to the spec kernels, and the laws of
// the World that asks them. Each `trust_import_` term is the E6 import of a contract! kernel's
// body, so this island is checked where Trust imports bodies (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: each compiled Rust kernel (`trust_import_aterm_mesh_core__
    -- revocation__<name>`, the E6 import of its body) equals its spec kernel, for
    -- every argument; so the compiled kernels are Exact, and the World that asks
    -- them satisfies law 2 and the writer's rule. Mounted last (lib.rs), because an
    -- island that names `trust_import_` defers itself and every island after it.

    theorem takes_is_exact (a b c d : Bool) : trust_import_aterm_mesh_core__revocation__takes a b c d = spec_takes a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem precedes_is_exact (a b : Bool) : trust_import_aterm_mesh_core__revocation__precedes a b = spec_precedes a b := by
      cases a <;> cases b <;> rfl

    theorem restores_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__revocation__restores a b c = spec_restores a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem row_revokes_is_exact (a b : Bool) : trust_import_aterm_mesh_core__revocation__row_revokes a b = spec_row_revokes a b := by
      cases a <;> cases b <;> rfl

    theorem gate_row_is_exact (a b c d : Bool) : trust_import_aterm_mesh_core__revocation__gate_row a b c d = spec_gate_row a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem current_is_exact (a b : Bool) : trust_import_aterm_mesh_core__revocation__current a b = spec_current a b := by
      cases a <;> cases b <;> rfl

    theorem wraps_removed_is_exact (a b : Bool) : trust_import_aterm_mesh_core__revocation__wraps_removed a b = spec_wraps_removed a b := by
      cases a <;> cases b <;> rfl

    theorem head_due_is_exact (a b : Bool) : trust_import_aterm_mesh_core__revocation__head_due a b = spec_head_due a b := by
      cases a <;> cases b <;> rfl


    namespace AtermMeshRevocation

    open AtermMeshTemporal

    def compiled : Kernels :=
      Kernels.mk trust_import_aterm_mesh_core__revocation__takes trust_import_aterm_mesh_core__revocation__precedes
        trust_import_aterm_mesh_core__revocation__restores trust_import_aterm_mesh_core__revocation__row_revokes
        trust_import_aterm_mesh_core__revocation__gate_row trust_import_aterm_mesh_core__revocation__current
        trust_import_aterm_mesh_core__revocation__wraps_removed trust_import_aterm_mesh_core__revocation__head_due

    theorem compiled_exact : Exact compiled :=
      ⟨takes_is_exact, precedes_is_exact, restores_is_exact, row_revokes_is_exact, gate_row_is_exact,
       current_is_exact, wraps_removed_is_exact, head_due_is_exact⟩

    -- Law 2 of the compiled decision: in every run of the World that asks the Rust
    -- kernels, only a person's Allow or a quorum's void lifts a revocation.
    theorem compiled_law2 (t0 : Table) (r : Nat) :
        Satisfies (SpecW compiled t0) (Always (Act (fun t t' =>
          revokedW compiled t r = true → revokedW compiled t' r = false →
            PersonBinds r t t' ∨ VoidStep t t'))) :=
      law2_of_exact compiled compiled_exact t0 r

    -- The writer's rule of the compiled decision, in every state of every run.
    theorem compiled_writer_rule (t0 : Table) :
        Satisfies (SpecW compiled t0) (Always (Lift (WriterRule compiled))) :=
      writer_rule_of_exact compiled compiled_exact t0

    end AtermMeshRevocation
}

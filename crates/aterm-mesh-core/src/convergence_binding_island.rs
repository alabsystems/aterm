// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Law 5, value convergence: the binding of the compiled Rust kernels to the spec kernels, and the
// law for the register that asks them. Each `trust_import_` term is the E6 import of a contract!
// kernel's body, so this island is checked where Trust imports bodies (`cfg(trust_verify)` until
// TG12).
clean {
    -- The binding: each compiled Rust kernel (`trust_import_aterm_mesh_core__
    -- convergence__<name>`, the E6 import of its body) equals its spec kernel, for
    -- every argument; so the compiled kernels are Exact, and the register that
    -- asks them converges. Mounted last (lib.rs), because an island that names
    -- `trust_import_` defers itself and every island after it.

    theorem carries_is_exact (a b c d : Bool) : trust_import_aterm_mesh_core__convergence__carries a b c d = spec_carries a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem replaces_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__convergence__replaces a b c = spec_replaces a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem drops_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__convergence__drops a b c = spec_drops a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem beats_is_exact (a b c : Bool) : trust_import_aterm_mesh_core__convergence__beats a b c = spec_beats a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    theorem conflicts_is_exact (a : Bool) : trust_import_aterm_mesh_core__convergence__conflicts a = spec_conflicts a := by
      cases a <;> rfl


    namespace AtermMeshConvergence

    def compiled : Kernels :=
      Kernels.mk trust_import_aterm_mesh_core__convergence__carries trust_import_aterm_mesh_core__convergence__replaces
        trust_import_aterm_mesh_core__convergence__drops trust_import_aterm_mesh_core__convergence__beats
        trust_import_aterm_mesh_core__convergence__conflicts

    theorem compiled_exact : Exact compiled :=
      ⟨carries_is_exact, replaces_is_exact, drops_is_exact, beats_is_exact, conflicts_is_exact⟩

    -- Law 5 of the compiled decision: two Macs that read the same ops off their
    -- logs (SameLog; the island's section 7 names what that takes of their
    -- records), each holding a record once and after its causal past, read the
    -- same winner (the same value), the same conflict, the same siblings, the
    -- same carries and the same reps at every entry, [everyone]'s included, when
    -- the register asks the Rust kernels.
    theorem compiled_converges (L M : Log) (hd : LogDistinct L) (hd' : LogDistinct M) (ho : LogOrder L)
        (ho' : LogOrder M) (h : SameLog L M) (e : Nat) :
        winnerRid compiled (regOf M e) = winnerRid compiled (regOf L e)
          ∧ conflict compiled (regOf M e) = conflict compiled (regOf L e)
          ∧ (∀ x, sibRid compiled (regOf M e) x = sibRid compiled (regOf L e) x)
          ∧ (∀ x, carryRid compiled (regOf M e) x = carryRid compiled (regOf L e) x)
          ∧ (∀ x r, repRid compiled (regOf M e) x r = repRid compiled (regOf L e) x r) :=
      macs_converge compiled (beats_exact_of_exact compiled compiled_exact) L M hd hd' ho ho' h e

    -- And any two arrival orders of one register's ops read it alike.
    theorem compiled_arrival_orders (t u : Reg) (pi sigma : Nat → Nat) (h : Reenumerates t u pi sigma)
        (hd : Distinct t) (ho : RecordOrder t) (ho' : RecordOrder u) :
        winnerRid compiled u = winnerRid compiled t ∧ conflict compiled u = conflict compiled t
          ∧ (∀ x, sibRid compiled u x = sibRid compiled t x) ∧ (∀ x, carryRid compiled u x = carryRid compiled t x)
          ∧ (∀ x r, repRid compiled u x r = repRid compiled t x r) :=
      arrival_orders_converge compiled (beats_exact_of_exact compiled compiled_exact) t u pi sigma h hd ho ho'

    end AtermMeshConvergence
}

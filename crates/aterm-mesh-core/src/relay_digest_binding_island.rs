// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// H1's close, the digest trigger: the binding of the compiled Rust kernel to its spec, and the
// trigger's laws for the fold that asks it. The `trust_import_` term is the E6 import of the
// contract! kernel's body, so this island is checked where Trust imports bodies
// (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: the compiled Rust kernel (the E6 import of its body) equals its spec
    -- kernel for every argument, so it is Exact, and the trigger that asks it keeps every
    -- law of relay_digest_island.rs. Mounted last (lib.rs), because an island that names
    -- the imports defers itself and every island after it.

    theorem digest_asks_is_exact (a b c d e : Bool) :
        trust_import_aterm_mesh_core__relay_digest__digest_asks a b c d e = spec_digest_asks a b c d e := by
      cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> rfl

    namespace AtermMeshRelayDigest

    theorem compiled_exact : Exact trust_import_aterm_mesh_core__relay_digest__digest_asks :=
      digest_asks_is_exact

    -- The compiled trigger never asks between Macs in step.
    theorem compiled_never_in_step (off : Bool) (w : Mem) (t : Nat) (f : Bool) :
        asksOf trust_import_aterm_mesh_core__relay_digest__digest_asks off w t t f = false :=
      never_in_step _ compiled_exact off w t f

    -- The compiled trigger asks once per pair of digests.
    theorem compiled_once_per_pair (off : Bool) (w : Mem) (t m : Nat) (f g : Bool)
        (h : asksOf trust_import_aterm_mesh_core__relay_digest__digest_asks off w t m f = true) :
        asksOf trust_import_aterm_mesh_core__relay_digest__digest_asks off
          (stepM trust_import_aterm_mesh_core__relay_digest__digest_asks off w (Ev.row t m f)) t m g = false :=
      once_per_pair _ compiled_exact off w t m f g h

    -- The compiled trigger asks on every difference that lasts two rows.
    theorem compiled_a_lasting_difference_is_asked (ev : Nat → Ev) (w : Nat → Mem)
        (hr : RunT trust_import_aterm_mesh_core__relay_digest__digest_asks false ev w) (n t m : Nat)
        (hd : differ t m = true) (hl : ∀ i, ev (n + i) = Ev.row t m true) :
        ∃ j, j < n + 2 ∧ ev j = Ev.row t m true ∧
          asksAt trust_import_aterm_mesh_core__relay_digest__digest_asks false (w j) (ev j) = true :=
      a_lasting_difference_is_asked _ compiled_exact ev w hr n t m hd hl

    end AtermMeshRelayDigest
}

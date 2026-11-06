// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P6, the membership log cannot wedge: the binding of the compiled Rust stop and wait to the spec
// kernels, and the law of the World that asks them. Each `trust_import_` term is the E6 import
// of a contract! kernel's body, so this island is checked where Trust imports bodies
// (`cfg(trust_verify)` until TG12). `keeps_room` joins them once E6 reads a comparison over a
// computed integer (Trust's I2'); until then capacity::tests' answer sheet binds it.
clean {
    -- The binding: each compiled Rust kernel (`trust_import_aterm_mesh_core__
    -- capacity__<name>`, the E6 import of its body) equals its spec kernel, for every
    -- argument. Mounted last (lib.rs), because an island that names `trust_import_`
    -- defers itself and every island after it.

    theorem stop_judges_is_exact (a : Bool) :
        trust_import_aterm_mesh_core__capacity__stop_judges a = spec_stop_judges a := by
      cases a <;> rfl

    theorem writer_waits_is_exact (a b c : Bool) :
        trust_import_aterm_mesh_core__capacity__writer_waits a b c = spec_writer_waits a b c := by
      cases a <;> cases b <;> cases c <;> rfl

    namespace AtermMeshCapacity

    open AtermMeshTemporal

    -- The law of the compiled stop and wait: in every run of the World that asks the
    -- Rust kernels (and the room's Nat meaning, which section 1's keeps_room_means
    -- proves spec_keeps_room computes), the log never wedges under the premises the
    -- shipped rule leaves.
    theorem compiled_never_wedges_when_calm (b : Budgets) :
        Satisfies
          (SpecNow b trust_import_aterm_mesh_core__capacity__stop_judges
            trust_import_aterm_mesh_core__capacity__writer_waits roomNat)
          (Always (Lift (Inv b))) :=
      spec_now_never_wedges_when_calm b trust_import_aterm_mesh_core__capacity__stop_judges
        stop_judges_is_exact trust_import_aterm_mesh_core__capacity__writer_waits roomNat
        (fun _ _ _ _ => rfl)

    -- A compiled writer waits only on a re-key the compiled stop never judges.
    theorem compiled_writer_waits_only_on_exempt (k m due : Bool)
        (h : trust_import_aterm_mesh_core__capacity__writer_waits k m due = true) :
        trust_import_aterm_mesh_core__capacity__stop_judges due = false :=
      writer_waits_only_on_exempt trust_import_aterm_mesh_core__capacity__stop_judges
        stop_judges_is_exact trust_import_aterm_mesh_core__capacity__writer_waits
        writer_waits_is_exact k m due h

    end AtermMeshCapacity
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P5b, the relay's liveness: the binding of the compiled Rust kernels to the spec kernels, and the
// liveness theorems for the model that asks them. Each `trust_import_` term is the E6 import of a
// contract! kernel's body, so this island is checked where Trust imports bodies
// (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: each compiled Rust kernel (the E6 import of its body) equals its
    -- spec kernel, for every argument; so the compiled kernels are Exact, and the
    -- model that asks them keeps the quiet rule safe, repairs what an edge let go and,
    -- with the digest trigger, brings the record to every seat. Mounted last (lib.rs),
    -- because an island that names the imports defers itself and every island after it.

    theorem cursor_is_exact (a b c d : Bool) :
        trust_import_aterm_mesh_core__relay_live__cursor a b c d = spec_cursor a b c d := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl

    theorem start_repair_is_exact (c : UInt8) :
        trust_import_aterm_mesh_core__relay_live__start_repair c = spec_start_repair c := rfl

    theorem requests_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__relay_live__requests a b = spec_requests a b := by
      cases a <;> cases b <;> rfl

    theorem runs_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__relay_live__runs a b = spec_runs a b := by
      cases a <;> cases b <;> rfl

    namespace AtermMeshRelayLive

    open AtermMeshTemporal

    def compiled : Kernels :=
      Kernels.mk trust_import_aterm_mesh_core__relay_live__cursor
        trust_import_aterm_mesh_core__relay_live__start_repair
        trust_import_aterm_mesh_core__relay_live__requests
        trust_import_aterm_mesh_core__relay_live__runs

    theorem compiled_exact : Exact compiled :=
      ⟨cursor_is_exact, start_repair_is_exact, requests_is_exact, runs_is_exact⟩

    -- The quiet rule of the compiled kernels, in every state of every run.
    theorem compiled_quiet_safe (noGap : Bool) (durable : Nat → Bool) (nb : Nat → Nat → Bool)
        (o : Nat) : Satisfies (SpecM (Model.mk compiled noGap durable nb) o) (Always (Lift QInv)) :=
      quiet_safe (Model.mk compiled noGap durable nb) o

    -- What the compiled relay's edge owes reaches the neighbour's stores, under its fair hop.
    theorem compiled_edge_is_repaired (durable : Nat → Bool) (nb : Nat → Nat → Bool) (o : Nat)
        (b : Nat → W) (ev : Nat → Ev) (hr : RunsE (Model.mk compiled false durable nb) o b ev)
        (s d n : Nat) (hd : durable s = true) (hf : FairHop b s d)
        (hra : RepairAfterIngest (Model.mk compiled false durable nb) b ev s d)
        (h : ((b n).owes s d && (b n).log s) = true) : ∃ k, (b k).store d = true :=
      the_edge_is_repaired (Model.mk compiled false durable nb) o b ev hr compiled_exact rfl s d n hd
        hf hra h

    -- Every seat the origin reaches, under the compiled kernels and the digest trigger.
    theorem compiled_every_seat_gets_the_record (noGap : Bool) (durable : Nat → Bool)
        (nb : Nat → Nat → Bool) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE (Model.mk compiled noGap durable nb) o b ev)
        (fair : ∀ s d, nb s d = true →
          FairDigest (Model.mk compiled noGap durable nb) b s d ∧ Fades (fun i => (b i).asked s d)) :
        ∀ d, Reach nb o d → ∃ k, (b k).store d = true :=
      every_seat_gets_the_record (Model.mk compiled noGap durable nb) o b ev hr compiled_exact fair

    -- With nothing let go, the compiled relay's edge and flood alone reach every seat.
    theorem compiled_flood_without_let_go (noGap : Bool) (durable : Nat → Bool)
        (nb : Nat → Nat → Bool) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE (Model.mk compiled noGap durable nb) o b ev) (calm : ∀ n, Calm (ev n) = true)
        (fl : ∀ s d, nb s d = true → Fades (fun i => (b i).owes s d && (b i).log s))
        (fi : ∀ d, Fades (fun i => (b i).log d && !(b i).store d)) :
        ∀ d, Reach nb o d → ∃ k, (b k).store d = true :=
      flood_without_let_go (Model.mk compiled noGap durable nb) o b ev hr compiled_exact calm fl fi

    end AtermMeshRelayLive
}

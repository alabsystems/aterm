// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P1b, no leak: spec_no_leak and spec_no_leak_mint for the World that asks the compiled Rust
// kernels (revocation_binding_island.rs's `compiled`, the E6 imports of revocation.rs's
// contract! kernels). Checked where Trust imports bodies (`cfg(trust_verify)` until TG12), after
// the revocation binding it cites.
clean {
    namespace AtermMeshNoLeak

    open AtermMeshTemporal
    open AtermMeshRevocation

    -- In every state of every run of the World that asks the Rust kernels, nothing sealed
    -- after a seat's removal is readable by the removed seat but after a re-grant.
    theorem compiled_no_leak (D : Dag) : Satisfies (SpecL compiled D) (Always (Lift (NoLeak D))) :=
      spec_no_leak compiled compiled_exact D

    -- Nothing minted after a seat's removal is handed to it but after a re-grant.
    theorem compiled_no_leak_mint (D : Dag) : Satisfies (SpecL compiled D) (Always (Lift (NoLeakMint D))) :=
      spec_no_leak_mint compiled compiled_exact D

    end AtermMeshNoLeak
}

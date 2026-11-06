// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The own-inbox move across this Mac's key rotation: the binding of the compiled Rust kernel to
// the spec kernel, and the law for the kernel as compiled. The `trust_import_` term is the E6
// import of the contract! kernel's body, so this island is checked where Trust imports bodies
// (`cfg(trust_verify)` until TG12).
clean {
    -- The binding: the compiled Rust kernel (the E6 import of its body) equals its spec
    -- kernel, for every argument. Mounted last (lib.rs), because an island that names
    -- the imports defers itself and every island after it.

    theorem move_opens_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__inbox_move__move_opens a b = spec_move_opens a b := by
      cases a <;> cases b <;> rfl

    theorem shows_anew_is_exact (a b : Bool) :
        trust_import_aterm_mesh_core__inbox_move__shows_anew a b = spec_shows_anew a b := by
      cases a <;> cases b <;> rfl

    namespace AtermMeshInboxMove

    -- The law of the compiled kernel: a move it opens resumes past every record the old
    -- node's bridge delivered, under section 1's premises.
    theorem compiled_move_resumes_past_every_delivery (mayDeliver owedUnmade : Bool) (d c o : Nat)
        (hGone : mayDeliver = false → d ≤ c ∨ d ≤ o)
        (hMade : owedUnmade = false → o ≤ c)
        (hOpen : trust_import_aterm_mesh_core__inbox_move__move_opens mayDeliver owedUnmade = true) :
        d ≤ c :=
      move_resumes_past_every_delivery trust_import_aterm_mesh_core__inbox_move__move_opens
        move_opens_is_exact mayDeliver owedUnmade d c o hGone hMade hOpen

    -- The endpoint's compiled kernel shows each piece of mail once, under section 4's
    -- premises.
    theorem compiled_each_mail_is_shown_once (offKnown wasKnown shownBefore : Bool)
        (hRemember : shownBefore = true → (offKnown || wasKnown) = true)
        (hFresh : (offKnown || wasKnown) = true → shownBefore = true) :
        trust_import_aterm_mesh_core__inbox_move__shows_anew offKnown wasKnown = !shownBefore :=
      each_mail_is_shown_once trust_import_aterm_mesh_core__inbox_move__shows_anew
        shows_anew_is_exact offKnown wasKnown shownBefore hRemember hFresh

    end AtermMeshInboxMove
}

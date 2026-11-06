// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P4's binding: each handshake kernel in lib.rs is its island definition, so the session laws
// `peer_check_island.rs` proves of any kernel meeting its premises hold of the compiled bodies.
// Mounted last, through `verify_island!`, because it names `trust_import_`.
clean {
    -- Each compiled kernel is its island definition, the equality each kernel's clause cites.
    theorem peer_check_start_is_exact (c : UInt8) :
        trust_import_aterm_db_core__peer_check_start c = peer_check_start_spec c := rfl
    theorem peer_check_next_is_exact (c s o : UInt8) (w : Bool) :
        trust_import_aterm_db_core__peer_check_next c s o w = peer_check_next_spec c s o w := rfl
    theorem peer_check_roster_is_exact (c r d : UInt8) :
        trust_import_aterm_db_core__peer_check_roster c r d = peer_check_roster_spec c r d := rfl
    theorem peer_check_view_is_exact (c o : UInt8) :
        trust_import_aterm_db_core__peer_check_view c o = peer_check_view_spec c o := rfl
    -- The removal predicate's import is held equal to its island def cell by cell, as
    -- aterm-mesh-core's bool kernels are: a bare `rfl` between a bool-valued kernel's import and
    -- its def was refused on Trust 16387 (`TypeMismatch`), while each concrete cell evaluates.
    theorem seat_out_is_exact (a r w k : Bool) :
        trust_import_aterm_db_core__seat_out a r w k = seat_out_spec a r w k := by
      cases a <;> cases r <;> cases w <;> cases k <;> rfl

    -- So the compiled removal predicate takes out a seat whose actor is out, whatever the rest
    -- says (the A15 immunity of the one removal predicate, `Log::seat_removed`).
    theorem seat_out_compiled_removal_by_actor (r w k : Bool) :
        trust_import_aterm_db_core__seat_out true r w k = true :=
      Eq.trans (seat_out_is_exact true r w k) (AtermDbCore.PeerCheck.removal_by_actor_needs_no_recipient r w k)

    -- So the compiled kernels meet the premises the session theorems rest on.
    theorem peer_check_compiled_premises :
        AtermDbCore.PeerCheck.Premises trust_import_aterm_db_core__peer_check_next
          trust_import_aterm_db_core__peer_check_start :=
      AtermDbCore.PeerCheck.kernel_premises

    -- The compiled handshake never fails open: a log endpoint passes a peer only after its held
    -- records read clear, its acknowledged log read clear or held no Commons judgement to make,
    -- and any log of the peer it combined read clear; any number of sessions, runs of any length.
    theorem peer_check_compiled_never_fails_open (n : Nat) :
        AtermDbCore.PeerCheck.Satisfies
          (AtermDbCore.PeerCheck.Spec trust_import_aterm_db_core__peer_check_next
            trust_import_aterm_db_core__peer_check_start n)
          (AtermDbCore.PeerCheck.Always
            (AtermDbCore.PeerCheck.Lift (AtermDbCore.PeerCheck.InvNoFailOpen n))) :=
      AtermDbCore.PeerCheck.no_fail_open _ _ peer_check_compiled_premises n

    -- Once every record held removes the peer, each grant of the compiled handshake spends a check
    -- that was in flight at the removal, one grant per such check.
    theorem peer_check_compiled_one_grant_per_inflight_check (n : Nat) :
        AtermDbCore.PeerCheck.Satisfies
          (AtermDbCore.PeerCheck.Spec trust_import_aterm_db_core__peer_check_next
            trust_import_aterm_db_core__peer_check_start n)
          (AtermDbCore.PeerCheck.Always (AtermDbCore.PeerCheck.Lift (fun w =>
            w.custody = 2 -> w.outAll = true ->
              w.grantsWhileOut + AtermDbCore.PeerCheck.cnt n w.stage <= w.atFlip))) :=
      AtermDbCore.PeerCheck.at_most_one_grant_per_inflight_check _ _ peer_check_compiled_premises n
}

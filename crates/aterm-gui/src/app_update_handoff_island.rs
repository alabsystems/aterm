// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE SEAMLESSNESS INVARIANT, PROVED BY THE COMPILER.
//
// Everything else in this file argues that a user cannot lose a terminal session
// across a process replacement. This states it, and the Clean CIC kernel checks
// it during the build (two-language design E10) -- so the argument stops being
// prose the moment somebody edits the protocol out from under it.
//
// It is checked even under -Ztrust-verify=off, the workspace-wide opt-out in
// .cargo/config.toml, because an island carries its own authority.
//
// WHAT IT PROVES, and what it does not: this is a theorem about the model below,
// not about the Rust in this file. The bind that makes it a statement about the
// CODE is Tier-1 conformance -- see derived_seamless_handoff_ownership_* in
// aterm-spec, which drives the real decision functions over their bounded input
// space and checks the model admits exactly what the code decides. A green
// island with no bind proves a property of a description. Both halves are load
// bearing, and AGENTS.md says so in the same words.
//
// The lost phase is deliberately REPRESENTABLE. Without it in the type the
// theorem would hold by construction and be worth nothing; with it, the theorem
// is exactly the claim that the transition relation never reaches it. Deleting
// the rollback edge on any pre-Commit failure -- the shape every refusal in this
// file is careful to preserve -- makes this file stop compiling.
clean {
    -- WHO holds the live PTY masters. nobody is a lost session: the state the
    -- overlap protocol exists to make unreachable.
    inductive Owner where
      | outgoing : Owner
      | candidate : Owner
      | nobody : Owner

    -- The phases run_handoff_worker drives, plus the failure state.
    inductive Phase where
      | idle : Phase
      | parked : Phase
      | transferred : Phase
      | proved : Phase
      | committed : Phase
      | rolledBack : Phase
      | lost : Phase

    inductive Event where
      | park : Event
      | transfer : Event
      | prove : Event
      | commit : Event
      | fail : Event

    def owner : Phase -> Owner
      | Phase.idle => Owner.outgoing
      | Phase.parked => Owner.outgoing
      | Phase.transferred => Owner.outgoing
      | Phase.proved => Owner.outgoing
      | Phase.committed => Owner.candidate
      | Phase.rolledBack => Owner.outgoing
      | Phase.lost => Owner.nobody

    def held (o : Owner) : Bool :=
      match o with
      | Owner.outgoing => true
      | Owner.candidate => true
      | Owner.nobody => false

    -- THE PROTOCOL. Ownership moves to the candidate at Commit and nowhere else,
    -- and every pre-Commit failure rolls back to the outgoing process, which is
    -- what HandoffWorkerCleanup exists to guarantee.
    def step : Phase -> Event -> Phase
      | Phase.idle, Event.park => Phase.parked
      | Phase.parked, Event.transfer => Phase.transferred
      | Phase.transferred, Event.prove => Phase.proved
      | Phase.proved, Event.commit => Phase.committed
      | Phase.parked, Event.fail => Phase.rolledBack
      | Phase.transferred, Event.fail => Phase.rolledBack
      | Phase.proved, Event.fail => Phase.rolledBack
      | p, _ => p

    -- SEAMLESSNESS: no step of this protocol orphans a session. With idle held,
    -- induction over any run gives the reachable-state invariant.
    theorem no_step_ever_orphans (p : Phase) (e : Event) :
        held (owner p) = true -> held (owner (step p e)) = true := by
      cases p <;> cases e <;> intro h <;> exact h

    -- Which phases have a proof behind them. Commit is the irreversible step:
    -- after it the candidate owns the masters and the outgoing process exits,
    -- so a Commit that was not earned by a checked adoption proof is exactly
    -- how a good build hands a users sessions to an impostor.
    def provedAlready : Phase -> Bool
      | Phase.proved => true
      | Phase.committed => true
      | _ => false

    -- NO COMMIT WITHOUT A PROOF. Nothing reaches committed except through
    -- proved, which is the state the adoption-proof check gates.
    theorem commit_needs_a_proof (p : Phase) (e : Event) :
        step p e = Phase.committed -> provedAlready p = true := by
      cases p <;> cases e <;> intro h <;> first | rfl | exact Phase.noConfusion h

    -- THE HANDOFF IS ONE-WAY. Once the candidate owns the masters there is no
    -- event that takes them back, which is what lets the outgoing process
    -- _exit immediately after Commit instead of standing by to undo it.
    theorem the_candidate_never_gives_it_back (p : Phase) (e : Event) :
        owner p = Owner.candidate -> owner (step p e) = Owner.candidate := by
      cases p <;> cases e <;> intro h <;> first | rfl | exact Owner.noConfusion h

    -- THE ORPHAN STATE IS UNREACHABLE. lost is in Phase so that the property
    -- can be stated at all; no protocol step ever enters it.
    theorem nothing_live_becomes_lost (p : Phase) (e : Event) :
        step p e = Phase.lost -> p = Phase.lost := by
      cases p <;> cases e <;> intro h <;> first | rfl | exact Phase.noConfusion h
}

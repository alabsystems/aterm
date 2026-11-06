// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The own-inbox move across this Mac's key rotation (one-mesh §11 C3b): a move the kernel opens
// resumes past every record the old node's bridge delivered, proved for every offset on every
// Trust build. The spec kernel is what inbox_move.rs's contract! kernel states it computes; the
// law asks the kernel as a parameter, and `inbox_move_binding_island.rs` instantiates it with the
// compiled Rust. `inbox_move::tests` recomputes sections 3's and 4's answer sheets from the Rust
// and reads their words from this file.
clean {
    -- The move's scalar kernel. The contract! kernel of
    -- crates/aterm-mesh-core/src/inbox_move.rs states `ensures result ==
    -- spec_move_opens(..) by move_opens_is_exact`, and the binding island proves its
    -- compiled body equal to the definition here. It sits outside the namespace so a
    -- clause can name it.

    -- spec_move_opens: the move opens only where no bridge of the old node may still
    -- deliver and the commit its last bridge handed over is made.
    def spec_move_opens (mayDeliver owedUnmade : Bool) : Bool := !mayDeliver && !owedUnmade

    -- spec_shows_anew: the endpoint shows a delivery only where it remembers neither its
    -- offset nor the offset a moved copy was first moved from.
    def spec_shows_anew (offKnown wasKnown : Bool) : Bool := !offKnown && !wasKnown

    namespace AtermMeshInboxMove

    -- ===========================================================================
    -- 1. The law, for any kernel exact to the spec.
    -- ===========================================================================

    -- The old node's inbox group on this Mac's log: its last bridge delivered every
    -- record through d; the group's commit stands at c when the move asks the kernel,
    -- and the move resumes one past it; the bridge handed over o as it went. The
    -- premises: a bridge that may deliver no more went, having committed or handed
    -- over what it delivered (hGone); the hand-over is made once the group's commit
    -- reaches it (hMade). A bridge killed before it committed, or one whose commit a
    -- record it could not deliver held below one it delivered, breaks hGone: such a
    -- record moves, and section 4's endpoint answers its copy from its memory of where
    -- it was first (aterm-link's SPECS.md, mesh-mail-one-hop).

    def Exact (opens : Bool → Bool → Bool) : Prop := ∀ a b, opens a b = spec_move_opens a b

    theorem spec_exact : Exact spec_move_opens := fun _ _ => rfl

    theorem opens_only_once_gone_and_made (opens : Bool → Bool → Bool) (hK : Exact opens)
        (m u : Bool) (h : opens m u = true) : m = false ∧ u = false := by
      rw [hK m u] at h; revert h; cases m <;> cases u <;> decide

    -- Plain-variable arithmetic (Clean's omega reads no structure projection).
    theorem le_through (d o c : Nat) (h1 : d ≤ o) (h2 : o ≤ c) : d ≤ c := by omega

    theorem move_resumes_past_every_delivery (opens : Bool → Bool → Bool) (hK : Exact opens)
        (mayDeliver owedUnmade : Bool) (d c o : Nat)
        (hGone : mayDeliver = false → d ≤ c ∨ d ≤ o)
        (hMade : owedUnmade = false → o ≤ c)
        (hOpen : opens mayDeliver owedUnmade = true) : d ≤ c := by
      have hBoth := opens_only_once_gone_and_made opens hK mayDeliver owedUnmade hOpen
      have hoc : o ≤ c := hMade hBoth.2
      rcases hGone hBoth.1 with h | h
      · exact h
      · exact le_through d o c h hoc

    -- ===========================================================================
    -- 2. Each input is needed: a kernel that drops one opens a move that resumes
    --    below a delivered record, the premises holding.
    -- ===========================================================================

    -- `breaks opens m u d c o`: section 1's premises hold (hGone, hMade, in Bool), the
    -- kernel opens, and the move resumes at or below a record the bridge delivered.
    def breaks (opens : Bool → Bool → Bool) (m u : Bool) (d c o : Nat) : Bool :=
      (m || (decide (d ≤ c) || decide (d ≤ o))) && (u || decide (o ≤ c)) && opens m u
        && !decide (d ≤ c)

    -- Without the gate: the old node's bridge still runs (mayDeliver), having
    -- delivered through 5 with its group at 3 and nothing handed over yet.
    def ungated : Bool → Bool → Bool := fun _ u => !u
    theorem ungated_moves_a_delivered_record : breaks ungated true false 5 3 0 = true := by rfl

    -- Without the hand-over: the bridge went, having delivered through 5 and handed
    -- over 5, and the group stands at 3, the hand-over unmade.
    def unhanded : Bool → Bool → Bool := fun m _ => !m
    theorem unhanded_moves_a_delivered_record : breaks unhanded false true 5 3 5 = true := by rfl

    -- The kernel opens neither.
    theorem the_kernel_opens_neither :
        breaks spec_move_opens true false 5 3 0 = false
          ∧ breaks spec_move_opens false true 5 3 5 = false := ⟨rfl, rfl⟩

    -- ===========================================================================
    -- 3. The answer sheet `inbox_move::tests` recomputes from the Rust.
    -- ===========================================================================

    def bit (k i : Nat) : Bool := (k / 2 ^ i) % 2 == 1
    def yes (v : Bool) : Nat := if v = true then 1 else 0

    -- The kernel over (mayDeliver, owedUnmade) as bits 1..0 of 0..3.
    def sheet : List Nat := (List.range 4).map (fun k => yes (spec_move_opens (bit k 1) (bit k 0)))

    theorem sheet_is : sheet = [1, 0, 0, 0] := by rfl

    -- ===========================================================================
    -- 4. The endpoint: each piece of mail is shown once across the move.
    -- ===========================================================================

    -- A delivery reaches the endpoint at an offset; a copy the move made also names where
    -- it was first (the offset the first move took, or the earliest its bridge read back
    -- to). offKnown: the endpoint remembers the delivery's offset; wasKnown: it remembers
    -- the offset the copy names (false for a record no move made); shownBefore: this mail
    -- was shown before. The premises: the endpoint remembers each piece of mail it showed
    -- by every offset a delivery of it named, so the next delivery of it names one it
    -- remembers (hRemember), and remembers no offset of mail it did not show (hFresh). The
    -- one memory outside hFresh, a broadcast refused over quota, is remembered unshown on
    -- purpose and answered unshown, so a refusal counts once.

    def ShowExact (shows : Bool → Bool → Bool) : Prop :=
      ∀ a b, shows a b = spec_shows_anew a b

    theorem show_spec_exact : ShowExact spec_shows_anew := fun _ _ => rfl

    theorem each_mail_is_shown_once (shows : Bool → Bool → Bool) (hK : ShowExact shows)
        (offKnown wasKnown shownBefore : Bool)
        (hRemember : shownBefore = true → (offKnown || wasKnown) = true)
        (hFresh : (offKnown || wasKnown) = true → shownBefore = true) :
        shows offKnown wasKnown = !shownBefore := by
      rw [hK offKnown wasKnown]; revert hRemember hFresh
      cases offKnown <;> cases wasKnown <;> cases shownBefore <;> decide

    -- `showBreaks shows o w s`: the premises hold and the kernel shows the mail where it
    -- was shown before, or hides it where it was not.
    def showBreaks (shows : Bool → Bool → Bool) (o w s : Bool) : Bool :=
      (!s || (o || w)) && (!(o || w) || s) && !(shows o w == !s)

    -- Without the origin: a copy of a record shown before the move, at its new offset.
    def offsetOnly : Bool → Bool → Bool := fun o _ => !o
    theorem offset_only_shows_a_moved_copy_again :
        showBreaks offsetOnly false true true = true := by rfl

    -- Without the offset: a plain repeat of a record shown before.
    def wasOnly : Bool → Bool → Bool := fun _ w => !w
    theorem was_only_shows_a_repeat_again : showBreaks wasOnly true false true = true := by rfl

    -- The kernel shows neither.
    theorem the_kernel_shows_neither_again :
        showBreaks spec_shows_anew false true true = false
          ∧ showBreaks spec_shows_anew true false true = false := ⟨rfl, rfl⟩

    -- The endpoint's answer sheet, over (offKnown, wasKnown) as bits 1..0 of 0..3.
    def showSheet : List Nat :=
      (List.range 4).map (fun k => yes (spec_shows_anew (bit k 1) (bit k 0)))

    theorem showSheet_is : showSheet = [1, 0, 0, 0] := by rfl

    end AtermMeshInboxMove
}

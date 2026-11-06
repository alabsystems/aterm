// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P2, gap heal: the heal, the carry and the removal guard of `gap_heal.rs`, and their laws,
// proved for every size by the kernel on every Trust build. The spec kernels are what
// gap_heal.rs's contract! kernels state they compute. P1's decisions (who is current, who is
// revoked, the gate, the re-key due) are asked through P1's Kernels with an Exact premise, never
// redefined; P2's own through GKernels with a GExact premise; `gap_heal_binding_island.rs`
// instantiates both with the compiled Rust. `gap_heal::tests` recomputes section 11's answer
// sheet from the Rust and reads its words from this file.
clean {
    -- P2's scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/gap_heal.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body equal to the definition here. They sit outside the namespace
    -- so a clause can name them.

    -- spec_heals_omission: a current seat a newest epoch omits is a gap of it,
    -- whatever its minter knew, unless the epoch wraps a revoked recipient and a
    -- current seat already holds every newest epoch of its audience (a carrier,
    -- whose re-key wraps every current seat). With no carrier, the omission heals
    -- even where the epoch is due: that heal is how a carrier comes to be.
    def spec_heals_omission (newest wrapsRevoked carried : Bool) : Bool :=
      newest && (!wrapsRevoked || !carried)
    -- spec_gap: a seat audience's epoch this Mac opens lacks a Wrap to a current
    -- seat, and the epoch heals omissions or its minter could not know the seat
    -- (the seat's entry is not in the epoch's causal past).
    def spec_gap (opened current wrapped known healsOmission : Bool) : Bool :=
      opened && (current && (!wrapped && (healsOmission || !known)))
    -- spec_denied: one deny, the union of this Mac's own roster and the opened one,
    -- read by the heal, the carry, the removal's mint and the admission alike.
    def spec_denied (localRoster openedRoster : Bool) : Bool := localRoster || openedRoster
    -- spec_heal_signs: the closer signs a gap's Wrap unless its seat is denied.
    def spec_heal_signs (gap denied : Bool) : Bool := gap && !denied
    -- spec_carried: a mint wraps a recipient that is current, not one the batch
    -- removes, and not denied.
    def spec_carried (current excluded denied : Bool) : Bool := current && (!excluded && !denied)
    -- spec_strands: a removal strands a newest epoch of a seat audience when no
    -- current seat holds it after the removal and the remover's batch does not
    -- mint the audience's next epoch (it does not open the newest).
    def spec_strands (holderLeft removerMints : Bool) : Bool := !holderLeft && !removerMints

    namespace AtermMeshGapHeal

    open AtermMeshTemporal
    open AtermMeshRevocation (Table Kernels Exact anyN allN anyN_intro anyN_elim anyN_false
      allN_elim allN_intro allN_false_intro allN_false_elim anyN_congr allN_congr anyN_mono allN_mono
      dtrue dfalse lt_pred_of_ne lt_zero_false lt_succ_of_lt' ne_of_lt')

    -- ===========================================================================
    -- 1. P2's kernels as parameters. Every decision below reads P1's decision
    --    through K (AtermMeshRevocation.Kernels, `Exact K`) and P2's own through
    --    G (`GExact G`); the binding island instantiates both with the compiled
    --    Rust. Nothing here redefines revocation.
    -- ===========================================================================

    structure GKernels where
      healsOmission : Bool → Bool → Bool → Bool
      gap : Bool → Bool → Bool → Bool → Bool → Bool
      denied : Bool → Bool → Bool
      healSigns : Bool → Bool → Bool
      carried : Bool → Bool → Bool → Bool
      strands : Bool → Bool → Bool

    def GExact (G : GKernels) : Prop :=
      (∀ a b c, G.healsOmission a b c = spec_heals_omission a b c)
        ∧ (∀ a b c d e, G.gap a b c d e = spec_gap a b c d e)
        ∧ (∀ a b, G.denied a b = spec_denied a b)
        ∧ (∀ a b, G.healSigns a b = spec_heal_signs a b)
        ∧ (∀ a b c, G.carried a b c = spec_carried a b c)
        ∧ (∀ a b, G.strands a b = spec_strands a b)

    def specG : GKernels :=
      GKernels.mk spec_heals_omission spec_gap spec_denied spec_heal_signs spec_carried spec_strands

    theorem specG_exact : GExact specG :=
      ⟨fun _ _ _ => rfl, fun _ _ _ _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl, fun _ _ _ => rfl,
       fun _ _ => rfl⟩

    -- ===========================================================================
    -- 2. The World: the membership as P1's Table, and the epochs and Wraps a view
    --    holds. Recipients are the ids below nr (every row's recipient is one);
    --    epochs are 0..ne-1, each of an audience (0 [everyone], 1 Commons, 2 + k a
    --    host list) and with its own record, which the Table's causal order reads.
    -- ===========================================================================

    structure World where
      t    : Table
      nr   : Nat
      ne   : Nat
      aud  : Nat → Nat            -- the epoch's audience
      eid  : Nat → Nat            -- the epoch's record
      sup  : Nat → Bool           -- a later epoch of its audience has it in its past
      wr   : Nat → Nat → Bool     -- a Wrap of epoch e to recipient r is held
      denL : Nat → Bool           -- this Mac's own roster denies recipient r
      denO : Nat → Bool           -- the opened roster denies recipient r

    -- A seat audience: [everyone] or Commons (SeatAudience). A host list names
    -- its Macs, so nothing here heals or strands one.
    def seatAud (a : Nat) : Bool := decide (a < 2)

    def newest (w : World) (e : Nat) : Bool := decide (e < w.ne) && !(w.sup e)
    def isHead (w : World) (a e : Nat) : Bool := newest w e && decide (w.aud e = a)

    def den (G : GKernels) (w : World) (r : Nat) : Bool := G.denied (w.denL r) (w.denO r)

    -- P1's answers, asked through K.
    def cur (K : Kernels) (w : World) (r : Nat) : Bool := AtermMeshRevocation.currentW K w.t r
    def rev (K : Kernels) (w : World) (r : Nat) : Bool := AtermMeshRevocation.revokedW K w.t r
    -- Seat row i is current: it is not out and its recipient is not revoked
    -- (seat_is_current, through P1's `current` kernel on the one row).
    def seatCur (K : Kernels) (w : World) (i : Nat) : Bool :=
      K.current (!(AtermMeshRevocation.isOutW K w.t i)) (rev K w (w.t.rrcp i))
    def gateOf (K : Kernels) (w : World) (e : Nat) : Bool := AtermMeshRevocation.gateW K w.t (w.eid e)

    -- Epoch e wraps a revoked recipient (P1's wraps_removed kernel, per recipient).
    def wrapsRev (K : Kernels) (w : World) (e : Nat) : Bool :=
      anyN w.nr (fun r => K.wrapsRemoved (rev K w r) (w.wr e r))
    -- Recipient r holds every newest epoch of audience a.
    def holdsAll (w : World) (a r : Nat) : Bool := allN w.ne (fun e => !(isHead w a e) || w.wr e r)
    -- Some current seat no roster denies holds every newest epoch of a: the due's
    -- carrier, which may mint a's next epoch (a denied Mac never mints).
    def carriedA (K : Kernels) (G : GKernels) (w : World) (a : Nat) : Bool :=
      anyN w.nr (fun r => cur K w r && (!(den G w r) && holdsAll w a r))
    -- A current seat holds epoch e.
    def holderOf (K : Kernels) (w : World) (e : Nat) : Bool := anyN w.nr (fun r => cur K w r && w.wr e r)

    def healsOm (K : Kernels) (G : GKernels) (w : World) (e : Nat) : Bool :=
      G.healsOmission (newest w e) (wrapsRev K w e) (carriedA K G w (w.aud e))
    -- The minter could know seat row i: its entry is in the epoch's causal past.
    def known (w : World) (e i : Nat) : Bool := w.t.before (w.t.rid i) (w.eid e) || decide (w.t.rid i = w.eid e)

    -- Log::wrap_gaps' verdict on (epoch e, seat row i) for the Mac whose recipient
    -- is m: a seat audience's epoch it opens.
    def gapW (K : Kernels) (G : GKernels) (w : World) (m e i : Nat) : Bool :=
      seatAud (w.aud e) && (decide (e < w.ne) &&
        G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))
    -- The closer's verdict (wrap_gaps_pass, gap_denied).
    def healW (K : Kernels) (G : GKernels) (w : World) (m e i : Nat) : Bool :=
      G.healSigns (gapW K G w m e i) (den G w (w.t.rrcp i))
    -- The carry of a mint (carried_audience_recipients' seat-audience branch).
    def carryW (K : Kernels) (G : GKernels) (w : World) (excl : Nat → Bool) (r : Nat) : Bool :=
      G.carried (cur K w r) (excl r) (den G w r)
    -- This Mac opens audience a: a has a newest epoch, and m holds every one
    -- (opens_newest).
    def opensNewest (w : World) (m a : Nat) : Bool := anyN w.ne (fun e => isHead w a e) && holdsAll w a m

    -- The re-key due of audience a (rekey_dues): a newest epoch is due by P1's
    -- one rule (head_due: its gate is shut, or it wraps a revoked recipient), or
    -- two newest epochs wrap different sets (not twins: a coalesce).
    def twinsA (w : World) (a : Nat) : Bool :=
      allN w.ne (fun e1 => allN w.ne (fun e2 =>
        !(isHead w a e1 && isHead w a e2) || allN w.nr (fun r => w.wr e1 r == w.wr e2 r)))
    def dueA (K : Kernels) (w : World) (a : Nat) : Bool :=
      anyN w.ne (fun e => isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e)) || !(twinsA w a)

    -- ===========================================================================
    -- 3. Steps.
    -- ===========================================================================

    def addWrap (w : World) (e r : Nat) : World :=
      { w with wr := fun e' r' => (decide (e' = e) && decide (r' = r)) || w.wr e' r' }

    -- A new epoch of audience a (record x) that has every epoch of a in its past,
    -- wrapping exactly the recipients rs.
    def mintEpoch (w : World) (a x : Nat) (rs : Nat → Bool) : World :=
      { w with ne := Nat.succ w.ne,
               aud := fun e => if e = w.ne then a else w.aud e,
               eid := fun e => if e = w.ne then x else w.eid e,
               sup := fun e => if e = w.ne then false else (w.sup e || decide (w.aud e = a)),
               wr := fun e r => if e = w.ne then rs r else w.wr e r }

    def withTable (w : World) (t : Table) : World := { w with t := t }

    -- The Mac m (its recipient) heals gap (e, i).
    def HealBy (K : Kernels) (G : GKernels) (m : Nat) (w w' : World) : Prop :=
      ∃ e i, i < w.t.rows ∧ healW K G w m e i = true ∧ w' = addWrap w e (w.t.rrcp i)

    -- Mac m mints seat audience a (record x), wrapping the carry less excl: m opens
    -- the audience, and its record has every record of the view in its past.
    def MintByA (K : Kernels) (G : GKernels) (m a : Nat) (excl : Nat → Bool) (w w' : World) : Prop :=
      ∃ x, seatAud a = true ∧ opensNewest w m a = true
        ∧ (∀ j, j < w.t.rms → w.t.before (w.t.mid j) x = true)
        ∧ (∀ i, i < w.t.rows → w.t.before (w.t.rid i) x = true)
        ∧ w' = mintEpoch w a x (carryW K G w excl)
    def MintBy (K : Kernels) (G : GKernels) (m : Nat) (excl : Nat → Bool) (w w' : World) : Prop :=
      ∃ a, MintByA K G m a excl w w'

    -- The removal batch of Mac m: the Remove of actor a (record x; q a quorum's),
    -- then a new epoch of each seat audience m opens (records y0, y1), each
    -- wrapped to the carry after the Remove, less excl.
    def afterRemove (w : World) (a x : Nat) (q : Bool) : World :=
      withTable w (AtermMeshRevocation.addRemove w.t a x q)
    def batch (K : Kernels) (G : GKernels) (w : World) (m a x : Nat) (q : Bool) (y0 y1 : Nat)
        (excl : Nat → Bool) : World :=
      if opensNewest w m 1 = true then
        mintEpoch (if opensNewest w m 0 = true then mintEpoch (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl) else afterRemove w a x q) 1 y1
          (carryW K G (afterRemove w a x q) excl)
      else (if opensNewest w m 0 = true then mintEpoch (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl) else afterRemove w a x q)

    -- remove_member's guard: the removal strands some newest epoch of a seat
    -- audience (no current seat holds it after the Remove, and m's batch does not
    -- re-key the audience).
    def strandsAny (K : Kernels) (G : GKernels) (w : World) (m a x : Nat) (q : Bool) : Bool :=
      anyN 2 (fun a' => anyN w.ne (fun e =>
        isHead w a' e && G.strands (holderOf K (afterRemove w a x q) e) (opensNewest w m a')))

    -- An honest desk admits actor b with recipient r (entry record z): it refuses
    -- a recipient its view revokes (P1's law 2), the actor is fresh (no Remove
    -- names it), and the entry has every Remove of the view in its past.
    def AdmitStep (K : Kernels) (w w' : World) : Prop :=
      ∃ (b r z : Nat), r < w.nr ∧ rev K w r = false
        ∧ (∀ j, j < w.t.rms → Not (w.t.mactor j = b))
        ∧ (∀ j, j < w.t.rms → w.t.before (w.t.mid j) z = true)
        ∧ w' = withTable w (AtermMeshRevocation.addRow w.t b r z)

    def RosterStep (w w' : World) : Prop := ∃ (dl dop : Nat → Bool), w' = { w with denL := dl, denO := dop }

    -- The lab: every step is one Mac's decision on the view the lab shares (the
    -- relay's delivery, PF5b, is what makes views meet). A healer and a minter
    -- are current; a minter is in its own carry.
    def HealStep (K : Kernels) (G : GKernels) (w w' : World) : Prop :=
      ∃ m, cur K w m = true ∧ HealBy K G m w w'
    def MintStepA (K : Kernels) (G : GKernels) (a : Nat) (w w' : World) : Prop :=
      ∃ m, m < w.nr ∧ cur K w m = true ∧ den G w m = false ∧ MintByA K G m a (fun _ => false) w w'
    def MintStep (K : Kernels) (G : GKernels) (w w' : World) : Prop := ∃ a, MintStepA K G a w w'
    def RemoveStep (K : Kernels) (G : GKernels) (guarded : Bool) (w w' : World) : Prop :=
      ∃ (m a x : Nat) (q : Bool) (y0 y1 : Nat) (excl : Nat → Bool), m < w.nr ∧ cur K (afterRemove w a x q) m = true ∧ den G w m = false
        ∧ excl m = false
        ∧ (guarded = false ∨ strandsAny K G w m a x q = false)
        ∧ (∀ j, j < Nat.succ w.t.rms → (afterRemove w a x q).t.before ((afterRemove w a x q).t.mid j) y0 = true)
        ∧ (∀ j, j < Nat.succ w.t.rms → (afterRemove w a x q).t.before ((afterRemove w a x q).t.mid j) y1 = true)
        ∧ w' = batch K G w m a x q y0 y1 excl

    def LabNext (K : Kernels) (G : GKernels) (guarded : Bool) (w w' : World) : Prop :=
      HealStep K G w w' ∨ MintStep K G w w' ∨ RemoveStep K G guarded w w' ∨ AdmitStep K w w'
        ∨ RosterStep w w'

    -- Every recipient a row binds is below nr.
    def RcpBound (w : World) : Prop := ∀ i, i < w.t.rows → w.t.rrcp i < w.nr

    -- No lockout's base: every newest epoch of a seat audience has a current holder.
    def HolderExists (K : Kernels) (w : World) : Prop :=
      ∀ a e, seatAud a = true → isHead w a e = true → holderOf K w e = true

    def LabInit (K : Kernels) (w : World) : Prop := RcpBound w ∧ HolderExists K w

    def LabSpec (K : Kernels) (G : GKernels) (guarded : Bool) : StateMachine World :=
      StateMachine.mk (LabInit K) (LabNext K G guarded)

    -- Audience a has converged: no re-key is due there, and every newest epoch of
    -- it wraps every current seat that is not denied.
    def Converged (K : Kernels) (G : GKernels) (w : World) (a : Nat) : Prop :=
      dueA K w a = false ∧
        ∀ e r, isHead w a e = true → r < w.nr → cur K w r = true → den G w r = false → w.wr e r = true

    -- One heal pass of the Mac m (wrap_gaps_pass): every gap it finds in the one
    -- view, signed at once.
    def passW (K : Kernels) (G : GKernels) (w : World) (m : Nat) : World :=
      { w with wr := fun e r => w.wr e r
          || anyN w.t.rows (fun i => decide (w.t.rrcp i = r) && healW K G w m e i) }

    -- ===========================================================================
    -- 5. Liveness premises, over a behavior b of the lab.
    -- ===========================================================================

    -- The membership is stable from n0: every step is a heal, a mint or a stutter
    -- (no Remove, Admit or roster change; one-mesh §9's ◇□ quiet membership).
    def StableFrom (K : Kernels) (G : GKernels) (b : Behavior World) (n0 : Nat) : Prop :=
      ∀ n, n0 ≤ n → HealStep K G (b n) (b (Nat.succ n)) ∨ MintStep K G (b n) (b (Nat.succ n))
        ∨ b (Nat.succ n) = b n
    -- Weak fairness of the heal: a missing Wrap (e, r) that some current holder can
    -- heal at every step from n on is eventually held.
    def HealFair (K : Kernels) (G : GKernels) (b : Behavior World) : Prop :=
      ∀ n e r, (∀ k, n ≤ k → ∃ m i, i < (b k).t.rows ∧ (b k).t.rrcp i = r ∧ cur K (b k) m = true
          ∧ healW K G (b k) m e i = true)
        → ∃ k, n ≤ k ∧ (b k).wr e r = true
    -- Weak fairness of the re-key: an audience due a re-key at every step from n on,
    -- with a carrier at every step, is eventually minted.
    def MintFair (K : Kernels) (G : GKernels) (b : Behavior World) (a : Nat) : Prop :=
      ∀ n, (∀ k, n ≤ k → dueA K (b k) a = true ∧ carriedA K G (b k) a = true)
        → ∃ k, n ≤ k ∧ MintStepA K G a (b k) (b (Nat.succ k))
    -- An honest desk's Admit of recipient r (AdmitStep, naming r).
    def AdmitsR (K : Kernels) (w w' : World) (r : Nat) : Prop :=
      ∃ (b z : Nat), r < w.nr ∧ rev K w r = false
        ∧ (∀ j, j < w.t.rms → Not (w.t.mactor j = b))
        ∧ (∀ j, j < w.t.rms → w.t.before (w.t.mid j) z = true)
        ∧ w' = withTable w (AtermMeshRevocation.addRow w.t b r z)

    -- ===========================================================================
    -- 4. Shared facts: the spec kernels of both properties, and the decisions read
    --    through exact kernels.
    -- ===========================================================================

    -- P1's spec kernels, as P1's Kernels (P1's spec_exact).
    def specK : Kernels :=
      Kernels.mk spec_takes spec_precedes spec_restores spec_row_revokes spec_gate_row spec_current
        spec_wraps_removed spec_head_due

    theorem specK_exact : Exact specK := AtermMeshRevocation.spec_exact

    theorem cur_eq (K : Kernels) (hK : Exact K) (w : World) (r : Nat) :
        cur K w r = AtermMeshRevocation.current w.t r :=
      AtermMeshRevocation.currentW_eq K hK w.t r
    theorem rev_eq (K : Kernels) (hK : Exact K) (w : World) (r : Nat) :
        rev K w r = AtermMeshRevocation.revoked w.t r :=
      AtermMeshRevocation.revokedW_eq K hK w.t r
    theorem gateOf_eq (K : Kernels) (hK : Exact K) (w : World) (e : Nat) :
        gateOf K w e = AtermMeshRevocation.gate w.t (w.eid e) :=
      AtermMeshRevocation.gateW_eq K hK w.t (w.eid e)

    -- A current seat row's recipient is current, and not revoked.
    theorem seatCur_cur (K : Kernels) (hK : Exact K) (w : World) (i : Nat) (hi : i < w.t.rows)
        (h : seatCur K w i = true) : cur K w (w.t.rrcp i) = true ∧ rev K w (w.t.rrcp i) = false := by
      have hc : K.current (!(AtermMeshRevocation.isOutW K w.t i)) (rev K w (w.t.rrcp i)) = true := h
      rw [hK.2.2.2.2.2.1] at hc
      have hb := Eq.mp (Bool.and_eq_true _ _) hc
      have hout : AtermMeshRevocation.isOutW K w.t i = false := by
        cases ho : AtermMeshRevocation.isOutW K w.t i with
        | false => rfl
        | true => rw [ho] at hb; exact Bool.noConfusion hb.1
      have hrev : rev K w (w.t.rrcp i) = false := by
        cases hr : rev K w (w.t.rrcp i) with
        | false => rfl
        | true => rw [hr] at hb; exact Bool.noConfusion hb.2
      have hbl : AtermMeshRevocation.boundLiveW K w.t (w.t.rrcp i) = true :=
        anyN_intro _ w.t.rows i hi (by
          show (decide (w.t.rrcp i = w.t.rrcp i) && !(AtermMeshRevocation.isOutW K w.t i)) = true
          rw [hout, dtrue rfl])
      refine ⟨?_, hrev⟩
      show K.current (AtermMeshRevocation.boundLiveW K w.t (w.t.rrcp i)) (AtermMeshRevocation.revokedW K w.t (w.t.rrcp i)) = true
      have hrv : AtermMeshRevocation.revokedW K w.t (w.t.rrcp i) = false := hrev
      rw [hbl, hrv]; exact hK.2.2.2.2.2.1 true false

    -- ===========================================================================
    -- 6. Counterexamples (rfl): each rule PF2 replaces, on a small view where it
    --    fails and PF2's rule holds.
    -- ===========================================================================

    -- One seat (actor 0, recipient 0), no Remove.
    def oneSeat : Table :=
      { rows := 1, ractor := fun _ => 0, rrcp := fun _ => 0, rid := fun _ => 0, rms := 0,
        mactor := fun _ => 0, mid := fun _ => 0, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }
    -- The seat is denied by this Mac's own roster only.
    def localDeny : World :=
      { t := oneSeat, nr := 1, ne := 0, aud := fun _ => 0, eid := fun _ => 0, sup := fun _ => false,
        wr := fun _ _ => false, denL := fun _ => true, denO := fun _ => false }
    -- The pre-PF2 carry (carried_audience_recipients, rotate_after_removal) read the
    -- opened roster only, while the closer read both: it carried a seat the heal
    -- refused.
    def carryOpenedOnly (K : Kernels) (w : World) (excl : Nat → Bool) (r : Nat) : Bool :=
      spec_carried (cur K w r) (excl r) (w.denO r)
    theorem opened_only_carry_wraps_a_local_deny :
        carryOpenedOnly specK localDeny (fun _ => false) 0 = true
          ∧ carryW specK specG localDeny (fun _ => false) 0 = false := ⟨rfl, rfl⟩

    -- Three seats (actors and recipients 0, 1, 2, entries record 0); a Remove of
    -- actor 2 at record 10 revokes recipient 2. Two concurrent newest [everyone]
    -- epochs (records 5 and 6): epoch 0 wraps {0, 2}, epoch 1 wraps {1, 2}.
    def threeSeats : Table :=
      { rows := 3, ractor := fun i => i, rrcp := fun i => i, rid := fun _ => 0, rms := 1,
        mactor := fun _ => 2, mid := fun _ => 10, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) && !(decide (a = 5) && decide (b = 6))
          && !(decide (a = 6) && decide (b = 5)) }
    def twoHeads : World :=
      { t := threeSeats, nr := 3, ne := 2, aud := fun _ => 0, eid := fun e => 5 + e,
        sup := fun _ => false,
        wr := fun e r => (decide (e = 0) && (decide (r = 0) || decide (r = 2)))
          || (decide (e = 1) && (decide (r = 1) || decide (r = 2))),
        denL := fun _ => false, denO := fun _ => false }
    -- The rule as shipped before PF2: an omission heals in a newest epoch only when
    -- it wraps no revoked recipient.
    def gapShipped (K : Kernels) (w : World) (m e i : Nat) : Bool :=
      seatAud (w.aud e) && (decide (e < w.ne) &&
        spec_gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i)
          (newest w e && !(wrapsRev K w e)))
    -- Both heads are due (each wraps revoked 2) and no seat holds both, so no Mac may
    -- re-key [everyone]; the shipped rule finds no gap anywhere (every seat each head
    -- omits was known to its minter), so nothing heals and the due never clears:
    -- a lockout. PF2's rule heals seat 1 into epoch 0 (by Mac 0) and seat 0 into
    -- epoch 1 (by Mac 1), after which either holder carries the re-key.
    theorem shipped_rule_locks_out :
        dueA specK twoHeads 0 = true ∧ carriedA specK specG twoHeads 0 = false
          ∧ allN 3 (fun m => allN 2 (fun e => allN 3 (fun i => !(gapShipped specK twoHeads m e i)))) = true
          ∧ healW specK specG twoHeads 0 0 1 = true ∧ healW specK specG twoHeads 1 1 0 = true := by
      decide

    -- The rule of 0.154.0 (OLD_GAP_RULE): no omission the minter knew is a gap.
    def gapOld (K : Kernels) (w : World) (m e i : Nat) : Bool :=
      seatAud (w.aud e) && (decide (e < w.ne) &&
        (w.wr e m && (seatCur K w i && (!(w.wr e (w.t.rrcp i)) && !(known w e i)))))
    -- One newest [everyone] epoch (record 5) that wraps 0 only; seats 0 and 1, both
    -- known to its minter. The old rule reads the omission of seat 1 as a revocation.
    def twoSeats : Table :=
      { rows := 2, ractor := fun i => i, rrcp := fun i => i, rid := fun _ => 0, rms := 0,
        mactor := fun _ => 0, mid := fun _ => 0, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }
    def omitOne : World :=
      { t := twoSeats, nr := 2, ne := 1, aud := fun _ => 0, eid := fun _ => 5, sup := fun _ => false,
        wr := fun _ r => decide (r = 0), denL := fun _ => false, denO := fun _ => false }
    theorem old_rule_reads_omission_as_revocation :
        gapOld specK omitOne 0 0 1 = false ∧ healW specK specG omitOne 0 0 1 = true := by decide

    -- The last holder (last_holder_strands): one Commons epoch (record 2) wrapped
    -- only to seat 1; Mac 0, which cannot open it, removes actor 1 (record 5).
    def strandW : World :=
      { t := twoSeats, nr := 2, ne := 1, aud := fun _ => 1, eid := fun _ => 2, sup := fun _ => false,
        wr := fun _ r => decide (r = 1), denL := fun _ => false, denO := fun _ => false }
    def strandAfter : World := afterRemove strandW 1 5 false
    -- The guard refuses it. Unguarded, the batch mints nothing (Mac 0 opens neither
    -- audience), and the epoch is left with no current holder, due (it wraps the
    -- revoked seat) and with no carrier, and no current Mac opens it: nothing can
    -- heal it or re-key it.
    theorem last_holder_strands :
        strandsAny specK specG strandW 0 1 5 false = true
          ∧ batch specK specG strandW 0 1 5 false 6 7 (fun _ => false) = strandAfter
          ∧ isHead strandAfter 1 0 = true ∧ holderOf specK strandAfter 0 = false
          ∧ dueA specK strandAfter 1 = true ∧ carriedA specK specG strandAfter 1 = false
          ∧ allN 2 (fun m => !(cur specK strandAfter m) || !(strandAfter.wr 0 m)) = true := by
      refine ⟨by decide, rfl, by decide, by decide, by decide, by decide, by decide⟩

    -- Only a Remove revokes (the frame): a heal, a mint and a pass write Wraps and
    -- Epochs only, so every P1 decision reads the same Table after them.
    theorem wraps_never_revoke (K : Kernels) (G : GKernels) (w : World) (e r m a x : Nat)
        (rs : Nat → Bool) (dl dop : Nat → Bool) :
        (addWrap w e r).t = w.t ∧ (mintEpoch w a x rs).t = w.t ∧ (passW K G w m).t = w.t
          ∧ ({ w with denL := dl, denO := dop } : World).t = w.t := ⟨rfl, rfl, rfl, rfl⟩
    -- ===========================================================================
    -- 7. Safety of the heal and the carry (all sizes).
    -- ===========================================================================

    -- A negation that holds is of a false Bool.
    theorem ga_of_not (b : Bool) (h : (!b) = true) : b = false := by
      cases b with
      | false => rfl
      | true => exact Bool.noConfusion h

    -- A disjunction keeps holding when its left side is strengthened.
    theorem ga_or_left (a b c : Bool) (hab : a = true → b = true) (h : (a || c) = true) :
        (b || c) = true := by
      cases a with
      | true => rw [hab rfl]
      | false =>
        have hc : c = true := h
        rw [hc]
        exact Bool.or_true b

    -- The deny is the union of the two rosters.
    theorem ga_den_eq (G : GKernels) (hG : GExact G) (w : World) (r : Nat) :
        den G w r = (w.denL r || w.denO r) := hG.2.2.1 (w.denL r) (w.denO r)

    -- A recipient the deny clears is cleared by both rosters.
    theorem ga_den_false (G : GKernels) (hG : GExact G) (w : World) (r : Nat) (h : den G w r = false) :
        w.denL r = false ∧ w.denO r = false := by
      rw [ga_den_eq G hG w r] at h
      cases hl : w.denL r with
      | true =>
        rw [hl] at h
        exact Bool.noConfusion h
      | false =>
        cases ho : w.denO r with
        | true =>
          rw [hl, ho] at h
          exact Bool.noConfusion h
        | false => exact ⟨rfl, rfl⟩

    -- A gap verdict, field by field.
    theorem ga_gapW_parts (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (h : gapW K G w m e i = true) :
        seatAud (w.aud e) = true ∧ e < w.ne ∧ w.wr e m = true ∧ seatCur K w i = true
          ∧ w.wr e (w.t.rrcp i) = false ∧ (healsOm K G w e || !(known w e i)) = true := by
      have h1 : (seatAud (w.aud e) && (decide (e < w.ne) &&
          spec_gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) = true := by
        have hx : (seatAud (w.aud e) && (decide (e < w.ne) &&
          G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) = true := h
        rw [hG.2.1] at hx
        exact hx
      have hb1 := Eq.mp (Bool.and_eq_true _ _) h1
      have hb2 := Eq.mp (Bool.and_eq_true _ _) hb1.2
      have hb3 := Eq.mp (Bool.and_eq_true _ _) hb2.2
      have hb4 := Eq.mp (Bool.and_eq_true _ _) hb3.2
      have hb5 := Eq.mp (Bool.and_eq_true _ _) hb4.2
      exact ⟨hb1.1, of_decide_eq_true hb2.1, hb3.1, hb4.1, ga_of_not _ hb5.1, hb5.2⟩

    -- A gap verdict from its fields.
    theorem ga_gapW_intro (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (ha : seatAud (w.aud e) = true) (he : e < w.ne) (hm : w.wr e m = true)
        (hc : seatCur K w i = true) (hom : w.wr e (w.t.rrcp i) = false)
        (hh : (healsOm K G w e || !(known w e i)) = true) : gapW K G w m e i = true := by
      show (seatAud (w.aud e) && (decide (e < w.ne) &&
        G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) = true
      rw [hG.2.1, ha, dtrue he, hm, hc, hom]
      exact hh

    -- A heal verdict is a gap the deny clears.
    theorem ga_healW_parts (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (h : healW K G w m e i = true) :
        gapW K G w m e i = true ∧ den G w (w.t.rrcp i) = false := by
      have h1 : (gapW K G w m e i && !(den G w (w.t.rrcp i))) = true :=
        Eq.trans (hG.2.2.2.1 (gapW K G w m e i) (den G w (w.t.rrcp i))).symm h
      have hb := Eq.mp (Bool.and_eq_true _ _) h1
      exact ⟨hb.1, ga_of_not _ hb.2⟩

    -- A gap the deny clears is healed.
    theorem ga_healW_intro (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (hg : gapW K G w m e i = true) (hd : den G w (w.t.rrcp i) = false) :
        healW K G w m e i = true := by
      show G.healSigns (gapW K G w m e i) (den G w (w.t.rrcp i)) = true
      rw [hG.2.2.2.1, hg, hd]
      rfl

    -- A heal is exact: an epoch of a seat audience the closer opens, a current seat
    -- row whose recipient P1 does not revoke, that neither roster denies, and that the
    -- epoch does not wrap.
    theorem heal_safe (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (hi : i < w.t.rows) (h : healW K G w m e i = true) :
        seatAud (w.aud e) = true ∧ e < w.ne ∧ w.wr e m = true
          ∧ AtermMeshRevocation.current w.t (w.t.rrcp i) = true
          ∧ AtermMeshRevocation.revoked w.t (w.t.rrcp i) = false
          ∧ w.denL (w.t.rrcp i) = false ∧ w.denO (w.t.rrcp i) = false
          ∧ w.wr e (w.t.rrcp i) = false := by
      have hp := ga_healW_parts K G hG w m e i h
      have hg := ga_gapW_parts K G hG w m e i hp.1
      have hcr := seatCur_cur K hK w i hi hg.2.2.2.1
      have hd := ga_den_false G hG w (w.t.rrcp i) hp.2
      exact ⟨hg.1, hg.2.1, hg.2.2.1, Eq.trans (cur_eq K hK w (w.t.rrcp i)).symm hcr.1,
        Eq.trans (rev_eq K hK w (w.t.rrcp i)).symm hcr.2, hd.1, hd.2, hg.2.2.2.2.1⟩

    -- An audience of at least 2 is not below 2.
    theorem ga_not_lt_two (a : Nat) (h : 2 ≤ a) : Not (a < 2) := fun hlt => by omega

    -- A heal never targets a host list (audience 2 + k), which names its Macs.
    theorem heal_never_hosts (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (ha : 2 ≤ w.aud e) : healW K G w m e i = false := by
      have hn : Not (w.aud e < 2) := ga_not_lt_two (w.aud e) ha
      have hs : seatAud (w.aud e) = false := dfalse hn
      have hg : gapW K G w m e i = false := by
        show (seatAud (w.aud e) && (decide (e < w.ne) &&
          G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) = false
        rw [hs]
      show G.healSigns (gapW K G w m e i) (den G w (w.t.rrcp i)) = false
      rw [hG.2.2.2.1, hg]
      rfl

    -- The carry is exactly the current recipients that neither roster denies and the
    -- batch does not remove.
    theorem carry_is_current (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (excl : Nat → Bool) (r : Nat) :
        carryW K G w excl r = (AtermMeshRevocation.current w.t r
          && (!(excl r) && (!(w.denL r) && !(w.denO r)))) := by
      show G.carried (cur K w r) (excl r) (den G w r) = (AtermMeshRevocation.current w.t r
          && (!(excl r) && (!(w.denL r) && !(w.denO r))))
      rw [hG.2.2.2.2.1, ga_den_eq G hG w r, cur_eq K hK w r]
      cases w.denL r <;> rfl

    -- One deny, the union of both rosters, for the heal and the carry alike: a seat
    -- either roster denies is never carried and never healed.
    theorem denied_never_wrapped (K : Kernels) (G : GKernels) (hG : GExact G) (w : World)
        (excl : Nat → Bool) (m e i : Nat)
        (h : w.denL (w.t.rrcp i) = true ∨ w.denO (w.t.rrcp i) = true) :
        carryW K G w excl (w.t.rrcp i) = false ∧ healW K G w m e i = false := by
      have hd : den G w (w.t.rrcp i) = true := by
        rw [ga_den_eq G hG w]
        cases h with
        | inl hl =>
          rw [hl]
        | inr ho =>
          rw [ho]
          exact Bool.or_true _
      refine ⟨?_, ?_⟩
      · show G.carried (cur K w (w.t.rrcp i)) (excl (w.t.rrcp i)) (den G w (w.t.rrcp i)) = false
        rw [hG.2.2.2.2.1, hd]
        cases cur K w (w.t.rrcp i) <;> cases excl (w.t.rrcp i) <;> rfl
      · show G.healSigns (gapW K G w m e i) (den G w (w.t.rrcp i)) = false
        rw [hG.2.2.2.1, hd]
        cases gapW K G w m e i <;> rfl

    -- An omission is a gap: a current seat that a newest epoch of a seat audience
    -- omits is a gap of every Mac that opens the epoch, whatever its minter knew,
    -- unless the epoch wraps a revoked recipient and a carrier will re-key it.
    theorem omission_is_gap (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (ha : seatAud (w.aud e) = true) (hn : newest w e = true)
        (hm : w.wr e m = true) (hc : seatCur K w i = true) (hom : w.wr e (w.t.rrcp i) = false)
        (hnc : wrapsRev K w e = false ∨ carriedA K G w (w.aud e) = false) :
        gapW K G w m e i = true := by
      have hne : e < w.ne := of_decide_eq_true (Eq.mp (Bool.and_eq_true _ _) hn).1
      have hh0 : healsOm K G w e = (newest w e && (!(wrapsRev K w e) || !(carriedA K G w (w.aud e)))) :=
        hG.1 (newest w e) (wrapsRev K w e) (carriedA K G w (w.aud e))
      have hh : healsOm K G w e = true := by
        rw [hh0, hn]
        cases hnc with
        | inl h1 =>
          rw [h1]
        | inr h2 =>
          rw [h2]
          exact Bool.or_true _
      have hho : (healsOm K G w e || !(known w e i)) = true := by
        rw [hh]
      exact ga_gapW_intro K G hG w m e i ha hne hm hc hom hho

    -- ...and its closer signs the Wrap unless a roster denies the seat.
    theorem omission_heals (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (ha : seatAud (w.aud e) = true) (hn : newest w e = true)
        (hm : w.wr e m = true) (hc : seatCur K w i = true) (hom : w.wr e (w.t.rrcp i) = false)
        (hnc : wrapsRev K w e = false ∨ carriedA K G w (w.aud e) = false)
        (hd : den G w (w.t.rrcp i) = false) :
        healW K G w m e i = true :=
      ga_healW_intro K G hG w m e i (omission_is_gap K hK G hG w m e i ha hn hm hc hom hnc) hd

    -- The one exception: a newest epoch that wraps a revoked recipient, with a
    -- carrier, is re-keyed (the re-key wraps every current seat), not healed.
    theorem carried_due_is_rekeyed (K : Kernels) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (hrev : wrapsRev K w e = true) (hcar : carriedA K G w (w.aud e) = true)
        (hk : known w e i = true) : gapW K G w m e i = false := by
      have hh : healsOm K G w e = false := by
        show G.healsOmission (newest w e) (wrapsRev K w e) (carriedA K G w (w.aud e)) = false
        rw [hG.1, hrev, hcar]
        cases newest w e <;> rfl
      cases hg : gapW K G w m e i with
      | false => rfl
      | true =>
        have hx := (ga_gapW_parts K G hG w m e i hg).2.2.2.2.2
        rw [hh, hk] at hx
        exact Bool.noConfusion hx

    -- A seat the minter could not know (its entry is not in the epoch's past) is a gap
    -- of any epoch of a seat audience it lacks, newest or not.
    theorem unknown_is_gap (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (ha : seatAud (w.aud e) = true) (he : e < w.ne) (hm : w.wr e m = true)
        (hc : seatCur K w i = true) (hom : w.wr e (w.t.rrcp i) = false) (hk : known w e i = false) :
        gapW K G w m e i = true :=
      ga_gapW_intro K G hG w m e i ha he hm hc hom (by
        rw [hk]
        exact Bool.or_true _)

    -- One pass closes every gap: the pass wraps each gap it found...
    theorem heal_pass_wraps (K : Kernels) (G : GKernels) (w : World) (m e i : Nat)
        (hi : i < w.t.rows) (h : healW K G w m e i = true) :
        (passW K G w m).wr e (w.t.rrcp i) = true := by
      have hany : anyN w.t.rows (fun i' => decide (w.t.rrcp i' = w.t.rrcp i) && healW K G w m e i') = true :=
        anyN_intro (fun i' => decide (w.t.rrcp i' = w.t.rrcp i) && healW K G w m e i') w.t.rows i hi (by
          show (decide (w.t.rrcp i = w.t.rrcp i) && healW K G w m e i) = true
          rw [dtrue rfl, h])
      show (w.wr e (w.t.rrcp i)
        || anyN w.t.rows (fun i' => decide (w.t.rrcp i' = w.t.rrcp i) && healW K G w m e i')) = true
      rw [hany]
      exact Bool.or_true _

    -- The pass keeps every Wrap it found.
    theorem ga_pass_grows (K : Kernels) (G : GKernels) (w : World) (m e r : Nat) (h : w.wr e r = true) :
        (passW K G w m).wr e r = true := by
      show (w.wr e r || anyN w.t.rows (fun i' => decide (w.t.rrcp i' = r) && healW K G w m e i')) = true
      rw [h]

    -- A Wrap after the pass was held before it, or the pass healed a gap to it.
    theorem ga_pass_wr (K : Kernels) (G : GKernels) (w : World) (m e r : Nat)
        (h : (passW K G w m).wr e r = true) :
        w.wr e r = true ∨ ∃ i', i' < w.t.rows ∧ w.t.rrcp i' = r ∧ healW K G w m e i' = true := by
      have h1 : (w.wr e r || anyN w.t.rows (fun i' => decide (w.t.rrcp i' = r) && healW K G w m e i')) = true := h
      cases hw : w.wr e r with
      | true => exact Or.inl rfl
      | false =>
        rw [hw] at h1
        have h2 : anyN w.t.rows (fun i' => decide (w.t.rrcp i' = r) && healW K G w m e i') = true := h1
        exact Or.inr (Exists.elim (anyN_elim _ w.t.rows h2) (fun i' hi' =>
          have hb := Eq.mp (Bool.and_eq_true _ _) hi'.2
          ⟨i', hi'.1, of_decide_eq_true hb.1, hb.2⟩))

    -- A Wrap the pass lacks was lacking before it, and no heal of the pass targets it.
    theorem ga_pass_absent (K : Kernels) (G : GKernels) (w : World) (m e i : Nat) (hi : i < w.t.rows)
        (h : (passW K G w m).wr e (w.t.rrcp i) = false) :
        w.wr e (w.t.rrcp i) = false ∧ healW K G w m e i = false := by
      refine ⟨?_, ?_⟩
      · cases hw : w.wr e (w.t.rrcp i) with
        | false => rfl
        | true => exact Bool.noConfusion (Eq.trans (ga_pass_grows K G w m e (w.t.rrcp i) hw).symm h)
      · cases hh : healW K G w m e i with
        | false => rfl
        | true => exact Bool.noConfusion (Eq.trans (heal_pass_wraps K G w m e i hi hh).symm h)

    -- The pass keeps the closer's own Wrap as it was: it never heals the closer.
    theorem ga_pass_self (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e : Nat)
        (h : (passW K G w m).wr e m = true) : w.wr e m = true :=
      Or.elim (ga_pass_wr K G w m e m h) (fun h1 => h1) (fun hx => Exists.elim hx (fun i' hi' =>
        (ga_gapW_parts K G hG w m e i' (ga_healW_parts K G hG w m e i' hi'.2.2).1).2.2.1))

    -- The pass only adds to the revoked recipients an epoch wraps.
    theorem ga_pass_wrapsRev (K : Kernels) (hK : Exact K) (G : GKernels) (w : World) (m e : Nat)
        (h : wrapsRev K w e = true) : wrapsRev K (passW K G w m) e = true :=
      anyN_mono (fun r => K.wrapsRemoved (rev K w r) (w.wr e r))
        (fun r => K.wrapsRemoved (rev K w r) ((passW K G w m).wr e r)) w.nr
        (fun r _ hr => by
          have h1 : (rev K w r && w.wr e r) = true :=
            Eq.trans (hK.2.2.2.2.2.2.1 (rev K w r) (w.wr e r)).symm hr
          have hb := Eq.mp (Bool.and_eq_true _ _) h1
          have h2 : (passW K G w m).wr e r = true := ga_pass_grows K G w m e r hb.2
          have h3 : (rev K w r && (passW K G w m).wr e r) = true := by
            rw [hb.1, h2]
          exact Eq.trans (hK.2.2.2.2.2.2.1 (rev K w r) ((passW K G w m).wr e r)) h3) h

    -- The pass only adds to what a recipient holds of an audience's newest epochs.
    theorem ga_pass_holdsAll (K : Kernels) (G : GKernels) (w : World) (m a r : Nat)
        (h : holdsAll w a r = true) : holdsAll (passW K G w m) a r = true :=
      allN_mono (fun e => !(isHead w a e) || w.wr e r)
        (fun e => !(isHead w a e) || (passW K G w m).wr e r) w.ne
        (fun e _ he => by
          have h1 : (!(isHead w a e) || w.wr e r) = true := he
          show (!(isHead w a e) || (passW K G w m).wr e r) = true
          cases hh : isHead w a e with
          | false => rfl
          | true =>
            rw [hh] at h1
            rw [ga_pass_grows K G w m e r h1]) h

    -- The pass only adds carriers.
    theorem ga_pass_carried (K : Kernels) (G : GKernels) (w : World) (m a : Nat)
        (h : carriedA K G w a = true) : carriedA K G (passW K G w m) a = true :=
      anyN_mono (fun r => cur K w r && (!(den G w r) && holdsAll w a r))
        (fun r => cur K w r && (!(den G w r) && holdsAll (passW K G w m) a r)) w.nr
        (fun r _ hr => by
          have h1 : (cur K w r && (!(den G w r) && holdsAll w a r)) = true := hr
          have hb := Eq.mp (Bool.and_eq_true _ _) h1
          have hb2 := Eq.mp (Bool.and_eq_true _ _) hb.2
          show (cur K w r && (!(den G w r) && holdsAll (passW K G w m) a r)) = true
          rw [hb.1, hb2.1, ga_pass_holdsAll K G w m a r hb2.2]) h

    -- An omission the pass's view heals was one the view before it healed.
    theorem ga_pass_healsOm (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e : Nat) (h : healsOm K G (passW K G w m) e = true) : healsOm K G w e = true := by
      have h1 : (newest w e && (!(wrapsRev K (passW K G w m) e)
          || !(carriedA K G (passW K G w m) (w.aud e)))) = true :=
        Eq.trans (hG.1 (newest w e) (wrapsRev K (passW K G w m) e)
          (carriedA K G (passW K G w m) (w.aud e))).symm h
      have hb := Eq.mp (Bool.and_eq_true _ _) h1
      show G.healsOmission (newest w e) (wrapsRev K w e) (carriedA K G w (w.aud e)) = true
      rw [hG.1, hb.1]
      cases hw : wrapsRev K w e with
      | false => rfl
      | true =>
        cases hc : carriedA K G w (w.aud e) with
        | false => rfl
        | true =>
          have hb2 := hb.2
          rw [ga_pass_wrapsRev K hK G w m e hw, ga_pass_carried K G w m (w.aud e) hc] at hb2
          exact Bool.noConfusion hb2

    -- ...and leaves the closer no gap at all: a heal adds only current recipients, so
    -- no epoch comes to wrap a revoked one, and a carrier it makes only narrows the
    -- rule (heal_pass_closes, no lockout in one pass).
    theorem heal_pass_closes (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (hi : i < w.t.rows) :
        healW K G (passW K G w m) m e i = false := by
      cases hh : healW K G (passW K G w m) m e i with
      | false => rfl
      | true =>
        have hp := ga_healW_parts K G hG (passW K G w m) m e i hh
        have hg := ga_gapW_parts K G hG (passW K G w m) m e i hp.1
        have hab := ga_pass_absent K G w m e i hi hg.2.2.2.2.1
        have hm : w.wr e m = true := ga_pass_self K G hG w m e hg.2.2.1
        have hho : (healsOm K G w e || !(known w e i)) = true :=
          ga_or_left (healsOm K G (passW K G w m) e) (healsOm K G w e) (!(known w e i))
            (ga_pass_healsOm K hK G hG w m e) hg.2.2.2.2.2
        have hyes : healW K G w m e i = true :=
          ga_healW_intro K G hG w m e i
            (ga_gapW_intro K G hG w m e i hg.1 hg.2.1 hm hg.2.2.2.1 hab.1 hho) hp.2
        rw [hab.2] at hyes
        exact Bool.noConfusion hyes

    -- Every Wrap a lab step adds goes to a recipient that is current and that no
    -- roster denies after the step (the lab's safety, as an action law).
    def WrapsSafe (K : Kernels) (G : GKernels) (w w' : World) : Prop :=
      ∀ e r, w'.wr e r = true → w.wr e r = true ∨ (cur K w' r = true ∧ den G w' r = false)

    -- WrapsSafe carries over an equal world.
    theorem ga_safe_eq (K : Kernels) (G : GKernels) (w w' v : World) (he : w' = v)
        (h : WrapsSafe K G w v) : WrapsSafe K G w w' :=
      Eq.mpr (congrArg (WrapsSafe K G w) he) h

    -- The carry's recipients are current and not denied.
    theorem ga_carry_parts (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (excl : Nat → Bool)
        (r : Nat) (h : carryW K G w excl r = true) : cur K w r = true ∧ den G w r = false := by
      have h1 : (cur K w r && (!(excl r) && !(den G w r))) = true :=
        Eq.trans (hG.2.2.2.2.1 (cur K w r) (excl r) (den G w r)).symm h
      have hb := Eq.mp (Bool.and_eq_true _ _) h1
      have hb2 := Eq.mp (Bool.and_eq_true _ _) hb.2
      exact ⟨hb.1, ga_of_not _ hb2.2⟩

    -- A heal step adds one Wrap, to the healed seat, which is current and not denied.
    theorem ga_heal_step_safe (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (m e i : Nat) (hi : i < w.t.rows) (hh : healW K G w m e i = true) :
        WrapsSafe K G w (addWrap w e (w.t.rrcp i)) := by
      intro e' r' h
      have h1 : ((decide (e' = e) && decide (r' = w.t.rrcp i)) || w.wr e' r') = true := h
      cases hw : w.wr e' r' with
      | true => exact Or.inl rfl
      | false =>
        cases hr : decide (r' = w.t.rrcp i) with
        | false =>
          rw [hw, hr] at h1
          cases hde : decide (e' = e) with
          | false =>
            rw [hde] at h1
            exact Bool.noConfusion h1
          | true =>
            rw [hde] at h1
            exact Bool.noConfusion h1
        | true =>
          have hreq : r' = w.t.rrcp i := of_decide_eq_true hr
          have hp := ga_healW_parts K G hG w m e i hh
          have hcr := seatCur_cur K hK w i hi (ga_gapW_parts K G hG w m e i hp.1).2.2.2.1
          subst hreq
          exact Or.inr ⟨hcr.1, hp.2⟩

    -- The frame of a removal batch: the Table and rosters of A, and Wraps that are
    -- w's or the carry C.
    def ga_Frame (w A : World) (C : Nat → Bool) (v : World) : Prop :=
      v.t = A.t ∧ v.denL = A.denL ∧ v.denO = A.denO ∧ ∀ e r, v.wr e r = true → w.wr e r = true ∨ C r = true

    -- A new epoch's Wraps are the old ones, or the new epoch's recipients.
    theorem ga_mint_wr (w : World) (a x : Nat) (rs : Nat → Bool) (e r : Nat)
        (h : (mintEpoch w a x rs).wr e r = true) : w.wr e r = true ∨ rs r = true := by
      have h1 : (if e = w.ne then rs r else w.wr e r) = true := h
      by_cases he : e = w.ne
      · exact Or.inr (Eq.trans (if_pos he).symm h1)
      · exact Or.inl (Eq.trans (if_neg he).symm h1)

    -- A mint of the carry keeps the frame.
    theorem ga_frame_mint (w A : World) (C : Nat → Bool) (v : World) (a y : Nat)
        (hv : ga_Frame w A C v) : ga_Frame w A C (mintEpoch v a y C) :=
      ⟨hv.1, hv.2.1, hv.2.2.1, fun e r h => Or.elim (ga_mint_wr v a y C e r h)
        (fun h1 => hv.2.2.2 e r h1) (fun h2 => Or.inr h2)⟩

    -- Either branch keeping the frame keeps it.
    theorem ga_frame_ite (w A : World) (C : Nat → Bool) (c : Prop) [Decidable c] (X Y : World)
        (hX : ga_Frame w A C X) (hY : ga_Frame w A C Y) : ga_Frame w A C (if c then X else Y) := by
      by_cases hc : c
      · rw [if_pos hc]
        exact hX
      · rw [if_neg hc]
        exact hY

    -- A world in the frame of a carry of current, undenied recipients is safe.
    theorem ga_frame_safe (K : Kernels) (G : GKernels) (w A : World) (C : Nat → Bool) (v : World)
        (hv : ga_Frame w A C v) (hC : ∀ r, C r = true → cur K A r = true ∧ den G A r = false) :
        WrapsSafe K G w v := by
      intro e r h
      cases hv.2.2.2 e r h with
      | inl h1 => exact Or.inl h1
      | inr h2 =>
        have hc := hC r h2
        refine Or.inr ⟨?_, ?_⟩
        · show AtermMeshRevocation.currentW K v.t r = true
          rw [hv.1]
          exact hc.1
        · show G.denied (v.denL r) (v.denO r) = false
          rw [hv.2.1, hv.2.2.1]
          exact hc.2

    -- A removal batch keeps the frame of the Remove and its carry.
    theorem ga_frame_batch (K : Kernels) (G : GKernels) (w : World) (m a x : Nat) (q : Bool)
        (y0 y1 : Nat) (excl : Nat → Bool) :
        ga_Frame w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl)
          (batch K G w m a x q y0 y1 excl) :=
      have hA : ga_Frame w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl)
          (afterRemove w a x q) := ⟨rfl, rfl, rfl, fun _ _ h => Or.inl h⟩
      have hI := ga_frame_ite w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl)
        (opensNewest w m 0 = true) _ _
        (ga_frame_mint w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl)
          (afterRemove w a x q) 0 y0 hA) hA
      ga_frame_ite w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl)
        (opensNewest w m 1 = true) _ _
        (ga_frame_mint w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl) _ 1 y1 hI) hI

    -- Each lab step is WrapsSafe.
    theorem ga_next_safe (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (g : Bool)
        (w w' : World) (hn : LabNext K G g w w') : WrapsSafe K G w w' :=
      Or.elim hn
        (fun hh => Exists.elim hh (fun m hm => Exists.elim hm.2 (fun e he => Exists.elim he (fun i hi =>
          ga_safe_eq K G w w' _ hi.2.2 (ga_heal_step_safe K hK G hG w m e i hi.1 hi.2.1)))))
        (fun hx => Or.elim hx
          (fun hmint => Exists.elim hmint (fun a ha => Exists.elim ha (fun m hm =>
            Exists.elim hm.2.2.2 (fun x hx2 =>
              ga_safe_eq K G w w' _ hx2.2.2.2.2
                (ga_frame_safe K G w w (carryW K G w (fun _ => false)) _
                  (ga_frame_mint w w (carryW K G w (fun _ => false)) w a x
                    ⟨rfl, rfl, rfl, fun _ _ h => Or.inl h⟩)
                  (fun r hr => ga_carry_parts K G hG w (fun _ => false) r hr))))))
          (fun hy => Or.elim hy
            (fun hrm => Exists.elim hrm (fun m h1 => Exists.elim h1 (fun a h2 => Exists.elim h2 (fun x h3 =>
              Exists.elim h3 (fun q h4 => Exists.elim h4 (fun y0 h5 => Exists.elim h5 (fun y1 h6 =>
                Exists.elim h6 (fun excl h7 =>
                  ga_safe_eq K G w w' _ h7.2.2.2.2.2.2.2
                    (ga_frame_safe K G w (afterRemove w a x q) (carryW K G (afterRemove w a x q) excl) _
                      (ga_frame_batch K G w m a x q y0 y1 excl)
                      (fun r hr => ga_carry_parts K G hG (afterRemove w a x q) excl r hr))))))))))
            (fun hz => Or.elim hz
              (fun had => Exists.elim had (fun b h1 => Exists.elim h1 (fun r h2 => Exists.elim h2 (fun z h3 =>
                ga_safe_eq K G w w' _ h3.2.2.2.2 (fun _ _ h => Or.inl h)))))
              (fun hro => Exists.elim hro (fun dl h1 => Exists.elim h1 (fun dop h2 =>
                ga_safe_eq K G w w' _ h2 (fun _ _ h => Or.inl h)))))))

    theorem lab_wraps_safe (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (g : Bool) :
        Satisfies (LabSpec K G g) (Always (Act (WrapsSafe K G))) :=
      actionSatisfies (LabSpec K G g) (WrapsSafe K G)
        (fun s t hn => ga_next_safe K hK G hG g s t hn) (fun _ _ _ h => Or.inl h)

    -- ===========================================================================
    -- 8. Holders: only a Remove revokes, and the guard keeps a holder of every
    --    newest epoch (all sizes).
    -- ===========================================================================

    -- A negation that is true negates false.
    theorem gb_not_true (b : Bool) (h : (!b) = true) : b = false := by
      cases b with
      | false => rfl
      | true => exact Bool.noConfusion h

    -- A false disjunction has both sides false.
    theorem gb_or_false (a b : Bool) (h : (a || b) = false) : a = false ∧ b = false := by
      cases a with
      | true => exact Bool.noConfusion h
      | false =>
        cases b with
        | true => exact Bool.noConfusion h
        | false => exact ⟨rfl, rfl⟩

    -- A conjunction with false on the right is false.
    theorem gb_and_false (a : Bool) : (a && false) = false := by
      cases a <;> rfl

    -- A false fold is false at every index below its bound.
    theorem gb_anyN_false_at (p : Nat → Bool) (n i : Nat) (h : anyN n p = false) (hi : i < n) :
        p i = false := by
      cases hp : p i with
      | false => rfl
      | true => exact Bool.noConfusion (Eq.trans (anyN_intro p n i hi hp).symm h)

    -- A row that does not revoke still does not when a restorer may be added.
    theorem gb_srr_mono (o x y : Bool) (h : spec_row_revokes o y = false) :
        spec_row_revokes o (x || y) = false := by
      cases o with
      | false => rfl
      | true =>
        cases y with
        | false => exact Bool.noConfusion h
        | true => cases x <;> rfl

    -- The new row of an Admit of a fresh actor is not out.
    theorem gb_new_row_not_out (t : Table) (b r z : Nat)
        (hfresh : ∀ j, j < t.rms → Not (t.mactor j = b)) :
        AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) t.rows = false := by
      apply anyN_false
      intro j hj
      have hrb : (AtermMeshRevocation.addRow t b r z).ractor t.rows = b := if_pos rfl
      show (t.mlive j && (decide (t.mactor j = (AtermMeshRevocation.addRow t b r z).ractor t.rows)
            && (t.mquorum j || !(t.before (t.mid j) ((AtermMeshRevocation.addRow t b r z).rid t.rows))))) = false
      rw [hrb, dfalse (hfresh j hj)]
      cases t.mlive j <;> rfl

    -- The new row of an Admit of a fresh actor revokes nothing.
    theorem gb_new_row_revokes (t : Table) (b r z : Nat)
        (hfresh : ∀ j, j < t.rms → Not (t.mactor j = b)) :
        AtermMeshRevocation.revokes (AtermMeshRevocation.addRow t b r z) t.rows = false := by
      show spec_row_revokes (AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) t.rows)
          (AtermMeshRevocation.restored (AtermMeshRevocation.addRow t b r z) t.rows) = false
      rw [gb_new_row_not_out t b r z hfresh]
      rfl

    -- Adding a row keeps every old row that is not out binding its recipient.
    theorem gb_addRow_boundLive (t : Table) (b r z r' : Nat)
        (h : AtermMeshRevocation.boundLive t r' = true) :
        AtermMeshRevocation.boundLive (AtermMeshRevocation.addRow t b r z) r' = true :=
      Exists.elim (anyN_elim _ t.rows h) (fun i hi =>
        anyN_intro (fun m => decide ((AtermMeshRevocation.addRow t b r z).rrcp m = r')
            && !(AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) m))
          (Nat.succ t.rows) i (lt_succ_of_lt' i t.rows hi.1) (by
          have hrc : (AtermMeshRevocation.addRow t b r z).rrcp i = t.rrcp i :=
            if_neg (ne_of_lt' i t.rows hi.1)
          show (decide ((AtermMeshRevocation.addRow t b r z).rrcp i = r')
              && !(AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) i)) = true
          rw [hrc, AtermMeshRevocation.addRow_isOut t b r z i hi.1]
          exact hi.2))

    -- Adding a row never makes an old row revoke.
    theorem gb_addRow_revokes_false (t : Table) (b r z i : Nat) (hi : i < t.rows)
        (h : AtermMeshRevocation.revokes t i = false) :
        AtermMeshRevocation.revokes (AtermMeshRevocation.addRow t b r z) i = false := by
      rw [AtermMeshRevocation.addRow_revokes t b r z i hi]
      exact gb_srr_mono _ _ _ h

    -- Adding a row of a fresh actor never revokes a recipient that was not revoked.
    theorem gb_addRow_revoked_false (t : Table) (b r z r' : Nat)
        (hfresh : ∀ j, j < t.rms → Not (t.mactor j = b))
        (h : AtermMeshRevocation.revoked t r' = false) :
        AtermMeshRevocation.revoked (AtermMeshRevocation.addRow t b r z) r' = false := by
      show anyN (Nat.succ t.rows) (fun i => decide ((AtermMeshRevocation.addRow t b r z).rrcp i = r')
          && AtermMeshRevocation.revokes (AtermMeshRevocation.addRow t b r z) i) = false
      apply anyN_false
      intro i hi
      show (decide ((AtermMeshRevocation.addRow t b r z).rrcp i = r')
          && AtermMeshRevocation.revokes (AtermMeshRevocation.addRow t b r z) i) = false
      by_cases hin : i = t.rows
      · rw [hin, gb_new_row_revokes t b r z hfresh]
        exact gb_and_false _
      · have hlt : i < t.rows := lt_pred_of_ne i t.rows hi hin
        have hrc : (AtermMeshRevocation.addRow t b r z).rrcp i = t.rrcp i := if_neg hin
        rw [hrc]
        cases hd : decide (t.rrcp i = r') with
        | false => rfl
        | true =>
          have hx : (decide (t.rrcp i = r') && AtermMeshRevocation.revokes t i) = false :=
            gb_anyN_false_at _ t.rows i h hlt
          rw [hd] at hx
          have hold : AtermMeshRevocation.revokes t i = false := hx
          rw [gb_addRow_revokes_false t b r z i hlt hold]

    -- An Admit of a fresh actor (no Remove names it) keeps every current recipient
    -- current.
    theorem addRow_keeps_current (t : Table) (b r z r' : Nat)
        (hfresh : ∀ j, j < t.rms → Not (t.mactor j = b))
        (h : AtermMeshRevocation.current t r' = true) :
        AtermMeshRevocation.current (AtermMeshRevocation.addRow t b r z) r' = true := by
      have hb : AtermMeshRevocation.boundLive t r' = true ∧ (!(AtermMeshRevocation.revoked t r')) = true :=
        Eq.mp (Bool.and_eq_true _ _) h
      show spec_current (AtermMeshRevocation.boundLive (AtermMeshRevocation.addRow t b r z) r')
          (AtermMeshRevocation.revoked (AtermMeshRevocation.addRow t b r z) r') = true
      rw [gb_addRow_boundLive t b r z r' hb.1,
        gb_addRow_revoked_false t b r z r' hfresh (AtermMeshRevocation.current_not_revoked t r' h)]
      rfl

    -- An honest desk's Admit (a fresh actor, every Remove in the entry's past) makes
    -- its recipient current: law 3's first half.
    theorem admitted_is_current (t : Table) (b r z : Nat)
        (hfresh : ∀ j, j < t.rms → Not (t.mactor j = b))
        (hpast : ∀ j, j < t.rms → t.before (t.mid j) z = true) :
        AtermMeshRevocation.current (AtermMeshRevocation.addRow t b r z) r = true := by
      have hbl : AtermMeshRevocation.boundLive (AtermMeshRevocation.addRow t b r z) r = true :=
        anyN_intro (fun m => decide ((AtermMeshRevocation.addRow t b r z).rrcp m = r)
            && !(AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) m))
          (Nat.succ t.rows) t.rows (Nat.lt_succ_self t.rows) (by
          have hrc : (AtermMeshRevocation.addRow t b r z).rrcp t.rows = r := if_pos rfl
          show (decide ((AtermMeshRevocation.addRow t b r z).rrcp t.rows = r)
              && !(AtermMeshRevocation.isOut (AtermMeshRevocation.addRow t b r z) t.rows)) = true
          rw [hrc, gb_new_row_not_out t b r z hfresh, dtrue (Eq.refl r)])
      have hrv : AtermMeshRevocation.revoked (AtermMeshRevocation.addRow t b r z) r = false :=
        AtermMeshRevocation.admit_restores_its_recipient t b r z hpast (fun j hj _ => hfresh j hj)
      show spec_current (AtermMeshRevocation.boundLive (AtermMeshRevocation.addRow t b r z) r)
          (AtermMeshRevocation.revoked (AtermMeshRevocation.addRow t b r z) r) = true
      rw [hbl, hrv]
      rfl

    -- Two worlds with one Table answer cur alike.
    theorem gb_cur_table (K : Kernels) (w w' : World) (r : Nat) (h : w'.t = w.t) :
        cur K w' r = cur K w r := by
      show AtermMeshRevocation.currentW K w'.t r = AtermMeshRevocation.currentW K w.t r
      rw [h]

    -- A step that keeps the Table cannot make a current recipient non-current.
    theorem gb_same_table_contra (K : Kernels) (w w' : World) (r : Nat) (h : w'.t = w.t)
        (hc : cur K w r = true) (hc' : cur K w' r = false) : False := by
      rw [gb_cur_table K w w' r h, hc] at hc'
      exact Bool.noConfusion hc'

    -- Only a Remove revokes (step form; away is no revocation): a lab step that
    -- makes a current recipient non-current is a Remove. A heal, a mint, an
    -- omission, an Admit, a roster change or a Mac being away never does.
    theorem only_remove_revokes (K : Kernels) (hK : Exact K) (G : GKernels) (g : Bool)
        (w w' : World) (r : Nat) (hn : LabNext K G g w w') (hc : cur K w r = true)
        (hc' : cur K w' r = false) : RemoveStep K G g w w' := by
      cases hn with
      | inl hh =>
        rcases hh with ⟨m, _, e, i, _, _, hw⟩
        exact False.elim (gb_same_table_contra K w w' r (Eq.trans (congrArg World.t hw) rfl) hc hc')
      | inr hn2 =>
        cases hn2 with
        | inl hmt =>
          rcases hmt with ⟨a0, m, _, _, _, x, _, _, _, _, hw⟩
          exact False.elim (gb_same_table_contra K w w' r (Eq.trans (congrArg World.t hw) rfl) hc hc')
        | inr hn3 =>
          cases hn3 with
          | inl hrm => exact hrm
          | inr hn4 =>
            cases hn4 with
            | inl had =>
              rcases had with ⟨b, r0, z, _, _, hfr, _, hw⟩
              have h1 : AtermMeshRevocation.current w.t r = true := Eq.trans (cur_eq K hK w r).symm hc
              have h2 : cur K w' r = true := by
                rw [hw]
                exact Eq.trans (cur_eq K hK _ r) (addRow_keeps_current w.t b r0 z r hfr h1)
              rw [h2] at hc'
              exact Bool.noConfusion hc'
            | inr hro =>
              rcases hro with ⟨dl, dop, hw⟩
              exact False.elim (gb_same_table_contra K w w' r (Eq.trans (congrArg World.t hw) rfl) hc hc')

    -- A seat audience is [everyone] or Commons.
    theorem gb_seat_cases (a : Nat) (h : seatAud a = true) : a = 0 ∨ a = 1 := by
      have hlt : a < 2 := of_decide_eq_true h
      omega

    -- A newest epoch of audience a is an epoch of the view, not superseded, of a.
    theorem gb_isHead_parts (w : World) (a e : Nat) (h : isHead w a e = true) :
        e < w.ne ∧ w.sup e = false ∧ w.aud e = a := by
      have hb : newest w e = true ∧ decide (w.aud e = a) = true := Eq.mp (Bool.and_eq_true _ _) h
      have hn : decide (e < w.ne) = true ∧ (!(w.sup e)) = true := Eq.mp (Bool.and_eq_true _ _) hb.1
      exact ⟨of_decide_eq_true hn.1, gb_not_true _ hn.2, of_decide_eq_true hb.2⟩

    -- An epoch of the view, not superseded, of audience a is a newest epoch of a.
    theorem gb_head_intro (w : World) (a e : Nat) (hlt : e < w.ne) (hs : w.sup e = false)
        (ha : w.aud e = a) : isHead w a e = true := by
      show ((decide (e < w.ne) && !(w.sup e)) && decide (w.aud e = a)) = true
      rw [dtrue hlt, hs, dtrue ha]

    -- A newest epoch after a mint of a0 is the new epoch (of a0), or an old newest
    -- epoch of another audience.
    theorem gb_mint_head (V : World) (a0 x : Nat) (rs : Nat → Bool) (a e : Nat)
        (h : isHead (mintEpoch V a0 x rs) a e = true) :
        (e = V.ne ∧ a = a0) ∨ (e < V.ne ∧ isHead V a e = true ∧ Not (a = a0)) := by
      have hp := gb_isHead_parts (mintEpoch V a0 x rs) a e h
      by_cases hne : e = V.ne
      · have ha : (mintEpoch V a0 x rs).aud V.ne = a0 := if_pos rfl
        have hpa := hp.2.2
        rw [hne, ha] at hpa
        exact Or.inl ⟨hne, hpa.symm⟩
      · have hs : (mintEpoch V a0 x rs).sup e = (V.sup e || decide (V.aud e = a0)) := if_neg hne
        have ha : (mintEpoch V a0 x rs).aud e = V.aud e := if_neg hne
        have hlt : e < V.ne := lt_pred_of_ne e V.ne hp.1 hne
        have hps := hp.2.1
        rw [hs] at hps
        have hor := gb_or_false _ _ hps
        have hpa := hp.2.2
        rw [ha] at hpa
        refine Or.inr ⟨hlt, gb_head_intro V a e hlt hor.1 hpa, ?_⟩
        intro haa
        rw [haa] at hpa
        exact Bool.noConfusion (Eq.trans (dtrue hpa).symm hor.2)

    -- A mint keeps every old epoch's holders.
    theorem gb_mint_holderOf_old (K : Kernels) (V : World) (a0 x : Nat) (rs : Nat → Bool) (e : Nat)
        (he : e < V.ne) : holderOf K (mintEpoch V a0 x rs) e = holderOf K V e :=
      anyN_congr (fun r => cur K (mintEpoch V a0 x rs) r && (mintEpoch V a0 x rs).wr e r)
        (fun r => cur K V r && V.wr e r) V.nr (fun r _ => by
          have hw : (mintEpoch V a0 x rs).wr e r = V.wr e r := if_neg (ne_of_lt' e V.ne he)
          show (cur K V r && (mintEpoch V a0 x rs).wr e r) = (cur K V r && V.wr e r)
          rw [hw])

    -- A mint's new epoch is held by every current recipient it wraps.
    theorem gb_mint_holderOf_new (K : Kernels) (V : World) (a0 x : Nat) (rs : Nat → Bool) (r : Nat)
        (hr : r < V.nr) (hc : cur K V r = true) (hs : rs r = true) :
        holderOf K (mintEpoch V a0 x rs) V.ne = true :=
      anyN_intro (fun r' => cur K (mintEpoch V a0 x rs) r' && (mintEpoch V a0 x rs).wr V.ne r')
        V.nr r hr (by
        have hw : (mintEpoch V a0 x rs).wr V.ne r = rs r := if_pos rfl
        show (cur K V r && (mintEpoch V a0 x rs).wr V.ne r) = true
        rw [hw, hc, hs])

    -- A mint of a0 that wraps a current recipient leaves every newest epoch held,
    -- given the old newest epochs of the other audiences are.
    theorem gb_mint_holders (K : Kernels) (V : World) (a0 x : Nat) (rs : Nat → Bool) (P : Nat → Prop)
        (hV : ∀ a e, P a → Not (a = a0) → isHead V a e = true → holderOf K V e = true)
        (hr : ∃ r, r < V.nr ∧ cur K V r = true ∧ rs r = true) :
        ∀ a e, P a → isHead (mintEpoch V a0 x rs) a e = true → holderOf K (mintEpoch V a0 x rs) e = true := by
      intro a e hp hh
      cases gb_mint_head V a0 x rs a e hh with
      | inl hn =>
        rw [hn.1]
        exact Exists.elim hr (fun r hrr => gb_mint_holderOf_new K V a0 x rs r hrr.1 hrr.2.1 hrr.2.2)
      | inr ho =>
        rw [gb_mint_holderOf_old K V a0 x rs e ho.1]
        exact hV a e hp ho.2.2 ho.2.1

    -- The batch's optional mint: audience a0's next epoch when o, else nothing.
    def gb_maybeMint (o : Bool) (V : World) (a0 x : Nat) (rs : Nat → Bool) : World :=
      if o = true then mintEpoch V a0 x rs else V

    -- The removal batch is the Remove, then the optional mints of 0 and of 1.
    theorem gb_batch_eq (K : Kernels) (G : GKernels) (w : World) (m a x : Nat) (q : Bool) (y0 y1 : Nat)
        (excl : Nat → Bool) :
        batch K G w m a x q y0 y1 excl
          = gb_maybeMint (opensNewest w m 1)
              (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0 (carryW K G (afterRemove w a x q) excl))
              1 y1 (carryW K G (afterRemove w a x q) excl) := rfl

    -- An optional mint keeps the Table and nr.
    theorem gb_maybe_frame (o : Bool) (V : World) (a0 x : Nat) (rs : Nat → Bool) :
        (gb_maybeMint o V a0 x rs).t = V.t ∧ (gb_maybeMint o V a0 x rs).nr = V.nr := by
      cases o with
      | false => exact ⟨rfl, rfl⟩
      | true => exact ⟨rfl, rfl⟩

    -- An optional mint of a0 keeps every newest epoch held, given the old ones of
    -- the audiences it does not mint are, and its carry holds a current recipient.
    theorem gb_maybe_holders (K : Kernels) (o : Bool) (V : World) (a0 x : Nat) (rs : Nat → Bool)
        (P : Nat → Prop)
        (hV : ∀ a e, P a → (o = false ∨ Not (a = a0)) → isHead V a e = true → holderOf K V e = true)
        (hr : o = true → ∃ r, r < V.nr ∧ cur K V r = true ∧ rs r = true) :
        ∀ a e, P a → isHead (gb_maybeMint o V a0 x rs) a e = true
          → holderOf K (gb_maybeMint o V a0 x rs) e = true := by
      cases o with
      | false => exact fun a e hp hh => hV a e hp (Or.inl rfl) hh
      | true =>
        exact gb_mint_holders K V a0 x rs P (fun a e hp hne hh => hV a e hp (Or.inr hne) hh) (hr rfl)

    -- The guard: a newest epoch of a seat audience the remover does not open keeps a
    -- current holder after the Remove.
    theorem gb_guard (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m a x : Nat) (q : Bool)
        (hs : strandsAny K G w m a x q = false) (a' e : Nat) (ha : a' < 2) (hh : isHead w a' e = true)
        (ho : opensNewest w m a' = false) : holderOf K (afterRemove w a x q) e = true := by
      have h1 : anyN w.ne (fun e => isHead w a' e
          && G.strands (holderOf K (afterRemove w a x q) e) (opensNewest w m a')) = false :=
        gb_anyN_false_at _ 2 a' hs ha
      have h2 : (isHead w a' e && G.strands (holderOf K (afterRemove w a x q) e) (opensNewest w m a')) = false :=
        gb_anyN_false_at _ w.ne e h1 (gb_isHead_parts w a' e hh).1
      cases hk : holderOf K (afterRemove w a x q) e with
      | true => rfl
      | false =>
        have h3 : (isHead w a' e && G.strands (holderOf K (afterRemove w a x q) e) (opensNewest w m a'))
            = (true && G.strands false false) := by
          rw [hh, ho, hk]
        have h4 : G.strands false false = true := hG.2.2.2.2.2 false false
        exact Bool.noConfusion (Eq.trans (Eq.trans h4.symm h3.symm) h2)

    -- The carry holds a current recipient that is not excluded and not denied.
    theorem gb_carry_m (K : Kernels) (G : GKernels) (hG : GExact G) (W : World) (excl : Nat → Bool)
        (m : Nat) (hc : cur K W m = true) (hx : excl m = false) (hd : den G W m = false) :
        carryW K G W excl m = true := by
      show G.carried (cur K W m) (excl m) (den G W m) = true
      rw [hG.2.2.2.2.1, hc, hx, hd]
      rfl

    -- A seat audience the batch does not mint is one the remover does not open.
    theorem gb_open_false (w : World) (m a : Nat) (ha : seatAud a = true)
        (h0 : opensNewest w m 0 = false ∨ Not (a = 0)) (h1 : opensNewest w m 1 = false ∨ Not (a = 1)) :
        opensNewest w m a = false := by
      cases gb_seat_cases a ha with
      | inl ha0 =>
        rw [ha0]
        cases h0 with
        | inl h => exact h
        | inr h => exact False.elim (h ha0)
      | inr ha1 =>
        rw [ha1]
        cases h1 with
        | inl h => exact h
        | inr h => exact False.elim (h ha1)

    -- A guarded removal batch leaves every newest epoch of a seat audience held.
    theorem gb_kept_core (K : Kernels) (G : GKernels) (hG : GExact G) (w : World)
        (m a x : Nat) (q : Bool) (y0 y1 : Nat) (excl : Nat → Bool)
        (hm : m < w.nr) (hc : cur K (afterRemove w a x q) m = true) (hd : den G w m = false)
        (hx : excl m = false) (hs : strandsAny K G w m a x q = false) :
        HolderExists K (batch K G w m a x q y0 y1 excl) := by
      have hcar : carryW K G (afterRemove w a x q) excl m = true :=
        gb_carry_m K G hG (afterRemove w a x q) excl m hc hx hd
      have hW0 : ∀ a' e, (seatAud a' = true ∧ (opensNewest w m 1 = false ∨ Not (a' = 1)))
          → (opensNewest w m 0 = false ∨ Not (a' = 0)) → isHead (afterRemove w a x q) a' e = true
          → holderOf K (afterRemove w a x q) e = true :=
        fun a' e hp h0 hh => gb_guard K G hG w m a x q hs a' e (of_decide_eq_true hp.1) hh
          (gb_open_false w m a' hp.1 h0 hp.2)
      have hB0 := gb_maybe_holders K (opensNewest w m 0) (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl)
          (fun a' => seatAud a' = true ∧ (opensNewest w m 1 = false ∨ Not (a' = 1))) hW0
          (fun _ => ⟨m, hm, hc, hcar⟩)
      have hfr := gb_maybe_frame (opensNewest w m 0) (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl)
      have hm1 : m < (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl)).nr := by
        rw [hfr.2]
        exact hm
      have hc1 : cur K (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl)) m = true :=
        Eq.trans (gb_cur_table K (afterRemove w a x q) (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0
          (carryW K G (afterRemove w a x q) excl)) m hfr.1) hc
      have hall := gb_maybe_holders K (opensNewest w m 1)
          (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0 (carryW K G (afterRemove w a x q) excl))
          1 y1 (carryW K G (afterRemove w a x q) excl) (fun a' => seatAud a' = true)
          (fun a' e hp h1 hh => hB0 a' e ⟨hp, h1⟩ hh)
          (fun _ => ⟨m, hm1, hc1, hcar⟩)
      rw [gb_batch_eq K G w m a x q y0 y1 excl]
      exact fun a' e hp hh => hall a' e hp hh

    -- The guard keeps a holder: a guarded removal batch leaves every newest epoch of
    -- each seat audience with a current holder (the audiences the remover opens are
    -- re-keyed to a carry that holds the remover; the guard covers the others).
    theorem holders_kept (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w w' : World)
        (h : HolderExists K w) (hs : RemoveStep K G true w w') : HolderExists K w' :=
      Exists.elim hs (fun m h1 => Exists.elim h1 (fun a h2 => Exists.elim h2 (fun x h3 =>
        Exists.elim h3 (fun q h4 => Exists.elim h4 (fun y0 h5 => Exists.elim h5 (fun y1 h6 =>
          Exists.elim h6 (fun excl hp =>
            Or.elim hp.2.2.2.2.1 (fun hf => Bool.noConfusion hf)
              (fun hst => Eq.mpr (congrArg (HolderExists K) hp.2.2.2.2.2.2.2)
                (gb_kept_core K G hG w m a x q y0 y1 excl hp.1 hp.2.1 hp.2.2.1 hp.2.2.2.1 hst)))))))))

    -- A heal only adds Wraps, so every holder stays one.
    theorem gb_heal_keeps (K : Kernels) (w : World) (e0 r0 : Nat) (h : HolderExists K w) :
        HolderExists K (addWrap w e0 r0) :=
      fun a e ha hh => anyN_mono (fun r => cur K w r && w.wr e r)
        (fun r => cur K (addWrap w e0 r0) r && (addWrap w e0 r0).wr e r) w.nr (fun r _ hr => by
          have hb : cur K w r = true ∧ w.wr e r = true := Eq.mp (Bool.and_eq_true _ _) hr
          show (cur K w r && ((decide (e = e0) && decide (r = r0)) || w.wr e r)) = true
          rw [hb.1, hb.2]
          exact Bool.or_true _) (h a e ha hh)

    -- A lab mint (its minter current, not denied, in its own carry) keeps every
    -- newest epoch held.
    theorem gb_mint_keeps (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m a0 x : Nat)
        (hm : m < w.nr) (hc : cur K w m = true) (hd : den G w m = false) (h : HolderExists K w) :
        HolderExists K (mintEpoch w a0 x (carryW K G w (fun _ => false))) :=
      gb_mint_holders K w a0 x (carryW K G w (fun _ => false)) (fun a => seatAud a = true)
        (fun a e ha _ hh => h a e ha hh) ⟨m, hm, hc, gb_carry_m K G hG w (fun _ => false) m hc rfl hd⟩

    -- An Admit of a fresh actor keeps every holder current.
    theorem gb_admit_keeps (K : Kernels) (hK : Exact K) (w : World) (b r z : Nat)
        (hfresh : ∀ j, j < w.t.rms → Not (w.t.mactor j = b)) (h : HolderExists K w) :
        HolderExists K (withTable w (AtermMeshRevocation.addRow w.t b r z)) :=
      fun a e ha hh => anyN_mono (fun r' => cur K w r' && w.wr e r')
        (fun r' => cur K (withTable w (AtermMeshRevocation.addRow w.t b r z)) r' && w.wr e r') w.nr
        (fun r' _ hr => by
          have hb : cur K w r' = true ∧ w.wr e r' = true := Eq.mp (Bool.and_eq_true _ _) hr
          have h1 : AtermMeshRevocation.current w.t r' = true := Eq.trans (cur_eq K hK w r').symm hb.1
          have h2 : cur K (withTable w (AtermMeshRevocation.addRow w.t b r z)) r' = true :=
            Eq.trans (cur_eq K hK _ r') (addRow_keeps_current w.t b r z r' hfresh h1)
          show (cur K (withTable w (AtermMeshRevocation.addRow w.t b r z)) r' && w.wr e r') = true
          rw [h2, hb.2]) (h a e ha hh)

    -- A heal step keeps a current holder of every newest epoch.
    theorem gb_holder_heal (K : Kernels) (G : GKernels) (w w' : World) (h : HolderExists K w)
        (hs : HealStep K G w w') : HolderExists K w' :=
      Exists.elim hs (fun _ h1 => Exists.elim h1.2 (fun e0 h2 => Exists.elim h2 (fun i h3 =>
        Eq.mpr (congrArg (HolderExists K) h3.2.2) (gb_heal_keeps K w e0 (w.t.rrcp i) h))))

    -- A mint step keeps a current holder of every newest epoch.
    theorem gb_holder_mint (K : Kernels) (G : GKernels) (hG : GExact G) (w w' : World) (h : HolderExists K w)
        (hs : MintStep K G w w') : HolderExists K w' :=
      Exists.elim hs (fun a0 h1 => Exists.elim h1 (fun m h2 => Exists.elim h2.2.2.2 (fun x h3 =>
        Eq.mpr (congrArg (HolderExists K) h3.2.2.2.2)
          (gb_mint_keeps K G hG w m a0 x h2.1 h2.2.1 h2.2.2.1 h))))

    -- An Admit step keeps a current holder of every newest epoch.
    theorem gb_holder_admit (K : Kernels) (hK : Exact K) (w w' : World) (h : HolderExists K w)
        (hs : AdmitStep K w w') : HolderExists K w' :=
      Exists.elim hs (fun b h1 => Exists.elim h1 (fun r0 h2 => Exists.elim h2 (fun z h3 =>
        Eq.mpr (congrArg (HolderExists K) h3.2.2.2.2) (gb_admit_keeps K hK w b r0 z h3.2.2.1 h))))

    -- A roster step keeps every holder (holderOf reads no roster).
    theorem gb_holder_roster (K : Kernels) (w w' : World) (h : HolderExists K w)
        (hs : RosterStep w w') : HolderExists K w' :=
      Exists.elim hs (fun dl h1 => Exists.elim h1 (fun dop h2 =>
        Eq.mpr (congrArg (HolderExists K) h2) h))

    -- Every step of the guarded lab keeps a current holder of every newest epoch.
    theorem gb_holder_step (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w w' : World)
        (h : HolderExists K w) (hn : LabNext K G true w w') : HolderExists K w' :=
      Or.elim hn (fun hh => gb_holder_heal K G w w' h hh) (fun hn2 =>
        Or.elim hn2 (fun hmt => gb_holder_mint K G hG w w' h hmt) (fun hn3 =>
          Or.elim hn3 (fun hrm => holders_kept K hK G hG w w' h hrm) (fun hn4 =>
            Or.elim hn4 (fun had => gb_holder_admit K hK w w' h had)
              (fun hro => gb_holder_roster K w w' h hro))))

    -- Every behavior of the guarded lab keeps a current holder of every newest epoch
    -- of each seat audience: no lockout's base.
    theorem lab_holder_exists (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) :
        Satisfies (LabSpec K G true) (Always (Lift (HolderExists K))) :=
      invariantSatisfies (LabSpec K G true) (HolderExists K) (fun _ hs => hs.2)
        (fun s t hs hn => gb_holder_step K hK G hG s t hs hn)

    -- An optional mint keeps every row's recipient below nr.
    theorem gb_maybe_rcp (o : Bool) (V : World) (a0 x : Nat) (rs : Nat → Bool) (h : RcpBound V) :
        RcpBound (gb_maybeMint o V a0 x rs) := by
      cases o with
      | false => exact h
      | true => exact h

    -- An Admit of a recipient below nr keeps every row's recipient below nr.
    theorem gb_admit_rcp (w : World) (b r z : Nat) (hr : r < w.nr) (h : RcpBound w) :
        RcpBound (withTable w (AtermMeshRevocation.addRow w.t b r z)) := by
      intro i hi
      show (AtermMeshRevocation.addRow w.t b r z).rrcp i < w.nr
      by_cases hin : i = w.t.rows
      · have hrc : (AtermMeshRevocation.addRow w.t b r z).rrcp i = r := by
          rw [hin]
          exact if_pos rfl
        rw [hrc]
        exact hr
      · have hrc : (AtermMeshRevocation.addRow w.t b r z).rrcp i = w.t.rrcp i := if_neg hin
        rw [hrc]
        exact h i (lt_pred_of_ne i w.t.rows hi hin)

    -- A removal batch keeps every row's recipient below nr.
    theorem gb_rcp_remove (K : Kernels) (G : GKernels) (g : Bool) (w w' : World) (h : RcpBound w)
        (hs : RemoveStep K G g w w') : RcpBound w' :=
      Exists.elim hs (fun m h1 => Exists.elim h1 (fun a h2 => Exists.elim h2 (fun x h3 =>
        Exists.elim h3 (fun q h4 => Exists.elim h4 (fun y0 h5 => Exists.elim h5 (fun y1 h6 =>
          Exists.elim h6 (fun excl hp =>
            Eq.mpr (congrArg RcpBound (Eq.trans hp.2.2.2.2.2.2.2 (gb_batch_eq K G w m a x q y0 y1 excl)))
              (gb_maybe_rcp (opensNewest w m 1)
                (gb_maybeMint (opensNewest w m 0) (afterRemove w a x q) 0 y0 (carryW K G (afterRemove w a x q) excl))
                1 y1 (carryW K G (afterRemove w a x q) excl)
                (gb_maybe_rcp (opensNewest w m 0) (afterRemove w a x q) 0 y0
                  (carryW K G (afterRemove w a x q) excl) h)))))))))

    -- Every step of the lab keeps every row's recipient below nr.
    theorem gb_rcp_step (K : Kernels) (G : GKernels) (g : Bool) (w w' : World) (h : RcpBound w)
        (hn : LabNext K G g w w') : RcpBound w' :=
      Or.elim hn
        (fun hh => Exists.elim hh (fun _ h1 => Exists.elim h1.2 (fun _ h2 => Exists.elim h2 (fun _ h3 =>
          Eq.mpr (congrArg RcpBound h3.2.2) h))))
        (fun hn2 => Or.elim hn2
          (fun hmt => Exists.elim hmt (fun _ h1 => Exists.elim h1 (fun _ h2 => Exists.elim h2.2.2.2 (fun _ h3 =>
            Eq.mpr (congrArg RcpBound h3.2.2.2.2) h))))
          (fun hn3 => Or.elim hn3
            (fun hrm => gb_rcp_remove K G g w w' h hrm)
            (fun hn4 => Or.elim hn4
              (fun had => Exists.elim had (fun b h1 => Exists.elim h1 (fun r0 h2 => Exists.elim h2 (fun z h3 =>
                Eq.mpr (congrArg RcpBound h3.2.2.2.2) (gb_admit_rcp w b r0 z h3.1 h)))))
              (fun hro => Exists.elim hro (fun _ h1 => Exists.elim h1 (fun _ h2 =>
                Eq.mpr (congrArg RcpBound h2) h))))))

    -- Every recipient a row binds stays below nr (an Admit binds one below it).
    theorem lab_rcp_bound (K : Kernels) (G : GKernels) (g : Bool) :
        Satisfies (LabSpec K G g) (Always (Lift RcpBound)) :=
      invariantSatisfies (LabSpec K G g) RcpBound (fun _ hs => hs.1)
        (fun s t hs hn => gb_rcp_step K G g s t hs hn)
    -- ===========================================================================
    -- 9. Liveness: law 4 (epoch convergence, no lockout) for all sizes.
    -- ===========================================================================

    -- ---------------------------------------------------------------------------
    -- 9a. Bool and order helpers.
    -- ---------------------------------------------------------------------------

    -- A conjunction that holds splits into its halves.
    theorem gc_and (x y : Bool) (h : (x && y) = true) : x = true ∧ y = true :=
      Eq.mp (Bool.and_eq_true x y) h

    -- A disjunction that holds has a side that holds.
    theorem gc_or (x y : Bool) (h : (x || y) = true) : x = true ∨ y = true := by
      cases x with
      | true => exact Or.inl rfl
      | false => exact Or.inr h

    -- A disjunction holds when its right side does.
    theorem gc_or_r (x y : Bool) (h : y = true) : (x || y) = true := by
      cases x with
      | true => rfl
      | false => exact h

    -- A disjunction that fails fails on both sides.
    theorem gc_or_false (x y : Bool) (h : (x || y) = false) : x = false ∧ y = false := by
      cases x with
      | true => exact Bool.noConfusion h
      | false => exact ⟨rfl, h⟩

    -- A negation that holds names a false Bool.
    theorem gc_bnot (x : Bool) (h : (!x) = true) : x = false := by
      cases x with
      | false => rfl
      | true => exact Bool.noConfusion h

    -- A negation that fails names a true Bool.
    theorem gc_bnot_false (x : Bool) (h : (!x) = false) : x = true := by
      cases x with
      | true => rfl
      | false => exact Bool.noConfusion h

    -- Every Bool is == to itself.
    theorem gc_beq_self (x : Bool) : (x == x) = true := by cases x <;> rfl

    -- A strict bound carries along an equal bound.
    theorem gc_lt_tr (r n n' : Nat) (h : n = n') (hr : r < n) : r < n' := by
      rw [h] at hr
      exact hr

    -- Induction from n0.
    theorem gc_ind_from (P : Nat → Prop) (n0 : Nat) (h0 : P n0)
        (hs : ∀ k, n0 ≤ k → P k → P (Nat.succ k)) (k : Nat) (hk : n0 ≤ k) : P k := by
      induction hk with
      | refl => exact h0
      | step m hle ih => exact hs m hle ih

    -- Finitely many eventualities, each kept once reached, hold together at one time.
    theorem gc_collect (P : Nat → Nat → Prop) (n : Nat)
        (hmono : ∀ i k k', n ≤ k → k ≤ k' → P i k → P i k') :
        ∀ N, (∀ i, i < N → ∃ k, n ≤ k ∧ P i k) → ∃ k, n ≤ k ∧ ∀ i, i < N → P i k := by
      intro N
      induction N with
      | zero =>
        intro _
        exact ⟨n, Nat.le_refl n, fun i hi => False.elim (lt_zero_false i hi)⟩
      | succ N ih =>
        intro h
        exact Exists.elim (ih (fun i hi => h i (lt_succ_of_lt' i N hi))) (fun k1 hk1 =>
          Exists.elim (h N (Nat.lt_succ_self N)) (fun k2 hk2 =>
            ⟨k1 + k2, Nat.le_trans hk1.1 (Nat.le_add_right k1 k2), fun i hi => by
              by_cases hin : i = N
              · rw [hin]
                exact hmono N k2 (k1 + k2) hk2.1 (Nat.le_add_left k2 k1) hk2.2
              · exact hmono i k1 (k1 + k2) hk1.1 (Nat.le_add_right k1 k2)
                  (hk1.2 i (lt_pred_of_ne i N hi hin))⟩))

    -- ---------------------------------------------------------------------------
    -- 9b. What P1's and P2's decisions read.
    -- ---------------------------------------------------------------------------

    -- cur reads only the Table.
    theorem gc_cur_tr (K : Kernels) (w w' : World) (ht : w'.t = w.t) (r : Nat) :
        cur K w' r = cur K w r := by
      show AtermMeshRevocation.currentW K w'.t r = AtermMeshRevocation.currentW K w.t r
      rw [ht]

    -- rev reads only the Table.
    theorem gc_rev_tr (K : Kernels) (w w' : World) (ht : w'.t = w.t) (r : Nat) :
        rev K w' r = rev K w r := by
      show AtermMeshRevocation.revokedW K w'.t r = AtermMeshRevocation.revokedW K w.t r
      rw [ht]

    -- den reads only the two rosters.
    theorem gc_den_tr (G : GKernels) (w w' : World) (hl : w'.denL = w.denL) (ho : w'.denO = w.denO)
        (r : Nat) : den G w' r = den G w r := by
      show G.denied (w'.denL r) (w'.denO r) = G.denied (w.denL r) (w.denO r)
      rw [hl, ho]

    -- An epoch's gate reads only the Table and its record.
    theorem gc_gate_tr (K : Kernels) (w w' : World) (e : Nat) (ht : w'.t = w.t)
        (he : w'.eid e = w.eid e) : gateOf K w' e = gateOf K w e := by
      show AtermMeshRevocation.gateW K w'.t (w'.eid e) = AtermMeshRevocation.gateW K w.t (w.eid e)
      rw [ht, he]

    -- A current recipient is not revoked.
    theorem gc_cur_not_rev (K : Kernels) (hK : Exact K) (w : World) (r : Nat) (h : cur K w r = true) :
        rev K w r = false :=
      Eq.trans (rev_eq K hK w r)
        (AtermMeshRevocation.current_not_revoked w.t r (Eq.trans (cur_eq K hK w r).symm h))

    -- wrapsRev reads the Table, nr and the epoch's Wraps to revoked recipients.
    theorem gc_wrapsRev_tr (K : Kernels) (hK : Exact K) (w w' : World) (e : Nat) (ht : w'.t = w.t)
        (hnr : w'.nr = w.nr) (hwr : ∀ r, r < w.nr → rev K w r = true → w'.wr e r = w.wr e r) :
        wrapsRev K w' e = wrapsRev K w e := by
      show anyN w'.nr (fun r => K.wrapsRemoved (rev K w' r) (w'.wr e r))
        = anyN w.nr (fun r => K.wrapsRemoved (rev K w r) (w.wr e r))
      rw [hnr]
      apply anyN_congr
      intro r hr
      show K.wrapsRemoved (rev K w' r) (w'.wr e r) = K.wrapsRemoved (rev K w r) (w.wr e r)
      rw [gc_rev_tr K w w' ht r]
      cases hrv : rev K w r with
      | true => rw [hwr r hr hrv]
      | false =>
        exact Eq.trans (hK.2.2.2.2.2.2.1 false (w'.wr e r)) (Eq.symm (hK.2.2.2.2.2.2.1 false (w.wr e r)))

    -- A head of a is a newest epoch below ne whose audience is a.
    theorem gc_head_parts (w : World) (a e : Nat) (h : isHead w a e = true) :
        e < w.ne ∧ newest w e = true ∧ w.aud e = a := by
      have h1 := gc_and (newest w e) (decide (w.aud e = a)) h
      have h2 := gc_and (decide (e < w.ne)) (!(w.sup e)) h1.1
      exact ⟨of_decide_eq_true h2.1, h1.1, of_decide_eq_true h1.2⟩

    -- A head whose due holds makes its audience due.
    theorem gc_due_of_head (K : Kernels) (w : World) (a e : Nat) (h : isHead w a e = true)
        (hd : K.headDue (gateOf K w e) (wrapsRev K w e) = true) : dueA K w a = true := by
      have hany : anyN w.ne (fun e => isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e)) = true :=
        anyN_intro _ w.ne e (gc_head_parts w a e h).1 (by
          show (isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e)) = true
          rw [h, hd])
      show (anyN w.ne (fun e => isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e))
        || !(twinsA w a)) = true
      rw [hany]

    -- A head that wraps a revoked recipient is due.
    theorem gc_headDue_rev (K : Kernels) (hK : Exact K) (g : Bool) : K.headDue g true = true :=
      Eq.trans (hK.2.2.2.2.2.2.2 g true) (by cases g <;> rfl)

    -- A current recipient has a current seat row that binds it.
    theorem gc_seatCur_of_cur (K : Kernels) (hK : Exact K) (w : World) (r : Nat) (h : cur K w r = true) :
        ∃ i, i < w.t.rows ∧ w.t.rrcp i = r ∧ seatCur K w i = true := by
      have hc : spec_current (AtermMeshRevocation.boundLiveW K w.t r)
          (AtermMeshRevocation.revokedW K w.t r) = true :=
        Eq.trans (Eq.symm (hK.2.2.2.2.2.1 _ _)) h
      have hb := gc_and _ _ hc
      have hrv : AtermMeshRevocation.revokedW K w.t r = false := gc_bnot _ hb.2
      exact Exists.elim (anyN_elim _ w.t.rows hb.1) (fun i hi => by
        have hd := gc_and _ _ hi.2
        have hri : w.t.rrcp i = r := of_decide_eq_true hd.1
        have hout : AtermMeshRevocation.isOutW K w.t i = false := gc_bnot _ hd.2
        refine ⟨i, hi.1, hri, ?_⟩
        show K.current (!(AtermMeshRevocation.isOutW K w.t i))
          (AtermMeshRevocation.revokedW K w.t (w.t.rrcp i)) = true
        rw [hout, hri, hrv]
        exact hK.2.2.2.2.2.1 true false)

    -- The closer heals an omission of a current seat once each input of its verdict holds.
    theorem gc_healW_intro (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (ha : seatAud (w.aud e) = true) (he : e < w.ne) (hm : w.wr e m = true)
        (hc : seatCur K w i = true) (hom : w.wr e (w.t.rrcp i) = false)
        (hh : healsOm K G w e = true) (hd : den G w (w.t.rrcp i) = false) :
        healW K G w m e i = true := by
      have hgap : gapW K G w m e i = true := by
        show (seatAud (w.aud e) && (decide (e < w.ne) &&
          G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) = true
        rw [hG.2.1, ha, dtrue he, hm, hc, hom, hh]
        rfl
      show G.healSigns (gapW K G w m e i) (den G w (w.t.rrcp i)) = true
      rw [hG.2.2.2.1, hgap, hd]
      rfl

    -- What a heal signs: a current seat row, not denied, that the epoch omits.
    theorem gc_healW_parts (K : Kernels) (G : GKernels) (hG : GExact G) (w : World) (m e i : Nat)
        (h : healW K G w m e i = true) :
        seatCur K w i = true ∧ den G w (w.t.rrcp i) = false ∧ w.wr e (w.t.rrcp i) = false := by
      have h1 : spec_heal_signs (gapW K G w m e i) (den G w (w.t.rrcp i)) = true :=
        Eq.trans (Eq.symm (hG.2.2.2.1 _ _)) h
      have h2 := gc_and (gapW K G w m e i) (!(den G w (w.t.rrcp i))) h1
      have hg0 : gapW K G w m e i = (seatAud (w.aud e) && (decide (e < w.ne) &&
          spec_gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e))) := by
        show (seatAud (w.aud e) && (decide (e < w.ne) &&
          G.gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e)))
          = (seatAud (w.aud e) && (decide (e < w.ne) &&
          spec_gap (w.wr e m) (seatCur K w i) (w.wr e (w.t.rrcp i)) (known w e i) (healsOm K G w e)))
        rw [hG.2.1]
      have hg := Eq.trans hg0.symm h2.1
      have g1 := gc_and (seatAud (w.aud e)) _ hg
      have g2 := gc_and (decide (e < w.ne)) _ g1.2
      have g3 := gc_and (w.wr e m) _ g2.2
      have g4 := gc_and (seatCur K w i) _ g3.2
      have g5 := gc_and (!(w.wr e (w.t.rrcp i))) _ g4.2
      exact ⟨g4.1, gc_bnot _ h2.2, gc_bnot _ g5.1⟩

    -- ---------------------------------------------------------------------------
    -- 9c. The heads of an audience under each step.
    -- ---------------------------------------------------------------------------

    -- A heal adds one Wrap, to a current seat below nr that no roster denies and the
    -- epoch lacked.
    theorem gc_heal_shape (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w w' : World)
        (hb : RcpBound w) (hs : HealStep K G w w') :
        ∃ e0 r0, w' = addWrap w e0 r0 ∧ r0 < w.nr ∧ cur K w r0 = true ∧ den G w r0 = false
          ∧ w.wr e0 r0 = false :=
      Exists.elim hs (fun m hm => Exists.elim hm.2 (fun e0 he0 => Exists.elim he0 (fun i hi =>
        have hp := gc_healW_parts K G hG w m e0 i hi.2.1
        ⟨e0, w.t.rrcp i, hi.2.2, hb i hi.1, (seatCur_cur K hK w i hi.1 hp.1).1, hp.2.1, hp.2.2⟩)))

    -- A mint is a new epoch wrapping some set.
    theorem gc_mint_shape (K : Kernels) (G : GKernels) (a : Nat) (w w' : World)
        (h : MintStepA K G a w w') : ∃ x rs, w' = mintEpoch w a x rs :=
      Exists.elim h (fun _ hm => Exists.elim hm.2.2.2 (fun x hx =>
        ⟨x, carryW K G w (fun _ => false), hx.2.2.2.2⟩))

    -- A heal leaves every other epoch's Wraps alone.
    theorem gc_addWrap_ne (w : World) (e0 r0 e r : Nat) (h : Not (e = e0)) :
        (addWrap w e0 r0).wr e r = w.wr e r := by
      show ((decide (e = e0) && decide (r = r0)) || w.wr e r) = w.wr e r
      rw [dfalse h]

    -- A mint leaves the old epochs' records alone.
    theorem gc_mint_eid (w : World) (a x : Nat) (rs : Nat → Bool) (e : Nat) (he : e < w.ne) :
        (mintEpoch w a x rs).eid e = w.eid e :=
      if_neg (ne_of_lt' e w.ne he)

    -- A mint leaves the old epochs' Wraps alone.
    theorem gc_mint_wr (w : World) (a x : Nat) (rs : Nat → Bool) (e r : Nat) (he : e < w.ne) :
        (mintEpoch w a x rs).wr e r = w.wr e r :=
      if_neg (ne_of_lt' e w.ne he)

    -- After a mint of a, the new epoch is a's one head.
    theorem gc_mint_head_self (w : World) (a x : Nat) (rs : Nat → Bool) (e : Nat) :
        isHead (mintEpoch w a x rs) a e = decide (e = w.ne) := by
      show ((decide (e < Nat.succ w.ne) && !((mintEpoch w a x rs).sup e))
          && decide ((mintEpoch w a x rs).aud e = a)) = decide (e = w.ne)
      by_cases he : e = w.ne
      · have hs : (mintEpoch w a x rs).sup e = false := if_pos he
        have hau : (mintEpoch w a x rs).aud e = a := if_pos he
        have hlt : e < Nat.succ w.ne := by
          rw [he]
          exact Nat.lt_succ_self w.ne
        rw [hs, hau, dtrue he, dtrue (Eq.refl a), dtrue hlt]
      · have hs : (mintEpoch w a x rs).sup e = (w.sup e || decide (w.aud e = a)) := if_neg he
        have hau : (mintEpoch w a x rs).aud e = w.aud e := if_neg he
        rw [hs, hau, dfalse he]
        cases decide (w.aud e = a) <;> cases w.sup e <;> cases decide (e < Nat.succ w.ne) <;> rfl

    -- A mint of another audience keeps the heads of a.
    theorem gc_mint_head_other (w : World) (a a' x : Nat) (rs : Nat → Bool) (hne : Not (a' = a))
        (e : Nat) : isHead (mintEpoch w a' x rs) a e = isHead w a e := by
      show ((decide (e < Nat.succ w.ne) && !((mintEpoch w a' x rs).sup e))
          && decide ((mintEpoch w a' x rs).aud e = a))
        = ((decide (e < w.ne) && !(w.sup e)) && decide (w.aud e = a))
      by_cases he : e = w.ne
      · have hau : (mintEpoch w a' x rs).aud e = a' := if_pos he
        rw [hau, dfalse hne, he, dfalse (Nat.lt_irrefl w.ne)]
        cases decide (w.ne < Nat.succ w.ne) <;> cases (mintEpoch w a' x rs).sup w.ne <;> rfl
      · have hs : (mintEpoch w a' x rs).sup e = (w.sup e || decide (w.aud e = a')) := if_neg he
        have hau : (mintEpoch w a' x rs).aud e = w.aud e := if_neg he
        have hlt : decide (e < Nat.succ w.ne) = decide (e < w.ne) := by
          by_cases hl : e < w.ne
          · rw [dtrue hl, dtrue (lt_succ_of_lt' e w.ne hl)]
          · rw [dfalse hl, dfalse (fun h => hl (lt_pred_of_ne e w.ne h he))]
        rw [hs, hau, hlt]
        by_cases ha : w.aud e = a
        · rw [ha, dfalse (show Not (a = a') from fun h => hne h.symm), dtrue (Eq.refl a)]
          cases w.sup e <;> cases decide (e < w.ne) <;> rfl
        · rw [dfalse ha]
          cases w.sup e <;> cases decide (e < w.ne) <;> cases decide (w.aud e = a') <;> rfl

    -- An audience with at most one head has twin heads.
    theorem gc_twins_of_one (w : World) (a : Nat)
        (h : ∀ e1 e2, isHead w a e1 = true → isHead w a e2 = true → e1 = e2) : twinsA w a = true := by
      show allN w.ne (fun e1 => allN w.ne (fun e2 =>
        !(isHead w a e1 && isHead w a e2) || allN w.nr (fun r => w.wr e1 r == w.wr e2 r))) = true
      apply allN_intro
      intro e1 _
      apply allN_intro
      intro e2 _
      show (!(isHead w a e1 && isHead w a e2) || allN w.nr (fun r => w.wr e1 r == w.wr e2 r)) = true
      cases h1 : isHead w a e1 with
      | false => rfl
      | true =>
        cases h2 : isHead w a e2 with
        | false => rfl
        | true =>
          rw [h e1 e2 h1 h2]
          show allN w.nr (fun r => w.wr e2 r == w.wr e2 r) = true
          exact allN_intro _ w.nr (fun r _ => gc_beq_self _)

    -- A mint's new epoch is not due: its gate is open and it wraps no revoked recipient.
    theorem gc_mint_new_ok (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (a x : Nat) (hpast : ∀ j, j < w.t.rms → w.t.before (w.t.mid j) x = true) :
        K.headDue (gateOf K (mintEpoch w a x (carryW K G w (fun _ => false))) w.ne)
          (wrapsRev K (mintEpoch w a x (carryW K G w (fun _ => false))) w.ne) = false := by
      have hg : gateOf K (mintEpoch w a x (carryW K G w (fun _ => false))) w.ne = true := by
        show AtermMeshRevocation.gateW K w.t (if w.ne = w.ne then x else w.eid w.ne) = true
        rw [if_pos rfl, AtermMeshRevocation.gateW_eq K hK w.t x]
        exact AtermMeshRevocation.gate_of_postdating w.t x hpast
      have hw : wrapsRev K (mintEpoch w a x (carryW K G w (fun _ => false))) w.ne = false := by
        apply anyN_false
        intro r _
        show K.wrapsRemoved (rev K w r)
          (if w.ne = w.ne then carryW K G w (fun _ => false) r else w.wr w.ne r) = false
        rw [if_pos rfl]
        refine Eq.trans (hK.2.2.2.2.2.2.1 _ _) ?_
        have hcar : carryW K G w (fun _ => false) r = spec_carried (cur K w r) false (den G w r) :=
          hG.2.2.2.2.1 _ _ _
        rw [hcar]
        cases hc : cur K w r with
        | false => (cases rev K w r <;> rfl)
        | true =>
          rw [gc_cur_not_rev K hK w r hc]
          rfl
      rw [hg, hw]
      exact hK.2.2.2.2.2.2.2 true false

    -- A mint of a by a minter holding every Remove converges a.
    theorem gc_mint_conv (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (w : World)
        (a x : Nat) (hpast : ∀ j, j < w.t.rms → w.t.before (w.t.mid j) x = true) :
        Converged K G (mintEpoch w a x (carryW K G w (fun _ => false))) a := by
      have hhd := gc_mint_head_self w a x (carryW K G w (fun _ => false))
      have hany : anyN (Nat.succ w.ne) (fun e => isHead (mintEpoch w a x (carryW K G w (fun _ => false))) a e
          && K.headDue (gateOf K (mintEpoch w a x (carryW K G w (fun _ => false))) e)
            (wrapsRev K (mintEpoch w a x (carryW K G w (fun _ => false))) e)) = false := by
        apply anyN_false
        intro e _
        show (isHead (mintEpoch w a x (carryW K G w (fun _ => false))) a e
          && K.headDue (gateOf K (mintEpoch w a x (carryW K G w (fun _ => false))) e)
            (wrapsRev K (mintEpoch w a x (carryW K G w (fun _ => false))) e)) = false
        rw [hhd e]
        by_cases he : e = w.ne
        · rw [dtrue he, he, gc_mint_new_ok K hK G hG w a x hpast]
        · rw [dfalse he]
      have htw : twinsA (mintEpoch w a x (carryW K G w (fun _ => false))) a = true :=
        gc_twins_of_one _ a (fun e1 e2 h1 h2 => Eq.trans (of_decide_eq_true (Eq.trans (hhd e1).symm h1))
          (of_decide_eq_true (Eq.trans (hhd e2).symm h2)).symm)
      refine ⟨?_, ?_⟩
      · show (anyN (Nat.succ w.ne) (fun e => isHead (mintEpoch w a x (carryW K G w (fun _ => false))) a e
          && K.headDue (gateOf K (mintEpoch w a x (carryW K G w (fun _ => false))) e)
            (wrapsRev K (mintEpoch w a x (carryW K G w (fun _ => false))) e))
          || !(twinsA (mintEpoch w a x (carryW K G w (fun _ => false))) a)) = false
        rw [hany, htw]
      · intro e r he _ hcur hden
        have heq : e = w.ne := of_decide_eq_true (Eq.trans (hhd e).symm he)
        have hcur' : cur K w r = true := hcur
        have hden' : den G w r = false := hden
        show (if e = w.ne then carryW K G w (fun _ => false) r else w.wr e r) = true
        rw [if_pos heq]
        have hcar : carryW K G w (fun _ => false) r = spec_carried (cur K w r) false (den G w r) :=
          hG.2.2.2.2.1 _ _ _
        rw [hcar, hcur', hden']
        rfl

    -- A mint of a converges a at once: its one newest epoch has every Remove in its
    -- past (gate open), wraps no revoked recipient, and wraps every current seat no
    -- roster denies.
    theorem mint_converges (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G)
        (w w' : World) (a : Nat) (hs : MintStepA K G a w w') : Converged K G w' a :=
      Exists.elim hs (fun _ hm => Exists.elim hm.2.2.2 (fun x hx => by
        rw [hx.2.2.2.2]
        exact gc_mint_conv K hK G hG w a x hx.2.2.1))

    -- ---------------------------------------------------------------------------
    -- 9d. dueA and Converged read only the Table, nr, the rosters, a's heads, their
    --     records and their Wraps.
    -- ---------------------------------------------------------------------------

    -- A due carries to a view with the same Table, nr, heads, records and head Wraps.
    theorem gc_due_transfer (K : Kernels) (hK : Exact K) (w w' : World) (a : Nat) (ht : w'.t = w.t)
        (hnr : w'.nr = w.nr) (hhd : ∀ e, isHead w' a e = isHead w a e)
        (heid : ∀ e, isHead w a e = true → w'.eid e = w.eid e)
        (hwr : ∀ e r, isHead w a e = true → w'.wr e r = w.wr e r)
        (h : dueA K w a = true) : dueA K w' a = true := by
      have hd : (anyN w.ne (fun e => isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e))
          || !(twinsA w a)) = true := h
      cases hany : anyN w.ne (fun e => isHead w a e && K.headDue (gateOf K w e) (wrapsRev K w e)) with
      | true =>
        exact Exists.elim (anyN_elim _ w.ne hany) (fun e he => by
          have hp := gc_and (isHead w a e) (K.headDue (gateOf K w e) (wrapsRev K w e)) he.2
          have hg : gateOf K w' e = gateOf K w e := gc_gate_tr K w w' e ht (heid e hp.1)
          have hw : wrapsRev K w' e = wrapsRev K w e :=
            gc_wrapsRev_tr K hK w w' e ht hnr (fun r _ _ => hwr e r hp.1)
          apply gc_due_of_head K w' a e (Eq.trans (hhd e) hp.1)
          rw [hg, hw]
          exact hp.2)
      | false =>
        rw [hany] at hd
        have htw : allN w.ne (fun e1 => allN w.ne (fun e2 =>
            !(isHead w a e1 && isHead w a e2) || allN w.nr (fun r => w.wr e1 r == w.wr e2 r))) = false :=
          gc_bnot _ hd
        have htw' : allN w'.ne (fun e1 => allN w'.ne (fun e2 =>
            !(isHead w' a e1 && isHead w' a e2) || allN w'.nr (fun r => w'.wr e1 r == w'.wr e2 r))) = false :=
          Exists.elim (allN_false_elim _ w.ne htw) (fun e1 h1 =>
            Exists.elim (allN_false_elim _ w.ne h1.2) (fun e2 h2 => by
              have h3 := gc_or_false _ _ h2.2
              have hx := gc_and _ _ (gc_bnot_false _ h3.1)
              have hx1 : isHead w' a e1 = true := Eq.trans (hhd e1) hx.1
              have hx2 : isHead w' a e2 = true := Eq.trans (hhd e2) hx.2
              apply allN_false_intro _ w'.ne e1 (gc_head_parts w' a e1 hx1).1
              apply allN_false_intro _ w'.ne e2 (gc_head_parts w' a e2 hx2).1
              show (!(isHead w' a e1 && isHead w' a e2) || allN w'.nr (fun r => w'.wr e1 r == w'.wr e2 r)) = false
              have hall : allN w'.nr (fun r => w'.wr e1 r == w'.wr e2 r)
                  = allN w.nr (fun r => w.wr e1 r == w.wr e2 r) := by
                rw [hnr]
                apply allN_congr
                intro r _
                show (w'.wr e1 r == w'.wr e2 r) = (w.wr e1 r == w.wr e2 r)
                rw [hwr e1 r hx.1, hwr e2 r hx.2]
              rw [hx1, hx2, hall]
              exact h3.2))
        show (anyN w'.ne (fun e => isHead w' a e && K.headDue (gateOf K w' e) (wrapsRev K w' e))
          || !(allN w'.ne (fun e1 => allN w'.ne (fun e2 =>
            !(isHead w' a e1 && isHead w' a e2) || allN w'.nr (fun r => w'.wr e1 r == w'.wr e2 r))))) = true
        rw [htw']
        exact gc_or_r _ _ rfl

    -- Converged carries to a view with the same Table, nr, rosters, heads, records
    -- and head Wraps.
    theorem gc_conv_transfer (K : Kernels) (hK : Exact K) (G : GKernels) (w w' : World) (a : Nat)
        (ht : w'.t = w.t) (hnr : w'.nr = w.nr) (hdl : w'.denL = w.denL) (hdo : w'.denO = w.denO)
        (hhd : ∀ e, isHead w' a e = isHead w a e)
        (heid : ∀ e, isHead w a e = true → w'.eid e = w.eid e)
        (hwr : ∀ e r, isHead w a e = true → w'.wr e r = w.wr e r)
        (hc : Converged K G w a) : Converged K G w' a := by
      refine ⟨?_, ?_⟩
      · cases hd : dueA K w' a with
        | false => rfl
        | true =>
          exact Bool.noConfusion (Eq.trans (gc_due_transfer K hK w' w a ht.symm hnr.symm
            (fun e => (hhd e).symm) (fun e he => (heid e (Eq.trans (hhd e).symm he)).symm)
            (fun e r he => (hwr e r (Eq.trans (hhd e).symm he)).symm) hd).symm hc.1)
      · intro e r he hr hcur hden
        have he' : isHead w a e = true := Eq.trans (hhd e).symm he
        exact Eq.trans (hwr e r he') (hc.2 e r he' (gc_lt_tr r _ _ hnr hr)
          (Eq.trans (gc_cur_tr K w w' ht r).symm hcur) (Eq.trans (gc_den_tr G w w' hdl hdo r).symm hden))

    -- One step of a stable membership keeps the Table, nr and both rosters.
    theorem gc_step_frame (K : Kernels) (G : GKernels) (w w' : World)
        (hs : HealStep K G w w' ∨ MintStep K G w w' ∨ w' = w) :
        w'.t = w.t ∧ w'.nr = w.nr ∧ w'.denL = w.denL ∧ w'.denO = w.denO := by
      cases hs with
      | inl hh =>
        exact Exists.elim hh (fun _ hm => Exists.elim hm.2 (fun e he => Exists.elim he (fun i hi => by
          rw [hi.2.2]
          exact ⟨rfl, rfl, rfl, rfl⟩)))
      | inr hr =>
        cases hr with
        | inl hm =>
          exact Exists.elim hm (fun a' ha' => Exists.elim (gc_mint_shape K G a' w w' ha') (fun x hx =>
            Exists.elim hx (fun rs hrs => by
              rw [hrs]
              exact ⟨rfl, rfl, rfl, rfl⟩)))
        | inr he =>
          rw [he]
          exact ⟨rfl, rfl, rfl, rfl⟩

    -- Converged is stable under the steps of a stable membership.
    theorem converged_stable (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G)
        (w w' : World) (a : Nat) (hb : RcpBound w) (hc : Converged K G w a)
        (hs : HealStep K G w w' ∨ MintStep K G w w' ∨ w' = w) : Converged K G w' a := by
      cases hs with
      | inl hheal =>
        exact Exists.elim (gc_heal_shape K hK G hG w w' hb hheal) (fun e0 h0 => Exists.elim h0 (fun r0 h1 => by
          have hne : ∀ e, isHead w a e = true → Not (e = e0) := fun e he heq => by
            rw [heq] at he
            exact Bool.noConfusion (Eq.trans (hc.2 e0 r0 he h1.2.1 h1.2.2.1 h1.2.2.2.1).symm h1.2.2.2.2)
          rw [h1.1]
          exact gc_conv_transfer K hK G w (addWrap w e0 r0) a rfl rfl rfl rfl (fun _ => rfl) (fun _ _ => rfl)
            (fun e r he => gc_addWrap_ne w e0 r0 e r (hne e he)) hc))
      | inr hrest =>
        cases hrest with
        | inl hmint =>
          exact Exists.elim hmint (fun a' ha' => by
            by_cases haa : a' = a
            · rw [haa] at ha'
              exact mint_converges K hK G hG w w' a ha'
            · exact Exists.elim (gc_mint_shape K G a' w w' ha') (fun x hx => Exists.elim hx (fun rs hrs => by
                rw [hrs]
                exact gc_conv_transfer K hK G w (mintEpoch w a' x rs) a rfl rfl rfl rfl
                  (gc_mint_head_other w a a' x rs haa)
                  (fun e he => gc_mint_eid w a' x rs e (gc_head_parts w a e he).1)
                  (fun e r he => gc_mint_wr w a' x rs e r (gc_head_parts w a e he).1) hc)))
        | inr heq =>
          rw [heq]
          exact hc

    -- Along a stable membership the Table and both rosters stay as they were at n0.
    theorem stable_frame (K : Kernels) (G : GKernels) (b : Behavior World) (n0 : Nat)
        (hst : StableFrom K G b n0) (k : Nat) (hk : n0 ≤ k) :
        (b k).t = (b n0).t ∧ (b k).nr = (b n0).nr ∧ (b k).denL = (b n0).denL
          ∧ (b k).denO = (b n0).denO :=
      gc_ind_from (fun k => (b k).t = (b n0).t ∧ (b k).nr = (b n0).nr ∧ (b k).denL = (b n0).denL
          ∧ (b k).denO = (b n0).denO) n0 ⟨rfl, rfl, rfl, rfl⟩
        (fun j hj ih =>
          have hf := gc_step_frame K G (b j) (b (Nat.succ j)) (hst j hj)
          ⟨Eq.trans hf.1 ih.1, Eq.trans hf.2.1 ih.2.1, Eq.trans hf.2.2.1 ih.2.2.1,
           Eq.trans hf.2.2.2 ih.2.2.2⟩) k hk

    -- ---------------------------------------------------------------------------
    -- 9e. A quiet stretch: from n0 every step heals, mints another audience or
    --     stutters, so a's heads and their records stay put, and their Wraps only
    --     grow, by current seats no roster denies.
    -- ---------------------------------------------------------------------------

    -- A quiet step for a: a heal, a mint of another audience, or a stutter.
    def gc_StepA (K : Kernels) (G : GKernels) (a : Nat) (w w' : World) : Prop :=
      HealStep K G w w' ∨ (∃ a', Not (a' = a) ∧ MintStepA K G a' w w') ∨ w' = w

    -- What one quiet step does to a's heads: the same heads and records, and Wraps that
    -- only grow, by current seats below nr that no roster denies.
    def gc_HeadStep (K : Kernels) (G : GKernels) (a : Nat) (w w' : World) : Prop :=
      (∀ e, isHead w' a e = isHead w a e)
        ∧ (∀ e, isHead w a e = true → w'.eid e = w.eid e)
        ∧ (∀ e r, isHead w a e = true → w.wr e r = true → w'.wr e r = true)
        ∧ (∀ e r, isHead w a e = true → w'.wr e r = true →
            w.wr e r = true ∨ (r < w.nr ∧ cur K w r = true ∧ den G w r = false))

    -- A stutter keeps a's heads.
    theorem gc_headstep_refl (K : Kernels) (G : GKernels) (a : Nat) (w : World) : gc_HeadStep K G a w w :=
      ⟨fun _ => rfl, fun _ _ => rfl, fun _ _ _ h => h, fun _ _ _ h => Or.inl h⟩

    -- A heal keeps a's heads and adds one Wrap, to a current seat no roster denies.
    theorem gc_heal_headstep (K : Kernels) (G : GKernels) (a : Nat) (w : World) (e0 r0 : Nat)
        (hr0 : r0 < w.nr) (hc0 : cur K w r0 = true) (hd0 : den G w r0 = false) :
        gc_HeadStep K G a w (addWrap w e0 r0) :=
      ⟨fun _ => rfl, fun _ _ => rfl, fun _ _ _ h => gc_or_r _ _ h, fun e r _ h => by
        cases gc_or _ _ h with
        | inl h1 =>
          have hr : r = r0 := of_decide_eq_true (gc_and _ _ h1).2
          rw [hr]
          exact Or.inr ⟨hr0, hc0, hd0⟩
        | inr h1 => exact Or.inl h1⟩

    -- A mint of another audience keeps a's heads, their records and their Wraps.
    theorem gc_mint_headstep (K : Kernels) (G : GKernels) (a a' : Nat) (w : World) (x : Nat)
        (rs : Nat → Bool) (hne : Not (a' = a)) : gc_HeadStep K G a w (mintEpoch w a' x rs) :=
      ⟨gc_mint_head_other w a a' x rs hne,
       fun e he => gc_mint_eid w a' x rs e (gc_head_parts w a e he).1,
       fun e r he h => Eq.trans (gc_mint_wr w a' x rs e r (gc_head_parts w a e he).1) h,
       fun e r he h => Or.inl (Eq.trans (gc_mint_wr w a' x rs e r (gc_head_parts w a e he).1).symm h)⟩

    -- Every quiet step keeps a's heads.
    theorem gc_stepA_headstep (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (a : Nat)
        (w w' : World) (hb : RcpBound w) (hs : gc_StepA K G a w w') : gc_HeadStep K G a w w' := by
      cases hs with
      | inl hh =>
        exact Exists.elim (gc_heal_shape K hK G hG w w' hb hh) (fun e0 h0 => Exists.elim h0 (fun r0 h1 => by
          rw [h1.1]
          exact gc_heal_headstep K G a w e0 r0 h1.2.1 h1.2.2.1 h1.2.2.2.1))
      | inr hr =>
        cases hr with
        | inl hm =>
          exact Exists.elim hm (fun a' ha' => Exists.elim (gc_mint_shape K G a' w w' ha'.2) (fun x hx =>
            Exists.elim hx (fun rs hrs => by
              rw [hrs]
              exact gc_mint_headstep K G a a' w x rs ha'.1)))
        | inr he =>
          rw [he]
          exact gc_headstep_refl K G a w

    -- A stable step that is no mint of a is a quiet step for a.
    theorem gc_stepA_of (K : Kernels) (G : GKernels) (a : Nat) (w w' : World)
        (hs : HealStep K G w w' ∨ MintStep K G w w' ∨ w' = w) (hn : Not (MintStepA K G a w w')) :
        gc_StepA K G a w w' := by
      cases hs with
      | inl hh => exact Or.inl hh
      | inr hr =>
        cases hr with
        | inl hm =>
          exact Exists.elim hm (fun a' ha' => by
            by_cases haa : a' = a
            · rw [haa] at ha'
              exact False.elim (hn ha')
            · exact Or.inr (Or.inl ⟨a', haa, ha'⟩))
        | inr he => exact Or.inr (Or.inr he)

    -- A quiet stretch for a from n0: quiet steps, recipients below nr, and the Table,
    -- nr and rosters of n0.
    def gc_Quiet (K : Kernels) (G : GKernels) (b : Behavior World) (a n0 : Nat) : Prop :=
      (∀ k, n0 ≤ k → gc_StepA K G a (b k) (b (Nat.succ k)))
        ∧ (∀ k, n0 ≤ k → RcpBound (b k))
        ∧ (∀ k, n0 ≤ k → (b k).t = (b n0).t ∧ (b k).nr = (b n0).nr ∧ (b k).denL = (b n0).denL
            ∧ (b k).denO = (b n0).denO)

    -- Each step of a quiet stretch keeps a's heads.
    theorem gc_q_step (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k) :
        gc_HeadStep K G a (b k) (b (Nat.succ k)) :=
      gc_stepA_headstep K hK G hG a (b k) (b (Nat.succ k)) (hq.2.1 k hk) (hq.1 k hk)

    -- The heads of a stay those of n0.
    theorem gc_q_head (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k) (e : Nat) :
        isHead (b k) a e = isHead (b n0) a e :=
      gc_ind_from (fun k => ∀ e, isHead (b k) a e = isHead (b n0) a e) n0 (fun _ => rfl)
        (fun j hj ih e => Eq.trans ((gc_q_step K hK G hG b a n0 hq j hj).1 e) (ih e)) k hk e

    -- A head of n0 is a head at every later step.
    theorem gc_q_head_true (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k) (e : Nat)
        (he : isHead (b n0) a e = true) : isHead (b k) a e = true :=
      Eq.trans (gc_q_head K hK G hG b a n0 hq k hk e) he

    -- A head keeps its record.
    theorem gc_q_eid (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k) (e : Nat)
        (he : isHead (b n0) a e = true) : (b k).eid e = (b n0).eid e :=
      gc_ind_from (fun k => (b k).eid e = (b n0).eid e) n0 rfl
        (fun j hj ih => Eq.trans ((gc_q_step K hK G hG b a n0 hq j hj).2.1 e (gc_q_head_true K hK G hG b a n0 hq j hj e he)) ih) k hk

    -- A head's Wraps only grow.
    theorem gc_q_grow (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (j : Nat) (hj : n0 ≤ j) (k : Nat) (hjk : j ≤ k) (e r : Nat)
        (he : isHead (b n0) a e = true) (h : (b j).wr e r = true) : (b k).wr e r = true :=
      gc_ind_from (fun k => (b k).wr e r = true) j h
        (fun i hi ih => (gc_q_step K hK G hG b a n0 hq i (Nat.le_trans hj hi)).2.2.1 e r
          (gc_q_head_true K hK G hG b a n0 hq i (Nat.le_trans hj hi) e he) ih) k hjk

    -- A head's new Wraps go to current seats below nr that no roster denies.
    theorem gc_q_new (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (j : Nat) (hj : n0 ≤ j) (k : Nat) (hjk : j ≤ k) (e r : Nat)
        (he : isHead (b n0) a e = true) (h : (b k).wr e r = true) :
        (b j).wr e r = true ∨ (r < (b n0).nr ∧ cur K (b n0) r = true ∧ den G (b n0) r = false) :=
      gc_ind_from (fun k => (b k).wr e r = true →
          (b j).wr e r = true ∨ (r < (b n0).nr ∧ cur K (b n0) r = true ∧ den G (b n0) r = false)) j
        (fun h => Or.inl h)
        (fun i hi ih h =>
          have hin : n0 ≤ i := Nat.le_trans hj hi
          have fi := hq.2.2 i hin
          Or.elim ((gc_q_step K hK G hG b a n0 hq i hin).2.2.2 e r (gc_q_head_true K hK G hG b a n0 hq i hin e he) h) ih
            (fun h1 => Or.inr ⟨gc_lt_tr r _ _ fi.2.1 h1.1,
              Eq.trans (gc_cur_tr K (b n0) (b i) fi.1 r).symm h1.2.1,
              Eq.trans (gc_den_tr G (b n0) (b i) fi.2.2.1 fi.2.2.2 r).symm h1.2.2⟩)) k hjk h

    -- A head keeps whether it wraps a revoked recipient (a heal adds only current ones).
    theorem gc_q_wrapsRev (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k) (e : Nat)
        (he : isHead (b n0) a e = true) : wrapsRev K (b k) e = wrapsRev K (b n0) e :=
      gc_wrapsRev_tr K hK (b n0) (b k) e (hq.2.2 k hk).1 (hq.2.2 k hk).2.1 (fun r _ hrv => by
        cases h0 : (b n0).wr e r with
        | true => exact gc_q_grow K hK G hG b a n0 hq n0 (Nat.le_refl n0) k hk e r he h0
        | false =>
          cases hk' : (b k).wr e r with
          | false => rfl
          | true =>
            exact Or.elim (gc_q_new K hK G hG b a n0 hq n0 (Nat.le_refl n0) k hk e r he hk')
              (fun h1 => Bool.noConfusion (Eq.trans h1.symm h0))
              (fun h1 => Bool.noConfusion (Eq.trans hrv.symm (gc_cur_not_rev K hK (b n0) r h1.2.1))))

    -- A carrier of a stays a carrier.
    theorem gc_q_carried (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (j : Nat) (hj : n0 ≤ j) (k : Nat) (hjk : j ≤ k)
        (h : carriedA K G (b j) a = true) : carriedA K G (b k) a = true :=
      Exists.elim (anyN_elim _ (b j).nr h) (fun r hr => by
        have hkn : n0 ≤ k := Nat.le_trans hj hjk
        have fj := hq.2.2 j hj
        have fk := hq.2.2 k hkn
        have h1 := gc_and (cur K (b j) r) _ hr.2
        have h2 := gc_and (!(den G (b j) r)) _ h1.2
        have hcur : cur K (b k) r = true :=
          Eq.trans (gc_cur_tr K (b n0) (b k) fk.1 r) (Eq.trans (gc_cur_tr K (b n0) (b j) fj.1 r).symm h1.1)
        have hden : den G (b k) r = false :=
          Eq.trans (gc_den_tr G (b n0) (b k) fk.2.2.1 fk.2.2.2 r)
            (Eq.trans (gc_den_tr G (b n0) (b j) fj.2.2.1 fj.2.2.2 r).symm (gc_bnot _ h2.1))
        have hhold : holdsAll (b k) a r = true := by
          show allN (b k).ne (fun e => !(isHead (b k) a e) || (b k).wr e r) = true
          apply allN_intro
          intro e _
          show (!(isHead (b k) a e) || (b k).wr e r) = true
          cases hh : isHead (b k) a e with
          | false => rfl
          | true =>
            have he0 : isHead (b n0) a e = true := Eq.trans (gc_q_head K hK G hG b a n0 hq k hkn e).symm hh
            have hej : isHead (b j) a e = true := Eq.trans (gc_q_head K hK G hG b a n0 hq j hj e) he0
            have hw : (!(isHead (b j) a e) || (b j).wr e r) = true :=
              allN_elim _ (b j).ne h2.2 e (gc_head_parts (b j) a e hej).1
            rw [hej] at hw
            exact gc_q_grow K hK G hG b a n0 hq j hj k hjk e r he0 hw
        exact anyN_intro _ (b k).nr r (gc_lt_tr r _ _ (Eq.trans fj.2.1 fk.2.1.symm) hr.1) (by
          show (cur K (b k) r && (!(den G (b k) r) && holdsAll (b k) a r)) = true
          rw [hcur, hden, hhold]))

    -- With no mint of a, no head of a wraps a revoked recipient while a has a carrier:
    -- the due and the carrier would last, and MintFair would mint a.
    theorem gc_q_no_due_carry (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (k1 : Nat) (hk1 : n0 ≤ k1) (e : Nat) (he : isHead (b n0) a e = true)
        (hw : wrapsRev K (b k1) e = true) (hc : carriedA K G (b k1) a = true) : False :=
      Exists.elim (hm k1 (fun k hk => by
          have hkn : n0 ≤ k := Nat.le_trans hk1 hk
          have hwk : wrapsRev K (b k) e = true :=
            Eq.trans (gc_q_wrapsRev K hK G hG b a n0 hq k hkn e he)
              (Eq.trans (gc_q_wrapsRev K hK G hG b a n0 hq k1 hk1 e he).symm hw)
          have hd : K.headDue (gateOf K (b k) e) (wrapsRev K (b k) e) = true := by
            rw [hwk]
            exact gc_headDue_rev K hK _
          exact ⟨gc_due_of_head K (b k) a e (gc_q_head_true K hK G hG b a n0 hq k hkn e he) hd,
            gc_q_carried K hK G hG b a n0 hq k1 hk1 k hk hc⟩))
        (fun k hk => hno k (Nat.le_trans hk1 hk.1) hk.2)

    -- With no mint of a, every head of a heals its omissions.
    theorem gc_q_healsOm (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (k : Nat) (hk : n0 ≤ k) (e : Nat) (he : isHead (b n0) a e = true) :
        healsOm K G (b k) e = true := by
      have hp := gc_head_parts (b k) a e (gc_q_head_true K hK G hG b a n0 hq k hk e he)
      have hh : healsOm K G (b k) e
          = spec_heals_omission (newest (b k) e) (wrapsRev K (b k) e) (carriedA K G (b k) ((b k).aud e)) :=
        hG.1 _ _ _
      rw [hh, hp.2.1, hp.2.2]
      cases hw : wrapsRev K (b k) e with
      | false => rfl
      | true =>
        cases hc : carriedA K G (b k) a with
        | false => rfl
        | true => exact False.elim (gc_q_no_due_carry K hK G hG b a n0 hq hno hm k hk e he hw hc)

    -- With no mint of a, each head of a comes to wrap each current seat no roster
    -- denies: until it does, the omission is a gap a current holder heals at every step.
    theorem gc_q_eventually (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (hh : HealFair K G b) (hH : ∀ k, n0 ≤ k → HolderExists K (b k)) (ha : seatAud a = true)
        (e : Nat) (he : isHead (b n0) a e = true) (r : Nat) (hcur : cur K (b n0) r = true)
        (hden : den G (b n0) r = false) : ∃ k, n0 ≤ k ∧ (b k).wr e r = true := by
      cases Classical.em (∃ k, n0 ≤ k ∧ (b k).wr e r = true) with
      | inl hy => exact hy
      | inr hnot =>
        apply hh n0 e r
        intro k hk
        have hwk : (b k).wr e r = false := by
          cases hx : (b k).wr e r with
          | false => rfl
          | true => exact False.elim (hnot ⟨k, hk, hx⟩)
        have fk := hq.2.2 k hk
        have hek := gc_q_head_true K hK G hG b a n0 hq k hk e he
        have hp := gc_head_parts (b k) a e hek
        have hcurk : cur K (b k) r = true := Eq.trans (gc_cur_tr K (b n0) (b k) fk.1 r) hcur
        have hdenk : den G (b k) r = false := Eq.trans (gc_den_tr G (b n0) (b k) fk.2.2.1 fk.2.2.2 r) hden
        have hsa : seatAud ((b k).aud e) = true := by
          rw [hp.2.2]
          exact ha
        have hho := gc_q_healsOm K hK G hG b a n0 hq hno hm k hk e he
        exact Exists.elim (anyN_elim _ (b k).nr (hH k hk a e ha hek)) (fun m hm' =>
          Exists.elim (gc_seatCur_of_cur K hK (b k) r hcurk) (fun i hi => by
            have hm2 := gc_and (cur K (b k) m) ((b k).wr e m) hm'.2
            have hom : (b k).wr e ((b k).t.rrcp i) = false := by
              rw [hi.2.1]
              exact hwk
            have hdi : den G (b k) ((b k).t.rrcp i) = false := by
              rw [hi.2.1]
              exact hdenk
            exact ⟨m, i, hi.1, hi.2.1, hm2.1,
              gc_healW_intro K G hG (b k) m e i hsa hp.1 hm2.2 hi.2.2 hom hho hdi⟩))

    -- At time k every head of a wraps every current seat of n0 no roster denies.
    def gc_Cover (K : Kernels) (G : GKernels) (b : Behavior World) (a n0 k : Nat) : Prop :=
      ∀ e r, isHead (b n0) a e = true → r < (b n0).nr → cur K (b n0) r = true
        → den G (b n0) r = false → (b k).wr e r = true

    -- One head comes to cover every current seat at one time.
    theorem gc_q_cover_one (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (hh : HealFair K G b) (hH : ∀ k, n0 ≤ k → HolderExists K (b k)) (ha : seatAud a = true)
        (e : Nat) (he : isHead (b n0) a e = true) :
        ∃ k, n0 ≤ k ∧ ∀ r, r < (b n0).nr → cur K (b n0) r = true → den G (b n0) r = false
          → (b k).wr e r = true :=
      Exists.elim (gc_collect (fun r k => cur K (b n0) r = true → den G (b n0) r = false
            → (b k).wr e r = true) n0
          (fun r k k' hk hkk' hP hc hd => gc_q_grow K hK G hG b a n0 hq k hk k' hkk' e r he (hP hc hd))
          (b n0).nr
          (fun r _ => by
            show ∃ k, n0 ≤ k ∧ (cur K (b n0) r = true → den G (b n0) r = false → (b k).wr e r = true)
            cases hc : cur K (b n0) r with
            | false => exact ⟨n0, Nat.le_refl n0, fun h _ => Bool.noConfusion h⟩
            | true =>
              cases hd : den G (b n0) r with
              | true => exact ⟨n0, Nat.le_refl n0, fun _ h => Bool.noConfusion h⟩
              | false =>
                exact Exists.elim (gc_q_eventually K hK G hG b a n0 hq hno hm hh hH ha e he r hc hd)
                  (fun k hk => ⟨k, hk.1, fun _ _ => hk.2⟩)))
        (fun k hk => ⟨k, hk.1, fun r hr => hk.2 r hr⟩)

    -- Every head comes to cover every current seat at one time.
    theorem gc_q_cover (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (hh : HealFair K G b) (hH : ∀ k, n0 ≤ k → HolderExists K (b k)) (ha : seatAud a = true) :
        ∃ k, n0 ≤ k ∧ gc_Cover K G b a n0 k :=
      Exists.elim (gc_collect (fun e k => isHead (b n0) a e = true → ∀ r, r < (b n0).nr →
            cur K (b n0) r = true → den G (b n0) r = false → (b k).wr e r = true) n0
          (fun e k k' hk hkk' hP he r hr hc hd => gc_q_grow K hK G hG b a n0 hq k hk k' hkk' e r he (hP he r hr hc hd))
          (b n0).ne
          (fun e _ => by
            show ∃ k, n0 ≤ k ∧ (isHead (b n0) a e = true → ∀ r, r < (b n0).nr →
              cur K (b n0) r = true → den G (b n0) r = false → (b k).wr e r = true)
            cases hhe : isHead (b n0) a e with
            | false => exact ⟨n0, Nat.le_refl n0, fun h => Bool.noConfusion h⟩
            | true =>
              exact Exists.elim (gc_q_cover_one K hK G hG b a n0 hq hno hm hh hH ha e hhe)
                (fun k hk => ⟨k, hk.1, fun _ => hk.2⟩)))
        (fun k hk => ⟨k, hk.1, fun e r he hr hc hd => hk.2 e (gc_head_parts (b n0) a e he).1 he r hr hc hd⟩)

    -- Once covered, a's heads' Wraps are frozen: every later Wrap of a head goes to a
    -- seat it already wraps.
    theorem gc_q_frozen (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k4 : Nat) (hk4 : n0 ≤ k4)
        (hcov : gc_Cover K G b a n0 k4) (k : Nat) (hk : k4 ≤ k) (e : Nat)
        (he : isHead (b n0) a e = true) (r : Nat) : (b k).wr e r = (b k4).wr e r := by
      cases h4 : (b k4).wr e r with
      | true => exact gc_q_grow K hK G hG b a n0 hq k4 hk4 k hk e r he h4
      | false =>
        cases hk' : (b k).wr e r with
        | false => rfl
        | true =>
          exact Or.elim (gc_q_new K hK G hG b a n0 hq k4 hk4 k hk e r he hk')
            (fun h1 => Bool.noConfusion (Eq.trans h1.symm h4))
            (fun h1 => Bool.noConfusion (Eq.trans (hcov e r he h1.1 h1.2.1 h1.2.2).symm h4))

    -- Once covered, a current seat no roster denies carries a.
    theorem gc_q_carried_cover (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0) (k : Nat) (hk : n0 ≤ k)
        (hcov : gc_Cover K G b a n0 k) (c : Nat) (hc0 : c < (b n0).nr) (hc1 : cur K (b n0) c = true)
        (hc2 : den G (b n0) c = false) : carriedA K G (b k) a = true := by
      have fk := hq.2.2 k hk
      have hhold : holdsAll (b k) a c = true := by
        show allN (b k).ne (fun e => !(isHead (b k) a e) || (b k).wr e c) = true
        apply allN_intro
        intro e _
        show (!(isHead (b k) a e) || (b k).wr e c) = true
        cases hh : isHead (b k) a e with
        | false => rfl
        | true => exact hcov e c (Eq.trans (gc_q_head K hK G hG b a n0 hq k hk e).symm hh) hc0 hc1 hc2
      have hck : cur K (b k) c = true := Eq.trans (gc_cur_tr K (b n0) (b k) fk.1 c) hc1
      have hdk : den G (b k) c = false := Eq.trans (gc_den_tr G (b n0) (b k) fk.2.2.1 fk.2.2.2 c) hc2
      exact anyN_intro _ (b k).nr c (gc_lt_tr c _ _ fk.2.1.symm hc0) (by
        show (cur K (b k) c && (!(den G (b k) c) && holdsAll (b k) a c)) = true
        rw [hck, hdk, hhold])

    -- Law 4 on a quiet stretch with no mint of a: once every head covers every current
    -- seat, a has a carrier for good and its heads are frozen, so a due would last and
    -- MintFair would mint a; hence nothing is due and a has converged.
    theorem gc_q_law4 (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G) (b : Behavior World)
        (a n0 : Nat) (hq : gc_Quiet K G b a n0)
        (hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k)))) (hm : MintFair K G b a)
        (hh : HealFair K G b) (hH : ∀ k, n0 ≤ k → HolderExists K (b k)) (ha : seatAud a = true)
        (hc : ∃ c, c < (b n0).nr ∧ cur K (b n0) c = true ∧ den G (b n0) c = false) :
        ∃ k, n0 ≤ k ∧ Converged K G (b k) a :=
      Exists.elim (gc_q_cover K hK G hG b a n0 hq hno hm hh hH ha) (fun k4 hk4 => Exists.elim hc (fun c hc' => by
        have f4 := hq.2.2 k4 hk4.1
        have hcar4 : carriedA K G (b k4) a = true :=
          gc_q_carried_cover K hK G hG b a n0 hq k4 hk4.1 hk4.2 c hc'.1 hc'.2.1 hc'.2.2
        have hdue : dueA K (b k4) a = false := by
          cases hd : dueA K (b k4) a with
          | false => rfl
          | true =>
            exact False.elim (Exists.elim (hm k4 (fun k hk =>
              have hkn : n0 ≤ k := Nat.le_trans hk4.1 hk
              have fk := hq.2.2 k hkn
              ⟨gc_due_transfer K hK (b k4) (b k) a (Eq.trans fk.1 f4.1.symm) (Eq.trans fk.2.1 f4.2.1.symm)
                (fun e => Eq.trans (gc_q_head K hK G hG b a n0 hq k hkn e) (gc_q_head K hK G hG b a n0 hq k4 hk4.1 e).symm)
                (fun e he =>
                  have he0 := Eq.trans (gc_q_head K hK G hG b a n0 hq k4 hk4.1 e).symm he
                  Eq.trans (gc_q_eid K hK G hG b a n0 hq k hkn e he0) (gc_q_eid K hK G hG b a n0 hq k4 hk4.1 e he0).symm)
                (fun e r he => gc_q_frozen K hK G hG b a n0 hq k4 hk4.1 hk4.2 k hk e
                  (Eq.trans (gc_q_head K hK G hG b a n0 hq k4 hk4.1 e).symm he) r) hd,
               gc_q_carried K hK G hG b a n0 hq k4 hk4.1 k hk hcar4⟩))
              (fun k hk => hno k (Nat.le_trans hk4.1 hk.1) hk.2))
        exact ⟨k4, hk4.1, hdue, fun e r he hr hcu hd => hk4.2 e r
          (Eq.trans (gc_q_head K hK G hG b a n0 hq k4 hk4.1 e).symm he) (gc_lt_tr r _ _ f4.2.1 hr)
          (Eq.trans (gc_cur_tr K (b n0) (b k4) f4.1 r).symm hcu)
          (Eq.trans (gc_den_tr G (b n0) (b k4) f4.2.2.1 f4.2.2.2 r).symm hd)⟩))

    -- Law 4 given a holder at every step (lab_holder_exists supplies it in the guarded
    -- lab): once the membership is stable, with weakly fair heals and re-keys and some
    -- current Mac no roster denies, the seat audience a converges.
    theorem law4_of_holders (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G)
        (b : Behavior World) (a n0 : Nat) (ha : seatAud a = true)
        (hst : StableFrom K G b n0) (hh : HealFair K G b) (hm : MintFair K G b a)
        (hH : ∀ k, n0 ≤ k → HolderExists K (b k)) (hB : ∀ k, n0 ≤ k → RcpBound (b k))
        (hc : ∃ c, c < (b n0).nr ∧ cur K (b n0) c = true ∧ den G (b n0) c = false) :
        ∃ k, n0 ≤ k ∧ Converged K G (b k) a := by
      cases Classical.em (∃ k, n0 ≤ k ∧ MintStepA K G a (b k) (b (Nat.succ k))) with
      | inl hyes =>
        exact Exists.elim hyes (fun k hk =>
          ⟨Nat.succ k, Nat.le.step hk.1, mint_converges K hK G hG (b k) (b (Nat.succ k)) a hk.2⟩)
      | inr hnot =>
        have hno : ∀ k, n0 ≤ k → Not (MintStepA K G a (b k) (b (Nat.succ k))) :=
          fun k hk hmint => hnot ⟨k, hk, hmint⟩
        have hq : gc_Quiet K G b a n0 :=
          ⟨fun k hk => gc_stepA_of K G a (b k) (b (Nat.succ k)) (hst k hk) (hno k hk), hB,
           fun k hk => stable_frame K G b n0 hst k hk⟩
        exact gc_q_law4 K hK G hG b a n0 hq hno hm hh hH ha hc

    -- ===========================================================================
    -- 10. Laws 3 and 4 of the guarded lab.
    -- ===========================================================================

    -- Law 4 (epoch convergence, no lockout): in every run of the guarded lab whose
    -- membership is stable from n0 (one-mesh §9's ◇□ quiet membership), with weakly
    -- fair heals (HealFair) and re-keys of a (MintFair), and some current Mac no roster
    -- denies at n0, the seat audience a converges: no re-key is due there and every
    -- newest epoch of it wraps every current seat no roster denies. The guard
    -- (lab_holder_exists) is what keeps a holder of every newest epoch through the
    -- removals before n0.
    theorem law4_converges (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G)
        (b : Behavior World) (hr : Runs (LabSpec K G true) b) (a n0 : Nat) (ha : seatAud a = true)
        (hst : StableFrom K G b n0) (hh : HealFair K G b) (hm : MintFair K G b a)
        (hc : ∃ c, c < (b n0).nr ∧ cur K (b n0) c = true ∧ den G (b n0) c = false) :
        ∃ k, n0 ≤ k ∧ Converged K G (b k) a :=
      law4_of_holders K hK G hG b a n0 ha hst hh hm
        (fun k _ => lab_holder_exists K hK G hG b hr k)
        (fun k _ => lab_rcp_bound K G true b hr k) hc

    -- Law 3 (join liveness): a Mac r that an honest desk admits at step n (AdmitsR) and
    -- no roster denies, after which the membership is stable with weakly fair heals and
    -- re-keys of a, comes to hold every newest epoch of the seat audience a.
    theorem law3_joiner_holds (K : Kernels) (hK : Exact K) (G : GKernels) (hG : GExact G)
        (b : Behavior World) (hr : Runs (LabSpec K G true) b) (a n r : Nat) (ha : seatAud a = true)
        (hadm : AdmitsR K (b n) (b (Nat.succ n)) r) (hden : den G (b (Nat.succ n)) r = false)
        (hst : StableFrom K G b (Nat.succ n)) (hh : HealFair K G b) (hm : MintFair K G b a) :
        ∃ k, Nat.succ n ≤ k ∧ ∀ e, isHead (b k) a e = true → (b k).wr e r = true :=
      Exists.elim hadm (fun bm h1 => Exists.elim h1 (fun z hp => by
        have hw : b (Nat.succ n) = withTable (b n) (AtermMeshRevocation.addRow (b n).t bm r z) :=
          hp.2.2.2.2
        have hnr : r < (b (Nat.succ n)).nr := by rw [hw]; exact hp.1
        have hcur : cur K (b (Nat.succ n)) r = true := by
          rw [cur_eq K hK, hw]
          exact admitted_is_current (b n).t bm r z hp.2.2.1 hp.2.2.2.1
        exact Exists.elim (law4_converges K hK G hG b hr a (Nat.succ n) ha hst hh hm ⟨r, hnr, hcur, hden⟩)
          (fun k hk => by
            have hf := stable_frame K G b (Nat.succ n) hst k hk.1
            refine ⟨k, hk.1, fun e he => hk.2.2 e r he ?_ ?_ ?_⟩
            · rw [hf.2.1]; exact hnr
            · rw [gc_cur_tr K (b (Nat.succ n)) (b k) hf.1 r]; exact hcur
            · rw [gc_den_tr G (b (Nat.succ n)) (b k) hf.2.2.1 hf.2.2.2 r]; exact hden)))

    -- ===========================================================================
    -- 11. The answer sheet: 512 views (three seats, two [everyone] epochs; bits 0-4
    --     the Wraps (bit 2 seat 2's in both epochs), 5-6 the Removes, 7 whether epoch
    --     1 has epoch 0 in its past, 8 the two rosters' denies), each read through
    --     the spec kernels to a byte of eight decisions (three heals, the carrier,
    --     the due, the carry, two removals' strands). The Rust test `gap_heal::tests::answer_sheet_matches_
    --     the_island` recomputes it from P1's Table and gap_heal.rs's kernels and
    --     folds, and reads the words below from this file. Until TG12 checks the Rust
    --     folds' kernels in every build, this sheet is what holds the folds to the
    --     island; TG12 removes it.
    -- ===========================================================================

    def gridBefore (sequential : Bool) (a b : Nat) : Bool :=
      decide (a < b) && !(!sequential && (decide (a = 5) && decide (b = 6)))

    def pow2 : Nat → Nat
      | 0 => 1
      | Nat.succ n => 2 * pow2 n

    def gridTable (k : Nat) : Table :=
      { rows := 3, ractor := fun i => i, rrcp := fun i => i,
        rid := fun i => if i = 2 then 7 else 0,
        rms := if (k / 32) % 4 = 0 then 0 else 1,
        mactor := fun _ => if (k / 32) % 4 = 1 then 1 else 2,
        mid := fun _ => if (k / 32) % 4 = 3 then 4 else 10,
        mlive := fun _ => true, mquorum := fun _ => false,
        before := gridBefore (decide ((k / 128) % 2 = 1)) }

    def gridWorld (k : Nat) : World :=
      { t := gridTable k, nr := 3, ne := 2, aud := fun _ => 0, eid := fun e => 5 + e,
        sup := fun e => decide (e = 0) && decide ((k / 128) % 2 = 1),
        wr := fun e r => decide ((k / pow2 (if r = 2 then 2 else 3 * e + r)) % 2 = 1),
        denL := fun r => decide (r = 1) && decide ((k / 256) % 2 = 1),
        denO := fun r => decide (r = 2) && decide ((k / 256) % 2 = 1) }

    def gbit (b : Bool) (v : Nat) : Nat := if b = true then v else 0

    def gridByte (w : World) : Nat :=
      gbit (healW specK specG w 0 0 1) 1 + gbit (healW specK specG w 0 1 2) 2
        + gbit (healW specK specG w 1 0 0) 4 + gbit (carriedA specK specG w 0) 8
        + gbit (dueA specK w 0) 16 + gbit (carryW specK specG w (fun _ => false) 1) 32
        + gbit (strandsAny specK specG w 0 1 11 false) 64
        + gbit (strandsAny specK specG w 2 0 11 false) 128

    -- Eight views to a word, view 8w's byte first.
    def gridWordOf (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 256 * gridWordOf w n + gridByte (gridWorld (8 * w + n))

    def gridSheetFrom (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => gridWordOf w 8 :: gridSheetFrom (w + 1) n

    -- Views 0 to 511, as 64 words.
    def gridSheet : List Nat := gridSheetFrom 0 64

    theorem grid_sheet_is : gridSheet =
      [0xe0f1f4f028393c38, 0xf2abf6ba38293c38, 0xf0f16c7838392c38, 0xf2bb7e2a38393c28,
       0xd0d0d4d018181818, 0xd29ad69a18181818, 0xd0d0d4d018181818, 0xd29ad69a18181818,
       0xf0f1f4f070717470, 0xf0b9f4b870387438, 0xf0f17c7870717878, 0xf0b97c3870387838,
       0xe0f1f4f028393c38, 0xf2abf6ba38293c38, 0xf0f16c7838392c38, 0xf2bb7e2a38393c28,
       0xe0e0e0e028282828, 0xaaaaaaaa28282828, 0x6868686828282828, 0x2a2a2a2a28282828,
       0xd0d0d0d018181818, 0x9a9a9a9a18181818, 0xd0d0d0d018181818, 0x9a9a9a9a18181818,
       0xf0f0f0f070707070, 0xb8b8b8b838383838, 0x7878787878787878, 0x3838383838383838,
       0xe0e0e0e028282828, 0xaaaaaaaa28282828, 0x6868686828282828, 0x2a2a2a2a28282828,
       0xc0d0d4d000101410, 0xd088d49810081418, 0xd0d0445010100410, 0xd098540810181408,
       0xd0d0d4d010101410, 0xd098d49810181418, 0xd0d0d4d010101410, 0xd098d49810181418,
       0xd0d0d4d050505450, 0xd098d49850185418, 0xd0d0545050505450, 0xd098541850185418,
       0xc0d0d4d000101410, 0xd088d49810081418, 0xd0d0445010100410, 0xd098540810181408,
       0xc0c0c0c000000000, 0x8888888808080808, 0x4040404000000000, 0x0808080808080808,
       0xd0d0d0d010101010, 0x9898989818181818, 0xd0d0d0d010101010, 0x9898989818181818,
       0xd0d0d0d050505050, 0x9898989818181818, 0x5050505050505050, 0x1818181818181818,
       0xc0c0c0c000000000, 0x8888888808080808, 0x4040404000000000, 0x0808080808080808] := rfl
    end AtermMeshGapHeal
}

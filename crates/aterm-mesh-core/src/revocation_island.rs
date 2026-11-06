// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P1, revocation: the decision of `revocation.rs` and its laws, proved for every size by the
// kernel on every Trust build. The spec kernels are what revocation.rs's contract! kernels state
// they compute; the folds are its Table methods, one for one; the World of section 7b asks the
// kernels as parameters, and `revocation_binding_island.rs` instantiates it with the compiled
// Rust. `revocation::tests` recomputes section 10's answer sheet from the Rust and reads its
// words from this file.
clean {
    -- The decision's scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/revocation.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body (the E6 import Trust makes of it) equal to the definition here. They sit outside the namespace so a clause can name them.

    -- spec_takes: Remove j names row i's actor, stands (no quorum voided it), and is a
    -- quorum's (out for good: every row of the actor, whenever it entered) or does
    -- not lie in the row's entry's past (a Remove beats a concurrent Admit).
    def spec_takes (live namesActor outForGood removeBefore : Bool) : Bool :=
      live && (namesActor && (outForGood || !removeBefore))
    -- spec_precedes: a Remove that took a row lies in the causal past of a record.
    def spec_precedes (took before : Bool) : Bool := !took || before
    -- spec_restores: a row of the same recipient, not out itself, whose entry has every
    -- Remove that took the row in its past.
    def spec_restores (sameRecipient restorerOut everyTakerBefore : Bool) : Bool :=
      sameRecipient && (!restorerOut && everyTakerBefore)
    -- spec_row_revokes: a row taken out that no row restores revokes its recipient.
    def spec_row_revokes (takenOut holderAfter : Bool) : Bool := takenOut && !holderAfter
    -- spec_gate_row: an epoch keeps the row's recipient out when the row does not
    -- revoke or every Remove that took it lies in the epoch's past.
    def spec_gate_row (revokes everyTakerBefore : Bool) : Bool := !revokes || everyTakerBefore
    -- spec_current: a row that is not out binds the recipient, and nothing revokes it.
    def spec_current (boundLive revoked : Bool) : Bool := boundLive && !revoked
    -- spec_wraps_removed: a newest epoch holds a Wrap to a revoked recipient.
    def spec_wraps_removed (revoked wrapped : Bool) : Bool := revoked && wrapped
    -- spec_head_due: a newest epoch is due a rotation, and sealed under by no writer,
    -- when its gate is shut or it wraps a revoked recipient.
    def spec_head_due (gateOpen wrapsRevoked : Bool) : Bool := !gateOpen || wrapsRevoked

    namespace AtermMeshRevocation

    open AtermMeshTemporal

    -- ===========================================================================
    -- 0. Helpers. Clean's core has no List.any lemmas, so rows and Removes are
    --    Nat-indexed and folded by anyN and allN.
    -- ===========================================================================

    theorem dtrue {p : Prop} [d : Decidable p] (h : p) : decide p = true := by
      simp only [decide_eq_true_eq]; exact h
    theorem dfalse {p : Prop} [d : Decidable p] (h : Not p) : decide p = false := by
      cases hd : decide p with
      | false => rfl
      | true => exact absurd (of_decide_eq_true hd) h

    theorem lt_pred_of_ne (i n : Nat) (h : i < n + 1) (hne : Not (i = n)) : i < n := by omega
    theorem lt_zero_false (i : Nat) (h : i < 0) : False := by omega
    theorem lt_succ_of_lt' (i n : Nat) (h : i < n) : i < n + 1 := by omega
    theorem ne_of_lt' (i n : Nat) (h : i < n) : Not (i = n) := by
      intro heq
      rw [heq] at h
      omega

    def anyN : Nat → (Nat → Bool) → Bool
      | 0, _ => false
      | Nat.succ n, p => p n || anyN n p
    def allN : Nat → (Nat → Bool) → Bool
      | 0, _ => true
      | Nat.succ n, p => p n && allN n p

    theorem anyN_intro (p : Nat → Bool) : ∀ (n i : Nat), i < n → p i = true → anyN n p = true := by
      intro n
      induction n with
      | zero => intro i h _; exact False.elim (lt_zero_false i h)
      | succ n ih =>
        intro i h hp
        show (p n || anyN n p) = true
        by_cases hin : i = n
        · rw [hin] at hp; rw [hp]
        · rw [ih i (lt_pred_of_ne i n h hin) hp]; exact Bool.or_true _

    theorem anyN_elim (p : Nat → Bool) : ∀ (n : Nat), anyN n p = true → ∃ i, i < n ∧ p i = true := by
      intro n
      induction n with
      | zero => intro h; exact Bool.noConfusion h
      | succ n ih =>
        intro h
        cases hp : p n with
        | true => exact ⟨n, Nat.lt_succ_self n, hp⟩
        | false =>
          have h2 : anyN n p = true := by simp only [anyN, hp, Bool.false_or] at h; exact h
          exact Exists.elim (ih h2) (fun k hk => ⟨k, lt_succ_of_lt' k n hk.1, hk.2⟩)

    theorem anyN_false (p : Nat → Bool) (n : Nat) (h : ∀ i, i < n → p i = false) : anyN n p = false := by
      cases hd : anyN n p with
      | false => rfl
      | true =>
        exact Exists.elim (anyN_elim p n hd) (fun i hi => Bool.noConfusion (Eq.trans hi.2.symm (h i hi.1)))

    theorem allN_elim (p : Nat → Bool) : ∀ (n : Nat), allN n p = true → ∀ i, i < n → p i = true := by
      intro n
      induction n with
      | zero => intro _ i hi; exact False.elim (lt_zero_false i hi)
      | succ n ih =>
        intro h i hi
        have hb := Eq.mp (Bool.and_eq_true (p n) (allN n p)) h
        by_cases hin : i = n
        · subst hin; exact hb.1
        · exact ih hb.2 i (lt_pred_of_ne i n hi hin)

    theorem allN_intro (p : Nat → Bool) : ∀ (n : Nat), (∀ i, i < n → p i = true) → allN n p = true := by
      intro n
      induction n with
      | zero => intro _; rfl
      | succ n ih =>
        intro h
        have hn : p n = true := h n (Nat.lt_succ_self n)
        have hr : allN n p = true := ih (fun i hi => h i (lt_succ_of_lt' i n hi))
        simp only [allN, hn, hr, Bool.and_self]

    theorem allN_false_intro (p : Nat → Bool) (n i : Nat) (hi : i < n) (hp : p i = false) :
        allN n p = false := by
      cases ha : allN n p with
      | false => rfl
      | true => exact Bool.noConfusion (Eq.trans (allN_elim p n ha i hi).symm hp)

    theorem allN_false_elim (p : Nat → Bool) (n : Nat) (h : allN n p = false) :
        ∃ i, i < n ∧ p i = false := by
      cases hex : anyN n (fun i => !(p i)) with
      | true =>
        exact Exists.elim (anyN_elim _ n hex) (fun i hi => by
          have hn : (!(p i)) = true := hi.2
          cases hpi : p i with
          | false => exact ⟨i, hi.1, hpi⟩
          | true =>
            rw [hpi] at hn
            exact Bool.noConfusion hn)
      | false =>
        have hall : allN n p = true := by
          apply allN_intro
          intro i hi
          cases hpi : p i with
          | true => rfl
          | false =>
            have hthis : anyN n (fun i => !(p i)) = true := by
              apply anyN_intro _ n i hi
              show (!(p i)) = true
              rw [hpi]
            rw [hex] at hthis
            exact Bool.noConfusion hthis
        rw [hall] at h
        exact Bool.noConfusion h

    theorem anyN_congr (p q : Nat → Bool) : ∀ (n : Nat), (∀ i, i < n → p i = q i) → anyN n p = anyN n q := by
      intro n
      induction n with
      | zero => intro _; rfl
      | succ n ih =>
        intro h
        show (p n || anyN n p) = (q n || anyN n q)
        rw [h n (Nat.lt_succ_self n), ih (fun i hi => h i (lt_succ_of_lt' i n hi))]

    theorem allN_congr (p q : Nat → Bool) : ∀ (n : Nat), (∀ i, i < n → p i = q i) → allN n p = allN n q := by
      intro n
      induction n with
      | zero => intro _; rfl
      | succ n ih =>
        intro h
        show (p n && allN n p) = (q n && allN n q)
        rw [h n (Nat.lt_succ_self n), ih (fun i hi => h i (lt_succ_of_lt' i n hi))]

    theorem anyN_mono (p q : Nat → Bool) (n : Nat) (h : ∀ i, i < n → p i = true → q i = true)
        (hp : anyN n p = true) : anyN n q = true :=
      Exists.elim (anyN_elim p n hp) (fun i hi => anyN_intro q n i hi.1 (h i hi.1 hi.2))

    theorem allN_mono (p q : Nat → Bool) (n : Nat) (h : ∀ i, i < n → p i = true → q i = true)
        (hp : allN n p = true) : allN n q = true :=
      allN_intro q n (fun i hi => h i hi (allN_elim p n hp i hi))

    -- Two folds over differently enumerated sets agree when each witness of one
    -- has a witness in the other (law 1: arrival order and duplicates play no role).
    theorem anyN_match (p q : Nat → Bool) (n m : Nat)
        (hpq : ∀ i, i < n → p i = true → ∃ k, k < m ∧ q k = true)
        (hqp : ∀ k, k < m → q k = true → ∃ i, i < n ∧ p i = true) :
        anyN n p = anyN m q := by
      cases hp : anyN n p with
      | true =>
        exact Exists.elim (anyN_elim p n hp) (fun i hi =>
          Exists.elim (hpq i hi.1 hi.2) (fun k hk => (anyN_intro q m k hk.1 hk.2).symm))
      | false =>
        cases hq : anyN m q with
        | false => rfl
        | true =>
          exact Exists.elim (anyN_elim q m hq) (fun k hk =>
            Exists.elim (hqp k hk.1 hk.2) (fun i hi =>
              Bool.noConfusion (Eq.trans (anyN_intro p n i hi.1 hi.2).symm hp)))

    theorem allN_match (p q : Nat → Bool) (n m : Nat)
        (hpq : ∀ i, i < n → p i = false → ∃ k, k < m ∧ q k = false)
        (hqp : ∀ k, k < m → q k = false → ∃ i, i < n ∧ p i = false) :
        allN n p = allN m q := by
      cases hp : allN n p with
      | false =>
        exact Exists.elim (allN_false_elim p n hp) (fun i hi =>
          Exists.elim (hpq i hi.1 hi.2) (fun k hk => (allN_false_intro q m k hk.1 hk.2).symm))
      | true =>
        cases hq : allN m q with
        | true => rfl
        | false =>
          exact Exists.elim (allN_false_elim q m hq) (fun k hk =>
            Exists.elim (hqp k hk.1 hk.2) (fun i hi =>
              Bool.noConfusion (Eq.trans hp.symm (allN_false_intro p n i hi.1 hi.2))))

    theorem anyN_succ_of (p : Nat → Bool) (n : Nat) (h : anyN n p = true) : anyN (Nat.succ n) p = true :=
      Exists.elim (anyN_elim p n h) (fun i hi => anyN_intro p (Nat.succ n) i (lt_succ_of_lt' i n hi.1) hi.2)

    theorem allN_of_succ (p : Nat → Bool) (n : Nat) (h : allN (Nat.succ n) p = true) : allN n p = true :=
      allN_intro p n (fun i hi => allN_elim p (Nat.succ n) h i (lt_succ_of_lt' i n hi))

    -- ===========================================================================
    -- 1. The decision. The folds over a view ask the spec kernels above; they are
    --    revocation.rs's Table methods, one for one (anyN and allN are left folds
    --    over the indices, as the Table's any_n and all_n are).
    -- ===========================================================================

    -- One Mac's view, as the evaluator reads it: every bound seat row (a founder's
    -- genesis binding or a binding Admit; superseded seats and the rows of Macs now
    -- out included), every Remove (quorum shares and the node-key removals
    -- included; live false: a quorum voided it), and the causal order.
    structure Table where
      rows   : Nat
      ractor : Nat → Nat      -- the row's MemberId (writer and incarnation), dense
      rrcp   : Nat → Nat      -- the row's age recipient, dense
      rid    : Nat → Nat      -- the row's entry record
      rms    : Nat
      mactor : Nat → Nat      -- the Remove's target
      mid    : Nat → Nat      -- the Remove record
      mlive  : Nat → Bool     -- false: a quorum voided the Remove (VoidedIn)
      mquorum : Nat → Bool    -- an accepted quorum's share: its actor is out for good
      before : Nat → Nat → Bool   -- before a b: a lies in b's causal past (Reach), never index order

    def takes (t : Table) (j i : Nat) : Bool :=
      spec_takes (t.mlive j) (decide (t.mactor j = t.ractor i)) (t.mquorum j) (t.before (t.mid j) (t.rid i))

    def isOut (t : Table) (i : Nat) : Bool := anyN t.rms (fun j => takes t j i)

    -- every Remove that took row i lies in record x's past
    def takersBefore (t : Table) (i x : Nat) : Bool :=
      allN t.rms (fun j => spec_precedes (takes t j i) (t.before (t.mid j) x))

    def restores (t : Table) (i m : Nat) : Bool :=
      spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut t m) (takersBefore t i (t.rid m))

    def restored (t : Table) (i : Nat) : Bool := anyN t.rows (fun m => restores t i m)

    def revokes (t : Table) (i : Nat) : Bool := spec_row_revokes (isOut t i) (restored t i)

    -- Membership::removed_recipients
    def revoked (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun i => decide (t.rrcp i = r) && revokes t i)

    def boundLive (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun m => decide (t.rrcp m = r) && !(isOut t m))

    -- Log::current_recipients
    def current (t : Table) (r : Nat) : Bool := spec_current (boundLive t r) (revoked t r)

    -- The writer's gate and the re-key due (one rule): every Remove that took out a
    -- row whose recipient stays revoked lies in the epoch's causal past.
    def gate (t : Table) (e : Nat) : Bool :=
      allN t.rows (fun i => spec_gate_row (revokes t i) (takersBefore t i e))

    -- ===========================================================================
    -- 2. The decision is exact (all sizes)
    -- ===========================================================================

    theorem row_revokes_exact (a b : Bool) : spec_row_revokes a b = (a && !b) := rfl

    -- Completeness: a row some Remove took out, that no row restores, revokes its
    -- recipient. A superseded seat is such a row (the A15 bug).
    theorem revoked_complete (t : Table) (i : Nat) (hi : i < t.rows)
        (hout : isOut t i = true) (hnr : ∀ m, m < t.rows → restores t i m = false) :
        revoked t (t.rrcp i) = true := by
      have hany : restored t i = false := anyN_false _ _ hnr
      apply anyN_intro _ t.rows i hi
      show (decide (t.rrcp i = t.rrcp i) && spec_row_revokes (isOut t i) (restored t i)) = true
      rw [hout, hany, dtrue (Eq.refl (t.rrcp i))]
      rfl

    theorem revokes_parts (t : Table) (i : Nat) (h : revokes t i = true) :
        isOut t i = true ∧ restored t i = false := by
      have hb : isOut t i = true ∧ (!(restored t i)) = true := Eq.mp (Bool.and_eq_true _ _) h
      refine ⟨hb.1, ?_⟩
      cases hr : restored t i with
      | false => rfl
      | true =>
        have hn := hb.2
        rw [hr] at hn
        exact Bool.noConfusion hn

    -- Soundness: nothing else is revoked (no lockout of a current seat).
    theorem revoked_sound (t : Table) (r : Nat) (h : revoked t r = true) :
        ∃ i, i < t.rows ∧ t.rrcp i = r ∧ isOut t i = true ∧ ∀ m, m < t.rows → restores t i m = false :=
      Exists.elim (anyN_elim _ t.rows h) (fun i hi =>
        have hb : decide (t.rrcp i = r) = true ∧ revokes t i = true :=
          Eq.mp (Bool.and_eq_true _ _) hi.2
        have hp := revokes_parts t i hb.2
        ⟨i, hi.1, of_decide_eq_true hb.1, hp.1, fun m hm => by
          cases hres : restores t i m with
          | false => rfl
          | true =>
            have hall : restored t i = true := anyN_intro _ t.rows m hm hres
            rw [hp.2] at hall
            exact Bool.noConfusion hall⟩)

    theorem restores_parts (t : Table) (i m : Nat) (h : restores t i m = true) :
        t.rrcp m = t.rrcp i ∧ isOut t m = false ∧ takersBefore t i (t.rid m) = true := by
      have hb : decide (t.rrcp m = t.rrcp i) = true
          ∧ (!(isOut t m) && takersBefore t i (t.rid m)) = true := Eq.mp (Bool.and_eq_true _ _) h
      have hb2 : (!(isOut t m)) = true ∧ takersBefore t i (t.rid m) = true :=
        Eq.mp (Bool.and_eq_true _ _) hb.2
      refine ⟨of_decide_eq_true hb.1, ?_, hb2.2⟩
      cases ho : isOut t m with
      | false => rfl
      | true =>
        have hn := hb2.1
        rw [ho] at hn
        exact Bool.noConfusion hn

    theorem takers_before_elim (t : Table) (i x j : Nat) (h : takersBefore t i x = true)
        (hj : j < t.rms) (htk : takes t j i = true) : t.before (t.mid j) x = true := by
      have hjj : spec_precedes (takes t j i) (t.before (t.mid j) x) = true := allN_elim _ t.rms h j hj
      rw [htk] at hjj
      exact hjj

    theorem restores_after (t : Table) (i m j : Nat) (h : restores t i m = true) (hj : j < t.rms)
        (htk : takes t j i = true) : t.before (t.mid j) (t.rid m) = true :=
      takers_before_elim t i (t.rid m) j (restores_parts t i m h).2.2 hj htk

    theorem restores_rcp_ne (t : Table) (i m : Nat) (h : Not (t.rrcp m = t.rrcp i)) :
        restores t i m = false := by
      show (decide (t.rrcp m = t.rrcp i) && (!(isOut t m) && takersBefore t i (t.rid m))) = false
      rw [dfalse h]

    -- A mint wraps only current recipients, so it excludes every revoked one.
    theorem current_not_revoked (t : Table) (r : Nat) (h : current t r = true) : revoked t r = false := by
      have hb : boundLive t r = true ∧ (!(revoked t r)) = true := Eq.mp (Bool.and_eq_true _ _) h
      cases hr : revoked t r with
      | false => rfl
      | true =>
        have hn := hb.2
        rw [hr] at hn
        exact Bool.noConfusion hn

    -- ===========================================================================
    -- 3. Steps on one view (all sizes)
    -- ===========================================================================

    -- A Remove of actor a lands (record x; q: an accepted quorum's share).
    def addRemove (t : Table) (a x : Nat) (q : Bool) : Table :=
      { t with rms := Nat.succ t.rms,
               mactor := fun j => if j = t.rms then a else t.mactor j,
               mid := fun j => if j = t.rms then x else t.mid j,
               mlive := fun j => if j = t.rms then true else t.mlive j,
               mquorum := fun j => if j = t.rms then q else t.mquorum j }

    -- An Admit lands: a new row (actor b, recipient r, entry record z).
    def addRow (t : Table) (b r z : Nat) : Table :=
      { t with rows := Nat.succ t.rows,
               ractor := fun i => if i = t.rows then b else t.ractor i,
               rrcp := fun i => if i = t.rows then r else t.rrcp i,
               rid := fun i => if i = t.rows then z else t.rid i }

    -- A quorum voids Remove j (VoidedIn: M3's undo of a mass removal).
    def voidRemove (t : Table) (j : Nat) : Table :=
      { t with mlive := fun k => if k = j then false else t.mlive k }

    -- P1, the A15 law: once a Remove of actor a lands, every row of a revokes its
    -- recipient, whatever Admit came before it (a superseding seat, a restore).
    theorem remove_revokes_every_row (t : Table) (a x : Nat) (q : Bool)
        (hfresh : ∀ m, m < t.rows → t.before x (t.rid m) = false)
        (i : Nat) (hi : i < t.rows) (ha : t.ractor i = a) :
        revoked (addRemove t a x q) (t.rrcp i) = true := by
      have hm : (addRemove t a x q).mactor t.rms = a := if_pos rfl
      have hx : (addRemove t a x q).mid t.rms = x := if_pos rfl
      have hl : (addRemove t a x q).mlive t.rms = true := if_pos rfl
      have hq : (addRemove t a x q).mquorum t.rms = q := if_pos rfl
      have htk : takes (addRemove t a x q) t.rms i = true := by
        show ((addRemove t a x q).mlive t.rms && (decide ((addRemove t a x q).mactor t.rms = t.ractor i)
              && ((addRemove t a x q).mquorum t.rms || !(t.before ((addRemove t a x q).mid t.rms) (t.rid i))))) = true
        rw [hl, hm, hx, hq, hfresh i hi, dtrue ha.symm]
        cases q <;> rfl
      have hout : isOut (addRemove t a x q) i = true :=
        anyN_intro _ (Nat.succ t.rms) t.rms (Nat.lt_succ_self t.rms) htk
      apply revoked_complete (addRemove t a x q) i hi hout
      intro m hm2
      cases hres : restores (addRemove t a x q) i m with
      | false => rfl
      | true =>
        have hafter := restores_after (addRemove t a x q) i m t.rms hres (Nat.lt_succ_self t.rms) htk
        rw [hx] at hafter
        have hbx : t.before x (t.rid m) = false := hfresh m hm2
        have hbt : t.before x (t.rid m) = true := hafter
        exact Bool.noConfusion (Eq.trans hbt.symm hbx)

    -- An Admit signed while holding every Remove restores its own recipient (a
    -- person's same-key Allow, a proof's Replace with old == new).
    theorem admit_restores_its_recipient (t : Table) (b r z : Nat)
        (hpast : ∀ j, j < t.rms → t.before (t.mid j) z = true)
        (hfree : ∀ j, j < t.rms → t.mquorum j = true → Not (t.mactor j = b)) :
        revoked (addRow t b r z) r = false := by
      have hrc : (addRow t b r z).rrcp t.rows = r := if_pos rfl
      have hrz : (addRow t b r z).rid t.rows = z := if_pos rfl
      have hnew_notout : isOut (addRow t b r z) t.rows = false := by
        apply anyN_false
        intro j hj
        have hrb : (addRow t b r z).ractor t.rows = b := if_pos rfl
        show (t.mlive j && (decide (t.mactor j = (addRow t b r z).ractor t.rows)
              && (t.mquorum j || !(t.before (t.mid j) ((addRow t b r z).rid t.rows))))) = false
        rw [hrz, hpast j hj, hrb]
        cases hq : t.mquorum j
        · cases t.mlive j <;> cases decide (t.mactor j = b) <;> rfl
        · rw [dfalse (hfree j hj hq)]
          cases t.mlive j <;> rfl
      show anyN (Nat.succ t.rows) (fun i => decide ((addRow t b r z).rrcp i = r) && revokes (addRow t b r z) i) = false
      apply anyN_false
      intro i hi
      show (decide ((addRow t b r z).rrcp i = r) && revokes (addRow t b r z) i) = false
      cases hd : decide ((addRow t b r z).rrcp i = r) with
      | false => rfl
      | true =>
        have hrcp : (addRow t b r z).rrcp i = r := of_decide_eq_true hd
        have hall : takersBefore (addRow t b r z) i ((addRow t b r z).rid t.rows) = true := by
          apply allN_intro
          intro j hj
          show (!(takes (addRow t b r z) j i) || t.before (t.mid j) ((addRow t b r z).rid t.rows)) = true
          rw [hrz, hpast j hj]
          exact Bool.or_true _
        have hrest : restores (addRow t b r z) i t.rows = true := by
          show (decide ((addRow t b r z).rrcp t.rows = (addRow t b r z).rrcp i)
                && (!(isOut (addRow t b r z) t.rows)
                && takersBefore (addRow t b r z) i ((addRow t b r z).rid t.rows))) = true
          rw [hall, hnew_notout, hrc, hrcp, dtrue (Eq.refl r)]
        have hany : restored (addRow t b r z) i = true :=
          anyN_intro _ (Nat.succ t.rows) t.rows (Nat.lt_succ_self t.rows) hrest
        show (true && spec_row_revokes (isOut (addRow t b r z) i) (restored (addRow t b r z) i)) = false
        rw [hany]
        show (true && (isOut (addRow t b r z) i && false)) = false
        cases isOut (addRow t b r z) i <;> rfl

    -- On the rows and Removes a step keeps, takes, isOut and restores keep their
    -- answers; addRow keeps every Remove.
    theorem addRow_takes (t : Table) (b r z j i : Nat) (hi : i < t.rows) :
        takes (addRow t b r z) j i = takes t j i := by
      have hoa : (addRow t b r z).ractor i = t.ractor i := if_neg (ne_of_lt' i t.rows hi)
      have hoi : (addRow t b r z).rid i = t.rid i := if_neg (ne_of_lt' i t.rows hi)
      show (t.mlive j && (decide (t.mactor j = (addRow t b r z).ractor i)
            && (t.mquorum j || !(t.before (t.mid j) ((addRow t b r z).rid i)))))
         = (t.mlive j && (decide (t.mactor j = t.ractor i) && (t.mquorum j || !(t.before (t.mid j) (t.rid i)))))
      rw [hoa, hoi]

    theorem addRow_isOut (t : Table) (b r z i : Nat) (hi : i < t.rows) :
        isOut (addRow t b r z) i = isOut t i :=
      anyN_congr _ _ t.rms (fun j _ => addRow_takes t b r z j i hi)

    theorem addRow_takersBefore (t : Table) (b r z i x : Nat) (hi : i < t.rows) :
        takersBefore (addRow t b r z) i x = takersBefore t i x :=
      allN_congr _ _ t.rms (fun j _ => by
        show spec_precedes (takes (addRow t b r z) j i) (t.before (t.mid j) x)
           = spec_precedes (takes t j i) (t.before (t.mid j) x)
        rw [addRow_takes t b r z j i hi])

    theorem addRow_restores (t : Table) (b r z i m : Nat) (hi : i < t.rows) (hm : m < t.rows) :
        restores (addRow t b r z) i m = restores t i m := by
      have hold : ∀ k, k < t.rows → (addRow t b r z).rrcp k = t.rrcp k :=
        fun k hk => if_neg (ne_of_lt' k t.rows hk)
      have hoi : (addRow t b r z).rid m = t.rid m := if_neg (ne_of_lt' m t.rows hm)
      show spec_restores (decide ((addRow t b r z).rrcp m = (addRow t b r z).rrcp i))
            (isOut (addRow t b r z) m) (takersBefore (addRow t b r z) i ((addRow t b r z).rid m))
         = spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut t m) (takersBefore t i (t.rid m))
      rw [hold m hm, hold i hi, addRow_isOut t b r z m hm, hoi, addRow_takersBefore t b r z i (t.rid m) hi]

    -- The new row is the one possible new restorer of an old row.
    theorem addRow_revokes (t : Table) (b r z i : Nat) (hi : i < t.rows) :
        revokes (addRow t b r z) i
          = spec_row_revokes (isOut t i) (restores (addRow t b r z) i t.rows || restored t i) := by
      show spec_row_revokes (isOut (addRow t b r z) i)
            (restores (addRow t b r z) i t.rows || anyN t.rows (fun m => restores (addRow t b r z) i m))
         = spec_row_revokes (isOut t i) (restores (addRow t b r z) i t.rows || restored t i)
      rw [addRow_isOut t b r z i hi,
          anyN_congr _ (fun m => restores t i m) t.rows (fun m hm => addRow_restores t b r z i m hi hm)]
      rfl

    -- The A15 law: a later Admit (a seat move, `allow --again` of a kept node key,
    -- a proof's Replace) restores only the recipient it binds. A moved seat's old
    -- recipient stays revoked until an epoch that postdates its Remove is minted.
    theorem admit_restores_only_its_recipient (t : Table) (b r z r' : Nat) (hne : Not (r' = r)) :
        revoked (addRow t b r z) r' = revoked t r' := by
      have hnr : Not (r = r') := fun h => hne h.symm
      have hold : ∀ i, i < t.rows → (addRow t b r z).rrcp i = t.rrcp i :=
        fun i hi => if_neg (ne_of_lt' i t.rows hi)
      have hrc : (addRow t b r z).rrcp t.rows = r := if_pos rfl
      show ((decide ((addRow t b r z).rrcp t.rows = r') && revokes (addRow t b r z) t.rows)
            || anyN t.rows (fun i => decide ((addRow t b r z).rrcp i = r') && revokes (addRow t b r z) i))
         = anyN t.rows (fun i => decide (t.rrcp i = r') && revokes t i)
      rw [hrc, dfalse hnr]
      show anyN t.rows (fun i => decide ((addRow t b r z).rrcp i = r') && revokes (addRow t b r z) i)
         = anyN t.rows (fun i => decide (t.rrcp i = r') && revokes t i)
      apply anyN_congr
      intro i hi
      show (decide ((addRow t b r z).rrcp i = r') && revokes (addRow t b r z) i)
         = (decide (t.rrcp i = r') && revokes t i)
      rw [hold i hi]
      cases hd : decide (t.rrcp i = r') with
      | false => rfl
      | true =>
        have hri : t.rrcp i = r' := of_decide_eq_true hd
        have hneq : Not ((addRow t b r z).rrcp t.rows = (addRow t b r z).rrcp i) := by
          rw [hrc, hold i hi, hri]
          exact hnr
        have hnew : restores (addRow t b r z) i t.rows = false := restores_rcp_ne _ i t.rows hneq
        show (true && revokes (addRow t b r z) i) = (true && revokes t i)
        rw [addRow_revokes t b r z i hi, hnew]
        rfl

    -- ===========================================================================
    -- 4. Finality: Removes only add revocations; a revocation lifts only by a new
    --    row that restores it or by a quorum's void (VoidedIn).
    -- ===========================================================================

    theorem addRemove_takes_old (t : Table) (a x : Nat) (q : Bool) (j i : Nat) (hj : j < t.rms) :
        takes (addRemove t a x q) j i = takes t j i := by
      have hne : Not (j = t.rms) := ne_of_lt' j t.rms hj
      have h1 : (addRemove t a x q).mlive j = t.mlive j := if_neg hne
      have h2 : (addRemove t a x q).mactor j = t.mactor j := if_neg hne
      have h3 : (addRemove t a x q).mid j = t.mid j := if_neg hne
      have h4 : (addRemove t a x q).mquorum j = t.mquorum j := if_neg hne
      show ((addRemove t a x q).mlive j && (decide ((addRemove t a x q).mactor j = t.ractor i)
            && ((addRemove t a x q).mquorum j || !(t.before ((addRemove t a x q).mid j) (t.rid i)))))
         = (t.mlive j && (decide (t.mactor j = t.ractor i) && (t.mquorum j || !(t.before (t.mid j) (t.rid i)))))
      rw [h1, h2, h3, h4]

    theorem addRemove_isOut_grows (t : Table) (a x : Nat) (q : Bool) (i : Nat) (h : isOut t i = true) :
        isOut (addRemove t a x q) i = true := by
      have heq : anyN t.rms (fun j => takes (addRemove t a x q) j i) = isOut t i :=
        anyN_congr _ _ t.rms (fun j hj => addRemove_takes_old t a x q j i hj)
      have hin : anyN t.rms (fun j => takes (addRemove t a x q) j i) = true := Eq.trans heq h
      exact anyN_succ_of _ t.rms hin

    theorem addRemove_restores_shrinks (t : Table) (a x : Nat) (q : Bool) (i m : Nat)
        (h : restores (addRemove t a x q) i m = true) : restores t i m = true := by
      have hp := restores_parts (addRemove t a x q) i m h
      have hout : isOut t m = false := by
        cases ho : isOut t m with
        | false => rfl
        | true =>
          have hg := addRemove_isOut_grows t a x q m ho
          rw [hp.2.1] at hg
          exact Bool.noConfusion hg
      have hold : allN t.rms (fun j => spec_precedes (takes (addRemove t a x q) j i)
                    ((addRemove t a x q).before ((addRemove t a x q).mid j) (t.rid m))) = true :=
        allN_of_succ _ t.rms hp.2.2
      have htb : takersBefore t i (t.rid m) = true := by
        have heq : allN t.rms (fun j => spec_precedes (takes (addRemove t a x q) j i)
                     ((addRemove t a x q).before ((addRemove t a x q).mid j) (t.rid m)))
                 = takersBefore t i (t.rid m) :=
          allN_congr _ _ t.rms (fun j hj => by
            have h3 : (addRemove t a x q).mid j = t.mid j := if_neg (ne_of_lt' j t.rms hj)
            show spec_precedes (takes (addRemove t a x q) j i) (t.before ((addRemove t a x q).mid j) (t.rid m))
               = spec_precedes (takes t j i) (t.before (t.mid j) (t.rid m))
            rw [addRemove_takes_old t a x q j i hj, h3])
        exact Eq.trans heq.symm hold
      show (decide (t.rrcp m = t.rrcp i) && (!(isOut t m) && takersBefore t i (t.rid m))) = true
      rw [dtrue hp.1, hout, htb]

    theorem addRemove_revokes_grows (t : Table) (a x : Nat) (q : Bool) (i : Nat) (h : revokes t i = true) :
        revokes (addRemove t a x q) i = true := by
      have hp := revokes_parts t i h
      have hout := addRemove_isOut_grows t a x q i hp.1
      have hnr : restored (addRemove t a x q) i = false := by
        apply anyN_false
        intro m hm
        show restores (addRemove t a x q) i m = false
        cases hr : restores (addRemove t a x q) i m with
        | false => rfl
        | true =>
          have hyes : restored t i = true := anyN_intro _ t.rows m hm (addRemove_restores_shrinks t a x q i m hr)
          rw [hp.2] at hyes
          exact Bool.noConfusion hyes
      show (isOut (addRemove t a x q) i && !(restored (addRemove t a x q) i)) = true
      rw [hout, hnr]

    theorem remove_keeps_row (t : Table) (a x : Nat) (q : Bool) (r i : Nat)
        (hi : (decide (t.rrcp i = r) && revokes t i) = true) :
        (decide (t.rrcp i = r) && revokes (addRemove t a x q) i) = true := by
      have hb : decide (t.rrcp i = r) = true ∧ revokes t i = true := Eq.mp (Bool.and_eq_true _ _) hi
      rw [hb.1, addRemove_revokes_grows t a x q i hb.2]

    theorem remove_keeps_revoked (t : Table) (a x : Nat) (q : Bool) (r : Nat) (h : revoked t r = true) :
        revoked (addRemove t a x q) r = true :=
      anyN_mono (fun i => decide (t.rrcp i = r) && revokes t i)
        (fun i => decide (t.rrcp i = r) && revokes (addRemove t a x q) i) t.rows
        (fun i _ hi => remove_keeps_row t a x q r i hi) h

    -- An Admit lifts a row's revocation only by restoring it.
    theorem admit_lifts_only_by_restoring (t : Table) (b r z i : Nat) (hi : i < t.rows)
        (hrv : revokes t i = true) (hnot : restores (addRow t b r z) i t.rows = false) :
        revokes (addRow t b r z) i = true := by
      rw [addRow_revokes t b r z i hi, hnot]
      exact hrv

    -- VoidedIn is the other way out: a quorum's void of the one Remove that took a
    -- seat lifts its recipient (eval.rs M3, `fleet undo`). Evaluated by the kernel.
    def oneRemoved : Table :=
      { rows := 1, ractor := fun _ => 0, rrcp := fun _ => 0, rid := fun _ => 0,
        rms := 1, mactor := fun _ => 0, mid := fun _ => 1, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }
    theorem removed_seat_revoked : revoked oneRemoved 0 = true := rfl
    theorem voided_remove_lifts : revoked (voidRemove oneRemoved 0) 0 = false := rfl

    -- A quorum's removal is out for good: its share takes every row of its actor,
    -- whenever that row entered, so no later Admit of the actor is current again
    -- (eval.rs `standing`: an actor in `out` is never a member). Another actor may
    -- still bind the recipient again, by a person's Allow.
    theorem quorum_takes_every_row (t : Table) (j i : Nat) (hj : j < t.rms) (hl : t.mlive j = true)
        (hq : t.mquorum j = true) (ha : t.mactor j = t.ractor i) : isOut t i = true :=
      anyN_intro _ t.rms j hj (by
        show (t.mlive j && (decide (t.mactor j = t.ractor i) && (t.mquorum j || !(t.before (t.mid j) (t.rid i))))) = true
        rw [hl, hq, dtrue ha])

    -- ===========================================================================
    -- 5. The gate, and the core of confidentiality (all sizes)
    -- ===========================================================================

    theorem gate_sound (t : Table) (e i j : Nat) (hg : gate t e = true) (hi : i < t.rows)
        (hrv : revokes t i = true) (hj : j < t.rms) (htk : takes t j i = true) :
        t.before (t.mid j) e = true := by
      have hrow : spec_gate_row (revokes t i) (takersBefore t i e) = true := allN_elim _ t.rows hg i hi
      rw [hrv] at hrow
      exact takers_before_elim t i e j hrow hj htk

    -- Progress, one step: an epoch minted by a Mac holding every Remove (its past is
    -- the minter's whole view) opens the gate, so the re-key clears the due there.
    theorem gate_of_postdating (t : Table) (e : Nat) (h : ∀ j, j < t.rms → t.before (t.mid j) e = true) :
        gate t e = true := by
      apply allN_intro
      intro i _
      have hall : takersBefore t i e = true := by
        apply allN_intro
        intro j hj
        show (!(takes t j i) || t.before (t.mid j) e) = true
        rw [h j hj]
        exact Bool.or_true _
      show (!(revokes t i) || takersBefore t i e) = true
      rw [hall]
      exact Bool.or_true _

    -- The gate is exactly its premises: an epoch whose causal past holds every
    -- Remove that took out a row which revokes its recipient has its gate open.
    -- With gate_sound, this is why Log::head_due may read the gate as the union of
    -- those Removes (Membership::revoking_removes) all lying in the epoch's past.
    theorem gate_of_takers (t : Table) (e : Nat)
        (h : ∀ i j, i < t.rows → revokes t i = true → j < t.rms → takes t j i = true →
          t.before (t.mid j) e = true) :
        gate t e = true := by
      apply allN_intro
      intro i hi
      show (!(revokes t i) || takersBefore t i e) = true
      cases hr : revokes t i
      · rfl
      · have hall : takersBefore t i e = true := by
          apply allN_intro
          intro j hj
          show (!(takes t j i) || t.before (t.mid j) e) = true
          cases ht : takes t j i
          · rfl
          · rw [h i j hi hr hj ht]
        rw [hall]

    -- The writer's rule and the due are one kernel: a head that is not due has its
    -- gate open, so gate_sound applies to every write sealed under it.
    theorem head_not_due_opens_gate (t : Table) (e : Nat) (w : Bool)
        (h : spec_head_due (gate t e) w = false) : gate t e = true := by
      cases hg : gate t e with
      | true => rfl
      | false =>
        rw [hg] at h
        exact Bool.noConfusion h

    -- An honest Wrap of e is signed only to a recipient current in its signer's
    -- view. If a writer sealed under e past its gate while row i stayed revoked,
    -- then in the view of any Mac that wraps e (it holds e, so every Remove in e's
    -- past), row i is out again, so some row there restores its recipient: an
    -- explicit later Admit of that key. Nothing else lets a removed recipient read
    -- what was sealed after its removal.
    theorem honest_wrap_needs_restorer (t1 t2 : Table) (e i1 i2 : Nat)
        (hbefore : t2.before = t1.before)
        (hra : t2.ractor i2 = t1.ractor i1) (hrid : t2.rid i2 = t1.rid i1)
        (hi1 : i1 < t1.rows) (hi2 : i2 < t2.rows)
        (hrv : revokes t1 i1 = true) (hg : gate t1 e = true)
        (hholds : ∀ j, j < t1.rms → t1.before (t1.mid j) e = true →
           ∃ j2, j2 < t2.rms ∧ t2.mactor j2 = t1.mactor j ∧ t2.mid j2 = t1.mid j ∧ t2.mlive j2 = t1.mlive j
             ∧ t2.mquorum j2 = t1.mquorum j)
        (hcur : current t2 (t2.rrcp i2) = true) :
        ∃ m, m < t2.rows ∧ restores t2 i2 m = true := by
      have hrvb := revokes_parts t1 i1 hrv
      exact Exists.elim (anyN_elim _ t1.rms hrvb.1) (fun j hjt =>
        have hbe : t1.before (t1.mid j) e = true := gate_sound t1 e i1 j hg hi1 hrv hjt.1 hjt.2
        Exists.elim (hholds j hjt.1 hbe) (fun j2 hj2 => by
          have htk2 : takes t2 j2 i2 = true := by
            show (t2.mlive j2 && (decide (t2.mactor j2 = t2.ractor i2)
                  && (t2.mquorum j2 || !(t2.before (t2.mid j2) (t2.rid i2))))) = true
            rw [hj2.2.2.2.1, hj2.2.2.2.2, hj2.2.1, hj2.2.2.1, hra, hrid, hbefore]
            exact hjt.2
          have hout2 : isOut t2 i2 = true := anyN_intro _ t2.rms j2 hj2.1 htk2
          have hnrev : revoked t2 (t2.rrcp i2) = false := current_not_revoked t2 _ hcur
          cases hex : restored t2 i2 with
          | true => exact anyN_elim _ t2.rows hex
          | false =>
            have hall : ∀ m, m < t2.rows → restores t2 i2 m = false := by
              intro m hm
              cases hr : restores t2 i2 m with
              | false => rfl
              | true =>
                have hthis : restored t2 i2 = true := anyN_intro _ t2.rows m hm hr
                rw [hex] at hthis
                exact Bool.noConfusion hthis
            have hyes := revoked_complete t2 i2 hi2 hout2 hall
            rw [hnrev] at hyes
            exact Bool.noConfusion hyes))

    -- ===========================================================================
    -- 6. Law 1 (determinism): the evaluation depends only on the record set and
    --    its causal order. Rows and Removes may be listed in any order, with
    --    duplicates, and the records may be numbered in any arrival order: two
    --    Macs holding the same records compute the same answers.
    -- ===========================================================================

    def RowMatch (t u : Table) (i k : Nat) : Prop :=
      u.ractor k = t.ractor i ∧ u.rrcp k = t.rrcp i ∧ u.rid k = t.rid i
    def RmMatch (t u : Table) (j l : Nat) : Prop :=
      u.mlive l = t.mlive j ∧ u.mquorum l = t.mquorum j ∧ u.mactor l = t.mactor j ∧ u.mid l = t.mid j
    -- The same rows and Removes (each of one has its like in the other), and the
    -- same causal order.
    def SameRecords (t u : Table) : Prop :=
      (∀ i, i < t.rows → ∃ k, k < u.rows ∧ RowMatch t u i k) ∧
      (∀ k, k < u.rows → ∃ i, i < t.rows ∧ RowMatch t u i k) ∧
      (∀ j, j < t.rms → ∃ l, l < u.rms ∧ RmMatch t u j l) ∧
      (∀ l, l < u.rms → ∃ j, j < t.rms ∧ RmMatch t u j l) ∧
      u.before = t.before

    theorem takes_match (t u : Table) (j l i k : Nat) (hm : RmMatch t u j l) (hr : RowMatch t u i k)
        (hb : u.before = t.before) : takes u l k = takes t j i := by
      show spec_takes (u.mlive l) (decide (u.mactor l = u.ractor k)) (u.mquorum l) (u.before (u.mid l) (u.rid k))
         = spec_takes (t.mlive j) (decide (t.mactor j = t.ractor i)) (t.mquorum j) (t.before (t.mid j) (t.rid i))
      rw [hm.1, hm.2.1, hm.2.2.1, hm.2.2.2, hr.1, hr.2.2, hb]

    theorem isOut_match (t u : Table) (h : SameRecords t u) (i k : Nat) (hr : RowMatch t u i k) :
        isOut u k = isOut t i :=
      (anyN_match (fun j => takes t j i) (fun l => takes u l k) t.rms u.rms
        (fun j hj htk => Exists.elim (h.2.2.1 j hj) (fun l hl =>
          ⟨l, hl.1, Eq.trans (takes_match t u j l i k hl.2 hr h.2.2.2.2) htk⟩))
        (fun l hl htk => Exists.elim (h.2.2.2.1 l hl) (fun j hj =>
          ⟨j, hj.1, Eq.trans (takes_match t u j l i k hj.2 hr h.2.2.2.2).symm htk⟩))).symm

    theorem takersBefore_match (t u : Table) (h : SameRecords t u) (i k x : Nat) (hr : RowMatch t u i k) :
        takersBefore u k x = takersBefore t i x := by
      have hp : ∀ j l, RmMatch t u j l →
          spec_precedes (takes u l k) (u.before (u.mid l) x) = spec_precedes (takes t j i) (t.before (t.mid j) x) :=
        fun j l hm => by
          rw [takes_match t u j l i k hm hr h.2.2.2.2, hm.2.2.2, h.2.2.2.2]
      exact (allN_match (fun j => spec_precedes (takes t j i) (t.before (t.mid j) x))
        (fun l => spec_precedes (takes u l k) (u.before (u.mid l) x)) t.rms u.rms
        (fun j hj hf => Exists.elim (h.2.2.1 j hj) (fun l hl => ⟨l, hl.1, Eq.trans (hp j l hl.2) hf⟩))
        (fun l hl hf => Exists.elim (h.2.2.2.1 l hl) (fun j hj => ⟨j, hj.1, Eq.trans (hp j l hj.2).symm hf⟩))).symm

    theorem restores_match (t u : Table) (h : SameRecords t u) (i k m p : Nat)
        (hik : RowMatch t u i k) (hmp : RowMatch t u m p) : restores u k p = restores t i m := by
      show spec_restores (decide (u.rrcp p = u.rrcp k)) (isOut u p) (takersBefore u k (u.rid p))
         = spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut t m) (takersBefore t i (t.rid m))
      rw [hmp.2.1, hik.2.1, isOut_match t u h m p hmp, hmp.2.2, takersBefore_match t u h i k (t.rid m) hik]

    theorem restored_match (t u : Table) (h : SameRecords t u) (i k : Nat) (hik : RowMatch t u i k) :
        restored u k = restored t i :=
      (anyN_match (fun m => restores t i m) (fun p => restores u k p) t.rows u.rows
        (fun m hm hres => Exists.elim (h.1 m hm) (fun p hp =>
          ⟨p, hp.1, Eq.trans (restores_match t u h i k m p hik hp.2) hres⟩))
        (fun p hp hres => Exists.elim (h.2.1 p hp) (fun m hm =>
          ⟨m, hm.1, Eq.trans (restores_match t u h i k m p hik hm.2).symm hres⟩))).symm

    theorem revokes_match (t u : Table) (h : SameRecords t u) (i k : Nat) (hik : RowMatch t u i k) :
        revokes u k = revokes t i := by
      show spec_row_revokes (isOut u k) (restored u k) = spec_row_revokes (isOut t i) (restored t i)
      rw [isOut_match t u h i k hik, restored_match t u h i k hik]

    theorem revoked_match (t u : Table) (h : SameRecords t u) (r : Nat) : revoked u r = revoked t r := by
      have hp : ∀ i k, RowMatch t u i k →
          (decide (u.rrcp k = r) && revokes u k) = (decide (t.rrcp i = r) && revokes t i) :=
        fun i k hik => by rw [hik.2.1, revokes_match t u h i k hik]
      exact (anyN_match (fun i => decide (t.rrcp i = r) && revokes t i)
        (fun k => decide (u.rrcp k = r) && revokes u k) t.rows u.rows
        (fun i hi hv => Exists.elim (h.1 i hi) (fun k hk => ⟨k, hk.1, Eq.trans (hp i k hk.2) hv⟩))
        (fun k hk hv => Exists.elim (h.2.1 k hk) (fun i hi => ⟨i, hi.1, Eq.trans (hp i k hi.2).symm hv⟩))).symm

    theorem current_match (t u : Table) (h : SameRecords t u) (r : Nat) : current u r = current t r := by
      have hp : ∀ i k, RowMatch t u i k →
          (decide (u.rrcp k = r) && !(isOut u k)) = (decide (t.rrcp i = r) && !(isOut t i)) :=
        fun i k hik => by rw [hik.2.1, isOut_match t u h i k hik]
      have hb : boundLive u r = boundLive t r :=
        (anyN_match (fun i => decide (t.rrcp i = r) && !(isOut t i))
          (fun k => decide (u.rrcp k = r) && !(isOut u k)) t.rows u.rows
          (fun i hi hv => Exists.elim (h.1 i hi) (fun k hk => ⟨k, hk.1, Eq.trans (hp i k hk.2) hv⟩))
          (fun k hk hv => Exists.elim (h.2.1 k hk) (fun i hi => ⟨i, hi.1, Eq.trans (hp i k hi.2).symm hv⟩))).symm
      show spec_current (boundLive u r) (revoked u r) = spec_current (boundLive t r) (revoked t r)
      rw [hb, revoked_match t u h r]

    theorem gate_match (t u : Table) (h : SameRecords t u) (e : Nat) : gate u e = gate t e := by
      have hp : ∀ i k, RowMatch t u i k →
          spec_gate_row (revokes u k) (takersBefore u k e) = spec_gate_row (revokes t i) (takersBefore t i e) :=
        fun i k hik => by rw [revokes_match t u h i k hik, takersBefore_match t u h i k e hik]
      exact (allN_match (fun i => spec_gate_row (revokes t i) (takersBefore t i e))
        (fun k => spec_gate_row (revokes u k) (takersBefore u k e)) t.rows u.rows
        (fun i hi hv => Exists.elim (h.1 i hi) (fun k hk => ⟨k, hk.1, Eq.trans (hp i k hk.2) hv⟩))
        (fun k hk hv => Exists.elim (h.2.1 k hk) (fun i hi => ⟨i, hi.1, Eq.trans (hp i k hi.2).symm hv⟩))).symm

    -- Numbering the records in another arrival order (any renaming pi that carries
    -- the causal order) changes no answer: entry and Remove records are read only
    -- through `before`.
    def relabel (t : Table) (pi : Nat → Nat) (before' : Nat → Nat → Bool) : Table :=
      { t with rid := fun i => pi (t.rid i), mid := fun j => pi (t.mid j), before := before' }

    theorem relabel_takes (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (j i : Nat) :
        takes (relabel t pi b') j i = takes t j i := by
      show spec_takes (t.mlive j) (decide (t.mactor j = t.ractor i)) (t.mquorum j) (b' (pi (t.mid j)) (pi (t.rid i)))
         = spec_takes (t.mlive j) (decide (t.mactor j = t.ractor i)) (t.mquorum j) (t.before (t.mid j) (t.rid i))
      rw [hpi]

    theorem relabel_isOut (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (i : Nat) :
        isOut (relabel t pi b') i = isOut t i :=
      anyN_congr _ _ t.rms (fun j _ => relabel_takes t pi b' hpi j i)

    theorem relabel_takersBefore (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (i x : Nat) :
        takersBefore (relabel t pi b') i (pi x) = takersBefore t i x :=
      allN_congr _ _ t.rms (fun j _ => by
        show spec_precedes (takes (relabel t pi b') j i) (b' (pi (t.mid j)) (pi x))
           = spec_precedes (takes t j i) (t.before (t.mid j) x)
        rw [relabel_takes t pi b' hpi j i, hpi])

    theorem relabel_revokes (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (i : Nat) :
        revokes (relabel t pi b') i = revokes t i := by
      have hres : ∀ m, restores (relabel t pi b') i m = restores t i m := fun m => by
        show spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut (relabel t pi b') m)
              (takersBefore (relabel t pi b') i (pi (t.rid m)))
           = spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut t m) (takersBefore t i (t.rid m))
        rw [relabel_isOut t pi b' hpi m, relabel_takersBefore t pi b' hpi i (t.rid m)]
      show spec_row_revokes (isOut (relabel t pi b') i) (anyN t.rows (fun m => restores (relabel t pi b') i m))
         = spec_row_revokes (isOut t i) (anyN t.rows (fun m => restores t i m))
      rw [relabel_isOut t pi b' hpi i, anyN_congr _ _ t.rows (fun m _ => hres m)]

    theorem relabel_revoked (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (r : Nat) :
        revoked (relabel t pi b') r = revoked t r :=
      anyN_congr _ _ t.rows (fun i _ => by
        show (decide (t.rrcp i = r) && revokes (relabel t pi b') i) = (decide (t.rrcp i = r) && revokes t i)
        rw [relabel_revokes t pi b' hpi i])

    theorem relabel_current (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (r : Nat) :
        current (relabel t pi b') r = current t r := by
      have hb : boundLive (relabel t pi b') r = boundLive t r :=
        anyN_congr _ _ t.rows (fun i _ => by
          show (decide (t.rrcp i = r) && !(isOut (relabel t pi b') i)) = (decide (t.rrcp i = r) && !(isOut t i))
          rw [relabel_isOut t pi b' hpi i])
      show spec_current (boundLive (relabel t pi b') r) (revoked (relabel t pi b') r)
         = spec_current (boundLive t r) (revoked t r)
      rw [hb, relabel_revoked t pi b' hpi r]

    theorem relabel_gate (t : Table) (pi : Nat → Nat) (b' : Nat → Nat → Bool)
        (hpi : ∀ x y, b' (pi x) (pi y) = t.before x y) (e : Nat) :
        gate (relabel t pi b') (pi e) = gate t e :=
      allN_congr _ _ t.rows (fun i _ => by
        show spec_gate_row (revokes (relabel t pi b') i) (takersBefore (relabel t pi b') i (pi e))
           = spec_gate_row (revokes t i) (takersBefore t i e)
        rw [relabel_revokes t pi b' hpi i, relabel_takersBefore t pi b' hpi i e])

    -- Law 1, whole: two Macs that hold the same records, listed and numbered in
    -- any order, answer the same.
    theorem evaluation_depends_only_on_records_and_order (t u : Table) (h : SameRecords t u)
        (pi : Nat → Nat) (b' : Nat → Nat → Bool) (hpi : ∀ x y, b' (pi x) (pi y) = u.before x y)
        (r e : Nat) :
        revoked (relabel u pi b') r = revoked t r ∧ current (relabel u pi b') r = current t r
          ∧ gate (relabel u pi b') (pi e) = gate t e :=
      ⟨Eq.trans (relabel_revoked u pi b' hpi r) (revoked_match t u h r),
       Eq.trans (relabel_current u pi b' hpi r) (current_match t u h r),
       Eq.trans (relabel_gate u pi b' hpi e) (gate_match t u h e)⟩

    -- ===========================================================================
    -- 7. Law 2: no automatic path restores a revoked recipient; only a person's
    --    Allow (or a quorum's void) does. TLA style: Spec |= [] [Unrevokes => ...].
    -- ===========================================================================

    -- The steps a Mac's view takes. A Remove (a person's, a proof's Replace, an
    -- automatic quorum share). An automatic Admit (the desk's home-LAN admission,
    -- a proof's re-admission): it refuses a recipient its view revokes. An
    -- automatic Replace (a Remove of the old seat, then the Admit of the new
    -- binding, in one batch): its new recipient is not revoked before the batch.
    -- A person's Allow binds any recipient. A quorum voids a Remove.
    def RemoveStep (t t' : Table) : Prop := ∃ a x q, t' = addRemove t a x q
    def AutoAdmit (t t' : Table) : Prop := ∃ b r z, revoked t r = false ∧ t' = addRow t b r z
    def AutoReplace (t t' : Table) : Prop :=
      ∃ a x b r z, revoked t r = false ∧ t' = addRow (addRemove t a x false) b r z
    def PersonAdmit (t t' : Table) : Prop := ∃ b r z, t' = addRow t b r z
    def VoidStep (t t' : Table) : Prop := ∃ j, t' = voidRemove t j

    def AutoNext (t t' : Table) : Prop := RemoveStep t t' ∨ AutoAdmit t t' ∨ AutoReplace t t'
    def Next (t t' : Table) : Prop := AutoNext t t' ∨ PersonAdmit t t' ∨ VoidStep t t'

    def PersonBinds (r : Nat) (t t' : Table) : Prop := ∃ b z, t' = addRow t b r z

    theorem auto_admit_keeps (t : Table) (b r z r' : Nat) (hg : revoked t r = false)
        (h : revoked t r' = true) : revoked (addRow t b r z) r' = true := by
      by_cases hrr : r' = r
      · rw [hrr] at h
        rw [hg] at h
        exact Bool.noConfusion h
      · exact Eq.trans (admit_restores_only_its_recipient t b r z r' hrr) h

    theorem auto_replace_keeps (t : Table) (a x b r0 z r : Nat) (hg : revoked t r0 = false)
        (h : revoked t r = true) : revoked (addRow (addRemove t a x false) b r0 z) r = true := by
      by_cases hrr : r = r0
      · rw [hrr] at h
        rw [hg] at h
        exact Bool.noConfusion h
      · exact Eq.trans (admit_restores_only_its_recipient (addRemove t a x false) b r0 z r hrr)
          (remove_keeps_revoked t a x false r h)

    theorem auto_step_keeps (r : Nat) (t t' : Table) (hn : AutoNext t t') (h : revoked t r = true) :
        revoked t' r = true := by
      cases hn with
      | inl hr =>
        cases hr with
        | intro a hx =>
          cases hx with
          | intro x hq =>
            cases hq with
            | intro q he =>
              rw [he]
              exact remove_keeps_revoked t a x q r h
      | inr hx =>
        cases hx with
        | inl ha =>
          cases ha with
          | intro b h1 =>
            cases h1 with
            | intro r0 h2 =>
              cases h2 with
              | intro z hz =>
                rw [hz.2]
                exact auto_admit_keeps t b r0 z r hz.1 h
        | inr hp =>
          cases hp with
          | intro a h1 =>
            cases h1 with
            | intro x h2 =>
              cases h2 with
              | intro b h3 =>
                cases h3 with
                | intro r0 h4 =>
                  cases h4 with
                  | intro z hz =>
                    rw [hz.2]
                    exact auto_replace_keeps t a x b r0 z r hz.1 h

    -- Law 2, invariant form: from any view where r is revoked, every run of
    -- automatic steps keeps it revoked.
    def AutoSpec (t0 : Table) : StateMachine Table := StateMachine.mk (fun t => t = t0) AutoNext

    theorem automatic_paths_keep_revocations (t0 : Table) (r : Nat) (h0 : revoked t0 r = true) :
        Satisfies (AutoSpec t0) (Always (Lift (fun t => revoked t r = true))) :=
      invariantSatisfies (AutoSpec t0) (fun t => revoked t r = true)
        (fun s hs => by
          have hs' : s = t0 := hs
          rw [hs']; exact h0)
        (fun s t hs hn => auto_step_keeps r s t hn hs)

    -- Law 2, action form: in every run of every step, a step that lifts r's
    -- revocation is a person's Allow binding r, or a quorum's void.
    def Spec (t0 : Table) : StateMachine Table := StateMachine.mk (fun t => t = t0) Next

    theorem next_unrevokes_only_by_person_or_void (r : Nat) (s t : Table) (hn : Next s t)
        (hs : revoked s r = true) (ht : revoked t r = false) : PersonBinds r s t ∨ VoidStep s t := by
      cases hn with
      | inl ha =>
        have hk := auto_step_keeps r s t ha hs
        rw [ht] at hk
        exact Bool.noConfusion hk
      | inr hx =>
        cases hx with
        | inl hp =>
          cases hp with
          | intro b h1 =>
            cases h1 with
            | intro r0 h2 =>
              cases h2 with
              | intro z hz =>
                by_cases hrr : r = r0
                · rw [← hrr] at hz
                  exact Or.inl ⟨b, z, hz⟩
                · have hk : revoked t r = true := by
                    rw [hz]
                    exact Eq.trans (admit_restores_only_its_recipient s b r0 z r hrr) hs
                  rw [ht] at hk
                  exact Bool.noConfusion hk
        | inr hv => exact Or.inr hv

    theorem only_a_person_or_a_void_restores (t0 : Table) (r : Nat) :
        Satisfies (Spec t0) (Always (Act (fun t t' =>
          revoked t r = true → revoked t' r = false → PersonBinds r t t' ∨ VoidStep t t'))) :=
      actionSatisfies (Spec t0) _
        (fun s t hn hs ht => next_unrevokes_only_by_person_or_void r s t hn hs ht)
        (fun s hs hs' => by
          rw [hs] at hs'
          exact Bool.noConfusion hs')

    -- ===========================================================================
    -- 7b. The World asks its decisions as parameters. Every fold above, and the
    --     steps of section 7, read through kernels K; Exact K says K computes the
    --     spec kernels. The binding island instantiates K with the compiled Rust
    --     kernels and discharges Exact by their k_is_exact theorems, so the laws
    --     below hold of the Rust decision.
    -- ===========================================================================

    structure Kernels where
      takes : Bool → Bool → Bool → Bool → Bool
      precedes : Bool → Bool → Bool
      restores : Bool → Bool → Bool → Bool
      rowRevokes : Bool → Bool → Bool
      gateRow : Bool → Bool → Bool
      current : Bool → Bool → Bool
      wrapsRemoved : Bool → Bool → Bool
      headDue : Bool → Bool → Bool

    def Exact (K : Kernels) : Prop :=
      (∀ a b c d, K.takes a b c d = spec_takes a b c d)
        ∧ (∀ a b, K.precedes a b = spec_precedes a b)
        ∧ (∀ a b c, K.restores a b c = spec_restores a b c)
        ∧ (∀ a b, K.rowRevokes a b = spec_row_revokes a b)
        ∧ (∀ a b, K.gateRow a b = spec_gate_row a b)
        ∧ (∀ a b, K.current a b = spec_current a b)
        ∧ (∀ a b, K.wrapsRemoved a b = spec_wraps_removed a b)
        ∧ (∀ a b, K.headDue a b = spec_head_due a b)

    def takesW (K : Kernels) (t : Table) (j i : Nat) : Bool :=
      K.takes (t.mlive j) (decide (t.mactor j = t.ractor i)) (t.mquorum j) (t.before (t.mid j) (t.rid i))
    def isOutW (K : Kernels) (t : Table) (i : Nat) : Bool := anyN t.rms (fun j => takesW K t j i)
    def takersBeforeW (K : Kernels) (t : Table) (i x : Nat) : Bool :=
      allN t.rms (fun j => K.precedes (takesW K t j i) (t.before (t.mid j) x))
    def restoresW (K : Kernels) (t : Table) (i m : Nat) : Bool :=
      K.restores (decide (t.rrcp m = t.rrcp i)) (isOutW K t m) (takersBeforeW K t i (t.rid m))
    def restoredW (K : Kernels) (t : Table) (i : Nat) : Bool := anyN t.rows (fun m => restoresW K t i m)
    def revokesW (K : Kernels) (t : Table) (i : Nat) : Bool := K.rowRevokes (isOutW K t i) (restoredW K t i)
    def revokedW (K : Kernels) (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun i => decide (t.rrcp i = r) && revokesW K t i)
    def boundLiveW (K : Kernels) (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun m => decide (t.rrcp m = r) && !(isOutW K t m))
    def currentW (K : Kernels) (t : Table) (r : Nat) : Bool := K.current (boundLiveW K t r) (revokedW K t r)
    def gateW (K : Kernels) (t : Table) (e : Nat) : Bool :=
      allN t.rows (fun i => K.gateRow (revokesW K t i) (takersBeforeW K t i e))
    def headDueW (K : Kernels) (t : Table) (e : Nat) (wrapsRevoked : Bool) : Bool :=
      K.headDue (gateW K t e) wrapsRevoked

    theorem takesW_eq (K : Kernels) (hK : Exact K) (t : Table) (j i : Nat) : takesW K t j i = takes t j i :=
      hK.1 _ _ _ _
    theorem isOutW_eq (K : Kernels) (hK : Exact K) (t : Table) (i : Nat) : isOutW K t i = isOut t i :=
      anyN_congr _ _ t.rms (fun j _ => takesW_eq K hK t j i)
    theorem takersBeforeW_eq (K : Kernels) (hK : Exact K) (t : Table) (i x : Nat) :
        takersBeforeW K t i x = takersBefore t i x :=
      allN_congr _ _ t.rms (fun j _ => by
        show K.precedes (takesW K t j i) (t.before (t.mid j) x) = spec_precedes (takes t j i) (t.before (t.mid j) x)
        rw [takesW_eq K hK t j i]
        exact hK.2.1 _ _)
    theorem restoresW_eq (K : Kernels) (hK : Exact K) (t : Table) (i m : Nat) :
        restoresW K t i m = restores t i m := by
      show K.restores (decide (t.rrcp m = t.rrcp i)) (isOutW K t m) (takersBeforeW K t i (t.rid m))
        = spec_restores (decide (t.rrcp m = t.rrcp i)) (isOut t m) (takersBefore t i (t.rid m))
      rw [isOutW_eq K hK t m, takersBeforeW_eq K hK t i (t.rid m)]
      exact hK.2.2.1 _ _ _
    theorem restoredW_eq (K : Kernels) (hK : Exact K) (t : Table) (i : Nat) : restoredW K t i = restored t i :=
      anyN_congr _ _ t.rows (fun m _ => restoresW_eq K hK t i m)
    theorem revokesW_eq (K : Kernels) (hK : Exact K) (t : Table) (i : Nat) : revokesW K t i = revokes t i := by
      show K.rowRevokes (isOutW K t i) (restoredW K t i) = spec_row_revokes (isOut t i) (restored t i)
      rw [isOutW_eq K hK t i, restoredW_eq K hK t i]
      exact hK.2.2.2.1 _ _
    theorem revokedW_eq (K : Kernels) (hK : Exact K) (t : Table) (r : Nat) : revokedW K t r = revoked t r :=
      anyN_congr _ _ t.rows (fun i _ => by
        show (decide (t.rrcp i = r) && revokesW K t i) = (decide (t.rrcp i = r) && revokes t i)
        rw [revokesW_eq K hK t i])
    theorem boundLiveW_eq (K : Kernels) (hK : Exact K) (t : Table) (r : Nat) : boundLiveW K t r = boundLive t r :=
      anyN_congr _ _ t.rows (fun m _ => by
        show (decide (t.rrcp m = r) && !(isOutW K t m)) = (decide (t.rrcp m = r) && !(isOut t m))
        rw [isOutW_eq K hK t m])
    theorem currentW_eq (K : Kernels) (hK : Exact K) (t : Table) (r : Nat) : currentW K t r = current t r := by
      show K.current (boundLiveW K t r) (revokedW K t r) = spec_current (boundLive t r) (revoked t r)
      rw [boundLiveW_eq K hK t r, revokedW_eq K hK t r]
      exact hK.2.2.2.2.2.1 _ _
    theorem gateW_eq (K : Kernels) (hK : Exact K) (t : Table) (e : Nat) : gateW K t e = gate t e :=
      allN_congr _ _ t.rows (fun i _ => by
        show K.gateRow (revokesW K t i) (takersBeforeW K t i e) = spec_gate_row (revokes t i) (takersBefore t i e)
        rw [revokesW_eq K hK t i, takersBeforeW_eq K hK t i e]
        exact hK.2.2.2.2.1 _ _)
    theorem headDueW_eq (K : Kernels) (hK : Exact K) (t : Table) (e : Nat) (w : Bool) :
        headDueW K t e w = spec_head_due (gate t e) w := by
      show K.headDue (gateW K t e) w = spec_head_due (gate t e) w
      rw [gateW_eq K hK t e]
      exact hK.2.2.2.2.2.2.2 _ _

    -- The World: a Mac's view, stepping by the steps of section 7, whose automatic
    -- Admit and Replace ask the kernels whether the recipient is revoked.
    def AutoAdmitW (K : Kernels) (t t' : Table) : Prop := ∃ b r z, revokedW K t r = false ∧ t' = addRow t b r z
    def AutoReplaceW (K : Kernels) (t t' : Table) : Prop :=
      ∃ a x b r z, revokedW K t r = false ∧ t' = addRow (addRemove t a x false) b r z
    def NextW (K : Kernels) (t t' : Table) : Prop :=
      (RemoveStep t t' ∨ AutoAdmitW K t t' ∨ AutoReplaceW K t t') ∨ PersonAdmit t t' ∨ VoidStep t t'
    def SpecW (K : Kernels) (t0 : Table) : StateMachine Table := StateMachine.mk (fun t => t = t0) (NextW K)

    theorem nextW_next (K : Kernels) (hK : Exact K) (s t : Table) (h : NextW K s t) : Next s t := by
      cases h with
      | inl ha =>
        cases ha with
        | inl hr => exact Or.inl (Or.inl hr)
        | inr hx =>
          cases hx with
          | inl hd =>
            cases hd with
            | intro b h1 =>
              cases h1 with
              | intro r h2 =>
                cases h2 with
                | intro z hz =>
                  have hg : revoked s r = false := Eq.trans (Eq.symm (revokedW_eq K hK s r)) hz.1
                  exact Or.inl (Or.inr (Or.inl ⟨b, r, z, hg, hz.2⟩))
          | inr hp =>
            cases hp with
            | intro a h1 =>
              cases h1 with
              | intro x h2 =>
                cases h2 with
                | intro b h3 =>
                  cases h3 with
                  | intro r h4 =>
                    cases h4 with
                    | intro z hz =>
                      have hg : revoked s r = false := Eq.trans (Eq.symm (revokedW_eq K hK s r)) hz.1
                      exact Or.inl (Or.inr (Or.inr ⟨a, x, b, r, z, hg, hz.2⟩))
      | inr hx => exact Or.inr hx

    -- Law 2 of the World that asks K: a step that lifts r's revocation is a
    -- person's Allow binding r, or a quorum's void.
    theorem law2_of_exact (K : Kernels) (hK : Exact K) (t0 : Table) (r : Nat) :
        Satisfies (SpecW K t0) (Always (Act (fun t t' =>
          revokedW K t r = true → revokedW K t' r = false → PersonBinds r t t' ∨ VoidStep t t'))) :=
      actionSatisfies (SpecW K t0) _
        (fun s t hn hs ht => next_unrevokes_only_by_person_or_void r s t (nextW_next K hK s t hn)
          (Eq.trans (Eq.symm (revokedW_eq K hK s r)) hs) (Eq.trans (Eq.symm (revokedW_eq K hK t r)) ht))
        (fun s hs hs' => by
          rw [hs] at hs'
          exact Bool.noConfusion hs')

    -- The writer's rule of the World that asks K, in every state of every run: a
    -- head the kernels call not due has every Remove that took a row which still
    -- revokes its recipient in its causal past, so nothing sealed under it reaches
    -- that recipient (section 5).
    theorem writer_rule_of_exact (K : Kernels) (hK : Exact K) (t0 : Table) :
        Satisfies (SpecW K t0) (Always (Lift (fun t => ∀ e w i j, headDueW K t e w = false →
          i < t.rows → revokesW K t i = true → j < t.rms → takesW K t j i = true →
          t.before (t.mid j) e = true))) :=
      invariantSatisfies (SpecW K t0) _
        (fun s _ e w i j hd hi hrv hj htk => gate_sound s e i j
          (head_not_due_opens_gate s e w (Eq.trans (Eq.symm (headDueW_eq K hK s e w)) hd)) hi
          (Eq.trans (Eq.symm (revokesW_eq K hK s i)) hrv) hj (Eq.trans (Eq.symm (takesW_eq K hK s j i)) htk))
        (fun _ t _ _ e w i j hd hi hrv hj htk => gate_sound t e i j
          (head_not_due_opens_gate t e w (Eq.trans (Eq.symm (headDueW_eq K hK t e w)) hd)) hi
          (Eq.trans (Eq.symm (revokesW_eq K hK t i)) hrv) hj (Eq.trans (Eq.symm (takesW_eq K hK t j i)) htk))

    -- The spec kernels are exact, so every law above holds of the spec World too.
    theorem spec_exact : Exact (Kernels.mk spec_takes spec_precedes spec_restores spec_row_revokes
        spec_gate_row spec_current spec_wraps_removed spec_head_due) :=
      ⟨fun _ _ _ _ => rfl, fun _ _ => rfl, fun _ _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl,
       fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl⟩

    -- ===========================================================================
    -- 8. Founders. eval.rs kept a revoked recipient that a waiting (unproved)
    --    founder lists. Graph::unproved lists a founder only when no bound seat
    --    of a member or of a removed Mac holds its recipient, and every row is
    --    such a seat, so that clause never fired: PF1 drops it, and this is why.
    -- ===========================================================================

    def revokedWithFounders (t : Table) (waiting : Nat → Bool) (r : Nat) : Bool :=
      revoked t r && !(waiting r)

    theorem waiting_founders_change_nothing (t : Table) (waiting : Nat → Bool) (r : Nat)
        (hunproved : ∀ i, i < t.rows → waiting (t.rrcp i) = false) :
        revokedWithFounders t waiting r = revoked t r := by
      cases hr : revoked t r with
      | false =>
        show (revoked t r && !(waiting r)) = false
        rw [hr]
      | true =>
        exact Exists.elim (revoked_sound t r hr) (fun i hi => by
          have hw : waiting r = false := by rw [← hi.2.1]; exact hunproved i hi.1
          show (revoked t r && !(waiting r)) = true
          rw [hr, hw])

    -- ===========================================================================
    -- 9. Negative controls, each evaluated by the kernel: every rule the decision
    --    replaced answers otherwise on a concrete view, so no proof of the laws
    --    above exists for it.
    -- ===========================================================================

    -- Row 0: the old seat (actor 0, recipient 0, entry record 0). Remove record 1
    -- of actor 0. Row 1: the seat that took its node key back (actor 1, a new
    -- recipient 1, entry record 2, after the Remove). The A15 shape.
    def moved : Table :=
      { rows := 2, ractor := fun i => i, rrcp := fun i => i, rid := fun i => if i = 0 then 0 else 2,
        rms := 1, mactor := fun _ => 0, mid := fun _ => 1, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }

    theorem moved_old_recipient_revoked : revoked moved 0 = true := rfl
    theorem moved_new_recipient_current : current moved 1 = true := rfl

    -- The rule before A15 (eval.rs `superseded_by`): a removed seat is skipped once
    -- another seat that is not out entered after every Remove that took it.
    def supersededB (t : Table) (i : Nat) : Bool :=
      anyN t.rows (fun m => !(decide (m = i)) && (!(isOut t m) && takersBefore t i (t.rid m)))
    def revokedPreA15 (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun i => decide (t.rrcp i = r) && (!(supersededB t i) && revokes t i))
    theorem pre_a15_misses_the_moved_seat : revokedPreA15 moved 0 = false := rfl

    -- The A15 run as steps: a seat (actor 0, recipient 0) is removed by record 1,
    -- then its node key comes back as actor 1 with recipient 1 (record 2).
    def base : Table :=
      { rows := 1, ractor := fun _ => 0, rrcp := fun _ => 0, rid := fun _ => 0,
        rms := 0, mactor := fun _ => 0, mid := fun _ => 0, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }
    def afterRemove : Table := addRemove base 0 1 false
    def afterMove : Table := addRow afterRemove 1 1 2

    theorem a15_law_instance : revoked afterMove 0 = revoked afterRemove 0 :=
      admit_restores_only_its_recipient afterRemove 1 1 2 0 (fun h => Nat.noConfusion h)
    theorem a15_removed_instance : revoked afterRemove 0 = true := rfl
    -- Mutant 1 (the rule before A15): the moved seat's recipient drops out of the
    -- revoked set when the Admit lands, contradicting a15_law_instance.
    theorem pre_a15_breaks_the_law :
        And (revokedPreA15 afterRemove 0 = true) (revokedPreA15 afterMove 0 = false) := ⟨rfl, rfl⟩

    -- Mutant 2 (A15's loop for superseded seats): any current seat that binds the
    -- recipient keeps it, whenever it entered. Row 1 (another incarnation,
    -- recipient 0) entered before the Remove of actor 0 (record 2).
    def coholder : Table :=
      { rows := 2, ractor := fun i => i, rrcp := fun _ => 0, rid := fun i => i,
        rms := 1, mactor := fun _ => 0, mid := fun _ => 2, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) }
    def restoresAny (t : Table) (i m : Nat) : Bool := decide (t.rrcp m = t.rrcp i) && !(isOut t m)
    def revokedAny (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun i => decide (t.rrcp i = r)
        && spec_row_revokes (isOut t i) (anyN t.rows (fun m => restoresAny t i m)))
    theorem coholder_revoked : revoked coholder 0 = true := rfl
    theorem any_holder_keeps_it : revokedAny coholder 0 = false := rfl

    -- Mutant 3 (eval.rs branch (a), index order): a seat of the same recipient
    -- whose entry has a higher index counts as newer. Records 1 (the Remove) and
    -- 2 (another Mac's Admit of recipient 0) are concurrent; only record 0 precedes
    -- them. The index rule keeps the recipient; the causal rule does not.
    def concurrent : Table :=
      { rows := 2, ractor := fun i => i, rrcp := fun _ => 0, rid := fun i => if i = 0 then 0 else 2,
        rms := 1, mactor := fun _ => 0, mid := fun _ => 1, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a = 0) && !(decide (b = 0)) }
    def revokedIdx (t : Table) (r : Nat) : Bool :=
      anyN t.rows (fun i => decide (t.rrcp i = r)
        && spec_row_revokes (isOut t i) (anyN t.rows (fun m =>
             decide (t.rrcp m = t.rrcp i) && (!(isOut t m) && decide (t.rid i < t.rid m)))))
    theorem concurrent_revoked : revoked concurrent 0 = true := rfl
    theorem index_rule_keeps_it : revokedIdx concurrent 0 = false := rfl

    -- Law 1's negative control: two seats of recipient 0 entered concurrently
    -- (records 1 and 2), and a Remove of the first (record 3) follows both. Two
    -- Macs that received records 1 and 2 in either order number them either way.
    -- The index rule answers by that numbering; the causal rule does not.
    def racing : Table :=
      { rows := 2, ractor := fun i => i, rrcp := fun _ => 0, rid := fun i => i + 1,
        rms := 1, mactor := fun _ => 0, mid := fun _ => 3, mlive := fun _ => true, mquorum := fun _ => false,
        before := fun a b => decide (a < b) && !(decide (a = 1) && decide (b = 2)) }
    def swap12 (x : Nat) : Nat := if x = 1 then 2 else (if x = 2 then 1 else x)
    def racingSwapped : Table := relabel racing swap12 (fun a b => racing.before (swap12 a) (swap12 b))
    theorem index_rule_depends_on_arrival :
        And (revokedIdx racing 0 = false) (revokedIdx racingSwapped 0 = true) := ⟨rfl, rfl⟩
    theorem causal_rule_does_not :
        And (revoked racing 0 = true) (revoked racingSwapped 0 = true) := ⟨rfl, rfl⟩

    -- The writer's gate before PF1 (log_entries.rs rotation_due): judged by the
    -- Wraps this Mac holds. Epoch record 0 (before the Remove) whose Wrap to the
    -- removed recipient has not arrived here: the held-wrap gate opens, the causal
    -- gate stays shut. Without `gate`, honest_wrap_needs_restorer has no premise.
    def wrapGate (t : Table) (wrapsHeld : Nat → Bool) : Bool :=
      allN t.rows (fun i => !(revokes t i) || !(wrapsHeld (t.rrcp i)))
    theorem held_wrap_gate_opens : wrapGate moved (fun r => decide (r = 1)) = true := rfl
    theorem causal_gate_stays_shut : gate moved 0 = false := rfl

    -- Law 2's negative control: an automatic Admit without the guard (it binds a
    -- recipient its view revokes, with the Remove in its past) restores it.
    theorem unguarded_auto_admit_restores :
        And (revoked afterRemove 0 = true) (revoked (addRow afterRemove 1 0 2) 0 = false) := ⟨rfl, rfl⟩

    -- ===========================================================================
    -- 10. The answer sheet. One byte per view of a fixed grid of 2048 views (two
    --     rows; no Remove, one, two, or two of which the second is void; shared
    --     and moved recipients; the same actor back or another; total and
    --     non-total causal orders, a Remove concurrent with an Admit; a quorum's
    --     share or a plain Remove). The Rust test
    --     `revocation::tests::answer_sheet_matches_the_island` recomputes it from
    --     the Rust Table and reads the words below from this file. Until TG12
    --     checks the Rust folds' kernels in every build, this sheet is what holds
    --     the folds to the island; TG12 removes it.
    -- ===========================================================================

    def gridBefore (shape a b : Nat) : Bool :=
      decide (a < b)
        && (!(decide (shape % 2 = 1) && (decide (a = 3) && decide (b = 4)))
        && !(decide (shape / 2 = 1) && (decide (a = 5) && decide (b = 6))))

    -- View k of the grid, eleven bits: bit 0 row 1's actor, bit 1 its recipient,
    -- bits 2-3 its entry (record 0, 2, 4 or 6), bits 4-5 the Removes (none, one,
    -- two, two of which the second is void), bits 6-7 their actors, bits 8-9 the
    -- causal order's shape, bit 10 whether Remove 0 is a quorum's share.
    def gridTable (k : Nat) : Table :=
      { rows := 2,
        ractor := fun i => if i = 1 then k % 2 else 0,
        rrcp := fun i => if i = 1 then (k / 2) % 2 else 0,
        rid := fun i => if i = 1 then 2 * ((k / 4) % 4) else 0,
        rms := if (k / 16) % 4 = 0 then 0 else (if (k / 16) % 4 = 1 then 1 else 2),
        mactor := fun j => if j = 0 then (k / 64) % 2 else (k / 128) % 2,
        mid := fun j => if j = 0 then 3 else 5,
        mlive := fun j => !(decide (j = 1) && decide ((k / 16) % 4 = 3)),
        mquorum := fun j => decide (j = 0) && decide ((k / 1024) % 2 = 1),
        before := gridBefore ((k / 256) % 4) }

    def bit (b : Bool) (v : Nat) : Nat := if b = true then v else 0

    def gridByte (t : Table) : Nat :=
      bit (revoked t 0) 1 + bit (revoked t 1) 2 + bit (current t 0) 4 + bit (current t 1) 8
        + bit (gate t 4) 16 + bit (gate t 7) 32 + bit (revokes t 0) 64 + bit (revokes t 1) 128

    -- Eight views to a word (Clean's numerals fit 64 bits), view 8w's byte first.
    def wordOf (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 256 * wordOf w n + gridByte (gridTable (8 * w + n))

    def sheetFrom (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => wordOf w 8 :: sheetFrom (w + 1) n

    -- Views 0 to 2047, as 256 words.
    def sheet : List Nat := sheetFrom 0 256

    theorem sheet_is : sheet =
      [0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0x3434797934347979,
       0xe161e369e161e369, 0xe161e36934346969, 0xf171f379f171f379, 0x3434797934347979,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0xe1e1e3e3e1e1e3e3, 0xe161e36934346969, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0x3434797934347979,
       0xf1e1f3e3f1e1f3e3, 0x34e179e334347979, 0xf171f379f171f379, 0x3434797934347979,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34a13ca634a13ca6, 0x34a13ca634343c3c, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e36934346969,
       0xe161e369e161e369, 0xe161e36934346969, 0xe161e369e161e369, 0xe161e36934346969,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e334346969, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e36934346969,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e334346969, 0xe161e369e161e369, 0xe161e36934346969,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34a13ca634a13ca6, 0x34a13ca634343c3c, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0x3434797934347979,
       0xe161e369e161e369, 0xe161e369e161e369, 0xf171f379f171f379, 0x3434797934347979,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0xe1e1e3e3e1e1e3e3, 0xe161e369e161e369, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0x3434797934347979,
       0xf1e1f3e3f1e1f3e3, 0x34e179e334e179e3, 0xf171f379f171f379, 0x3434797934347979,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34b13cb634b13cb6, 0x34343c3c34343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e36934346969,
       0xe161e369e161e369, 0xe161e369e161e369, 0xe161e369e161e369, 0xe161e36934346969,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e3e161e369, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e36934346969,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e334e169e3, 0xe161e369e161e369, 0xe161e36934346969,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634343c3c,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0xf134f379f134f379,
       0xe161e369e161e369, 0xe161e369e134e369, 0xf171f379f171f379, 0xf134f379f134f379,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e334e169e3, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0xf134f379f134f379,
       0xf1e1f3e3f1e1f3e3, 0xf1e1f3e3f134f379, 0xf171f379f171f379, 0xf134f379f134f379,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34a13ca634a13ca6, 0x34a13ca634b13cb6, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e369e134e369,
       0xe161e369e161e369, 0xe161e369e134e369, 0xe161e369e161e369, 0xe161e369e134e369,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e334e169e3, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e369e134e369,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e3e134e369, 0xe161e369e161e369, 0xe161e369e134e369,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0xf134f379f134f379,
       0xe161e369e161e369, 0xe161e369e161e369, 0xf171f379f171f379, 0xf134f379f134f379,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e3e1e1e3e3, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xf171f379f171f379, 0xf134f379f134f379,
       0xf1e1f3e3f1e1f3e3, 0xf1e1f3e3f1e1f3e3, 0xf171f379f171f379, 0xf134f379f134f379,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34b13cb634b13cb6, 0x34b13cb634b13cb6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e369e134e369,
       0xe161e369e161e369, 0xe161e369e161e369, 0xe161e369e161e369, 0xe161e369e134e369,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e3e1e1e3e3, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0xe161e369e161e369, 0xe161e369e134e369,
       0xe1e1e3e3e1e1e3e3, 0xe1e1e3e3e1e1e3e3, 0xe161e369e161e369, 0xe161e369e134e369,
       0x34343c3c34343c3c, 0x34343c3c34343c3c, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6,
       0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6, 0x34a13ca634a13ca6] := rfl

    end AtermMeshRevocation
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P1b, no leak (P1's Tier 2, spec_no_leak): no key minted or sealed after a seat's removal, in the
// log's causal order, is readable by the removed seat but after a re-grant. The World signs the
// records of a fixed DAG one by one, each under the Guard its Rust decision asks against all its
// signer holds: a Put follows a counting epoch whose gate Log::head_due (heads_due) calls open, a
// Wrap goes only to a recipient not revoked. It reads every Remove as plain, since a quorum's
// acceptance is a view's: its Removes are those authorized with nothing void, which every view
// that holds one reads alike, and each Rust decision is at least as strict. The kernels are
// parameters, and revocation_no_leak_binding_island.rs closes the laws for the compiled ones. The
// writer's gate here is revocation_island.rs's gate with the row's own entry in the epoch's past
// as well (P1b's fix: without it, a Mac that lacks a seat move concurrent with its Remove wraps a
// sealed epoch to the moved seat's recipient, mutant A). Section 7's mutants are computed runs,
// never rejected declarations.
clean {
    namespace AtermMeshNoLeak

    open AtermMeshTemporal
    open AtermMeshRevocation

    -- ===========================================================================
    -- 1. The lab. Its records and their causal order are a DAG fixed in advance:
    --    every state of every run of the real lab is a state of a run over the
    --    DAG of the records signed by then, so a law of every state of every run,
    --    over every DAG, is a law of the lab. A run reveals the records one by one
    --    (Sign) and hands them between Macs (Deliver). A record's causal past is
    --    held by its signer, but need not be all of it (Log::draft_following keeps
    --    at most MAX_PARENTS parents); the signer's decision reads all it holds.
    -- ===========================================================================

    structure Dag where
      n      : Nat
      kind   : Nat → Nat          -- 1 a binding seat row (genesis or Admit), 2 Remove, 3 Epoch, 4 Wrap, 5 Put
      actor  : Nat → Nat          -- a seat row's or a Remove's MemberId
      rcp    : Nat → Nat          -- a seat row's or a Wrap's age recipient
      epoch  : Nat → Nat          -- the Epoch a Wrap hands on or a Put is sealed under
      signer : Nat → Nat          -- the Mac that signs the record
      sact   : Nat → Nat          -- the MemberId it signs as
      past   : Nat → Nat → Bool   -- past y x: y lies in x's strict causal past (Reach)

    -- What a view V (the records a Mac holds) gives the decision: every seat row
    -- and every Remove it holds, under the DAG's causal order; a record V does not
    -- hold is no row (actor 0, recipient 0) and no live Remove. Every Remove reads
    -- as plain: whether a quorum accepts a share is a view's (eval.rs weighs the
    -- shares it holds), and a Mac that holds an epoch need not hold the shares its
    -- sealer held. The World's Removes are the log's authorized with nothing void,
    -- which its causal past decides, so every view that holds one reads it alike:
    -- an Undo's share a Mac its target removed signed is none, and the quorum it
    -- forms lies outside the World, as a quorum's void does. A share only adds
    -- takes, so a plain reading revokes no more than the real one, and each Rust
    -- decision is at least as strict as the plain one: a wrapper's recipient not
    -- revoked, an epoch's minter a member, and the gate, whose one-Remove clause
    -- counts a Remove only where it takes the row by itself and is authorized with
    -- nothing void, and whose live seats are the plain ones (RevokingRow::alone,
    -- recipient_live).
    def isRow (D : Dag) (V : Nat → Bool) (x : Nat) : Bool := V x && decide (D.kind x = 1)
    def isRm (D : Dag) (V : Nat → Bool) (x : Nat) : Bool := V x && decide (D.kind x = 2)
    def rowAct (D : Dag) (V : Nat → Bool) (x : Nat) : Nat := if isRow D V x = true then D.actor x + 1 else 0
    def rowRcp (D : Dag) (V : Nat → Bool) (x : Nat) : Nat := if isRow D V x = true then D.rcp x + 1 else 0

    def tableOf (D : Dag) (V : Nat → Bool) : Table :=
      { rows := D.n, ractor := rowAct D V, rrcp := rowRcp D V, rid := fun x => x,
        rms := D.n, mactor := fun x => D.actor x + 1, mid := fun x => x, mlive := isRm D V,
        mquorum := fun _ => false, before := D.past }

    -- A record's causal view: its strict causal past, which every Mac holding it holds.
    def viewOf (D : Dag) (x : Nat) : Nat → Bool := fun y => D.past y x

    -- ===========================================================================
    -- 2. The gate the writer asks (Log::head_due through heads_due). For each seat
    --    row that revokes its recipient, the epoch's causal past holds the row and
    --    every Remove that took it, or the row and one such Remove while no row
    --    that is not out binds the recipient.
    -- ===========================================================================

    def gateFix (t : Table) (e : Nat) : Bool :=
      allN t.rows (fun i => spec_gate_row (revokes t i) (t.before (t.rid i) e && takersBefore t i e)
        (boundLive t (t.rrcp i)) (t.before (t.rid i) e && someTakerBefore t i e))

    def gateFixW (K : Kernels) (t : Table) (e : Nat) : Bool :=
      allN t.rows (fun i => K.gateRow (revokesW K t i) (t.before (t.rid i) e && takersBeforeW K t i e)
        (boundLiveW K t (t.rrcp i)) (t.before (t.rid i) e && someTakerBeforeW K t i e))

    def headDueFixW (K : Kernels) (t : Table) (e : Nat) (wrapsRevoked : Bool) : Bool :=
      K.headDue (gateFixW K t e) wrapsRevoked

    -- An epoch counts where its minter is a member: a seat row of the MemberId it
    -- signs as, not out, lies in its causal past (eval.rs: a record counts only
    -- when its signer was a member in its own causal past).
    def memberW (K : Kernels) (t : Table) (a : Nat) : Bool :=
      anyN t.rows (fun g => decide (t.ractor g = a) && !(isOutW K t g))

    theorem gateFixW_eq (K : Kernels) (hK : Exact K) (t : Table) (e : Nat) : gateFixW K t e = gateFix t e :=
      allN_congr _ _ t.rows (fun i _ => by
        show K.gateRow (revokesW K t i) (t.before (t.rid i) e && takersBeforeW K t i e) (boundLiveW K t (t.rrcp i))
            (t.before (t.rid i) e && someTakerBeforeW K t i e)
          = spec_gate_row (revokes t i) (t.before (t.rid i) e && takersBefore t i e) (boundLive t (t.rrcp i))
            (t.before (t.rid i) e && someTakerBefore t i e)
        rw [revokesW_eq K hK t i, takersBeforeW_eq K hK t i e, boundLiveW_eq K hK t (t.rrcp i),
          someTakerBeforeW_eq K hK t i e]
        exact hK.2.2.2.2.1 _ _ _ _)

    -- A head the kernels call not due has its gate open.
    theorem not_due_opens (K : Kernels) (hK : Exact K) (t : Table) (e : Nat) (w : Bool)
        (h : headDueFixW K t e w = false) : gateFix t e = true := by
      have h2 : spec_head_due (gateFixW K t e) w = false := Eq.trans (Eq.symm (hK.2.2.2.2.2.2.2 _ _)) h
      rw [gateFixW_eq K hK t e] at h2
      cases hg : gateFix t e with
      | true => rfl
      | false =>
        rw [hg] at h2
        exact Bool.noConfusion h2

    theorem or_tt (a : Bool) : (a || true) = true := by
      cases a <;> rfl

    theorem or_step {a b : Bool} (h : (a || b) = true) : a = true ∨ b = true := by
      cases a with
      | true => exact Or.inl rfl
      | false => exact Or.inr h

    -- An open gate holds, for a row that revokes, the row and a Remove that took it.
    theorem gateFix_some (t : Table) (e i : Nat) (hg : gateFix t e = true) (hi : i < t.rows)
        (hrv : revokes t i = true) :
        t.before (t.rid i) e = true ∧ ∃ j, j < t.rms ∧ takes t j i = true ∧ t.before (t.mid j) e = true := by
      have hrow : spec_gate_row (revokes t i) (t.before (t.rid i) e && takersBefore t i e) (boundLive t (t.rrcp i))
          (t.before (t.rid i) e && someTakerBefore t i e) = true := allN_elim _ t.rows hg i hi
      rw [hrv] at hrow
      have hrow2 : ((t.before (t.rid i) e && takersBefore t i e)
          || (!(boundLive t (t.rrcp i)) && (t.before (t.rid i) e && someTakerBefore t i e))) = true := hrow
      cases h1 : (t.before (t.rid i) e && takersBefore t i e) with
      | true =>
        have h1' := Eq.mp (Bool.and_eq_true _ _) h1
        exact ⟨h1'.1, Exists.elim (anyN_elim (fun j => takes t j i) t.rms (revokes_parts t i hrv).1) (fun j hj =>
          ⟨j, hj.1, hj.2, takers_before_elim t i e j h1'.2 hj.1 hj.2⟩)⟩
      | false =>
        rw [h1] at hrow2
        have h2 : (!(boundLive t (t.rrcp i)) && (t.before (t.rid i) e && someTakerBefore t i e)) = true := hrow2
        have h3 := Eq.mp (Bool.and_eq_true _ _) (Eq.mp (Bool.and_eq_true _ _) h2).2
        exact ⟨h3.1, Exists.elim (anyN_elim (fun j => takes t j i && t.before (t.mid j) e) t.rms h3.2) (fun j hj =>
          have hb := Eq.mp (Bool.and_eq_true _ _) hj.2
          ⟨j, hj.1, hb.1, hb.2⟩)⟩

    -- The writer's gate is P1's gate (the Remove clause, revocation_island.rs's
    -- gate and Table::gate) and the row clause together, so every law of P1's
    -- gate holds of it.
    def entered (t : Table) (e : Nat) : Bool :=
      allN t.rows (fun i => !(revokes t i) || t.before (t.rid i) e)
    theorem and4 (a b c d : Bool) : ((a && b) && (c && d)) = ((a && c) && (b && d)) := by
      cases a <;> cases b <;> cases c <;> cases d <;> rfl
    theorem allN_and (f g : Nat → Bool) (n : Nat) : allN n (fun i => f i && g i) = (allN n f && allN n g) := by
      induction n with
      | zero => rfl
      | succ k ih =>
        show ((f k && g k) && allN k (fun i => f i && g i)) = ((f k && allN k f) && (g k && allN k g))
        rw [ih]
        exact and4 (f k) (g k) (allN k f) (allN k g)
    theorem row_split (rv rb ev lv so : Bool) :
        spec_gate_row rv (rb && ev) lv (rb && so) = (spec_gate_row rv ev lv so && (!rv || rb)) := by
      cases rv <;> cases rb <;> cases ev <;> cases lv <;> cases so <;> rfl
    theorem gateFix_split (t : Table) (e : Nat) : gateFix t e = (gate t e && entered t e) := by
      show allN t.rows (fun i => spec_gate_row (revokes t i) (t.before (t.rid i) e && takersBefore t i e)
          (boundLive t (t.rrcp i)) (t.before (t.rid i) e && someTakerBefore t i e))
        = (allN t.rows (fun i => spec_gate_row (revokes t i) (takersBefore t i e) (boundLive t (t.rrcp i))
            (someTakerBefore t i e)) && allN t.rows (fun i => !(revokes t i) || t.before (t.rid i) e))
      rw [← allN_and]
      exact allN_congr _ _ t.rows (fun i _ => row_split _ _ _ _ _)

    -- ===========================================================================
    -- 3. Views. A Remove takes a row in a view only when the view holds both, and
    --    then in every view that holds both: the decision reads records and the
    --    causal order, nothing a Mac adds.
    -- ===========================================================================

    theorem not_true_of_false {b : Bool} (h : b = false) : Not (b = true) :=
      fun e => Bool.noConfusion (Eq.trans h.symm e)

    theorem rowAct_of (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRow D V x = true) :
        rowAct D V x = D.actor x + 1 := if_pos h
    theorem rowAct_not (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRow D V x = false) :
        rowAct D V x = 0 := if_neg (not_true_of_false h)
    theorem rowRcp_of (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRow D V x = true) :
        rowRcp D V x = D.rcp x + 1 := if_pos h
    theorem rowRcp_not (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRow D V x = false) :
        rowRcp D V x = 0 := if_neg (not_true_of_false h)

    theorem held_of_isRow (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRow D V x = true) :
        V x = true ∧ D.kind x = 1 :=
      have h2 := Eq.mp (Bool.and_eq_true _ _) h
      ⟨h2.1, of_decide_eq_true h2.2⟩
    theorem held_of_isRm (D : Dag) (V : Nat → Bool) (x : Nat) (h : isRm D V x = true) :
        V x = true ∧ D.kind x = 2 :=
      have h2 := Eq.mp (Bool.and_eq_true _ _) h
      ⟨h2.1, of_decide_eq_true h2.2⟩
    theorem isRow_of (D : Dag) (V : Nat → Bool) (x : Nat) (hv : V x = true) (hk : D.kind x = 1) :
        isRow D V x = true := by
      show (V x && decide (D.kind x = 1)) = true
      rw [hv, dtrue hk]
    theorem isRm_of (D : Dag) (V : Nat → Bool) (x : Nat) (hv : V x = true) (hk : D.kind x = 2) :
        isRm D V x = true := by
      show (V x && decide (D.kind x = 2)) = true
      rw [hv, dtrue hk]

    -- What a take says: the view holds the Remove and the row, of one MemberId, and
    -- the Remove does not lie in the row's entry's past.
    theorem takes_parts (D : Dag) (V : Nat → Bool) (j i : Nat) (h : takes (tableOf D V) j i = true) :
        isRm D V j = true ∧ isRow D V i = true ∧ D.actor j = D.actor i
          ∧ (false || !(D.past j i)) = true := by
      have h1 : (isRm D V j && (decide (D.actor j + 1 = rowAct D V i) && (false || !(D.past j i)))) = true := h
      have h2 := Eq.mp (Bool.and_eq_true _ _) h1
      have h3 := Eq.mp (Bool.and_eq_true _ _) h2.2
      have hact : D.actor j + 1 = rowAct D V i := of_decide_eq_true h3.1
      cases hr : isRow D V i with
      | false =>
        rw [rowAct_not D V i hr] at hact
        exact absurd hact (fun e => Nat.noConfusion e)
      | true =>
        rw [rowAct_of D V i hr] at hact
        exact ⟨h2.1, rfl, congrArg Nat.pred hact, h3.2⟩

    theorem takes_transfer (D : Dag) (V W : Nat → Bool) (j i : Nat) (hj : W j = true) (hi : W i = true)
        (h : takes (tableOf D V) j i = true) : takes (tableOf D W) j i = true := by
      have hp := takes_parts D V j i h
      have hrm : isRm D W j = true := isRm_of D W j hj (held_of_isRm D V j hp.1).2
      have hrow : isRow D W i = true := isRow_of D W i hi (held_of_isRow D V i hp.2.1).2
      show (isRm D W j && (decide (D.actor j + 1 = rowAct D W i) && (false || !(D.past j i)))) = true
      rw [hrm, rowAct_of D W i hrow, hp.2.2.1, dtrue (Eq.refl (D.actor i + 1)), hp.2.2.2]

    -- A row taken out whose recipient a view does not revoke is restored there.
    theorem restored_of_out_unrevoked (t : Table) (i : Nat) (hi : i < t.rows) (ho : isOut t i = true)
        (hnrev : revoked t (t.rrcp i) = false) : ∃ g, g < t.rows ∧ restores t i g = true := by
      cases hex : restored t i with
      | true => exact anyN_elim _ t.rows hex
      | false =>
        have hall : ∀ m, m < t.rows → restores t i m = false := by
          intro m hm
          cases hr : restores t i m with
          | false => rfl
          | true =>
            have hthis : restored t i = true := anyN_intro _ t.rows m hm hr
            rw [hex] at hthis
            exact Bool.noConfusion hthis
        have hyes := revoked_complete t i hi ho hall
        rw [hnrev] at hyes
        exact Bool.noConfusion hyes

    -- ===========================================================================
    -- 4. What the law allows. A key handed to a removed seat's recipient, or
    --    minted by its MemberId, is lawful only after a re-grant: a seat row of
    --    that recipient (or MemberId), not out, whose entry has a Remove that took
    --    the seat in its causal past. Nothing else lets a removed seat read what
    --    was minted or sealed after its removal.
    -- ===========================================================================

    -- regrant D V W i: view W (what a Wrap's signer held) holds a seat row of row
    -- i's recipient, not out there, entered after a Remove that took row i in view V.
    def regrant (D : Dag) (V W : Nat → Bool) (i : Nat) : Bool :=
      anyN D.n (fun g => anyN D.n (fun j =>
        isRow D W g && (decide (D.rcp g = D.rcp i) && (!(isOut (tableOf D W) g)
          && (takes (tableOf D V) j i && D.past j g)))))

    -- readmitted D V e i: the causal view of epoch e holds a seat row of row i's
    -- MemberId, not out there, entered after a Remove that took row i in view V.
    def readmitted (D : Dag) (V : Nat → Bool) (e i : Nat) : Bool :=
      anyN D.n (fun g => anyN D.n (fun j =>
        isRow D (viewOf D e) g && (decide (D.actor g = D.actor i) && (!(isOut (tableOf D (viewOf D e)) g)
          && (takes (tableOf D V) j i && D.past j g)))))

    -- The core, one Wrap: a wrapper whose view W holds row i and a Remove that took
    -- it in view V, and that wraps only to a recipient not revoked there (the
    -- kernels' revoked), wraps row i's recipient only after a re-grant.
    theorem wrap_regrant (K : Kernels) (hK : Exact K) (D : Dag) (V W : Nat → Bool) (i j : Nat)
        (hin : i < D.n) (hjn : j < D.n) (htk : takes (tableOf D V) j i = true)
        (hjw : W j = true) (hiw : W i = true)
        (hnrev : revokedW K (tableOf D W) (D.rcp i + 1) = false) :
        regrant D V W i = true := by
      have htw : takes (tableOf D W) j i = true := takes_transfer D V W j i hjw hiw htk
      have ho : isOut (tableOf D W) i = true := anyN_intro _ D.n j hjn htw
      have hrow : isRow D W i = true := (takes_parts D W j i htw).2.1
      have hc0 : revoked (tableOf D W) (D.rcp i + 1) = false :=
        Eq.trans (Eq.symm (revokedW_eq K hK _ _)) hnrev
      have hc : revoked (tableOf D W) ((tableOf D W).rrcp i) = false := by
        show revoked (tableOf D W) (rowRcp D W i) = false
        rw [rowRcp_of D W i hrow]
        exact hc0
      exact Exists.elim (restored_of_out_unrevoked (tableOf D W) i hin ho hc) (fun g hg => by
        have hp := restores_parts (tableOf D W) i g hg.2
        have hrr : rowRcp D W g = rowRcp D W i := hp.1
        rw [rowRcp_of D W i hrow] at hrr
        have hgrow : isRow D W g = true := by
          cases hgr : isRow D W g with
          | true => rfl
          | false =>
            rw [rowRcp_not D W g hgr] at hrr
            exact absurd hrr.symm (fun e => Nat.noConfusion e)
        rw [rowRcp_of D W g hgrow] at hrr
        have hgrcp : D.rcp g = D.rcp i := congrArg Nat.pred hrr
        have hjg : D.past j g = true := takers_before_elim (tableOf D W) i g j hp.2.2 hjn htw
        have hgout : isOut (tableOf D W) g = false := hp.2.1
        apply anyN_intro _ D.n g hg.1
        apply anyN_intro _ D.n j hjn
        show (isRow D W g && (decide (D.rcp g = D.rcp i) && (!(isOut (tableOf D W) g)
          && (takes (tableOf D V) j i && D.past j g)))) = true
        rw [hgrow, dtrue hgrcp, hgout, htk, hjg])

    -- The core, one mint: an epoch that counts, minted by row i's MemberId, whose
    -- causal view holds a Remove that took row i in view V, is minted after a
    -- re-admission.
    theorem minter_readmitted (K : Kernels) (hK : Exact K) (D : Dag) (V : Nat → Bool) (e i j : Nat)
        (hjn : j < D.n) (htk : takes (tableOf D V) j i = true) (hje : D.past j e = true)
        (hmem : memberW K (tableOf D (viewOf D e)) (D.actor i + 1) = true) :
        readmitted D V e i = true :=
      Exists.elim (anyN_elim (fun g => decide (rowAct D (viewOf D e) g = D.actor i + 1)
          && !(isOutW K (tableOf D (viewOf D e)) g)) D.n hmem) (fun g hg => by
        have hb := Eq.mp (Bool.and_eq_true _ _) hg.2
        have hact : rowAct D (viewOf D e) g = D.actor i + 1 := of_decide_eq_true hb.1
        have hgrow : isRow D (viewOf D e) g = true := by
          cases hgr : isRow D (viewOf D e) g with
          | true => rfl
          | false =>
            rw [rowAct_not D (viewOf D e) g hgr] at hact
            exact absurd hact.symm (fun e => Nat.noConfusion e)
        rw [rowAct_of D (viewOf D e) g hgrow] at hact
        have hga : D.actor g = D.actor i := congrArg Nat.pred hact
        have hgout : isOut (tableOf D (viewOf D e)) g = false := by
          have hw : (!(isOutW K (tableOf D (viewOf D e)) g)) = true := hb.2
          rw [isOutW_eq K hK _ g] at hw
          cases ho : isOut (tableOf D (viewOf D e)) g with
          | false => rfl
          | true =>
            rw [ho] at hw
            exact Bool.noConfusion hw
        have hp := takes_parts D V j i htk
        have hrm : isRm D (viewOf D e) j = true := isRm_of D (viewOf D e) j hje (held_of_isRm D V j hp.1).2
        have hjg : D.past j g = true := by
          cases hpj : D.past j g with
          | true => rfl
          | false =>
            have htg : takes (tableOf D (viewOf D e)) j g = true := by
              show (isRm D (viewOf D e) j && (decide (D.actor j + 1 = rowAct D (viewOf D e) g)
                && (false || !(D.past j g)))) = true
              rw [hrm, rowAct_of D (viewOf D e) g hgrow, hp.2.2.1, ← hga, dtrue (Eq.refl (D.actor g + 1)), hpj]
            have hout : isOut (tableOf D (viewOf D e)) g = true := anyN_intro _ D.n j hjn htg
            rw [hgout] at hout
            exact Bool.noConfusion hout
        apply anyN_intro _ D.n g hg.1
        apply anyN_intro _ D.n j hjn
        show (isRow D (viewOf D e) g && (decide (D.actor g = D.actor i) && (!(isOut (tableOf D (viewOf D e)) g)
          && (takes (tableOf D V) j i && D.past j g)))) = true
        rw [hgrow, dtrue hga, hgout, htk, hjg])

    -- ===========================================================================
    -- 5. The World. Each step signs one record of the DAG under the Guard of its
    --    kind, read against V, all its signer holds; the Guard asks the decision
    --    as parameters K (Exact K: they compute the spec kernels). Every path that
    --    mints or seals is such a step:
    --    - a Put under an epoch it does not mint (the writer's seal, sealing_epoch;
    --      the waiting-write pass; the Commons control descriptor; the admin
    --      mirror's draft and a host list's write, which seal through them; a
    --      dropped section's tombstones): it follows the epoch, the epoch counts,
    --      and Log::head_due (heads_due) calls it not due;
    --    - a Wrap (a fresh epoch's Wraps: a removal's rotation, the tender's re-key
    --      and coalesce, a writer's fresh epoch, the admin mirror's, a host list's
    --      rotation; the heal, close_wrap_gaps; the fill, close_plan_gaps; an
    --      admission's Wraps): it follows the epoch and the recipient is not
    --      revoked (Rust asks less of it than current: a [machines]-only key no seat
    --      binds is wrapped too);
    --    - an Epoch, a seat row, a Remove: no guard. A fresh epoch's own Puts meet
    --      the Put's Guard by fresh_batch_open: Log::draft_epoch_following refuses
    --      an epoch whose drafted past leaves its minter's gate shut
    --      (Log::gate_opens_after).
    -- ===========================================================================

    def Guard (K : Kernels) (D : Dag) (V : Nat → Bool) (x : Nat) : Prop :=
      (D.kind x = 4 → D.past (D.epoch x) x = true ∧ revokedW K (tableOf D V) (D.rcp x + 1) = false) ∧
      (D.kind x = 5 → D.past (D.epoch x) x = true ∧
          memberW K (tableOf D (viewOf D (D.epoch x))) (D.sact (D.epoch x) + 1) = true ∧
          ∃ w, headDueFixW K (tableOf D V) (D.epoch x) w = false)

    structure Lab where
      made  : Nat → Bool          -- the records signed so far
      holds : Nat → Nat → Bool    -- holds m x: Mac m holds record x
      dec   : Nat → Nat → Bool    -- dec x y: x's signer held y when it signed x

    -- A record's causal past is held by its signer and closed (a DAG's Reach).
    def Sign (K : Kernels) (D : Dag) (x : Nat) (L L' : Lab) : Prop :=
      x < D.n ∧ L.made x = false ∧ (∀ y, D.past y x = true → L.holds (D.signer x) y = true) ∧
      (∀ y z, D.past y x = true → D.past z y = true → D.past z x = true) ∧
      Guard K D (L.holds (D.signer x)) x ∧
      L'.made = (fun y => decide (y = x) || L.made y) ∧
      L'.holds = (fun m y => (decide (m = D.signer x) && decide (y = x)) || L.holds m y) ∧
      L'.dec = (fun y => if y = x then L.holds (D.signer x) else L.dec y)

    -- Log::insert: a record lands after its causal past.
    def Deliver (D : Dag) (m x : Nat) (L L' : Lab) : Prop :=
      L.made x = true ∧ (∀ y, D.past y x = true → L.holds m y = true) ∧ L'.made = L.made ∧
      L'.holds = (fun k y => (decide (k = m) && decide (y = x)) || L.holds k y) ∧ L'.dec = L.dec

    def InitL (L : Lab) : Prop := (∀ x, L.made x = false) ∧ (∀ m x, L.holds m x = false)
    def NextL (K : Kernels) (D : Dag) (L L' : Lab) : Prop :=
      (∃ x, Sign K D x L L') ∨ (∃ m x, Deliver D m x L L')
    def SpecL (K : Kernels) (D : Dag) : StateMachine Lab := StateMachine.mk InitL (NextL K D)

    -- ===========================================================================
    -- 6. The laws.
    --    NoLeak (sealed): a Put sealed while row i revokes its recipient in what its
    --    writer holds is readable by that recipient, or by the seat's MemberId as
    --    the epoch's minter, only after a re-grant (re-admission) that postdates a
    --    Remove that took the seat.
    --    NoLeakMint (minted): an epoch whose causal past holds a seat row and a
    --    Remove that took it is handed to the seat's recipient only after a
    --    re-grant.
    -- ===========================================================================

    def NoLeak (D : Dag) (L : Lab) : Prop :=
      ∀ p i, L.made p = true → D.kind p = 5 → i < D.n → revokes (tableOf D (L.dec p)) i = true →
        (∀ w, L.made w = true → D.kind w = 4 → D.epoch w = D.epoch p → D.rcp w = D.rcp i →
           regrant D (L.dec p) (L.dec w) i = true) ∧
        (D.sact (D.epoch p) = D.actor i → readmitted D (L.dec p) (D.epoch p) i = true)

    def NoLeakMint (D : Dag) (L : Lab) : Prop :=
      ∀ e i, i < D.n → isOut (tableOf D (viewOf D e)) i = true →
        ∀ w, L.made w = true → D.kind w = 4 → D.epoch w = e → D.rcp w = D.rcp i →
          regrant D (viewOf D e) (L.dec w) i = true

    -- The invariant: every signed record met its Guard against what its signer
    -- held, its causal past was held, and the causal order is closed at it.
    def Inv (K : Kernels) (D : Dag) (L : Lab) : Prop :=
      (∀ x, L.made x = true → Guard K D (L.dec x) x) ∧
      (∀ x y, L.made x = true → D.past y x = true → L.dec x y = true) ∧
      (∀ x y z, L.made x = true → D.past y x = true → D.past z y = true → D.past z x = true)

    theorem init_inv (K : Kernels) (D : Dag) (L : Lab) (h : InitL L) : Inv K D L :=
      ⟨fun x hx => absurd (Eq.trans (h.1 x).symm hx) (fun e => Bool.noConfusion e),
       fun x _ hx _ => absurd (Eq.trans (h.1 x).symm hx) (fun e => Bool.noConfusion e),
       fun x _ _ hx _ _ => absurd (Eq.trans (h.1 x).symm hx) (fun e => Bool.noConfusion e)⟩

    theorem made_cases (x z : Nat) (m : Nat → Bool) (h : (decide (z = x) || m z) = true) :
        z = x ∨ (Not (z = x) ∧ m z = true) := by
      cases hz : decide (z = x) with
      | true => exact Or.inl (of_decide_eq_true hz)
      | false =>
        rw [hz] at h
        exact Or.inr ⟨fun e => Bool.noConfusion (Eq.trans hz.symm (dtrue e)), h⟩

    theorem sign_inv (K : Kernels) (D : Dag) (x : Nat) (L L' : Lab) (hi : Inv K D L) (hs : Sign K D x L L') :
        Inv K D L' := by
      have hmade := hs.2.2.2.2.2.1
      have hdec := hs.2.2.2.2.2.2.2
      have dec_x : L'.dec x = L.holds (D.signer x) := by
        rw [hdec]
        exact if_pos rfl
      have dec_z : ∀ z, Not (z = x) → L'.dec z = L.dec z := fun z hne => by
        rw [hdec]
        exact if_neg hne
      have mk : ∀ z, L'.made z = true → z = x ∨ (Not (z = x) ∧ L.made z = true) := fun z hz =>
        made_cases x z L.made (Eq.trans (congrFun hmade z).symm hz)
      have g1 : ∀ z, L'.made z = true → Guard K D (L'.dec z) z := by
        intro z hz
        cases mk z hz with
        | inl e =>
          rw [e, dec_x]
          exact hs.2.2.2.2.1
        | inr e =>
          rw [dec_z z e.1]
          exact hi.1 z e.2
      have g2 : ∀ z y, L'.made z = true → D.past y z = true → L'.dec z y = true := by
        intro z y hz hy
        cases mk z hz with
        | inl e =>
          rw [e] at hy
          rw [e, dec_x]
          exact hs.2.2.1 y hy
        | inr e =>
          rw [dec_z z e.1]
          exact hi.2.1 z y e.2 hy
      have g3 : ∀ z y w, L'.made z = true → D.past y z = true → D.past w y = true → D.past w z = true := by
        intro z y w hz hy hw
        cases mk z hz with
        | inl e =>
          rw [e] at hy
          rw [e]
          exact hs.2.2.2.1 y w hy hw
        | inr e => exact hi.2.2 z y w e.2 hy hw
      exact ⟨g1, g2, g3⟩

    theorem deliver_inv (K : Kernels) (D : Dag) (m0 x : Nat) (L L' : Lab) (hi : Inv K D L)
        (hd : Deliver D m0 x L L') : Inv K D L' := by
      have hmade := hd.2.2.1
      have hdec := hd.2.2.2.2
      have old : ∀ z, L'.made z = true → L.made z = true := fun z hz => Eq.trans (congrFun hmade z).symm hz
      have g1 : ∀ z, L'.made z = true → Guard K D (L'.dec z) z := by
        intro z hz
        rw [hdec]
        exact hi.1 z (old z hz)
      have g2 : ∀ z y, L'.made z = true → D.past y z = true → L'.dec z y = true := by
        intro z y hz hy
        rw [hdec]
        exact hi.2.1 z y (old z hz) hy
      exact ⟨g1, g2, fun z y w hz hy hw => hi.2.2 z y w (old z hz) hy hw⟩

    theorem next_inv (K : Kernels) (D : Dag) (L L' : Lab) (hi : Inv K D L) (hn : NextL K D L L') : Inv K D L' := by
      cases hn with
      | inl h => exact Exists.elim h (fun x hs => sign_inv K D x L L' hi hs)
      | inr h => exact Exists.elim h (fun m h2 => Exists.elim h2 (fun x hd => deliver_inv K D m x L L' hi hd))

    -- Both laws in a state that holds the invariant.
    theorem no_leak_of_inv (K : Kernels) (hK : Exact K) (D : Dag) (L : Lab) (hi : Inv K D L) : NoLeak D L :=
      fun p i hp hk hin hrv =>
        have hg := (hi.1 p hp).2 hk
        have hopen : gateFix (tableOf D (L.dec p)) (D.epoch p) = true :=
          Exists.elim hg.2.2 (fun w hw => not_due_opens K hK _ _ w hw)
        have hsome := gateFix_some (tableOf D (L.dec p)) (D.epoch p) i hopen hin hrv
        Exists.elim hsome.2 (fun j hj =>
          ⟨fun w hw hkw hew hrw =>
             have hgw := (hi.1 w hw).1 hkw
             have hepw : D.past (D.epoch p) w = true := by rw [← hew]; exact hgw.1
             have hcur : revokedW K (tableOf D (L.dec w)) (D.rcp i + 1) = false := by rw [← hrw]; exact hgw.2
             wrap_regrant K hK D (L.dec p) (L.dec w) i j hin hj.1 hj.2.1
               (hi.2.1 w j hw (hi.2.2 w (D.epoch p) j hw hepw hj.2.2))
               (hi.2.1 w i hw (hi.2.2 w (D.epoch p) i hw hepw hsome.1)) hcur,
           fun hsa =>
             have hmem : memberW K (tableOf D (viewOf D (D.epoch p))) (D.actor i + 1) = true := by
               rw [← hsa]; exact hg.2.1
             minter_readmitted K hK D (L.dec p) (D.epoch p) i j hj.1 hj.2.1 hj.2.2 hmem⟩)

    theorem no_leak_mint_of_inv (K : Kernels) (hK : Exact K) (D : Dag) (L : Lab) (hi : Inv K D L) :
        NoLeakMint D L :=
      fun e i hin ho w hw hkw hew hrw =>
        have hgw := (hi.1 w hw).1 hkw
        have hepw : D.past e w = true := by rw [← hew]; exact hgw.1
        have hcur : revokedW K (tableOf D (L.dec w)) (D.rcp i + 1) = false := by rw [← hrw]; exact hgw.2
        Exists.elim (anyN_elim (fun j => takes (tableOf D (viewOf D e)) j i) D.n ho) (fun j hj =>
          have hp := takes_parts D (viewOf D e) j i hj.2
          wrap_regrant K hK D (viewOf D e) (L.dec w) i j hin hj.1 hj.2
            (hi.2.1 w j hw (hi.2.2 w e j hw hepw (held_of_isRm D (viewOf D e) j hp.1).1))
            (hi.2.1 w i hw (hi.2.2 w e i hw hepw (held_of_isRow D (viewOf D e) i hp.2.1).1)) hcur)

    theorem inv_always (K : Kernels) (D : Dag) : Satisfies (SpecL K D) (Always (Lift (Inv K D))) :=
      invariantSatisfies (SpecL K D) (Inv K D) (init_inv K D) (next_inv K D)

    -- spec_no_leak: in every state of every run of the World that asks K, nothing
    -- sealed after a seat's removal is readable by the removed seat but after a
    -- re-grant.
    theorem spec_no_leak (K : Kernels) (hK : Exact K) (D : Dag) :
        Satisfies (SpecL K D) (Always (Lift (NoLeak D))) :=
      fun b runs n => no_leak_of_inv K hK D _ (inv_always K D b runs n)

    -- spec_no_leak_mint: nothing minted after a seat's removal is handed to it but
    -- after a re-grant.
    theorem spec_no_leak_mint (K : Kernels) (hK : Exact K) (D : Dag) :
        Satisfies (SpecL K D) (Always (Lift (NoLeakMint D))) :=
      fun b runs n => no_leak_mint_of_inv K hK D _ (inv_always K D b runs n)

    -- A fresh epoch's own batch: Log::draft_epoch_following refuses an epoch whose
    -- drafted past leaves its minter's gate shut (Log::gate_opens_after asks the
    -- gate of the records the draft's parents reach, the epoch's causal past), so
    -- the paths that mint and seal in one batch (a removal's rotation, a re-key, a
    -- coalesce, a writer's fresh epoch, a host list's rotation, a sealed-away
    -- section, a dropped section's tombstones) meet the Put's gate clause with no
    -- question of their own epoch: an open gate is a head the kernels call not
    -- due, whatever it wraps.
    theorem fresh_batch_open (K : Kernels) (hK : Exact K) (t : Table) (e : Nat) (hg : gateFix t e = true) :
        headDueFixW K t e false = false :=
      Eq.trans (hK.2.2.2.2.2.2.2 (gateFixW K t e) false) (by
        rw [gateFixW_eq K hK t e, hg]
        rfl)

    -- ===========================================================================
    -- 7. The mutants. Each is the World's step with one check changed, and its run
    --    is computed: the mutant signs a record the law forbids (the law fails in
    --    the state its run reaches), and the World's own Guard refuses that record.
    -- ===========================================================================

    -- The kernels as the spec states them.
    def specK : Kernels :=
      { takes := spec_takes, precedes := spec_precedes, restores := spec_restores,
        rowRevokes := spec_row_revokes, gateRow := spec_gate_row, current := spec_current,
        wrapsRemoved := spec_wraps_removed, headDue := spec_head_due }
    theorem specK_exact : Exact specK :=
      ⟨fun _ _ _ _ => rfl, fun _ _ => rfl, fun _ _ _ => rfl, fun _ _ => rfl, fun _ _ _ _ => rfl,
       fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl⟩

    -- A guard as a step computes it: a Wrap's and a Put's checks of record x
    -- against view V, what its signer holds.
    def guardOf (wrapOk putOk : (Nat → Bool) → Nat → Bool) (D : Dag) (V : Nat → Bool) (x : Nat) : Bool :=
      (!(decide (D.kind x = 4)) || (D.past (D.epoch x) x && wrapOk V x))
        && (!(decide (D.kind x = 5)) || (D.past (D.epoch x) x && putOk V x))
    def wrapsUnrevoked (K : Kernels) (D : Dag) (V : Nat → Bool) (x : Nat) : Bool :=
      !(revokedW K (tableOf D V) (D.rcp x + 1))
    def counts (K : Kernels) (D : Dag) (x : Nat) : Bool :=
      memberW K (tableOf D (viewOf D (D.epoch x))) (D.sact (D.epoch x) + 1)
    def opens (K : Kernels) (D : Dag) (V : Nat → Bool) (x : Nat) : Bool :=
      !(headDueFixW K (tableOf D V) (D.epoch x) false)
    -- The World's Guard, computed.
    def guardB (K : Kernels) (D : Dag) : (Nat → Bool) → Nat → Bool :=
      guardOf (wrapsUnrevoked K D) (fun V x => counts K D x && opens K D V x) D

    theorem not_true {b : Bool} (h : (!b) = true) : b = false := by
      cases b with
      | false => rfl
      | true => exact Bool.noConfusion h

    theorem guardB_sound (K : Kernels) (D : Dag) (V : Nat → Bool) (x : Nat) (h : guardB K D V x = true) :
        Guard K D V x :=
      have h2 := Eq.mp (Bool.and_eq_true _ _) h
      ⟨fun hk => by
         have h3 : (!(decide (D.kind x = 4)) || (D.past (D.epoch x) x && wrapsUnrevoked K D V x)) = true := h2.1
         rw [dtrue hk] at h3
         have h4 : (D.past (D.epoch x) x && wrapsUnrevoked K D V x) = true := h3
         have h5 := Eq.mp (Bool.and_eq_true _ _) h4
         exact ⟨h5.1, not_true h5.2⟩,
       fun hk => by
         have h3 : (!(decide (D.kind x = 5)) || (D.past (D.epoch x) x && (counts K D x && opens K D V x))) = true := h2.2
         rw [dtrue hk] at h3
         have h4 : (D.past (D.epoch x) x && (counts K D x && opens K D V x)) = true := h3
         have h5 := Eq.mp (Bool.and_eq_true _ _) h4
         have h6 := Eq.mp (Bool.and_eq_true _ _) h5.2
         exact ⟨h5.1, h6.1, false, not_true h6.2⟩⟩

    inductive Ev where
      | sign : Nat → Ev
      | deliver : Nat → Nat → Ev

    -- One step under guard g: a Sign of a record whose causal past its signer
    -- holds, read against all it holds, or a Deliver after the causal past.
    def stepG (g : (Nat → Bool) → Nat → Bool) (D : Dag) (L : Lab) : Ev → Lab
      | Ev.sign x =>
          if (decide (x < D.n) && (!(L.made x) && (allN D.n (fun y => !(D.past y x) || L.holds (D.signer x) y)
              && g (L.holds (D.signer x)) x))) = true then
            { made := fun y => decide (y = x) || L.made y,
              holds := fun m y => (decide (m = D.signer x) && decide (y = x)) || L.holds m y,
              dec := fun y => if y = x then L.holds (D.signer x) else L.dec y }
          else L
      | Ev.deliver m x =>
          if (L.made x && allN D.n (fun y => !(D.past y x) || L.holds m y)) = true then
            { made := L.made, holds := fun k y => (decide (k = m) && decide (y = x)) || L.holds k y, dec := L.dec }
          else L

    def runG (g : (Nat → Bool) → Nat → Bool) (D : Dag) (L : Lab) : List Ev → Lab
      | [] => L
      | e :: rest => runG g D (stepG g D L e) rest

    def lab0 : Lab := { made := fun _ => false, holds := fun _ _ => false, dec := fun _ _ => false }

    -- A DAG written record by record.
    def nth : List Nat → Nat → Nat
      | [], _ => 0
      | a :: _, 0 => a
      | _ :: rest, Nat.succ k => nth rest k
    def memN (y : Nat) : List Nat → Bool
      | [] => false
      | a :: rest => decide (y = a) || memN y rest
    def nthL : List (List Nat) → Nat → List Nat
      | [], _ => []
      | a :: _, 0 => a
      | _ :: rest, Nat.succ k => nthL rest k
    def dagOf (n : Nat) (kind actor rcp epoch signer sact : List Nat) (past : List (List Nat)) : Dag :=
      { n := n, kind := nth kind, actor := nth actor, rcp := nth rcp,
        epoch := nth epoch, signer := nth signer, sact := nth sact, past := fun y x => memN y (nthL past x) }

    -- Mutant A, the gate before PF1b (Tier 1's gate, Log::head_due on main before
    -- it): every Remove that took a revoking row lies in the epoch's past, but the
    -- row need not. Row 3 moves A's seat onto recipient 1, which B's row 0 also
    -- binds, concurrently with Remove 2 of A. Epoch 4 (minted by B) follows the
    -- Remove but not the move. Mac 2 lacks the move, holds recipient 1 current and
    -- wraps epoch 4 to it (Wrap 5); Mac 3 holds the move, holds recipient 1
    -- revoked, and seals Put 6 under epoch 4: A's recipient reads it.
    def dRowless : Dag :=
      dagOf 7 [1, 1, 2, 1, 3, 4, 5] [1, 0, 0, 0, 0, 0, 0] [1, 0, 0, 1, 0, 1, 0] [0, 0, 0, 0, 0, 4, 4]
        [0, 0, 0, 1, 2, 2, 3] [0, 0, 0, 0, 1, 1, 0]
        [[], [0], [0, 1], [0, 1], [0, 1, 2], [0, 1, 2, 4], [0, 1, 2, 3, 4]]
    def guardRowless (D : Dag) : (Nat → Bool) → Nat → Bool :=
      guardOf (wrapsUnrevoked specK D) (fun V x => counts specK D x && !(headDueW specK (tableOf D V) (D.epoch x) false)) D
    def evRowless : List Ev :=
      [Ev.sign 0, Ev.sign 1, Ev.sign 2, Ev.deliver 1 0, Ev.deliver 1 1, Ev.sign 3, Ev.deliver 2 0, Ev.deliver 2 1, Ev.deliver 2 2, Ev.sign 4, Ev.sign 5, Ev.deliver 3 0, Ev.deliver 3 1, Ev.deliver 3 2, Ev.deliver 3 3, Ev.deliver 3 4, Ev.sign 6]
    def runRowless : Lab := runG (guardRowless dRowless) dRowless lab0 evRowless
    theorem world_run_rowless : (runG (guardB specK dRowless) dRowless lab0 evRowless).made 6 = false := rfl
    theorem rowless_regrant : regrant dRowless (runRowless.dec 6) (runRowless.dec 5) 3 = false := rfl
    theorem rowless_gate_leaks : Not (NoLeak dRowless runRowless) := fun h =>
      Bool.noConfusion (Eq.trans rowless_regrant.symm ((h 6 3 rfl rfl (by decide) rfl).1 5 rfl rfl rfl rfl))
    theorem world_refuses_rowless : guardB specK dRowless (fun y => dRowless.past y 6) 6 = false := rfl

    -- Mutant B, a seal that skips the gate: Wrap 3 hands epoch 2 to S (row 1)
    -- before Remove 4 of S; Put 5 is sealed under epoch 2 after it.
    def dNoGate : Dag :=
      dagOf 6 [1, 1, 3, 4, 2, 5] [1, 0, 0, 0, 0, 0] [1, 0, 0, 0, 0, 0] [0, 0, 0, 2, 0, 2]
        [0, 0, 0, 0, 0, 0] [0, 0, 1, 0, 0, 0]
        [[], [0], [0, 1], [0, 1, 2], [0, 1, 2, 3], [0, 1, 2, 3, 4]]
    def guardNoGate (D : Dag) : (Nat → Bool) → Nat → Bool :=
      guardOf (wrapsUnrevoked specK D) (fun _ x => counts specK D x) D
    def evNoGate : List Ev :=
      [Ev.sign 0, Ev.sign 1, Ev.sign 2, Ev.sign 3, Ev.sign 4, Ev.sign 5]
    def runNoGate : Lab := runG (guardNoGate dNoGate) dNoGate lab0 evNoGate
    theorem world_run_no_gate : (runG (guardB specK dNoGate) dNoGate lab0 evNoGate).made 5 = false := rfl
    theorem no_gate_regrant : regrant dNoGate (runNoGate.dec 5) (runNoGate.dec 3) 1 = false := rfl
    theorem seal_without_gate_leaks : Not (NoLeak dNoGate runNoGate) := fun h =>
      Bool.noConfusion (Eq.trans no_gate_regrant.symm ((h 5 1 rfl rfl (by decide) rfl).1 3 rfl rfl rfl rfl))
    theorem world_refuses_no_gate : guardB specK dNoGate (fun y => dNoGate.past y 5) 5 = false := rfl

    -- Mutant C, the writer's gate before PF1 (Tier 1's wrapGate): judged by the
    -- Wraps the writer holds. Wrap 3 of epoch 2 to S (Mac 1) is concurrent with
    -- Remove 4 of S; the writer (Mac 0) lacks it and seals Put 5 under epoch 2.
    def dHeldWrap : Dag :=
      dagOf 6 [1, 1, 3, 4, 2, 5] [1, 0, 0, 0, 0, 0] [1, 0, 0, 0, 0, 0] [0, 0, 0, 2, 0, 2]
        [0, 0, 0, 1, 0, 0] [0, 0, 1, 0, 0, 0]
        [[], [0], [0, 1], [0, 1, 2], [0, 1, 2], [0, 1, 2, 4]]
    def wrapsHeld (D : Dag) (V : Nat → Bool) (x r : Nat) : Bool :=
      anyN D.n (fun w => V w && (decide (D.kind w = 4) && (decide (D.epoch w = D.epoch x) && decide (D.rcp w + 1 = r))))
    def guardHeldWrap (D : Dag) : (Nat → Bool) → Nat → Bool :=
      guardOf (wrapsUnrevoked specK D) (fun V x => counts specK D x && wrapGate (tableOf D V) (wrapsHeld D V x)) D
    def evHeldWrap : List Ev :=
      [Ev.sign 0, Ev.sign 1, Ev.sign 2, Ev.deliver 1 0, Ev.deliver 1 1, Ev.deliver 1 2, Ev.sign 3, Ev.sign 4, Ev.sign 5]
    def runHeldWrap : Lab := runG (guardHeldWrap dHeldWrap) dHeldWrap lab0 evHeldWrap
    theorem world_run_held_wrap : (runG (guardB specK dHeldWrap) dHeldWrap lab0 evHeldWrap).made 5 = false := rfl
    theorem held_wrap_regrant : regrant dHeldWrap (runHeldWrap.dec 5) (runHeldWrap.dec 3) 1 = false := rfl
    theorem held_wrap_gate_leaks : Not (NoLeak dHeldWrap runHeldWrap) := fun h =>
      Bool.noConfusion (Eq.trans held_wrap_regrant.symm ((h 5 1 rfl rfl (by decide) rfl).1 3 rfl rfl rfl rfl))
    theorem world_refuses_held_wrap : guardB specK dHeldWrap (fun y => dHeldWrap.past y 5) 5 = false := rfl

    -- Mutant D, a Wrap that skips current (a heal, a fill or a mint wrapping a
    -- recipient its view revokes): epoch 2 follows Remove 1 of S, and Wrap 3 hands
    -- it to S's recipient.
    def dWrapAny : Dag :=
      dagOf 4 [1, 2, 3, 4] [0, 0, 0, 0] [0, 0, 0, 0] [0, 0, 0, 2] [0, 0, 0, 0] [0, 0, 1, 0]
        [[], [0], [0, 1], [0, 1, 2]]
    def guardWrapAny (D : Dag) : (Nat → Bool) → Nat → Bool :=
      guardOf (fun _ _ => true) (fun V x => counts specK D x && opens specK D V x) D
    def evWrapAny : List Ev :=
      [Ev.sign 0, Ev.sign 1, Ev.sign 2, Ev.sign 3]
    def runWrapAny : Lab := runG (guardWrapAny dWrapAny) dWrapAny lab0 evWrapAny
    theorem world_run_wrap_any : (runG (guardB specK dWrapAny) dWrapAny lab0 evWrapAny).made 3 = false := rfl
    theorem wrap_any_regrant : regrant dWrapAny (viewOf dWrapAny 2) (runWrapAny.dec 3) 0 = false := rfl
    theorem wrap_without_current_leaks : Not (NoLeakMint dWrapAny runWrapAny) := fun h =>
      Bool.noConfusion (Eq.trans wrap_any_regrant.symm (h 2 0 (by decide) rfl 3 rfl rfl rfl rfl))
    theorem world_refuses_wrap_any : guardB specK dWrapAny (fun y => dWrapAny.past y 3) 3 = false := rfl

    -- Mutant E, a seal under an epoch that does not count: S (row 1) mints epoch 3
    -- after its own Remove 2; T seals Put 4 under it, the gate open.
    def dNoMember : Dag :=
      dagOf 5 [1, 1, 2, 3, 5] [1, 0, 0, 0, 0] [1, 0, 0, 0, 0] [0, 0, 0, 0, 3] [0, 0, 0, 1, 0] [0, 0, 0, 0, 0]
        [[], [0], [0, 1], [0, 1, 2], [0, 1, 2, 3]]
    def guardNoMember (D : Dag) : (Nat → Bool) → Nat → Bool := guardOf (wrapsUnrevoked specK D) (opens specK D) D
    def evNoMember : List Ev :=
      [Ev.sign 0, Ev.sign 1, Ev.sign 2, Ev.deliver 1 0, Ev.deliver 1 1, Ev.deliver 1 2, Ev.sign 3, Ev.deliver 0 3, Ev.sign 4]
    def runNoMember : Lab := runG (guardNoMember dNoMember) dNoMember lab0 evNoMember
    theorem world_run_no_member : (runG (guardB specK dNoMember) dNoMember lab0 evNoMember).made 4 = false := rfl
    theorem no_member_readmitted : readmitted dNoMember (runNoMember.dec 4) 3 1 = false := rfl
    theorem uncounted_epoch_leaks : Not (NoLeak dNoMember runNoMember) := fun h =>
      Bool.noConfusion (Eq.trans no_member_readmitted.symm ((h 4 1 rfl rfl (by decide) rfl).2 rfl))
    theorem world_refuses_no_member : guardB specK dNoMember (fun y => dNoMember.past y 4) 4 = false := rfl

    end AtermMeshNoLeak
}

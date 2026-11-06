// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P5b, the relay's liveness: the push edge and its flood bring every record of a current seat to
// every current seat under fair steps, and Inventory repairs what the edge let go (a retention cut
// below a cursor, and a destination log that lost records it had answered), proved for any number
// of Macs on every Trust build. The spec kernels are what relay_live.rs's contract! kernels state
// they compute; the model asks them as parameters, and `relay_live_binding_island.rs`
// instantiates it with the compiled Rust. `relay_live::tests` recomputes section 8's answer sheet
// from the Rust and reads its words from this file.
clean {
    -- The relay's and Inventory's scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/relay_live.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body equal to the definition here. They sit outside the namespace
    -- so a clause can name them.

    -- spec_cursor: the cursor a relay kept for its neighbour, from whether it has an
    -- entry, the entry is within CURSOR_KEPT, the neighbour's log is the same, and its
    -- lineage continues the one the entry names: 1 Kept, 2 Moved, 0 Gone.
    def spec_cursor (has fresh same continues : Bool) : UInt8 :=
      if (has && fresh) = true then (if same = true then 1 else if continues = true then 2 else 0) else 0
    -- spec_start_repair: the exchange a relay starting at cursor code c asks: 0 none
    -- (Kept), 1 a lineage exchange (Moved), 2 a new link's (Gone, and any other code).
    def spec_start_repair (c : UInt8) : UInt8 :=
      if c = 1 then 0 else if c = 2 then 1 else 2
    -- spec_requests: a request for an exchange is queued, unless it is a gap notice
    -- under NO_INVENTORY_ON_GAP.
    def spec_requests (gap noGap : Bool) : Bool := !(gap && noGap)
    -- spec_runs: a queued exchange runs, unless the pair's last one moved nothing and
    -- this Mac's summary has not changed since (the quiet rule).
    def spec_runs (remembers quietSame : Bool) : Bool := !(remembers && quietSame)
    -- spec_asks_again: a start that asked an exchange asks it once more, when the ingest
    -- has passed the last table record its log held at the start, unless its stores already
    -- held what its log held then (finding H2: the first exchange can run before the ingest
    -- and move nothing).
    def spec_asks_again (asked ingested : Bool) : Bool := asked && !ingested

    namespace AtermMeshRelayLive

    open AtermMeshTemporal

    -- ===========================================================================
    -- 1. One record, any number of Macs, a fixed overlay, the kernels as parameters.
    -- ===========================================================================

    -- The record r, written at its origin o, as each Mac holds it: in its tables log
    -- (`log`, what its relays read), in its stores (`store`, what Inventory lists);
    -- per relay edge s→d, whether the edge still owes d the record (`owes`: the
    -- relay's cursor is below it), whether d's log lost what the edge had landed in
    -- it (`lost`: the next relay start reads a log that moved), whether an exchange is
    -- asked (`asked`), whether s remembers the pair's last exchange as quiet, and whether a
    -- relay start owes the pair a second ask once s's stores ingest the record (`again`).

    structure W where
      log   : Nat → Bool
      store : Nat → Bool
      owes  : Nat → Nat → Bool
      lost  : Nat → Nat → Bool
      asked : Nat → Nat → Bool
      quiet : Nat → Nat → Bool
      again : Nat → Nat → Bool

    structure Kernels where
      cursor : Bool → Bool → Bool → Bool → UInt8
      startRepair : UInt8 → UInt8
      requests : Bool → Bool → Bool
      runs : Bool → Bool → Bool
      asksAgain : Bool → Bool → Bool

    def Exact (K : Kernels) : Prop :=
      (∀ a b c d, K.cursor a b c d = spec_cursor a b c d) ∧ (∀ c, K.startRepair c = spec_start_repair c) ∧
      (∀ a b, K.requests a b = spec_requests a b) ∧ (∀ a b, K.runs a b = spec_runs a b) ∧
      (∀ a b, K.asksAgain a b = spec_asks_again a b)

    def spec : Kernels := Kernels.mk spec_cursor spec_start_repair spec_requests spec_runs spec_asks_again

    theorem spec_exact : Exact spec :=
      ⟨fun _ _ _ _ => rfl, fun _ => rfl, fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl⟩

    -- The events: an edge lands the record in d's log (its flood owes d's own edges
    -- the record when d's log did not hold it), a Mac ingests its log into its stores,
    -- a retention cut drops the record from s's log (never past its ingest floor, so
    -- its stores hold it; each edge it owed asks an exchange by the gap notice), d's log
    -- loses it (an unclean stop or a renewed log; never a durable Mac's), a relay
    -- starts again (its entry's presence and freshness, and whether the lineage
    -- continues, are the run's; a start that asks while s's stores lack what s's log
    -- holds owes a second ask, which s's ingest, or a cut, answers), an asked exchange
    -- happens, and the digest trigger asks one (H1, B7c).
    inductive Ev where
      | land (s d : Nat)
      | ingest (m : Nat)
      | cut (s : Nat)
      | lose (d : Nat)
      | restart (s d : Nat) (has fresh cont : Bool)
      | exchange (s d : Nat)
      | digest (s d : Nat)

    def set1 (f : Nat → Bool) (m : Nat) (v : Bool) : Nat → Bool := fun x => if x = m then v else f x
    def set2 (f : Nat → Nat → Bool) (s d : Nat) (v : Bool) : Nat → Nat → Bool :=
      fun x y => if x = s then (if y = d then v else f x y) else f x y

    theorem set1_eq (f : Nat → Bool) (m : Nat) (v : Bool) : set1 f m v m = v := if_pos rfl
    theorem set1_ne (f : Nat → Bool) (m x : Nat) (v : Bool) (h : x ≠ m) : set1 f m v x = f x := if_neg h
    theorem set2_eq (f : Nat → Nat → Bool) (s d : Nat) (v : Bool) : set2 f s d v s d = v := by
      unfold set2; rw [if_pos rfl, if_pos rfl]
    theorem set2_ne (f : Nat → Nat → Bool) (s d x y : Nat) (v : Bool) (h : ¬ (x = s ∧ y = d)) :
        set2 f s d v x y = f x y := by
      unfold set2
      by_cases hx : x = s
      · rw [if_pos hx]
        by_cases hy : y = d
        · exact absurd ⟨hx, hy⟩ h
        · rw [if_neg hy]
      · rw [if_neg hx]

    def cursorOf (K : Kernels) (w : W) (s d : Nat) (has fresh cont : Bool) : UInt8 :=
      K.cursor has fresh (!w.lost s d) (w.lost s d && cont)

    def landW (nb : Nat → Nat → Bool) (w : W) (s d : Nat) : W :=
      { w with log := set1 w.log d true,
               owes := fun x y => if x = s ∧ y = d then false
                                  else if x = d then (w.owes x y || (!w.log d && nb x y)) else w.owes x y }

    def ingestW (w : W) (m : Nat) : W :=
      { w with store := set1 w.store m true,
               quiet := fun x y => if x = m then (w.quiet x y && w.store m) else w.quiet x y,
               asked := fun x y => if x = m then (w.asked x y || w.again x y) else w.asked x y,
               again := fun x y => if x = m then false else w.again x y }

    def cutW (K : Kernels) (noGap : Bool) (w : W) (s : Nat) : W :=
      { w with log := set1 w.log s false,
               owes := fun x y => if x = s then false else w.owes x y,
               asked := fun x y => if x = s then ((w.asked x y || (w.owes x y && K.requests true noGap)) ||
                                                  w.again x y)
                                   else w.asked x y,
               again := fun x y => if x = s then false else w.again x y }

    def loseW (w : W) (d : Nat) : W :=
      { w with log := set1 w.log d false,
               owes := fun x y => if x = d then false else w.owes x y,
               lost := fun x y => if y = d then true else w.lost x y }

    def restartW (K : Kernels) (w : W) (s d : Nat) (has fresh cont : Bool) : W :=
      { w with owes := set2 w.owes s d
                 (if cursorOf K w s d has fresh cont = 1 ∨ cursorOf K w s d has fresh cont = 2
                  then w.owes s d else false),
               asked := set2 w.asked s d
                 (w.asked s d || !(decide (K.startRepair (cursorOf K w s d has fresh cont) = 0))),
               lost := set2 w.lost s d false,
               again := set2 w.again s d
                 (w.again s d || K.asksAgain (!(decide (K.startRepair (cursorOf K w s d has fresh cont) = 0)))
                   (w.store s || !w.log s)) }

    def keepQ (w : W) (s d x : Nat) (u : Bool) : Bool :=
      if x = s then decide (w.store s = u) else if x = d then decide (w.store d = u) else true

    -- The exchange runs: both stores take the union; the pair remembers it as quiet when it moved
    -- nothing, and each side whose store changed forgets every quiet it remembered (its summary moved).
    def exRun (w : W) (s d : Nat) : W :=
      { w with store := fun x => if x = s ∨ x = d then (w.store s || w.store d) else w.store x,
               asked := set2 w.asked s d false,
               quiet := fun x y => if (x = s ∧ y = d) ∨ (x = d ∧ y = s) then decide (w.store s = w.store d)
                                   else (w.quiet x y && keepQ w s d x (w.store s || w.store d)) }

    -- The exchange is skipped: the request is answered and nothing moves.
    def exSkip (w : W) (s d : Nat) : W := { w with asked := set2 w.asked s d false }

    def exchangeW (K : Kernels) (w : W) (s d : Nat) : W :=
      if K.runs true (w.quiet s d) = true then exRun w s d else exSkip w s d

    theorem exchangeW_run (K : Kernels) (w : W) (s d : Nat) (hr : K.runs true (w.quiet s d) = true) :
        exchangeW K w s d = exRun w s d := if_pos hr
    theorem exchangeW_skip (K : Kernels) (w : W) (s d : Nat) (hr : ¬ K.runs true (w.quiet s d) = true) :
        exchangeW K w s d = exSkip w s d := if_neg hr

    def digestW (w : W) (s d : Nat) : W := { w with asked := set2 w.asked s d true }

    def guard (durable : Nat → Bool) : Ev → W → Bool
      | Ev.land s d, w => w.owes s d && w.log s
      | Ev.ingest m, w => w.log m
      | Ev.cut s, w => w.log s && w.store s
      | Ev.lose d, w => w.log d && !durable d
      | Ev.restart _ _ _ _ _, _ => true
      | Ev.exchange s d, w => w.asked s d
      | Ev.digest s d, w => !(decide (w.store s = w.store d))

    def eff (K : Kernels) (noGap : Bool) (nb : Nat → Nat → Bool) : Ev → W → W
      | Ev.land s d, w => landW nb w s d
      | Ev.ingest m, w => ingestW w m
      | Ev.cut s, w => cutW K noGap w s
      | Ev.lose d, w => loseW w d
      | Ev.restart s d h f c, w => restartW K w s d h f c
      | Ev.exchange s d, w => exchangeW K w s d
      | Ev.digest s d, w => digestW w s d

    structure Model where
      K : Kernels
      noGap : Bool
      durable : Nat → Bool
      nb : Nat → Nat → Bool

    def step (M : Model) (e : Ev) (w : W) : W :=
      if guard M.durable e w = true then eff M.K M.noGap M.nb e w else w

    theorem step_spec (M : Model) (e : Ev) (w : W) :
        (guard M.durable e w = true ∧ step M e w = eff M.K M.noGap M.nb e w) ∨ step M e w = w := by
      unfold step
      by_cases hg : guard M.durable e w = true
      · exact Or.inl ⟨hg, if_pos hg⟩
      · exact Or.inr (if_neg hg)

    def Init (nb : Nat → Nat → Bool) (o : Nat) (w : W) : Prop :=
      (∀ x, w.log x = decide (x = o)) ∧ (∀ x, w.store x = decide (x = o)) ∧
      (∀ x y, w.owes x y = (decide (x = o) && nb x y)) ∧ (∀ x y, w.lost x y = false) ∧
      (∀ x y, w.asked x y = false) ∧ (∀ x y, w.quiet x y = false) ∧ (∀ x y, w.again x y = false)

    def SpecM (M : Model) (o : Nat) : StateMachine W :=
      StateMachine.mk (Init M.nb o) (fun w w' => ∃ e, w' = step M e w)

    def RunsE (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) : Prop :=
      Init M.nb o (b 0) ∧ ∀ n, b (n + 1) = step M (ev n) (b n)

    theorem runsE_runs (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (r : RunsE M o b ev) :
        Runs (SpecM M o) b :=
      ⟨r.1, fun n => Or.inl ⟨ev n, r.2 n⟩⟩


    -- ===========================================================================
    -- 2. Safety facts every liveness proof below stands on.
    -- ===========================================================================

    theorem or_l (a b : Bool) (h : a = true) : (a || b) = true := by rw [h]
    theorem or_r (a b : Bool) (h : b = true) : (a || b) = true := by rw [h]; cases a <;> rfl
    theorem and_l (a b : Bool) (h : (a && b) = true) : a = true := by
      cases a <;> cases b <;> first | rfl | exact absurd h (by decide)
    theorem and_r (a b : Bool) (h : (a && b) = true) : b = true := by
      cases a <;> cases b <;> first | rfl | exact absurd h (by decide)

    -- A store never loses the record.
    theorem store_mono (M : Model) (e : Ev) (w : W) (x : Nat) (h : w.store x = true) :
        (step M e w).store x = true := by
      rcases step_spec M e w with ⟨_, he⟩ | he
      · rw [he]
        cases e with
        | land s d => exact h
        | ingest m =>
          show set1 w.store m true x = true
          by_cases hx : x = m
          · rw [hx, set1_eq]
          · rw [set1_ne _ _ _ _ hx]; exact h
        | cut s => exact h
        | lose d => exact h
        | restart s d f g c => exact h
        | exchange s d =>
          show (exchangeW M.K w s d).store x = true
          by_cases hr : M.K.runs true (w.quiet s d) = true
          · rw [exchangeW_run M.K w s d hr]
            show (if x = s ∨ x = d then (w.store s || w.store d) else w.store x) = true
            by_cases hx : x = s ∨ x = d
            · rw [if_pos hx]
              cases hx with
              | inl e => rw [← e]; exact or_l _ _ h
              | inr e => rw [← e]; exact or_r _ _ h
            · rw [if_neg hx]; exact h
          · rw [exchangeW_skip M.K w s d hr]; exact h
        | digest s d => exact h
      · rw [he]; exact h

    -- The quiet rule's law: an exchange this Mac remembers as quiet is one whose
    -- neighbour holds the record whenever this Mac does.
    def QInv (w : W) : Prop := ∀ s d, w.quiet s d = true → w.store s = true → w.store d = true

    theorem init_qinv (nb : Nat → Nat → Bool) (o : Nat) (w : W) (h : Init nb o w) : QInv w :=
      fun s d hq _ => absurd (hq.symm.trans (h.2.2.2.2.2.1 s d)) (by decide)

    theorem step_qinv (M : Model) (e : Ev) (w : W) (hi : QInv w) : QInv (step M e w) := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · intro s d hq hs
        rw [he] at hq hs ⊢
        cases e with
        | land a b => exact hi s d hq hs
        | ingest m =>
          have hq' : (if s = m then (w.quiet s d && w.store m) else w.quiet s d) = true := hq
          show set1 w.store m true d = true
          by_cases hd : d = m
          · rw [hd, set1_eq]
          · rw [set1_ne _ _ _ _ hd]
            by_cases hsm : s = m
            · rw [if_pos hsm] at hq'
              have hm : w.store m = true := and_r _ _ hq'
              exact hi s d (and_l _ _ hq') (by rw [hsm]; exact hm)
            · rw [if_neg hsm] at hq'
              have hs' : w.store s = true := by
                have : set1 w.store m true s = true := hs
                rw [set1_ne _ _ _ _ hsm] at this; exact this
              exact hi s d hq' hs'
        | cut a => exact hi s d hq hs
        | lose a => exact hi s d hq hs
        | restart a b f g c => exact hi s d hq hs
        | exchange a b =>
          have hq0 : (exchangeW M.K w a b).quiet s d = true := hq
          have hs0 : (exchangeW M.K w a b).store s = true := hs
          show (exchangeW M.K w a b).store d = true
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at hq0 hs0 ⊢
            have hq1 : (if (s = a ∧ d = b) ∨ (s = b ∧ d = a) then decide (w.store a = w.store b)
                else (w.quiet s d && keepQ w a b s (w.store a || w.store b))) = true := hq0
            have hs1 : (if s = a ∨ s = b then (w.store a || w.store b) else w.store s) = true := hs0
            show (if d = a ∨ d = b then (w.store a || w.store b) else w.store d) = true
            by_cases hp : (s = a ∧ d = b) ∨ (s = b ∧ d = a)
            · have hsab : s = a ∨ s = b := Or.elim hp (fun h1 => Or.inl h1.1) (fun h2 => Or.inr h2.1)
              have hdab : d = a ∨ d = b := Or.elim hp (fun h1 => Or.inr h1.2) (fun h2 => Or.inl h2.2)
              rw [if_pos hsab] at hs1
              rw [if_pos hdab]; exact hs1
            · rw [if_neg hp] at hq1
              have hqo : w.quiet s d = true := and_l _ _ hq1
              have hk : keepQ w a b s (w.store a || w.store b) = true := and_r _ _ hq1
              have hss : w.store s = true := by
                by_cases hsa : s = a
                · unfold keepQ at hk; rw [if_pos hsa] at hk
                  rw [if_pos (Or.inl hsa)] at hs1
                  rw [hsa]; exact (of_decide_eq_true hk).trans hs1
                · by_cases hsb : s = b
                  · unfold keepQ at hk; rw [if_neg hsa, if_pos hsb] at hk
                    rw [if_pos (Or.inr hsb)] at hs1
                    rw [hsb]; exact (of_decide_eq_true hk).trans hs1
                  · rw [if_neg (fun (h : s = a ∨ s = b) => Or.elim h hsa hsb)] at hs1; exact hs1
              have hdd : w.store d = true := hi s d hqo hss
              by_cases hdab : d = a ∨ d = b
              · rw [if_pos hdab]
                cases hdab with
                | inl e => rw [← e]; exact or_l _ _ hdd
                | inr e => rw [← e]; exact or_r _ _ hdd
              · rw [if_neg hdab]; exact hdd
          · rw [exchangeW_skip M.K w a b hr] at hq0 hs0 ⊢
            exact hi s d hq0 hs0
        | digest a b => exact hi s d hq hs
      · intro s d hq hs
        rw [he] at hq hs ⊢; exact hi s d hq hs

    theorem quiet_safe (M : Model) (o : Nat) : Satisfies (SpecM M o) (Always (Lift QInv)) :=
      invariantSatisfies (SpecM M o) QInv (init_qinv M.nb o)
        (fun w w' hi hn => Exists.elim hn (fun e he => by rw [he]; exact step_qinv M e w hi))


    -- ===========================================================================
    -- 3. What ends an edge's debt, a lost log and an asked exchange, step by step.
    -- ===========================================================================

    theorem lose_lost (w : W) (a y : Nat) : (loseW w a).lost y a = true := by
      show (if a = a then true else w.lost y a) = true
      rw [if_pos rfl]
    theorem asks_of_ne (u : UInt8) (h : u ≠ 0) : (!(decide (u = 0))) = true := by
      cases hd : decide (u = 0) with
      | true => exact absurd (of_decide_eq_true hd) h
      | false => rfl
    theorem ff_and (a : Bool) (h : (a && false) = true) : False := by
      cases a <;> exact absurd h (by decide)
    theorem ne_true (a : Bool) (h : a = true) (h' : a = false) : False := absurd (h.symm.trans h') (by decide)

    -- What a Mac carries stays carried: a durable Mac's log loses the record only to a
    -- cut, which never passes its ingest floor, so its stores hold it.
    theorem step_carrier (M : Model) (e : Ev) (w : W) (s : Nat) (hd : M.durable s = true)
        (h : (w.log s || w.store s) = true) : ((step M e w).log s || (step M e w).store s) = true := by
      cases hs : w.store s with
      | true => exact or_r _ _ (store_mono M e w s hs)
      | false =>
        have hl : w.log s = true := by
          rw [hs] at h
          cases hl0 : w.log s with
          | true => rfl
          | false => rw [hl0] at h; exact absurd h (by decide)
        have hl' : (step M e w).log s = true := by
          rcases step_spec M e w with ⟨hg, he⟩ | he
          · rw [he]
            cases e with
            | land a b =>
              show set1 w.log b true s = true
              by_cases hb : s = b
              · rw [hb, set1_eq]
              · rw [set1_ne _ _ _ _ hb]; exact hl
            | ingest m => exact hl
            | cut a =>
              show set1 w.log a false s = true
              by_cases ha : s = a
              · have hg' : (w.log a && w.store a) = true := hg
                rw [← ha, hs] at hg'; exact (ff_and _ hg').elim
              · rw [set1_ne _ _ _ _ ha]; exact hl
            | lose a =>
              show set1 w.log a false s = true
              by_cases ha : s = a
              · have hg' : (w.log a && !M.durable a) = true := hg
                rw [← ha, hd] at hg'; exact (ff_and _ hg').elim
              · rw [set1_ne _ _ _ _ ha]; exact hl
            | restart a b f g c => exact hl
            | exchange a b =>
              show (exchangeW M.K w a b).log s = true
              by_cases hr : M.K.runs true (w.quiet a b) = true
              · rw [exchangeW_run M.K w a b hr]; exact hl
              · rw [exchangeW_skip M.K w a b hr]; exact hl
            | digest a b => exact hl
          · rw [he]; exact hl
        exact or_l _ _ hl'

    theorem quiet_of_skip (K : Kernels) (hK : Exact K) (q : Bool) (hr : ¬ K.runs true q = true) : q = true := by
      rw [hK.2.2.2.1 true q] at hr
      cases q with
      | false => exact absurd rfl hr
      | true => rfl

    -- An asked exchange ends only by running or by being skipped; either way, the
    -- neighbour then holds what this Mac held.
    theorem asked_drop (M : Model) (hK : Exact M.K) (e : Ev) (w : W) (s d : Nat) (hi : QInv w)
        (ha : w.asked s d = true) (ha' : (step M e w).asked s d = false) (hs : w.store s = true) :
        (step M e w).store d = true := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he] at ha' ⊢
        cases e with
        | land a b => exact (ne_true _ ha ha').elim
        | ingest m =>
          have h1 : (if s = m then (w.asked s d || w.again s d) else w.asked s d) = false := ha'
          by_cases hsm : s = m
          · rw [if_pos hsm] at h1; exact (ne_true _ (or_l _ _ ha) h1).elim
          · rw [if_neg hsm] at h1; exact (ne_true _ ha h1).elim
        | cut a =>
          have h1 : (if s = a then ((w.asked s d || (w.owes s d && M.K.requests true M.noGap)) || w.again s d)
              else w.asked s d) = false := ha'
          by_cases hsa : s = a
          · rw [if_pos hsa] at h1; exact (ne_true _ (or_l _ _ (or_l _ _ ha)) h1).elim
          · rw [if_neg hsa] at h1; exact (ne_true _ ha h1).elim
        | lose a => exact (ne_true _ ha ha').elim
        | restart a b f g c =>
          have h1 : set2 w.asked a b (w.asked a b || !(decide (M.K.startRepair (cursorOf M.K w a b f g c) = 0))) s d = false := ha'
          by_cases hp : s = a ∧ d = b
          · rw [hp.1, hp.2, set2_eq] at h1; rw [hp.1, hp.2] at ha; exact (ne_true _ (or_l _ _ ha) h1).elim
          · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ ha h1).elim
        | exchange a b =>
          show (exchangeW M.K w a b).store d = true
          have h0 : (exchangeW M.K w a b).asked s d = false := ha'
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at h0 ⊢
            have h1 : set2 w.asked a b false s d = false := h0
            by_cases hp : s = a ∧ d = b
            · show (if d = a ∨ d = b then (w.store a || w.store b) else w.store d) = true
              rw [if_pos (Or.inr hp.2)]; rw [← hp.1]; exact or_l _ _ hs
            · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ ha h1).elim
          · rw [exchangeW_skip M.K w a b hr] at h0 ⊢
            have h1 : set2 w.asked a b false s d = false := h0
            by_cases hp : s = a ∧ d = b
            · show w.store d = true
              have hq : w.quiet a b = true := quiet_of_skip M.K hK _ hr
              rw [hp.1] at hs; rw [hp.2]; exact hi a b hq hs
            · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ ha h1).elim
        | digest a b =>
          have h1 : set2 w.asked a b true s d = false := ha'
          by_cases hp : s = a ∧ d = b
          · rw [hp.1, hp.2, set2_eq] at h1; exact absurd h1 (by decide)
          · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ ha h1).elim
      · rw [he] at ha'; exact (ne_true _ ha ha').elim

    -- A cursor read where the neighbour's log moved is never Kept, and its start asks.
    theorem moved_cursor (K : Kernels) (hK : Exact K) (h f c : Bool) :
        K.cursor h f false c ≠ 1 ∧ K.startRepair (K.cursor h f false c) ≠ 0 := by
      rw [hK.1 h f false c, hK.2.1]
      cases h <;> cases f <;> cases c <;> decide

    theorem start_repair_asks (K : Kernels) (hK : Exact K) (c : UInt8) (h1 : c ≠ 1) (h2 : c ≠ 2) :
        K.startRepair c ≠ 0 := by
      rw [hK.2.1 c]; unfold spec_start_repair; rw [if_neg h1, if_neg h2]; decide

    -- A relay that finds its neighbour's log moved starts again and asks an exchange.
    theorem lost_drop (M : Model) (hK : Exact M.K) (e : Ev) (w : W) (s d : Nat)
        (hl : w.lost s d = true) (hl' : (step M e w).lost s d = false) :
        (step M e w).asked s d = true ∧ ∃ a f c, e = Ev.restart s d a f c := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he] at hl' ⊢
        cases e with
        | land a b => exact (ne_true _ hl hl').elim
        | ingest m => exact (ne_true _ hl hl').elim
        | cut a => exact (ne_true _ hl hl').elim
        | lose a =>
          have h1 : (if d = a then true else w.lost s d) = false := hl'
          by_cases hda : d = a
          · rw [if_pos hda] at h1; exact absurd h1 (by decide)
          · rw [if_neg hda] at h1; exact (ne_true _ hl h1).elim
        | restart a b f g c =>
          have h1 : set2 w.lost a b false s d = false := hl'
          by_cases hp : s = a ∧ d = b
          · refine ⟨?_, ⟨f, g, c, by rw [hp.1, hp.2]⟩⟩
            show set2 w.asked a b (w.asked a b || !(decide (M.K.startRepair (cursorOf M.K w a b f g c) = 0))) s d = true
            rw [hp.1, hp.2, set2_eq]
            rw [hp.1, hp.2] at hl
            have hc : cursorOf M.K w a b f g c = M.K.cursor f g false c := by
              unfold cursorOf; rw [hl]
            rw [hc]
            apply or_r
            exact asks_of_ne _ (moved_cursor M.K hK f g c).2
          · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ hl h1).elim
        | exchange a b =>
          have h0 : (exchangeW M.K w a b).lost s d = false := hl'
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at h0; exact (ne_true _ hl h0).elim
          · rw [exchangeW_skip M.K w a b hr] at h0; exact (ne_true _ hl h0).elim
        | digest a b => exact (ne_true _ hl hl').elim
      · rw [he] at hl'; exact (ne_true _ hl hl').elim

    -- A log loses the record only to a cut (its stores hold it) or to a loss of
    -- what it answered (every relay into it then finds its log moved).
    theorem log_drop (M : Model) (e : Ev) (w : W) (x : Nat) (hl : w.log x = true)
        (hl' : (step M e w).log x = false) : w.store x = true ∨ ∀ y, (step M e w).lost y x = true := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he] at hl' ⊢
        cases e with
        | land a b =>
          have h1 : set1 w.log b true x = false := hl'
          by_cases hb : x = b
          · rw [hb, set1_eq] at h1; exact absurd h1 (by decide)
          · rw [set1_ne _ _ _ _ hb] at h1; exact (ne_true _ hl h1).elim
        | ingest m => exact (ne_true _ hl hl').elim
        | cut a =>
          have h1 : set1 w.log a false x = false := hl'
          by_cases ha : x = a
          · have hg' : (w.log a && w.store a) = true := hg
            rw [← ha] at hg'; exact Or.inl (and_r _ _ hg')
          · rw [set1_ne _ _ _ _ ha] at h1; exact (ne_true _ hl h1).elim
        | lose a =>
          have h1 : set1 w.log a false x = false := hl'
          by_cases ha : x = a
          · rw [ha]; exact Or.inr (fun y => lose_lost w a y)
          · rw [set1_ne _ _ _ _ ha] at h1; exact (ne_true _ hl h1).elim
        | restart a b f g c => exact (ne_true _ hl hl').elim
        | exchange a b =>
          have h0 : (exchangeW M.K w a b).log x = false := hl'
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at h0; exact (ne_true _ hl h0).elim
          · rw [exchangeW_skip M.K w a b hr] at h0; exact (ne_true _ hl h0).elim
        | digest a b => exact (ne_true _ hl hl').elim
      · rw [he] at hl'; exact (ne_true _ hl hl').elim

    theorem land_log (nb : Nat → Nat → Bool) (w : W) (a b x : Nat) (h : w.log x = true) :
        (landW nb w a b).log x = true := by
      show set1 w.log b true x = true
      by_cases hb : x = b
      · rw [hb, set1_eq]
      · rw [set1_ne _ _ _ _ hb]; exact h

    theorem land_owes (nb : Nat → Nat → Bool) (w : W) (a b s d : Nat) (hp : ¬ (s = a ∧ d = b))
        (h : w.owes s d = true) : (landW nb w a b).owes s d = true := by
      show (if s = a ∧ d = b then false
            else if s = b then (w.owes s d || (!w.log b && nb s d)) else w.owes s d) = true
      rw [if_neg hp]
      by_cases hb : s = b
      · rw [if_pos hb]; exact or_l _ _ h
      · rw [if_neg hb]; exact h

    theorem both (a b : Bool) (ha : a = true) (hb : b = true) : (a && b) = true := by rw [ha, hb]
    theorem or_false_l (x : Bool) (h : (x || false) = true) : x = true := by
      cases x <;> first | rfl | exact absurd h (by decide)
    theorem and_not_false (x : Bool) (h : (x && !false) = false) : x = false := by
      cases x <;> first | rfl | exact absurd h (by decide)
    theorem and_true_false (x : Bool) (h : (x && true) = false) : x = false := by
      cases x <;> first | rfl | exact absurd h (by decide)
    theorem true_not_false (x : Bool) (h : (true && !x) = false) : x = true := by
      cases x <;> first | rfl | exact absurd h (by decide)
    theorem digest_left (x : Bool) (h : (true && true && !false && !x) = false) : x = true := by
      cases x <;> first | rfl | exact absurd h (by decide)
    theorem not_true (a : Bool) (h : ¬ a = true) : a = false := by
      cases a with
      | false => rfl
      | true => exact absurd rfl h
    theorem dec_true {p : Prop} [Decidable p] (h : p) : decide p = true := by
      cases hd : decide p with
      | true => rfl
      | false => exact absurd h (of_decide_eq_false hd)
    theorem or_mono (a b a' b' : Bool) (ha : a = true → a' = true) (hb : b = true → b' = true)
        (h : (a || b) = true) : (a' || b') = true := by
      cases hx : a with
      | true => exact or_l _ _ (ha hx)
      | false => rw [hx] at h; exact or_r _ _ (hb h)

    -- An edge's debt ends by landing, by a cut (which asks the exchange, its stores holding the
    -- record), or by a relay that starts again anywhere but its kept cursor (which asks too).
    theorem owes_drop (M : Model) (hK : Exact M.K) (hg0 : M.noGap = false) (e : Ev) (w : W)
        (s d : Nat) (hd : M.durable s = true) (h : (w.owes s d && w.log s) = true)
        (h' : ((step M e w).owes s d && (step M e w).log s) = false) :
        (step M e w).log d = true ∨ ((step M e w).asked s d = true ∧ w.store s = true) ∨
          (∃ f g c, e = Ev.restart s d f g c ∧ cursorOf M.K w s d f g c ≠ 1 ∧
            (step M e w).asked s d = true) := by
      have ho : w.owes s d = true := and_l _ _ h
      have hl : w.log s = true := and_r _ _ h
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he] at h' ⊢
        cases e with
        | land a b =>
          by_cases hp : s = a ∧ d = b
          · exact Or.inl (by show set1 w.log b true d = true; rw [← hp.2, set1_eq])
          · exact (ne_true _ (both _ _ (land_owes M.nb w a b s d hp ho) (land_log M.nb w a b s hl)) h').elim
        | ingest m => exact (ne_true _ h h').elim
        | cut a =>
          by_cases hsa : s = a
          · have hg' : (w.log a && w.store a) = true := hg
            rw [← hsa] at hg'
            refine Or.inr (Or.inl ⟨?_, and_r _ _ hg'⟩)
            show (if s = a then ((w.asked s d || (w.owes s d && M.K.requests true M.noGap)) || w.again s d)
                  else w.asked s d) = true
            rw [if_pos hsa, ho, hK.2.2.1 true M.noGap, hg0]
            exact or_l _ _ (or_r _ _ rfl)
          · have hx : ((cutW M.K M.noGap w a).owes s d && (cutW M.K M.noGap w a).log s) = true := by
              show ((if s = a then false else w.owes s d) && set1 w.log a false s) = true
              rw [if_neg hsa, set1_ne _ _ _ _ hsa]; exact h
            exact (ne_true _ hx h').elim
        | lose a =>
          by_cases hsa : s = a
          · have hg' : (w.log a && !M.durable a) = true := hg
            rw [← hsa, hd] at hg'; exact (ff_and _ hg').elim
          · have hx : ((loseW w a).owes s d && (loseW w a).log s) = true := by
              show ((if s = a then false else w.owes s d) && set1 w.log a false s) = true
              rw [if_neg hsa, set1_ne _ _ _ _ hsa]; exact h
            exact (ne_true _ hx h').elim
        | restart a b f g c =>
          by_cases hp : s = a ∧ d = b
          · have ho' : w.owes a b = true := by rw [← hp.1, ← hp.2]; exact ho
            have hl' : w.log a = true := by rw [← hp.1]; exact hl
            rw [hp.1, hp.2] at h' ⊢
            by_cases hk : cursorOf M.K w a b f g c = 1 ∨ cursorOf M.K w a b f g c = 2
            · have hx : ((restartW M.K w a b f g c).owes a b && (restartW M.K w a b f g c).log a) = true := by
                show (set2 w.owes a b (if cursorOf M.K w a b f g c = 1 ∨ cursorOf M.K w a b f g c = 2
                        then w.owes a b else false) a b && w.log a) = true
                rw [set2_eq, if_pos hk, ho', hl']
              exact (ne_true _ hx h').elim
            · have n1 : cursorOf M.K w a b f g c ≠ 1 := fun h1 => hk (Or.inl h1)
              have n2 : cursorOf M.K w a b f g c ≠ 2 := fun h2 => hk (Or.inr h2)
              refine Or.inr (Or.inr ⟨f, g, c, rfl, n1, ?_⟩)
              show set2 w.asked a b
                (w.asked a b || !(decide (M.K.startRepair (cursorOf M.K w a b f g c) = 0))) a b = true
              rw [set2_eq]
              exact or_r _ _ (asks_of_ne _ (start_repair_asks M.K hK _ n1 n2))
          · have hx : ((restartW M.K w a b f g c).owes s d && (restartW M.K w a b f g c).log s) = true := by
              show (set2 w.owes a b (if cursorOf M.K w a b f g c = 1 ∨ cursorOf M.K w a b f g c = 2
                      then w.owes a b else false) s d && w.log s) = true
              rw [set2_ne _ _ _ _ _ _ hp]; exact h
            exact (ne_true _ hx h').elim
        | exchange a b =>
          have h0 : ((exchangeW M.K w a b).owes s d && (exchangeW M.K w a b).log s) = false := h'
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at h0; exact (ne_true _ h h0).elim
          · rw [exchangeW_skip M.K w a b hr] at h0; exact (ne_true _ h h0).elim
        | digest a b => exact (ne_true _ h h').elim
      · rw [he] at h'; exact (ne_true _ h h').elim

    -- A relay that starts again anywhere but its kept cursor, its log holding the record, asks an
    -- exchange, and while its stores lack the record it owes the pair a second ask.
    theorem restart_asks (M : Model) (hK : Exact M.K) (w : W) (s d : Nat) (f g c : Bool)
        (hc : cursorOf M.K w s d f g c ≠ 1) (hl : w.log s = true) :
        (step M (Ev.restart s d f g c) w).asked s d = true ∧
          ((step M (Ev.restart s d f g c) w).store s = true ∨
            (step M (Ev.restart s d f g c) w).again s d = true) := by
      have hs : step M (Ev.restart s d f g c) w = restartW M.K w s d f g c := rfl
      have ha : M.K.startRepair (cursorOf M.K w s d f g c) ≠ 0 := by
        rw [hK.2.1]; unfold spec_start_repair; rw [if_neg hc]
        by_cases h2 : cursorOf M.K w s d f g c = 2
        · rw [if_pos h2]; decide
        · rw [if_neg h2]; decide
      rw [hs]
      refine ⟨?_, ?_⟩
      · show set2 w.asked s d
          (w.asked s d || !(decide (M.K.startRepair (cursorOf M.K w s d f g c) = 0))) s d = true
        rw [set2_eq]; exact or_r _ _ (asks_of_ne _ ha)
      · by_cases hst : w.store s = true
        · exact Or.inl hst
        · apply Or.inr
          show set2 w.again s d (w.again s d || M.K.asksAgain
            (!(decide (M.K.startRepair (cursorOf M.K w s d f g c) = 0))) (w.store s || !w.log s)) s d = true
          rw [set2_eq, hK.2.2.2.2, asks_of_ne _ ha, not_true _ hst, hl]
          exact or_r _ _ rfl

    -- A start's second ask ends only when s ingests, or cuts (which only a record its stores hold
    -- allows): the exchange is then asked, s's stores holding the record.
    theorem again_drop (M : Model) (e : Ev) (w : W) (s d : Nat)
        (ha : w.again s d = true) (ha' : (step M e w).again s d = false) :
        (step M e w).asked s d = true ∧ (step M e w).store s = true := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he] at ha' ⊢
        cases e with
        | land a b => exact (ne_true _ ha ha').elim
        | ingest m =>
          by_cases hsm : s = m
          · refine ⟨?_, ?_⟩
            · show (if s = m then (w.asked s d || w.again s d) else w.asked s d) = true
              rw [if_pos hsm]; exact or_r _ _ ha
            · show set1 w.store m true s = true
              rw [hsm, set1_eq]
          · have h1 : (if s = m then false else w.again s d) = false := ha'
            rw [if_neg hsm] at h1; exact (ne_true _ ha h1).elim
        | cut a =>
          by_cases hsa : s = a
          · have hg' : (w.log a && w.store a) = true := hg
            refine ⟨?_, ?_⟩
            · show (if s = a then ((w.asked s d || (w.owes s d && M.K.requests true M.noGap)) || w.again s d)
                  else w.asked s d) = true
              rw [if_pos hsa]; exact or_r _ _ ha
            · show w.store s = true
              rw [hsa]; exact and_r _ _ hg'
          · have h1 : (if s = a then false else w.again s d) = false := ha'
            rw [if_neg hsa] at h1; exact (ne_true _ ha h1).elim
        | lose a => exact (ne_true _ ha ha').elim
        | restart a b f g c =>
          have h1 : set2 w.again a b (w.again a b || M.K.asksAgain
              (!(decide (M.K.startRepair (cursorOf M.K w a b f g c) = 0))) (w.store a || !w.log a)) s d = false := ha'
          by_cases hp : s = a ∧ d = b
          · rw [hp.1, hp.2, set2_eq] at h1; rw [hp.1, hp.2] at ha; exact (ne_true _ (or_l _ _ ha) h1).elim
          · rw [set2_ne _ _ _ _ _ _ hp] at h1; exact (ne_true _ ha h1).elim
        | exchange a b =>
          have h0 : (exchangeW M.K w a b).again s d = false := ha'
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · rw [exchangeW_run M.K w a b hr] at h0; exact (ne_true _ ha h0).elim
          · rw [exchangeW_skip M.K w a b hr] at h0; exact (ne_true _ ha h0).elim
        | digest a b => exact (ne_true _ ha ha').elim
      · rw [he] at ha'; exact (ne_true _ ha ha').elim


    -- ===========================================================================
    -- 4. Runs, and the fairness premises a run is held to.
    -- ===========================================================================

    -- A condition that does not last forever: whenever it holds, a later state lacks it. For a
    -- step whose condition its own firing ends, this is weak fairness of that step.
    def Fades (P : Nat → Bool) : Prop := ∀ n, P n = true → ∃ k, P (n + k) = false

    theorem first_drop (P : Nat → Bool) (n : Nat) (h0 : P n = true) :
        ∀ k, P (n + k) = false → ∃ i, P (n + i) = true ∧ P (n + i + 1) = false := by
      intro k
      induction k with
      | zero => intro hk; exact absurd (h0.symm.trans hk) (by decide)
      | succ k ih =>
        intro hk
        by_cases hp : P (n + k) = true
        · exact ⟨k, hp, hk⟩
        · exact ih (not_true _ hp)

    theorem fade_edge (P : Nat → Bool) (hf : Fades P) (n : Nat) (h : P n = true) :
        ∃ i, P (n + i) = true ∧ P (n + i + 1) = false :=
      Exists.elim (hf n h) (fun k hk => first_drop P n h k hk)

    theorem run_store (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev)
        (x n : Nat) (h : (b n).store x = true) : ∀ k, (b (n + k)).store x = true := by
      intro k
      induction k with
      | zero => exact h
      | succ k ih =>
        show (b (n + k + 1)).store x = true
        rw [hr.2 (n + k)]
        exact store_mono M (ev (n + k)) (b (n + k)) x ih

    theorem run_qinv (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev) :
        ∀ n, QInv (b n) := by
      intro n
      induction n with
      | zero => exact init_qinv M.nb o (b 0) hr.1
      | succ n ih =>
        show QInv (b (n + 1))
        rw [hr.2 n]
        exact step_qinv M (ev n) (b n) ih

    theorem run_carrier (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev)
        (s : Nat) (hd : M.durable s = true) (n : Nat) (h : ((b n).log s || (b n).store s) = true) :
        ∀ k, ((b (n + k)).log s || (b (n + k)).store s) = true := by
      intro k
      induction k with
      | zero => exact h
      | succ k ih =>
        show ((b (n + k + 1)).log s || (b (n + k + 1)).store s) = true
        rw [hr.2 (n + k)]
        exact step_carrier M (ev (n + k)) (b (n + k)) s hd ih

    -- The fair hop from s to d: its relay lands what it owes, d ingests what its log holds, a relay
    -- that finds d's log moved starts again, an asked exchange happens, and a start's second ask is
    -- answered (s's ingest passes the last table record its log held at the start: H2's repair,
    -- `h2Run` below).
    def FairHop (b : Nat → W) (s d : Nat) : Prop :=
      Fades (fun i => (b i).owes s d && (b i).log s) ∧ Fades (fun i => (b i).log d && !(b i).store d) ∧
      Fades (fun i => (b i).lost s d) ∧ Fades (fun i => (b i).asked s d) ∧
      Fades (fun i => (b i).again s d)


    -- ===========================================================================
    -- 5. Inventory repairs what the edge let go.
    -- ===========================================================================

    -- An asked exchange, from a Mac whose stores hold the record, brings it to the neighbour.
    theorem asked_leads_to_store (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (s d n : Nat) (hfx : Fades (fun i => (b i).asked s d))
        (ha : (b n).asked s d = true) (hs : (b n).store s = true) : ∃ k, (b k).store d = true :=
      Exists.elim (fade_edge (fun i => (b i).asked s d) hfx n ha) (fun i hj => by
        have h0 : (b (n + i + 1)).asked s d = false := hj.2
        rw [hr.2 (n + i)] at h0
        exact ⟨n + i + 1, by
          rw [hr.2 (n + i)]
          exact asked_drop M hK (ev (n + i)) (b (n + i)) s d (run_qinv M o b ev hr (n + i)) hj.1 h0
            (run_store M o b ev hr s n hs i)⟩)

    -- A relay that starts again anywhere but its kept cursor, its log holding the record, asks an
    -- exchange from stores that hold it: at once, or by its second ask once s ingests (H2).
    theorem restart_leads_to_ask (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (s d t : Nat) (f g c : Bool)
        (hfa : Fades (fun i => (b i).again s d)) (he : ev t = Ev.restart s d f g c)
        (hc : cursorOf M.K (b t) s d f g c ≠ 1) (hl : (b t).log s = true) :
        ∃ j, (b j).asked s d = true ∧ (b j).store s = true := by
      have hst : b (t + 1) = step M (Ev.restart s d f g c) (b t) := by rw [hr.2 t, he]
      have hra := restart_asks M hK (b t) s d f g c hc hl
      rcases hra.2 with hs | hag
      · exact ⟨t + 1, by rw [hst]; exact hra.1, by rw [hst]; exact hs⟩
      · have hag1 : (b (t + 1)).again s d = true := by rw [hst]; exact hag
        exact Exists.elim (fade_edge _ hfa (t + 1) hag1) (fun i hj => by
          have h0 : (b (t + 1 + i + 1)).again s d = false := hj.2
          rw [hr.2 (t + 1 + i)] at h0
          have hd := again_drop M (ev (t + 1 + i)) (b (t + 1 + i)) s d hj.1 h0
          exact ⟨t + 1 + i + 1, by rw [hr.2 (t + 1 + i)]; exact hd.1,
            by rw [hr.2 (t + 1 + i)]; exact hd.2⟩)

    -- A relay into a log that lost what it answered starts again and asks, its stores holding it.
    theorem lost_leads_to_ask (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (s d m : Nat) (hd : M.durable s = true)
        (hc : ((b m).log s || (b m).store s) = true) (hfr : Fades (fun i => (b i).lost s d))
        (hfa : Fades (fun i => (b i).again s d)) (hl : (b m).lost s d = true) :
        ∃ j, (b j).asked s d = true ∧ (b j).store s = true :=
      Exists.elim (fade_edge _ hfr m hl) (fun i hj => by
        have hlj : (b (m + i)).lost s d = true := hj.1
        have h0 : (b (m + i + 1)).lost s d = false := hj.2
        rw [hr.2 (m + i)] at h0
        have hd' := lost_drop M hK (ev (m + i)) (b (m + i)) s d hlj h0
        have hcj : ((b (m + i)).log s || (b (m + i)).store s) = true := run_carrier M o b ev hr s hd m hc i
        by_cases hsj : (b (m + i)).store s = true
        · exact ⟨m + i + 1, by rw [hr.2 (m + i)]; exact hd'.1,
            by rw [hr.2 (m + i)]; exact store_mono M (ev (m + i)) (b (m + i)) s hsj⟩
        · have hls : (b (m + i)).log s = true := by
            rw [not_true _ hsj] at hcj; exact or_false_l _ hcj
          exact Exists.elim hd'.2 (fun f hf => Exists.elim hf (fun g hg => Exists.elim hg (fun c hc' => by
            have hcur : cursorOf M.K (b (m + i)) s d f g c ≠ 1 := by
              unfold cursorOf; rw [hlj]; exact (moved_cursor M.K hK f g c).1
            exact restart_leads_to_ask M o b ev hr hK s d (m + i) f g c hfa hc' hcur hls))))

    -- What d's log took from the edge reaches d's stores, or d's log lost it and Inventory brings it.
    theorem log_reaches_store (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (s d m : Nat) (hd : M.durable s = true)
        (hc : ((b m).log s || (b m).store s) = true) (hf : FairHop b s d)
        (hl : (b m).log d = true) : ∃ k, (b k).store d = true := by
      cases hsd : (b m).store d with
      | true => exact ⟨m, hsd⟩
      | false =>
        have hp : ((b m).log d && !(b m).store d) = true := by rw [hl, hsd]
        exact Exists.elim (fade_edge _ hf.2.1 m hp) (fun i hj => by
          have hpj : ((b (m + i)).log d && !(b (m + i)).store d) = true := hj.1
          have hp1 : ((b (m + i + 1)).log d && !(b (m + i + 1)).store d) = false := hj.2
          cases hs1 : (b (m + i + 1)).store d with
          | true => exact ⟨m + i + 1, hs1⟩
          | false =>
            rw [hs1] at hp1
            have hl1 : (step M (ev (m + i)) (b (m + i))).log d = false := by
              have h0 : (b (m + i + 1)).log d = false := and_not_false _ hp1
              rw [hr.2 (m + i)] at h0; exact h0
            rcases log_drop M (ev (m + i)) (b (m + i)) d (and_l _ _ hpj) hl1 with hst | hlost
            · have hn : (!(b (m + i)).store d) = true := and_r _ _ hpj
              rw [hst] at hn; exact absurd hn (by decide)
            · have hls : (b (m + i + 1)).lost s d = true := by rw [hr.2 (m + i)]; exact hlost s
              have hcj : ((b (m + i + 1)).log s || (b (m + i + 1)).store s) = true :=
                run_carrier M o b ev hr s hd m hc (i + 1)
              exact Exists.elim
                (lost_leads_to_ask M o b ev hr hK s d (m + i + 1) hd hcj hf.2.2.1 hf.2.2.2.2 hls)
                (fun k hk => asked_leads_to_store M o b ev hr hK s d k hf.2.2.2.1 hk.1 hk.2))

    -- The edge's debt is paid: a record the relay from a durable s owes d reaches d's stores, by the
    -- push, or by Inventory after a retention cut (gap notice), after d's log lost what it answered
    -- (lineage event), or after a relay that could not keep its cursor (new link), the start's
    -- second ask covering an exchange that ran before s ingested (H2).
    theorem the_edge_is_repaired (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (hg0 : M.noGap = false) (s d n : Nat)
        (hd : M.durable s = true) (hf : FairHop b s d)
        (h : ((b n).owes s d && (b n).log s) = true) : ∃ k, (b k).store d = true :=
      Exists.elim (fade_edge _ hf.1 n h) (fun i hj => by
        have hpj : ((b (n + i)).owes s d && (b (n + i)).log s) = true := hj.1
        have hp1 : ((b (n + i + 1)).owes s d && (b (n + i + 1)).log s) = false := hj.2
        rw [hr.2 (n + i)] at hp1
        have hlj : (b (n + i)).log s = true := and_r _ _ hpj
        have hc1 : ((b (n + i + 1)).log s || (b (n + i + 1)).store s) = true :=
          run_carrier M o b ev hr s hd (n + i) (or_l _ _ hlj) 1
        rcases owes_drop M hK hg0 (ev (n + i)) (b (n + i)) s d hd hpj hp1 with hland | hcut | ⟨f, g, c, hc'⟩
        · exact log_reaches_store M o b ev hr hK s d (n + i + 1) hd hc1 hf
            (by rw [hr.2 (n + i)]; exact hland)
        · exact asked_leads_to_store M o b ev hr hK s d (n + i + 1) hf.2.2.2.1
            (by rw [hr.2 (n + i)]; exact hcut.1)
            (by rw [hr.2 (n + i)]; exact store_mono M (ev (n + i)) (b (n + i)) s hcut.2)
        · exact Exists.elim
            (restart_leads_to_ask M o b ev hr hK s d (n + i) f g c hf.2.2.2.2 hc'.1 hc'.2.1 hlj)
            (fun k hk => asked_leads_to_store M o b ev hr hK s d k hf.2.2.2.1 hk.1 hk.2))


    -- ===========================================================================
    -- 6. Every current seat: the push edge and the flood, then Inventory hop by hop.
    -- ===========================================================================

    -- The seats the record's origin reaches over the overlay's edges.
    inductive Reach (nb : Nat → Nat → Bool) (o : Nat) : Nat → Prop where
      | here : Reach nb o o
      | hop (s d : Nat) : Reach nb o s → nb s d = true → Reach nb o d

    -- Premise H1, the digest trigger: while a neighbour's stores lack what s's stores hold and no
    -- exchange is asked, one is asked (B7c's presence digest; Inventory never republishes what it
    -- brought, so the edge alone repairs one hop).
    def FairDigest (M : Model) (b : Nat → W) (s d : Nat) : Prop :=
      Fades (fun i => M.nb s d && (b i).store s && !(b i).store d && !(b i).asked s d)

    theorem a_neighbour_gets_what_a_store_holds (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (s d n : Nat) (hnb : M.nb s d = true)
        (hdg : FairDigest M b s d) (hfx : Fades (fun i => (b i).asked s d))
        (hs : (b n).store s = true) : ∃ k, (b k).store d = true := by
      cases hsd : (b n).store d with
      | true => exact ⟨n, hsd⟩
      | false =>
        cases ha : (b n).asked s d with
        | true => exact asked_leads_to_store M o b ev hr hK s d n hfx ha hs
        | false =>
          have hp : (M.nb s d && (b n).store s && !(b n).store d && !(b n).asked s d) = true := by
            rw [hnb, hs, hsd, ha]
          exact Exists.elim (fade_edge _ hdg n hp) (fun i hj => by
            have hp1 : (M.nb s d && (b (n + i + 1)).store s && !(b (n + i + 1)).store d
                && !(b (n + i + 1)).asked s d) = false := hj.2
            have hs1 : (b (n + i + 1)).store s = true := run_store M o b ev hr s n hs (i + 1)
            rw [hnb, hs1] at hp1
            cases hd1 : (b (n + i + 1)).store d with
            | true => exact ⟨n + i + 1, hd1⟩
            | false =>
              rw [hd1] at hp1
              exact asked_leads_to_store M o b ev hr hK s d (n + i + 1) hfx (digest_left _ hp1) hs1)

    theorem dec_self (o : Nat) : decide (o = o) = true := dec_true rfl

    -- Every seat the origin reaches gets the record into its stores, whatever the edges let go.
    theorem every_seat_gets_the_record (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K)
        (fair : ∀ s d, M.nb s d = true → FairDigest M b s d ∧ Fades (fun i => (b i).asked s d)) :
        ∀ d, Reach M.nb o d → ∃ k, (b k).store d = true := by
      intro d hreach
      induction hreach with
      | here => exact ⟨0, by rw [hr.1.2.1 o]; exact dec_self o⟩
      | hop s' d' _ hnb ih =>
        exact Exists.elim ih (fun k hk =>
          a_neighbour_gets_what_a_store_holds M o b ev hr hK s' d' k hnb (fair s' d' hnb).1
            (fair s' d' hnb).2 hk)

    -- A run that lets nothing go: no retention cut, no lost log, and every relay that starts
    -- again finds its entry fresh.
    def Calm : Ev → Bool
      | Ev.land _ _ => true
      | Ev.ingest _ => true
      | Ev.cut _ => false
      | Ev.lose _ => false
      | Ev.restart _ _ h f _ => h && f
      | Ev.exchange _ _ => true
      | Ev.digest _ _ => true

    def LostNone (w : W) : Prop := ∀ x y, w.lost x y = false
    def FloodInv (nb : Nat → Nat → Bool) (w : W) : Prop :=
      ∀ m q, w.log m = true → nb m q = true → (w.owes m q || w.log q) = true

    theorem set2_same (f : Nat → Nat → Bool) (s d x y : Nat) : set2 f s d (f s d) x y = f x y := by
      unfold set2
      by_cases hx : x = s
      · rw [if_pos hx]
        by_cases hy : y = d
        · rw [if_pos hy, hx, hy]
        · rw [if_neg hy]
      · rw [if_neg hx]

    theorem kept_when_calm (K : Kernels) (hK : Exact K) (w : W) (hn : LostNone w) (a b : Nat) (c : Bool) :
        cursorOf K w a b true true c = 1 := by
      unfold cursorOf; rw [hn a b]
      exact (hK.1 true true (!false) (false && c)).trans rfl

    theorem calm_step (M : Model) (hK : Exact M.K) (e : Ev) (w : W) (hc : Calm e = true)
        (hn : LostNone w) (hfl : FloodInv M.nb w) :
        LostNone (step M e w) ∧ FloodInv M.nb (step M e w) ∧
          (∀ x, w.log x = true → (step M e w).log x = true) := by
      rcases step_spec M e w with ⟨hg, he⟩ | he
      · rw [he]
        cases e with
        | land a b =>
          refine ⟨hn, ?_, fun x h => land_log M.nb w a b x h⟩
          intro m q hm hq
          by_cases hqb : q = b
          · exact or_r _ _ (by show set1 w.log b true q = true; rw [hqb, set1_eq])
          · have hnp : ¬ (m = a ∧ q = b) := fun hp => hqb hp.2
            by_cases hmb : m = b
            · cases hlb : w.log b with
              | true =>
                have hq' : M.nb b q = true := by rw [← hmb]; exact hq
                have key : (w.owes b q || w.log q) = true := hfl b q hlb hq'
                have key' : (w.owes m q || w.log q) = true := by rw [hmb]; exact key
                exact or_mono _ _ _ _ (fun ho => land_owes M.nb w a b m q hnp ho)
                  (fun hl => land_log M.nb w a b q hl) key'
              | false =>
                apply or_l
                show (if m = a ∧ q = b then false
                      else if m = b then (w.owes m q || (!w.log b && M.nb m q)) else w.owes m q) = true
                rw [if_neg hnp, if_pos hmb, hlb, hq]
                exact or_r _ _ rfl
            · have hm' : w.log m = true := by
                have h0 : set1 w.log b true m = true := hm
                rw [set1_ne _ _ _ _ hmb] at h0; exact h0
              exact or_mono _ _ _ _ (fun ho => land_owes M.nb w a b m q hnp ho)
                (fun hl => land_log M.nb w a b q hl) (hfl m q hm' hq)
        | ingest m => exact ⟨hn, hfl, fun x h => h⟩
        | cut a => have hc' : false = true := hc; exact absurd hc' (by decide)
        | lose a => have hc' : false = true := hc; exact absurd hc' (by decide)
        | restart a b f g c =>
          have hf' : f = true := and_l _ _ hc
          have hg' : g = true := and_r _ _ hc
          have hk1 : cursorOf M.K w a b f g c = 1 := by
            rw [hf', hg']; exact kept_when_calm M.K hK w hn a b c
          refine ⟨?_, ?_, fun x h => h⟩
          · intro x y
            show set2 w.lost a b false x y = false
            by_cases hp : x = a ∧ y = b
            · rw [hp.1, hp.2, set2_eq]
            · rw [set2_ne _ _ _ _ _ _ hp]; exact hn x y
          · intro m q hm hq
            show (set2 w.owes a b (if cursorOf M.K w a b f g c = 1 ∨ cursorOf M.K w a b f g c = 2
                    then w.owes a b else false) m q || w.log q) = true
            rw [if_pos (Or.inl hk1), set2_same]
            exact hfl m q hm hq
        | exchange a b =>
          by_cases hr : M.K.runs true (w.quiet a b) = true
          · have e1 : eff M.K M.noGap M.nb (Ev.exchange a b) w = exRun w a b := exchangeW_run M.K w a b hr
            rw [e1]; exact ⟨hn, hfl, fun x h => h⟩
          · have e1 : eff M.K M.noGap M.nb (Ev.exchange a b) w = exSkip w a b := exchangeW_skip M.K w a b hr
            rw [e1]; exact ⟨hn, hfl, fun x h => h⟩
        | digest a b => exact ⟨hn, hfl, fun x h => h⟩
      · rw [he]; exact ⟨hn, hfl, fun x h => h⟩

    theorem init_flood (nb : Nat → Nat → Bool) (o : Nat) (w : W) (h : Init nb o w) :
        LostNone w ∧ FloodInv nb w :=
      ⟨h.2.2.2.1, fun m q hm hq => by
        rw [h.2.2.1 m q]
        rw [h.1 m] at hm
        exact or_l _ _ (by rw [hm, hq])⟩

    theorem run_calm (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev)
        (hK : Exact M.K) (calm : ∀ n, Calm (ev n) = true) : ∀ n, LostNone (b n) ∧ FloodInv M.nb (b n) := by
      intro n
      induction n with
      | zero => exact init_flood M.nb o (b 0) hr.1
      | succ n ih =>
        show LostNone (b (n + 1)) ∧ FloodInv M.nb (b (n + 1))
        rw [hr.2 n]
        have h := calm_step M hK (ev n) (b n) (calm n) ih.1 ih.2
        exact ⟨h.1, h.2.1⟩

    theorem run_log (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev)
        (hK : Exact M.K) (calm : ∀ n, Calm (ev n) = true) (x n : Nat) (h : (b n).log x = true) :
        ∀ k, (b (n + k)).log x = true := by
      intro k
      induction k with
      | zero => exact h
      | succ k ih =>
        show (b (n + k + 1)).log x = true
        rw [hr.2 (n + k)]
        have hi := run_calm M o b ev hr hK calm (n + k)
        exact (calm_step M hK (ev (n + k)) (b (n + k)) (calm (n + k)) hi.1 hi.2).2.2 x ih

    -- With nothing let go, the push edge and the flood alone bring the record to every seat's log.
    theorem flood_reaches (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev) (hr : RunsE M o b ev)
        (hK : Exact M.K) (calm : ∀ n, Calm (ev n) = true)
        (fl : ∀ s d, M.nb s d = true → Fades (fun i => (b i).owes s d && (b i).log s)) :
        ∀ d, Reach M.nb o d → ∃ k, (b k).log d = true := by
      intro d hreach
      induction hreach with
      | here => exact ⟨0, by rw [hr.1.1 o]; exact dec_self o⟩
      | hop s' d' _ hnb ih =>
        exact Exists.elim ih (fun k hk => by
          have hfk := (run_calm M o b ev hr hK calm k).2 s' d' hk hnb
          cases hod : (b k).owes s' d' with
          | false => rw [hod] at hfk; exact ⟨k, hfk⟩
          | true =>
            have hp : ((b k).owes s' d' && (b k).log s') = true := by rw [hod, hk]
            exact Exists.elim (fade_edge _ (fl s' d' hnb) k hp) (fun i hj => by
              have hp1 : ((b (k + i + 1)).owes s' d' && (b (k + i + 1)).log s') = false := hj.2
              have hl1 : (b (k + i + 1)).log s' = true := run_log M o b ev hr hK calm s' k hk (i + 1)
              rw [hl1] at hp1
              have hf1 := (run_calm M o b ev hr hK calm (k + i + 1)).2 s' d' hl1 hnb
              rw [and_true_false _ hp1] at hf1
              exact ⟨k + i + 1, hf1⟩))

    theorem flood_without_let_go (M : Model) (o : Nat) (b : Nat → W) (ev : Nat → Ev)
        (hr : RunsE M o b ev) (hK : Exact M.K) (calm : ∀ n, Calm (ev n) = true)
        (fl : ∀ s d, M.nb s d = true → Fades (fun i => (b i).owes s d && (b i).log s))
        (fi : ∀ d, Fades (fun i => (b i).log d && !(b i).store d)) :
        ∀ d, Reach M.nb o d → ∃ k, (b k).store d = true := by
      intro d hreach
      exact Exists.elim (flood_reaches M o b ev hr hK calm fl d hreach) (fun k hk => by
        cases hs : (b k).store d with
        | true => exact ⟨k, hs⟩
        | false =>
          have hp : ((b k).log d && !(b k).store d) = true := by rw [hk, hs]
          exact Exists.elim (fade_edge _ (fi d) k hp) (fun i hj => by
            have hp1 : ((b (k + i + 1)).log d && !(b (k + i + 1)).store d) = false := hj.2
            have hl1 : (b (k + i + 1)).log d = true := run_log M o b ev hr hK calm d k hk (i + 1)
            rw [hl1] at hp1
            exact ⟨k + i + 1, true_not_false _ hp1⟩))


    -- ===========================================================================
    -- 7. Runs that show each premise and each kernel decision is needed (by rfl).
    -- ===========================================================================

    def runE (M : Model) : List Ev → W → W
      | [], w => w
      | e :: es, w => runE M es (step M e w)

    -- A line of Macs 0 - 1 - 2; Mac 0, the record's origin, is the one durable seat.
    def line (x y : Nat) : Bool := (x + 1 == y) || (y + 1 == x)
    def only0 (x : Nat) : Bool := decide (x = 0)
    def w0 : W :=
      W.mk (fun x => decide (x = 0)) (fun x => decide (x = 0)) (fun x y => decide (x = 0) && line x y)
        (fun _ _ => false) (fun _ _ => false) (fun _ _ => false) (fun _ _ => false)
    theorem w0_init : Init line 0 w0 :=
      ⟨fun _ => rfl, fun _ => rfl, fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl,
        fun _ _ => rfl⟩

    def model (K : Kernels) (noGap : Bool) : Model := Model.mk K noGap only0 line

    -- Mutants of each kernel, each the decision the Rust negative controls take.
    def quietBlind : Kernels :=
      Kernels.mk spec_cursor spec_start_repair spec_requests (fun r _ => !r) spec_asks_again
    def gapDeaf : Kernels :=
      Kernels.mk spec_cursor spec_start_repair (fun _ _ => false) spec_runs spec_asks_again
    def keptOnMove : Kernels :=
      Kernels.mk (fun h f _ _ => if (h && f) = true then 1 else 0) spec_start_repair spec_requests spec_runs
        spec_asks_again
    def movedAsksNothing : Kernels :=
      Kernels.mk spec_cursor (fun c => if c = 0 then 2 else 0) spec_requests spec_runs spec_asks_again
    def noSecondAsk : Kernels :=
      Kernels.mk spec_cursor spec_start_repair spec_requests spec_runs (fun _ _ => false)

    -- The gap: Mac 0's retention cuts the record before its relay to Mac 1 lands it.
    def gapRun : List Ev := [Ev.cut 0, Ev.exchange 0 1]
    theorem gap_repaired : (runE (model spec false) gapRun w0).store 1 = true := rfl
    theorem gap_lost_without_the_notice : (runE (model spec true) gapRun w0).store 1 = false := rfl
    theorem gap_lost_when_deaf : (runE (model gapDeaf false) gapRun w0).store 1 = false := rfl
    theorem gap_lost_when_quiet_blind : (runE (model quietBlind false) gapRun w0).store 1 = false := rfl

    -- The lineage event: Mac 1's log answers the record, then loses it before its stores hold it;
    -- Mac 0's relay comes back to a fresh entry over a log that moved and continues.
    def lineageRun : List Ev := [Ev.land 0 1, Ev.lose 1, Ev.restart 0 1 true true true, Ev.exchange 0 1]
    theorem lineage_repaired : (runE (model spec false) lineageRun w0).store 1 = true := rfl
    theorem lineage_lost_when_kept : (runE (model keptOnMove false) lineageRun w0).store 1 = false := rfl
    theorem lineage_lost_when_moved_asks_nothing :
        (runE (model movedAsksNothing false) lineageRun w0).store 1 = false := rfl

    -- H1: Inventory brought the record to Mac 1, whose log never held it, so its relay owes Mac 2
    -- nothing and nothing asks; the digest trigger asks, and the exchange brings it.
    def h1End : W := runE (model spec false) gapRun w0
    theorem h1_hole : h1End.store 2 = false ∧ h1End.log 1 = false ∧ h1End.owes 1 2 = false ∧
        h1End.asked 1 2 = false ∧ h1End.lost 1 2 = false := ⟨rfl, rfl, rfl, rfl, rfl⟩
    theorem h1_digest_closes_it :
        (runE (model spec false) (gapRun ++ [Ev.digest 1 2, Ev.exchange 1 2]) w0).store 2 = true := rfl

    -- H2: Mac 1's relay to Mac 2 starts at its head (a new link) while Mac 1's log holds the record
    -- its stores have not ingested; the exchange it asks runs first and moves nothing. The start's
    -- second ask, answered once Mac 1 ingests, brings it; without it nothing asks again. Mac 1 is
    -- durable here (the run loses no log, so its states are `model`'s), as `the_edge_is_repaired`
    -- takes the source to be: under `noSecondAsk` nothing is owed, asked, lost or pending, so the
    -- run may stutter forever with `FairHop` holding and Mac 2 never gets the record.
    def modelH2 (K : Kernels) : Model := Model.mk K false (fun x => decide (x ≤ 1)) line
    def h2Run : List Ev :=
      [Ev.land 0 1, Ev.restart 1 2 false false false, Ev.exchange 1 2, Ev.ingest 1, Ev.exchange 1 2]
    theorem h2_second_ask_repairs : (runE (modelH2 spec) h2Run w0).store 2 = true := rfl
    def h2End : W := runE (modelH2 noSecondAsk) h2Run w0
    theorem h2_race_without_it : h2End.store 1 = true ∧ h2End.store 2 = false ∧ h2End.log 2 = false ∧
        h2End.owes 1 2 = false ∧ h2End.asked 1 2 = false ∧ h2End.lost 1 2 = false ∧
        h2End.again 1 2 = false :=
      ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩


    -- ===========================================================================
    -- 8. The answer sheet the Rust test recomputes (`relay_live::tests`).
    -- ===========================================================================

    -- Every code other than Kept and Moved, an out-of-range one included, starts at the head and
    -- asks a new-link exchange.
    theorem start_repair_refuses (c : UInt8) (h1 : c ≠ 1) (h2 : c ≠ 2) : spec_start_repair c = 2 := by
      unfold spec_start_repair; rw [if_neg h1, if_neg h2]

    def bit (k i : Nat) : Bool := (k / 2 ^ i) % 2 == 1
    def yes (v : Bool) : Nat := if v = true then 1 else 0

    -- The cursor over (has, fresh, same, continues) as bits 3..0 of 0..15, then the start over the
    -- codes 0..3, then requests over (gap, no_gap), runs over (remembers, quiet_same) and
    -- asks_again over (asked, ingested), bits 1..0.
    def sheet : List Nat :=
      (List.range 16).map (fun k => (spec_cursor (bit k 3) (bit k 2) (bit k 1) (bit k 0)).toNat) ++
      (List.range 4).map (fun k => (spec_start_repair (UInt8.ofNat k)).toNat) ++
      (List.range 4).map (fun k => yes (spec_requests (bit k 1) (bit k 0))) ++
      (List.range 4).map (fun k => yes (spec_runs (bit k 1) (bit k 0))) ++
      (List.range 4).map (fun k => yes (spec_asks_again (bit k 1) (bit k 0)))

    theorem sheet_is : sheet =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 1, 2, 0, 1, 2, 1, 1, 1, 0, 1, 1, 1, 0,
          0, 0, 1, 0] := by rfl

    end AtermMeshRelayLive
}

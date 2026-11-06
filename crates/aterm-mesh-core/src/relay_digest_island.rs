// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// H1's close, the digest trigger: the spec of its kernel, the fold over one neighbour's rows that
// asks it, and the fold's laws, proved for every run on every Trust build. The spec kernel is what
// relay_digest.rs's contract! kernel states it computes; the fold asks it as a parameter, and
// `relay_digest_binding_island.rs` proves the compiled Rust equal to it. `relay_digest::tests`
// recomputes section 4's answer sheets from the Rust and reads their words from this file.
clean {
    -- spec_digest_asks: a neighbour's presence row asks an Inventory exchange when its
    -- digest of a store differs from this Mac's own latest row's (both known), the two are
    -- the ones its last row was compared under (steady), this Mac's tables relay to it
    -- follows, the trigger has not asked for this pair since the last exchange that
    -- failed, and the trigger is not off (NO_DIGEST_TRIGGER). Outside the namespace so
    -- the contract! clause can name it.
    def spec_digest_asks (differs steady follows asked off : Bool) : Bool :=
      differs && steady && follows && !asked && !off

    namespace AtermMeshRelayDigest

    -- ===========================================================================
    -- 1. The trigger over one neighbour's rows, the kernel as a parameter.
    -- ===========================================================================

    -- What reaches the trigger of one neighbour and one store: a row of it (its digest of
    -- the store, this Mac's own latest row's, 0 where unknown, and whether this Mac's tables
    -- relay to it follows), or an exchange with it that failed.
    inductive Ev where
      | row (theirs mine : Nat) (follows : Bool)
      | failed

    -- Its memory: whether a row was read, the two digests the last one was compared
    -- under, and whether it asked an exchange since the last that failed, for which pair.
    structure Mem where
      seen : Bool
      lastT : Nat
      lastM : Nat
      asked : Bool
      askedT : Nat
      askedM : Nat

    def fresh : Mem := Mem.mk false 0 0 false 0 0

    def Exact (K : Bool → Bool → Bool → Bool → Bool → Bool) : Prop :=
      ∀ a b c d e, K a b c d e = spec_digest_asks a b c d e

    theorem spec_exact : Exact spec_digest_asks := fun _ _ _ _ _ => rfl

    -- Two digests differ: both known, and not the same.
    def differ (t m : Nat) : Bool := !(decide (t = 0)) && !(decide (m = 0)) && !(decide (t = m))
    def steadyOf (w : Mem) (t m : Nat) : Bool := w.seen && decide (w.lastT = t) && decide (w.lastM = m)
    def askedOf (w : Mem) (t m : Nat) : Bool := w.asked && decide (w.askedT = t) && decide (w.askedM = m)

    def asksOf (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) (w : Mem) (t m : Nat)
        (f : Bool) : Bool :=
      K (differ t m) (steadyOf w t m) f (askedOf w t m) off

    def asksAt (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) (w : Mem) : Ev → Bool
      | Ev.row t m f => asksOf K off w t m f
      | Ev.failed => false

    -- A row is remembered as the pair it was compared under, and as the pair asked
    -- when it asks; a failed exchange forgets what was asked.
    def stepM (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) (w : Mem) : Ev → Mem
      | Ev.row t m f =>
        if asksOf K off w t m f = true then Mem.mk true t m true t m
        else Mem.mk true t m w.asked w.askedT w.askedM
      | Ev.failed => Mem.mk w.seen w.lastT w.lastM false w.askedT w.askedM

    -- A run of the trigger: its memory before each event, from a fresh one.
    def RunT (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) (ev : Nat → Ev)
        (w : Nat → Mem) : Prop :=
      w 0 = fresh ∧ ∀ n, w (n + 1) = stepM K off (w n) (ev n)


    -- ===========================================================================
    -- 2. Its laws, for any exact kernel.
    -- ===========================================================================

    theorem spec_asks (a b c d e : Bool) (h : spec_digest_asks a b c d e = true) :
        a = true ∧ b = true ∧ c = true ∧ d = false ∧ e = false := by
      cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> first | exact ⟨rfl, rfl, rfl, rfl, rfl⟩ | exact absurd h (by decide)

    theorem dec_self (x : Nat) : decide (x = x) = true := by
      cases hd : decide (x = x) with
      | true => rfl
      | false => exact absurd rfl (of_decide_eq_false hd)

    theorem and_not_true (a b : Bool) : (a && b && !true) = false := by
      cases a <;> cases b <;> rfl

    theorem and_l3 (a b c : Bool) (h : (a && b && c) = true) : a = true := by
      cases a <;> cases b <;> cases c <;> first | rfl | exact absurd h (by decide)
    theorem and_m3 (a b c : Bool) (h : (a && b && c) = true) : b = true := by
      cases a <;> cases b <;> cases c <;> first | rfl | exact absurd h (by decide)
    theorem and_r3 (a b c : Bool) (h : (a && b && c) = true) : c = true := by
      cases a <;> cases b <;> cases c <;> first | rfl | exact absurd h (by decide)

    -- It asks only on a lasting difference, with the relay following: two known
    -- digests that differ, the pair its last row was compared under, not the pair it
    -- asked for since the last failure, and the trigger on.
    theorem asks_only_on_a_lasting_difference (K : Bool → Bool → Bool → Bool → Bool → Bool)
        (hK : Exact K) (off : Bool) (w : Mem) (t m : Nat) (f : Bool)
        (h : asksOf K off w t m f = true) :
        differ t m = true ∧ steadyOf w t m = true ∧ f = true ∧ askedOf w t m = false ∧ off = false := by
      have h' : K (differ t m) (steadyOf w t m) f (askedOf w t m) off = true := h
      rw [hK] at h'
      exact spec_asks _ _ _ _ _ h'

    -- No anti-entropy timer: between Macs in step it never asks.
    theorem never_in_step (K : Bool → Bool → Bool → Bool → Bool → Bool) (hK : Exact K) (off : Bool)
        (w : Mem) (t : Nat) (f : Bool) : asksOf K off w t t f = false := by
      cases h : asksOf K off w t t f with
      | false => rfl
      | true =>
        have hd : differ t t = true := (asks_only_on_a_lasting_difference K hK off w t t f h).1
        have h0 : differ t t = false := by
          unfold differ
          rw [dec_self t, and_not_true]
        exact absurd (hd.symm.trans h0) (by decide)

    -- Once per pair: the row after an ask, of the same two digests, does not ask again.
    theorem once_per_pair (K : Bool → Bool → Bool → Bool → Bool → Bool) (hK : Exact K) (off : Bool)
        (w : Mem) (t m : Nat) (f g : Bool) (h : asksOf K off w t m f = true) :
        asksOf K off (stepM K off w (Ev.row t m f)) t m g = false := by
      have hs : stepM K off w (Ev.row t m f) = Mem.mk true t m true t m := if_pos h
      rw [hs]
      have ha : askedOf (Mem.mk true t m true t m) t m = true := by
        show (true && decide (t = t) && decide (m = m)) = true
        rw [dec_self t, dec_self m]
      cases h2 : asksOf K off (Mem.mk true t m true t m) t m g with
      | false => rfl
      | true =>
        have hn : askedOf (Mem.mk true t m true t m) t m = false :=
          (asks_only_on_a_lasting_difference K hK off _ t m g h2).2.2.2.1
        exact absurd (ha.symm.trans hn) (by decide)

    -- A row leaves the trigger steady on its own two digests.
    theorem step_steady (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) (w : Mem)
        (t m : Nat) (f : Bool) : steadyOf (stepM K off w (Ev.row t m f)) t m = true := by
      by_cases h : asksOf K off w t m f = true
      · have hs : stepM K off w (Ev.row t m f) = Mem.mk true t m true t m := if_pos h
        rw [hs]
        show (true && decide (t = t) && decide (m = m)) = true
        rw [dec_self t, dec_self m]
      · have hs : stepM K off w (Ev.row t m f) = Mem.mk true t m w.asked w.askedT w.askedM := if_neg h
        rw [hs]
        show (true && decide (t = t) && decide (m = m)) = true
        rw [dec_self t, dec_self m]

    -- A steady row of a difference not yet asked, the relay following and the trigger
    -- on, asks.
    theorem asks_when_steady (K : Bool → Bool → Bool → Bool → Bool → Bool) (hK : Exact K) (w : Mem)
        (t m : Nat) (hd : differ t m = true) (hs : steadyOf w t m = true) (ha : askedOf w t m = false) :
        asksOf K false w t m true = true := by
      show K (differ t m) (steadyOf w t m) true (askedOf w t m) false = true
      rw [hK, hd, hs, ha]
      rfl

    -- A steady row of a difference, the relay following and the trigger on, leaves the
    -- pair asked: it asks now, or it asked already.
    theorem asked_after (K : Bool → Bool → Bool → Bool → Bool → Bool) (hK : Exact K) (w : Mem)
        (t m : Nat) (hd : differ t m = true) (hs : steadyOf w t m = true) :
        askedOf (stepM K false w (Ev.row t m true)) t m = true := by
      by_cases h : asksOf K false w t m true = true
      · have he : stepM K false w (Ev.row t m true) = Mem.mk true t m true t m := if_pos h
        rw [he]
        show (true && decide (t = t) && decide (m = m)) = true
        rw [dec_self t, dec_self m]
      · have he : stepM K false w (Ev.row t m true) = Mem.mk true t m w.asked w.askedT w.askedM :=
          if_neg h
        rw [he]
        show askedOf w t m = true
        cases ha : askedOf w t m with
        | true => rfl
        | false => exact absurd (asks_when_steady K hK w t m hd hs ha) h

    -- What the memory names as asked was asked by a row of that pair, the relay following.
    theorem asked_by_a_row (K : Bool → Bool → Bool → Bool → Bool → Bool) (hK : Exact K) (off : Bool)
        (ev : Nat → Ev) (w : Nat → Mem) (hr : RunT K off ev w) :
        ∀ k, (w k).asked = true → ∃ j, j < k ∧ ev j = Ev.row (w k).askedT (w k).askedM true ∧
          asksAt K off (w j) (ev j) = true := by
      intro k
      induction k with
      | zero =>
        intro h
        have h0 : (w 0).asked = false := by rw [hr.1]; rfl
        exact absurd (h.symm.trans h0) (by decide)
      | succ k ih =>
        intro h
        show ∃ j, j < k + 1 ∧ ev j = Ev.row (w (k + 1)).askedT (w (k + 1)).askedM true ∧
          asksAt K off (w j) (ev j) = true
        have hw : w (k + 1) = stepM K off (w k) (ev k) := hr.2 k
        cases he : ev k with
        | failed =>
          have h0 : (w (k + 1)).asked = false := by rw [hw, he]; rfl
          exact absurd (h.symm.trans h0) (by decide)
        | row t m f =>
          by_cases ha : asksOf K off (w k) t m f = true
          · have hs : w (k + 1) = Mem.mk true t m true t m := by rw [hw, he]; exact if_pos ha
            have hf : f = true := (asks_only_on_a_lasting_difference K hK off (w k) t m f ha).2.2.1
            refine ⟨k, Nat.lt_succ_self k, ?_, ?_⟩
            · rw [hs, he, hf]
            · rw [he]; exact ha
          · have hs : w (k + 1) = Mem.mk true t m (w k).asked (w k).askedT (w k).askedM := by
              rw [hw, he]; exact if_neg ha
            have hk : (w k).asked = true := by
              have h1 : (Mem.mk true t m (w k).asked (w k).askedT (w k).askedM).asked = true := by
                rw [← hs]; exact h
              exact h1
            exact Exists.elim (ih hk) (fun j hj => ⟨j, Nat.lt_of_lt_of_le hj.1 (Nat.le_of_lt (Nat.lt_succ_self k)),
              by rw [hs]; exact hj.2.1, hj.2.2⟩)

    -- Every difference that lasts two rows is asked: from row n on, the neighbour's rows
    -- show one pair of known digests that differ, this Mac's relay to it following and
    -- nothing failing; by row n + 2 a row of that pair asked an exchange. With faithful
    -- digests (one for each store an exchange moves, the membership log's records and the
    -- lab tables' operations and conflict records, so a record one Mac holds and its
    -- neighbour lacks makes one pair differ) and stores that rest for two rows, this is
    -- relay_live_island.rs's premise FairDigest: while a neighbour lacks what a Mac's
    -- stores hold, an exchange is asked for those very stores, and the exchange brings
    -- them (quiet_safe, exRun).
    theorem a_lasting_difference_is_asked (K : Bool → Bool → Bool → Bool → Bool → Bool)
        (hK : Exact K) (ev : Nat → Ev) (w : Nat → Mem) (hr : RunT K false ev w) (n t m : Nat)
        (hd : differ t m = true) (hl : ∀ i, ev (n + i) = Ev.row t m true) :
        ∃ j, j < n + 2 ∧ ev j = Ev.row t m true ∧ asksAt K false (w j) (ev j) = true := by
      have e0 : ev n = Ev.row t m true := hl 0
      have e1 : ev (n + 1) = Ev.row t m true := hl 1
      have w1 : w (n + 1) = stepM K false (w n) (Ev.row t m true) := by rw [hr.2 n, e0]
      have w2 : w (n + 2) = stepM K false (w (n + 1)) (Ev.row t m true) := by
        show w (n + 1 + 1) = stepM K false (w (n + 1)) (Ev.row t m true)
        rw [hr.2 (n + 1), e1]
      have hs : steadyOf (w (n + 1)) t m = true := by rw [w1]; exact step_steady K false (w n) t m true
      have ha : askedOf (w (n + 2)) t m = true := by rw [w2]; exact asked_after K hK (w (n + 1)) t m hd hs
      have ha1 : (w (n + 2)).asked = true := and_l3 _ _ _ ha
      have hT : (w (n + 2)).askedT = t := of_decide_eq_true (and_m3 _ _ _ ha)
      have hM : (w (n + 2)).askedM = m := of_decide_eq_true (and_r3 _ _ _ ha)
      exact Exists.elim (asked_by_a_row K hK false ev w hr (n + 2) ha1) (fun j hj =>
        ⟨j, hj.1, by rw [hj.2.1, hT, hM], hj.2.2⟩)


    -- ===========================================================================
    -- 3. Runs that show each input of the kernel is needed (by rfl).
    -- ===========================================================================

    def runM (K : Bool → Bool → Bool → Bool → Bool → Bool) (off : Bool) : List Ev → Mem → List Bool
      | [], _ => []
      | e :: es, w => asksAt K off w e :: runM K off es (stepM K off w e)

    -- Rows: in step; apart (a pair that differs); the same with no relay following; the
    -- neighbour's digest unknown; another pair that differs.
    def inStep : Ev := Ev.row 1 1 true
    def apart : Ev := Ev.row 2 1 true
    def unfollowed : Ev := Ev.row 2 1 false
    def unknown : Ev := Ev.row 0 1 true
    def other : Ev := Ev.row 1 2 true

    -- The trigger: a lasting difference asks once, a new pair or a failure asks again,
    -- and in step, unknown, unfollowed or off it never asks.
    theorem lasting_asks_once : runM spec_digest_asks false [apart, apart, apart] fresh = [false, true, false] := rfl
    theorem failure_asks_again :
        runM spec_digest_asks false [apart, apart, Ev.failed, apart] fresh = [false, true, false, true] := rfl
    theorem new_pair_asks :
        runM spec_digest_asks false [apart, apart, other, other] fresh = [false, true, false, true] := rfl
    theorem quiet_otherwise :
        runM spec_digest_asks false [inStep, inStep, unknown, unknown, unfollowed, unfollowed] fresh =
          [false, false, false, false, false, false] := rfl
    theorem off_never : runM spec_digest_asks true [apart, apart] fresh = [false, false] := rfl

    -- Mutants of the kernel, each the decision one of relay_digest::tests' mutants takes,
    -- each refuted by a run above: a timer between Macs in step; an ask while a digest
    -- just moved (a relay still carrying); an ask with no relay following; an ask on every
    -- row of a lasting difference (a timer); H1's hole left open; the control ignored.
    def inStepAsks (_ s f a o : Bool) : Bool := s && f && !a && !o
    def restless (d _ f a o : Bool) : Bool := d && f && !a && !o
    def followsBlind (d s _ a o : Bool) : Bool := d && s && !a && !o
    def repeats (d s f _ o : Bool) : Bool := d && s && f && !o
    def deaf (_ _ _ _ _ : Bool) : Bool := false
    def controlBlind (d s f a _ : Bool) : Bool := d && s && f && !a

    theorem a_timer_when_in_step_asks : runM inStepAsks false [inStep, inStep] fresh = [false, true] := rfl
    theorem in_flight_when_restless : runM restless false [inStep, apart] fresh = [false, true] := rfl
    theorem unfollowed_when_blind : runM followsBlind false [unfollowed, unfollowed] fresh = [false, true] := rfl
    theorem a_timer_when_repeating : runM repeats false [apart, apart, apart] fresh = [false, true, true] := rfl
    theorem h1_open_when_deaf : runM deaf false [apart, apart, apart] fresh = [false, false, false] := rfl
    theorem the_control_ignored : runM controlBlind true [apart, apart] fresh = [false, true] := rfl


    -- ===========================================================================
    -- 4. The answer sheets the Rust test recomputes (`relay_digest::tests`).
    -- ===========================================================================

    def bit (b : Bool) (v : Nat) : Nat := if b = true then v else 0
    def flag (k i : Nat) : Bool := decide ((k / 2 ^ i) % 2 = 1)

    -- The kernel over (differs, steady, follows, asked, off) as bits 4..0 of 0..31.
    def kernelSheet : List Nat :=
      (List.range 32).map (fun k => bit (spec_digest_asks (flag k 4) (flag k 3) (flag k 2) (flag k 1) (flag k 0)) 1)

    theorem kernel_sheet_is : kernelSheet =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
         0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0] := rfl

    -- The fold over every trace of four events that starts apart, then three of five
    -- letters (inStep, apart, unknown, other, failed; the relay's follows the fold hands
    -- the kernel as it reads it, and the kernel sheet takes it): trace k's event i + 1 is
    -- letter (k / 5^i) % 5, its asks bits 0..3 of a nibble, sixteen traces to a word
    -- (Clean's numerals fit 64 bits), trace 16w's nibble first, the last word's tail 0.
    def letter (c : Nat) : Ev :=
      if c = 0 then inStep else if c = 1 then apart else if c = 2 then unknown
      else if c = 3 then other else Ev.failed

    def nib : List Bool → Nat
      | [] => 0
      | b :: bs => bit b 1 + 2 * nib bs

    def traceOf (k : Nat) : List Ev := [apart, letter (k % 5), letter ((k / 5) % 5), letter ((k / 25) % 5)]

    def gridNib (k : Nat) : Nat := if k < 125 then nib (runM spec_digest_asks false (traceOf k) fresh) else 0

    def wordOf (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 16 * wordOf w n + gridNib (16 * w + n)

    def sheetFrom (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => wordOf w 16 :: sheetFrom (w + 1) n

    -- Traces 0 to 124, as 8 words.
    def foldSheet : List Nat := sheetFrom 0 8

    theorem fold_sheet_is : foldSheet =
      [0x0200002004020000, 0x2040020000200082, 0x88402000020400a0, 0x0802000020040200,
       0x0020400200002000, 0x02004020008a8480, 0x2080020000200402, 0x0000204002000000] := rfl

    end AtermMeshRelayDigest
}

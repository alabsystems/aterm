// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The publisher's compare-before-write, proved over the kernel `file_merge::settle`
// (docs/DESIGN-shared-files-2026-11-11.md section 8). A digest is a Nat here: the kernel asks
// only whether two digests are equal.
clean {
    namespace AtermDbCore
    namespace FileMerge
    namespace Settle

    -- Whether d is one of the list (the kernel's `contains`).
    def has (d : Nat) : List Nat -> Bool
      | [] => false
      | x :: rest => decide (x = d) || has d rest

    -- Whether every digest of the list is one of set.
    def covers (set : List Nat) : List Nat -> Bool
      | [] => true
      | x :: rest => has x set && covers set rest

    -- The kernel: the publisher writes when each side covers the other.
    def settle (now expected : List Nat) : Bool := covers now expected && covers expected now

    -- The sheet: the register's candidates, then the intent's expected ones, each ranging over
    -- every list of at most two of the digests 0, 1 and 2.
    def bit (b : Bool) : Char := Char.ofNat (if b then 49 else 48)
    def lists : List (List Nat) :=
      [[], [0], [1], [2], [0, 0], [0, 1], [0, 2], [1, 0], [1, 1], [1, 2], [2, 0], [2, 1], [2, 2]]
    def sheetSettle : String := String.mk (lists.foldr (fun now acc =>
      lists.foldr (fun expected inner => bit (settle now expected) :: inner) acc) [])
    theorem sheet_settle_answers : sheetSettle = "1000000000000010010000000000100000100000001000000001010010000000000000101000000000001000100000001010000000100000100000000000001010000000100010000000000010100001000000001" := by decide

    -- The Bool and decide facts the proofs cite.
    theorem bor_left (x y : Bool) (h : x = true) : (x || y) = true := by
      subst h
      rfl
    theorem bor_right (x y : Bool) (h : y = true) : (x || y) = true := by
      subst h
      cases x <;> rfl
    theorem bor_cases (x y : Bool) (h : (x || y) = true) : Or (x = true) (y = true) := by
      cases x with
      | true => exact Or.inl rfl
      | false => exact Or.inr h
    theorem band_intro (x y : Bool) (hx : x = true) (hy : y = true) : (x && y) = true := by
      subst hx
      subst hy
      rfl
    theorem band_left (x y : Bool) (h : (x && y) = true) : x = true := by
      cases x with
      | true => rfl
      | false => exact h
    theorem band_right (x y : Bool) (h : (x && y) = true) : y = true := by
      cases x with
      | true => exact h
      | false => exact Bool.noConfusion h
    theorem bool_ext (x y : Bool) (hxy : x = true -> y = true) (hyx : y = true -> x = true) : x = y := by
      cases x with
      | true => exact Eq.symm (hxy rfl)
      | false =>
        cases y with
        | false => rfl
        | true => exact hyx rfl
    theorem decide_true_of {p : Prop} [d : Decidable p] (h : p) : decide p = true := by
      cases hd : decide p with
      | true => rfl
      | false => exact absurd h (of_decide_eq_false hd)

    -- 1. MEMBERSHIP. A list has its head, has whatever its rest has, and has nothing else.
    theorem has_head (d : Nat) (rest : List Nat) : has d (d :: rest) = true := by
      show (decide (d = d) || has d rest) = true
      exact bor_left _ _ (decide_true_of rfl)
    theorem has_rest (d x : Nat) (rest : List Nat) (h : has d rest = true) : has d (x :: rest) = true := by
      show (decide (x = d) || has d rest) = true
      exact bor_right _ _ h
    theorem has_split (d x : Nat) (rest : List Nat) (h : has d (x :: rest) = true) :
        Or (x = d) (has d rest = true) := by
      have h1 : (decide (x = d) || has d rest) = true := h
      exact Or.elim (bor_cases _ _ h1) (fun e => Or.inl (of_decide_eq_true e)) (fun e => Or.inr e)

    -- 2. COVERING. A covered list has no member the set lacks, at any length ...
    theorem covers_sound (set : List Nat) :
        forall (l : List Nat), covers set l = true -> forall (d : Nat), has d l = true -> has d set = true := by
      intro l
      induction l with
      | nil => intro _ d hd; exact Bool.noConfusion hd
      | cons x rest ih =>
        intro h d hd
        have h1 : (has x set && covers set rest) = true := h
        exact Or.elim (has_split d x rest hd)
          (fun e => Eq.mp (congrArg (fun y => has y set = true) e) (band_left _ _ h1))
          (fun e => ih (band_right _ _ h1) d e)

    -- ... and a list with no member the set lacks is covered.
    theorem covers_complete (set : List Nat) :
        forall (l : List Nat), (forall (d : Nat), has d l = true -> has d set = true) -> covers set l = true := by
      intro l
      induction l with
      | nil => intro _; rfl
      | cons x rest ih =>
        intro h
        show (has x set && covers set rest) = true
        exact band_intro _ _ (h x (has_head x rest)) (ih (fun d hd => h d (has_rest d x rest hd)))

    -- 3. THE LAW (publish_observed_only). The publisher writes only a register whose candidates
    -- are the expected ones: the two lists have the same members ...
    theorem settle_writes_only_the_observed_set (now expected : List Nat) (h : settle now expected = true) :
        forall (d : Nat), has d now = has d expected := by
      have h1 : (covers now expected && covers expected now) = true := h
      intro d
      exact bool_ext _ _
        (covers_sound expected now (band_right _ _ h1) d)
        (covers_sound now expected (band_left _ _ h1) d)

    -- ... and it writes every such register, in whatever order and however often each side
    -- lists a digest.
    theorem settle_writes_every_observed_set (now expected : List Nat)
        (h : forall (d : Nat), has d now = has d expected) : settle now expected = true := by
      show (covers now expected && covers expected now) = true
      exact band_intro _ _
        (covers_complete now expected (fun d hd => Eq.trans (h d) hd))
        (covers_complete expected now (fun d hd => Eq.trans (Eq.symm (h d)) hd))

    -- Settle writes if and only if the two digest sets are equal.
    theorem settle_iff_the_sets_are_equal (now expected : List Nat) :
        Iff (settle now expected = true) (forall (d : Nat), has d now = has d expected) :=
      Iff.intro (settle_writes_only_the_observed_set now expected)
        (settle_writes_every_observed_set now expected)

    -- 4. A MOVED REGISTER. A candidate the intent never observed holds the write ...
    theorem ne_true_false : Not (true = false) := by decide
    theorem settle_holds_for_an_unobserved_candidate (now expected : List Nat) (d : Nat)
        (hin : has d now = true) (hout : has d expected = false) : settle now expected = false := by
      cases hs : settle now expected with
      | false => rfl
      | true =>
        have e : has d now = has d expected := settle_writes_only_the_observed_set now expected hs d
        rw [hin, hout] at e
        exact absurd e ne_true_false

    -- ... and so does an observed candidate the register no longer holds.
    theorem settle_holds_for_a_replaced_candidate (now expected : List Nat) (d : Nat)
        (hin : has d expected = true) (hout : has d now = false) : settle now expected = false := by
      cases hs : settle now expected with
      | false => rfl
      | true =>
        have e : has d now = has d expected := settle_writes_only_the_observed_set now expected hs d
        rw [hin, hout] at e
        exact absurd (Eq.symm e) ne_true_false

    -- Order and repeats count for nothing.
    theorem settle_ignores_order_and_repeats : settle [1, 2] [2, 1, 2] = true := rfl

    -- 5. THE NEGATIVE CONTROL. A publisher that asks only whether what it observed is still
    -- there, as a copy that dropped the other half would.
    def settleStillThere (now expected : List Nat) : Bool := covers now expected

    -- It writes over a register that gained a candidate nobody observed ...
    theorem still_there_writes_over_a_moved_register : settleStillThere [1, 2] [1] = true := rfl

    -- ... which the kernel holds ...
    theorem settle_holds_that_register : settle [1, 2] [1] = false := rfl

    -- ... so it breaks the law.
    theorem still_there_breaks_the_law :
        Not (forall (now expected : List Nat), settleStillThere now expected = true ->
          forall (d : Nat), has d now = has d expected) :=
      fun h => ne_true_false (h [1, 2] [1] rfl 2)

    end Settle
    end FileMerge
    end AtermDbCore
}

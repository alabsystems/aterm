// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Law 5, value convergence: the register of convergence.rs and its law, proved for every number
// of ops by the kernel on every Trust build. The spec kernels are what convergence.rs's
// contract! kernels state they compute; the folds are its Register's, one for one; every fold
// asks its kernels as parameters (K), and `convergence_binding_island.rs` instantiates them with
// the compiled Rust. `convergence::tests` recomputes section 9's answer sheet from the Rust and
// reads its words from this file.
clean {
    -- The register's scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/convergence.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body equal to the definition here. Outside the namespace so a
    -- clause can name them.

    -- spec_carries: an op is a carry when what its writer held has a winner and
    -- the op writes it again: a Put of the winner's value, or a Delete where every
    -- op the writer held is a Delete (a reseal re-issuing one, F21 rule 6).
    def spec_carries (isDelete hasWinner winnerSame allDeletes : Bool) : Bool :=
      hasWinner && ((isDelete && allDeletes) || (!isDelete && winnerSame))
    -- spec_replaces: a later op replaces an op in its causal past, unless it is a
    -- carry of another value (a carry replaces only ops writing what it carries).
    def spec_replaces (below laterCarries sameOp : Bool) : Bool := below && (!laterCarries || sameOp)
    -- spec_drops: an op of a set takes a carry of it out when it writes something
    -- else and the carry's rep lies in its causal past (a reseal loses to an edit
    -- or Delete made beside it).
    def spec_drops (carries sameOp repBelow : Bool) : Bool := carries && (!sameOp && repBelow)
    -- spec_beats: a sibling beats another in the order every Mac reads the winner
    -- by: the lower record id of what it stands for, then the lower id of its own.
    def spec_beats (repLess repSame ownLess : Bool) : Bool := repLess || (repSame && ownLess)
    -- spec_conflicts: a sibling that writes otherwise than the winner makes the
    -- register a conflict.
    def spec_conflicts (sameOp : Bool) : Bool := !sameOp

    namespace AtermMeshConvergence

    open AtermMeshRevocation

    -- ===========================================================================
    -- 1. The register. A Mac's view of one entry's ops (or of its whole log's,
    --    each op's entry its key), enumerated in the order its log holds them:
    --    the records' arrival order, which differs from Mac to Mac. Every op is
    --    named by its record id, the same on every Mac; the causal order is a
    --    relation between records, and every Mac reads the same one.
    -- ===========================================================================

    structure Reg where
      n   : Nat
      mem : Nat → Bool          -- the op at position i is one of this register's
      rid : Nat → Nat           -- its record id
      cls : Nat → Nat           -- what it writes: two ops of one class write the same thing
      del : Nat → Bool          -- it is a Delete
      ord : Nat → Nat → Bool    -- record a lies in record b's strict causal past

    -- What the classification has decided so far, by record id: whether the op is
    -- a carry, and the record id of the op it stands for (its rep).
    structure St where
      c : Nat → Bool
      p : Nat → Nat

    structure Kernels where
      carries : Bool → Bool → Bool → Bool → Bool
      replaces : Bool → Bool → Bool → Bool
      drops : Bool → Bool → Bool → Bool
      beats : Bool → Bool → Bool → Bool
      conflicts : Bool → Bool

    def Exact (K : Kernels) : Prop :=
      (∀ a b c d, K.carries a b c d = spec_carries a b c d)
        ∧ (∀ a b c, K.replaces a b c = spec_replaces a b c)
        ∧ (∀ a b c, K.drops a b c = spec_drops a b c)
        ∧ (∀ a b c, K.beats a b c = spec_beats a b c)
        ∧ (∀ a, K.conflicts a = spec_conflicts a)

    -- The law needs only the winner order to be the spec's: every other kernel may
    -- answer anything.
    def BeatsExact (K : Kernels) : Prop := ∀ a b c, K.beats a b c = spec_beats a b c

    theorem beats_exact_of_exact (K : Kernels) (h : Exact K) : BeatsExact K := h.2.2.2.1

    def specK : Kernels := Kernels.mk spec_carries spec_replaces spec_drops spec_beats spec_conflicts

    theorem spec_exact : Exact specK :=
      ⟨fun _ _ _ _ => rfl, fun _ _ _ => rfl, fun _ _ _ => rfl, fun _ _ _ => rfl, fun _ => rfl⟩

    -- Whether some op of the register lies strictly between records a and b.
    def between (t : Reg) (a b : Nat) : Bool :=
      anyN t.n (fun m => t.mem m && (t.ord a (t.rid m) && t.ord (t.rid m) b))
    -- Record x's frontier in the register: its ops newest in x's past.
    def front (t : Reg) (x j : Nat) : Bool :=
      t.mem j && (t.ord (t.rid j) x && !(between t (t.rid j) x))
    -- Whether an op of the set S takes op j out (j a carry whose rep lies below an
    -- op of S writing something else).
    def dropped (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) (j : Nat) : Bool :=
      anyN t.n (fun m => S m && K.drops (s.c (t.rid j)) (decide (t.cls m = t.cls j)) (t.ord (s.p (t.rid j)) (t.rid m)))
    -- S less each carry an op of S took out.
    def live (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) (j : Nat) : Bool :=
      S j && !(dropped K t s S j)
    -- What the writer of record x held: the live ops of its frontier.
    def held (K : Kernels) (t : Reg) (s : St) (x : Nat) : Nat → Bool := live K t s (front t x)

    -- Op i beats op w: the lower (rep's record id, own record id).
    def beatsAt (K : Kernels) (t : Reg) (s : St) (i w : Nat) : Bool :=
      K.beats (decide (s.p (t.rid i) < s.p (t.rid w))) (decide (s.p (t.rid i) = s.p (t.rid w)))
        (decide (t.rid i < t.rid w))
    -- The winner of S among the first i positions: a left fold that takes an op
    -- only when it beats the one taken so far (the Rust folds' reduce on beats).
    def winStep (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) (i : Nat) (o : Option Nat) : Option Nat :=
      match o with
      | none => if S i = true then some i else none
      | some w => if (S i && beatsAt K t s i w) = true then some i else some w
    def win (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) : Nat → Option Nat
      | 0 => none
      | Nat.succ i => winStep K t s S i (win K t s S i)

    def found (o : Option Nat) : Bool :=
      match o with
      | none => false
      | some _ => true
    def sameAs (t : Reg) (o : Option Nat) (k : Nat) : Bool :=
      match o with
      | none => false
      | some w => decide (t.cls w = t.cls k)
    def repOf (t : Reg) (s : St) (o : Option Nat) (k : Nat) : Nat :=
      match o with
      | none => t.rid k
      | some w => s.p (t.rid w)
    def allDel (t : Reg) (S : Nat → Bool) : Bool := allN t.n (fun j => !(S j) || t.del j)

    -- Op k's classification, given what was decided for the ops before it.
    def winAt (K : Kernels) (t : Reg) (s : St) (k : Nat) : Option Nat := win K t s (held K t s (t.rid k)) t.n
    def carryAt (K : Kernels) (t : Reg) (s : St) (k : Nat) : Bool :=
      K.carries (t.del k) (found (winAt K t s k)) (sameAs t (winAt K t s k) k) (allDel t (held K t s (t.rid k)))
    def repAt (K : Kernels) (t : Reg) (s : St) (k : Nat) : Nat :=
      if carryAt K t s k = true then repOf t s (winAt K t s k) k else t.rid k

    -- The classification pass, in the order the Mac's log holds the records.
    def upd (K : Kernels) (t : Reg) (s : St) (k : Nat) : St :=
      St.mk (fun x => if x = t.rid k then carryAt K t s k else s.c x)
        (fun x => if x = t.rid k then repAt K t s k else s.p x)
    def pass (K : Kernels) (t : Reg) : Nat → St
      | 0 => St.mk (fun _ => false) (fun x => x)
      | Nat.succ k => if t.mem k = true then upd K t (pass K t k) k else pass K t k
    def final (K : Kernels) (t : Reg) : St := pass K t t.n

    -- The siblings: the ops no other op replaces, less the carries an op of
    -- another class took out. The winner, and whether the register is a conflict.
    def replaced (K : Kernels) (t : Reg) (s : St) (a : Nat) : Bool :=
      anyN t.n (fun b => t.mem b && K.replaces (t.ord (t.rid a) (t.rid b)) (s.c (t.rid b)) (decide (t.cls a = t.cls b)))
    def maximal (K : Kernels) (t : Reg) (s : St) (a : Nat) : Bool := t.mem a && !(replaced K t s a)
    def sib (K : Kernels) (t : Reg) (s : St) : Nat → Bool := live K t s (maximal K t s)
    def winnerOf (K : Kernels) (t : Reg) (s : St) : Option Nat := win K t s (sib K t s) t.n
    def conflictOf (K : Kernels) (t : Reg) (s : St) : Bool :=
      match winnerOf K t s with
      | none => false
      | some w => anyN t.n (fun i => sib K t s i && K.conflicts (decide (t.cls i = t.cls w)))

    -- The register's answer: the winner's record id (the value every Mac reads is
    -- that record's), whether it is a conflict, whether record x is a sibling, is
    -- a carry, and stands for record r (its rep: what a winner's entry records).
    def ridOf (t : Reg) (o : Option Nat) : Option Nat :=
      match o with
      | none => none
      | some w => some (t.rid w)
    def winnerRid (K : Kernels) (t : Reg) : Option Nat := ridOf t (winnerOf K t (final K t))
    def conflict (K : Kernels) (t : Reg) : Bool := conflictOf K t (final K t)
    def sibRid (K : Kernels) (t : Reg) (x : Nat) : Bool :=
      anyN t.n (fun i => sib K t (final K t) i && decide (t.rid i = x))
    def carryRid (K : Kernels) (t : Reg) (x : Nat) : Bool :=
      anyN t.n (fun i => t.mem i && (decide (t.rid i = x) && (final K t).c (t.rid i)))
    def repRid (K : Kernels) (t : Reg) (x r : Nat) : Bool :=
      anyN t.n (fun i => t.mem i && (decide (t.rid i = x) && decide ((final K t).p (t.rid i) = r)))

    -- ===========================================================================
    -- 2. The winner order is a strict total order on (rep's record id, own record
    --    id), so the fold's winner is the one op no other beats.
    -- ===========================================================================

    def lexT (a1 a2 b1 b2 : Nat) : Bool := spec_beats (decide (a1 < b1)) (decide (a1 = b1)) (decide (a2 < b2))

    theorem lex_false_eq (a1 a2 b1 b2 : Nat) (h1 : Not (a1 < b1)) (h2 : a1 = b1) (h3 : Not (a2 < b2)) :
        lexT a1 a2 b1 b2 = false := by
      show spec_beats (decide (a1 < b1)) (decide (a1 = b1)) (decide (a2 < b2)) = false
      rw [dfalse h1, dtrue h2, dfalse h3]
      rfl
    theorem lex_false_ne (a1 a2 b1 b2 : Nat) (h1 : Not (a1 < b1)) (h2 : Not (a1 = b1)) :
        lexT a1 a2 b1 b2 = false := by
      show spec_beats (decide (a1 < b1)) (decide (a1 = b1)) (decide (a2 < b2)) = false
      rw [dfalse h1, dfalse h2]
      rfl

    theorem lex_elim (a1 a2 b1 b2 : Nat) (h : lexT a1 a2 b1 b2 = true) : a1 < b1 ∨ (a1 = b1 ∧ a2 < b2) := by
      by_cases h1 : a1 < b1
      · exact Or.inl h1
      · by_cases h2 : a1 = b1
        · by_cases h3 : a2 < b2
          · exact Or.inr ⟨h2, h3⟩
          · exact False.elim (Bool.noConfusion (Eq.trans h.symm (lex_false_eq a1 a2 b1 b2 h1 h2 h3)))
        · exact False.elim (Bool.noConfusion (Eq.trans h.symm (lex_false_ne a1 a2 b1 b2 h1 h2)))

    theorem lex_intro (a1 a2 b1 b2 : Nat) (h : a1 < b1 ∨ (a1 = b1 ∧ a2 < b2)) : lexT a1 a2 b1 b2 = true := by
      cases h with
      | inl h1 =>
        show spec_beats (decide (a1 < b1)) (decide (a1 = b1)) (decide (a2 < b2)) = true
        rw [dtrue h1]
        rfl
      | inr h2 =>
        show spec_beats (decide (a1 < b1)) (decide (a1 = b1)) (decide (a2 < b2)) = true
        rw [dtrue h2.1, dtrue h2.2]
        exact Bool.or_true _

    theorem lex_irrefl (a1 a2 : Nat) : lexT a1 a2 a1 a2 = false := by
      cases h : lexT a1 a2 a1 a2 with
      | false => rfl
      | true =>
        cases lex_elim a1 a2 a1 a2 h with
        | inl h1 => exact absurd h1 (Nat.lt_irrefl a1)
        | inr h2 => exact absurd h2.2 (Nat.lt_irrefl a2)

    theorem lex_trans (a1 a2 b1 b2 c1 c2 : Nat) (h1 : lexT a1 a2 b1 b2 = true) (h2 : lexT b1 b2 c1 c2 = true) :
        lexT a1 a2 c1 c2 = true := by
      apply lex_intro
      cases lex_elim a1 a2 b1 b2 h1 with
      | inl x =>
        cases lex_elim b1 b2 c1 c2 h2 with
        | inl y => exact Or.inl (Nat.lt_trans x y)
        | inr y => exact Or.inl (by rw [← y.1]; exact x)
      | inr x =>
        cases lex_elim b1 b2 c1 c2 h2 with
        | inl y => exact Or.inl (by rw [x.1]; exact y)
        | inr y => exact Or.inr ⟨Eq.trans x.1 y.1, Nat.lt_trans x.2 y.2⟩

    theorem lex_total (a1 a2 b1 b2 : Nat) (h1 : lexT a1 a2 b1 b2 = false) (h2 : lexT b1 b2 a1 a2 = false) :
        a1 = b1 ∧ a2 = b2 := by
      have e1 : a1 = b1 := by
        by_cases x : a1 < b1
        · have hc := lex_intro a1 a2 b1 b2 (Or.inl x)
          rw [h1] at hc
          exact Bool.noConfusion hc
        · by_cases y : b1 < a1
          · have hc := lex_intro b1 b2 a1 a2 (Or.inl y)
            rw [h2] at hc
            exact Bool.noConfusion hc
          · omega
      have e2 : a2 = b2 := by
        by_cases x : a2 < b2
        · have hc := lex_intro a1 a2 b1 b2 (Or.inr ⟨e1, x⟩)
          rw [h1] at hc
          exact Bool.noConfusion hc
        · by_cases y : b2 < a2
          · have hc := lex_intro b1 b2 a1 a2 (Or.inr ⟨e1.symm, y⟩)
            rw [h2] at hc
            exact Bool.noConfusion hc
          · omega
      exact ⟨e1, e2⟩

    theorem beatsAt_lex (K : Kernels) (hK : BeatsExact K) (t : Reg) (s : St) (i w : Nat) :
        beatsAt K t s i w = lexT (s.p (t.rid i)) (t.rid i) (s.p (t.rid w)) (t.rid w) :=
      hK _ _ _

    theorem and_l {a b : Bool} (h : (a && b) = true) : a = true := (Eq.mp (Bool.and_eq_true a b) h).1
    theorem and_r {a b : Bool} (h : (a && b) = true) : b = true := (Eq.mp (Bool.and_eq_true a b) h).2
    theorem and_mk {a b : Bool} (ha : a = true) (hb : b = true) : (a && b) = true := by rw [ha, hb]
    theorem ne_tf {b : Bool} (h1 : b = true) (h2 : b = false) : False := Bool.noConfusion (Eq.trans h1.symm h2)
    def getD0 (o : Option Nat) : Nat :=
      match o with
      | none => 0
      | some x => x
    theorem some_inj {a b : Nat} (h : some a = some b) : a = b := congrArg getD0 h
    theorem some_ne_none {a : Nat} (h : some a = none) : False := Bool.noConfusion (congrArg found h)
    theorem lt_succ_cases (i n : Nat) (h : i < n + 1) : i < n ∨ i = n := by
      by_cases e : i = n
      · exact Or.inr e
      · exact Or.inl (lt_pred_of_ne i n h e)

    -- The fold's winner: none when S is empty; else an op of S no op of S beats.
    def WinInv (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) (n : Nat) : Prop :=
      (win K t s S n = none → ∀ i, i < n → S i = false) ∧
      (∀ w, win K t s S n = some w → w < n ∧ S w = true ∧ ∀ i, i < n → S i = true → beatsAt K t s i w = false)

    theorem win_inv (K : Kernels) (hK : BeatsExact K) (t : Reg) (s : St) (S : Nat → Bool) :
        ∀ n, WinInv K t s S n := by
      intro n
      induction n with
      | zero =>
        exact ⟨fun _ i hi => False.elim (lt_zero_false i hi), fun w hw => False.elim (some_ne_none hw.symm)⟩
      | succ n ih =>
        cases hw : win K t s S n with
        | none =>
          have hall : ∀ i, i < n → S i = false := ih.1 hw
          cases hs : S n with
          | false =>
            have hn : win K t s S (n + 1) = none := by
              show winStep K t s S n (win K t s S n) = none
              rw [hw]
              show (if S n = true then some n else none) = none
              rw [hs]
            refine ⟨?_, fun w h => False.elim (some_ne_none (Eq.trans h.symm hn))⟩
            intro _ i hi
            cases lt_succ_cases i n hi with
            | inl x => exact hall i x
            | inr x => rw [x]; exact hs
          | true =>
            have hn : win K t s S (n + 1) = some n := by
              show winStep K t s S n (win K t s S n) = some n
              rw [hw]
              show (if S n = true then some n else none) = some n
              rw [if_pos hs]
            refine ⟨fun h => False.elim (some_ne_none (Eq.trans hn.symm h)), ?_⟩
            intro w h
            have hwn : n = w := some_inj (Eq.trans hn.symm h)
            rw [← hwn]
            refine ⟨Nat.lt_succ_self n, hs, ?_⟩
            intro i hi hsi
            cases lt_succ_cases i n hi with
            | inl x => exact False.elim (ne_tf hsi (hall i x))
            | inr x =>
              rw [x, beatsAt_lex K hK t s n n]
              exact lex_irrefl _ _
        | some w =>
          have hw3 := ih.2 w hw
          cases hb : (S n && beatsAt K t s n w) with
          | true =>
            have hn : win K t s S (n + 1) = some n := by
              show winStep K t s S n (win K t s S n) = some n
              rw [hw]
              show (if (S n && beatsAt K t s n w) = true then some n else some w) = some n
              rw [if_pos hb]
            refine ⟨fun h => False.elim (some_ne_none (Eq.trans hn.symm h)), ?_⟩
            intro w' h
            have hwn : n = w' := some_inj (Eq.trans hn.symm h)
            rw [← hwn]
            refine ⟨Nat.lt_succ_self n, and_l hb, ?_⟩
            intro i hi hsi
            cases lt_succ_cases i n hi with
            | inl x =>
              cases hc : beatsAt K t s i n with
              | false => rfl
              | true =>
                have hiw := hw3.2.2 i x hsi
                rw [beatsAt_lex K hK t s i w] at hiw
                rw [beatsAt_lex K hK t s i n] at hc
                have hnw := and_r hb
                rw [beatsAt_lex K hK t s n w] at hnw
                exact False.elim (ne_tf (lex_trans _ _ _ _ _ _ hc hnw) hiw)
            | inr x =>
              rw [x, beatsAt_lex K hK t s n n]
              exact lex_irrefl _ _
          | false =>
            have hn : win K t s S (n + 1) = some w := by
              show winStep K t s S n (win K t s S n) = some w
              rw [hw]
              show (if (S n && beatsAt K t s n w) = true then some n else some w) = some w
              rw [hb]
            refine ⟨fun h => False.elim (some_ne_none (Eq.trans hn.symm h)), ?_⟩
            intro w' h
            have hwn : w = w' := some_inj (Eq.trans hn.symm h)
            rw [← hwn]
            refine ⟨Nat.lt_trans hw3.1 (Nat.lt_succ_self n), hw3.2.1, ?_⟩
            intro i hi hsi
            cases lt_succ_cases i n hi with
            | inl x => exact hw3.2.2 i x hsi
            | inr x =>
              rw [x]
              rw [x] at hsi
              rw [hsi] at hb
              exact hb

    -- ===========================================================================
    -- 3. Two Macs' views of the same ops. Op i of one and op k of the other are
    --    one record's op when they agree on everything the register reads of it.
    -- ===========================================================================

    def Match (t u : Reg) (i k : Nat) : Prop :=
      u.mem k = t.mem i ∧ u.rid k = t.rid i ∧ u.cls k = t.cls i ∧ u.del k = t.del i

    -- The arrival-order relation: the same ops (each op of one view has its like
    -- in the other, in whatever position its arrival put it) and the same causal
    -- order. Two Macs reading the same ops, each in its own arrival order.
    def SameOps (t u : Reg) : Prop :=
      (∀ i, i < t.n → t.mem i = true → ∃ k, k < u.n ∧ Match t u i k) ∧
      (∀ k, k < u.n → u.mem k = true → ∃ i, i < t.n ∧ Match t u i k) ∧
      u.ord = t.ord

    -- A log holds each record once.
    def Distinct (t : Reg) : Prop :=
      ∀ i j, i < t.n → j < t.n → t.mem i = true → t.mem j = true → t.rid i = t.rid j → i = j

    -- A log holds a record after every record in its causal past (`Log::insert`
    -- keeps a record waiting until its parents have arrived).
    def RecordOrder (t : Reg) : Prop :=
      ∀ i j, i < t.n → j < t.n → t.mem i = true → t.mem j = true → t.ord (t.rid i) (t.rid j) = true → i < j

    theorem same_ops_refl (t : Reg) : SameOps t t :=
      ⟨fun i hi _ => ⟨i, hi, rfl, rfl, rfl, rfl⟩, fun k hk _ => ⟨k, hk, rfl, rfl, rfl, rfl⟩, rfl⟩
    theorem match_refl (t : Reg) (i : Nat) : Match t t i i := ⟨rfl, rfl, rfl, rfl⟩

    theorem any_like (t u : Reg) (h : SameOps t u) (P Q : Nat → Bool)
        (hP : ∀ i, i < t.n → P i = true → t.mem i = true)
        (hQ : ∀ k, k < u.n → Q k = true → u.mem k = true)
        (hPQ : ∀ i k, i < t.n → k < u.n → Match t u i k → Q k = P i) :
        anyN u.n Q = anyN t.n P :=
      (anyN_match P Q t.n u.n
        (fun i hi hp => Exists.elim (h.1 i hi (hP i hi hp)) (fun k hk =>
          ⟨k, hk.1, Eq.trans (hPQ i k hi hk.1 hk.2) hp⟩))
        (fun k hk hq => Exists.elim (h.2.1 k hk (hQ k hk hq)) (fun i hi =>
          ⟨i, hi.1, Eq.trans (hPQ i k hi.1 hk hi.2).symm hq⟩))).symm

    theorem all_like (t u : Reg) (h : SameOps t u) (P Q : Nat → Bool)
        (hP : ∀ i, i < t.n → P i = false → t.mem i = true)
        (hQ : ∀ k, k < u.n → Q k = false → u.mem k = true)
        (hPQ : ∀ i k, i < t.n → k < u.n → Match t u i k → Q k = P i) :
        allN u.n Q = allN t.n P :=
      (allN_match P Q t.n u.n
        (fun i hi hp => Exists.elim (h.1 i hi (hP i hi hp)) (fun k hk =>
          ⟨k, hk.1, Eq.trans (hPQ i k hi hk.1 hk.2) hp⟩))
        (fun k hk hq => Exists.elim (h.2.1 k hk (hQ k hk hq)) (fun i hi =>
          ⟨i, hi.1, Eq.trans (hPQ i k hi.1 hk hi.2).symm hq⟩))).symm

    theorem between_like (t u : Reg) (h : SameOps t u) (a b : Nat) : between u a b = between t a b :=
      any_like t u h (fun m => t.mem m && (t.ord a (t.rid m) && t.ord (t.rid m) b))
        (fun m => u.mem m && (u.ord a (u.rid m) && u.ord (u.rid m) b))
        (fun i _ hp => and_l hp) (fun k _ hq => and_l hq)
        (fun i k _ _ hm => by
          show (u.mem k && (u.ord a (u.rid k) && u.ord (u.rid k) b)) = (t.mem i && (t.ord a (t.rid i) && t.ord (t.rid i) b))
          rw [hm.1, hm.2.1, h.2.2])

    theorem front_like (t u : Reg) (h : SameOps t u) (x j l : Nat) (hm : Match t u j l) :
        front u x l = front t x j := by
      show (u.mem l && (u.ord (u.rid l) x && !(between u (u.rid l) x))) = (t.mem j && (t.ord (t.rid j) x && !(between t (t.rid j) x)))
      rw [hm.1, hm.2.1, between_like t u h, h.2.2]

    theorem front_mem (t : Reg) (x j : Nat) (h : front t x j = true) : t.mem j = true := and_l h

    theorem dropped_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (S S' : Nat → Bool)
        (hS : ∀ i, i < t.n → S i = true → t.mem i = true)
        (hS' : ∀ k, k < u.n → S' k = true → u.mem k = true)
        (hSS : ∀ i k, i < t.n → k < u.n → Match t u i k → S' k = S i)
        (j l : Nat) (hm : Match t u j l) (hc : s'.c (t.rid j) = s.c (t.rid j)) (hp : s'.p (t.rid j) = s.p (t.rid j)) :
        dropped K u s' S' l = dropped K t s S j :=
      any_like t u h
        (fun m => S m && K.drops (s.c (t.rid j)) (decide (t.cls m = t.cls j)) (t.ord (s.p (t.rid j)) (t.rid m)))
        (fun m => S' m && K.drops (s'.c (u.rid l)) (decide (u.cls m = u.cls l)) (u.ord (s'.p (u.rid l)) (u.rid m)))
        (fun i hi hq => hS i hi (and_l hq)) (fun k hk hq => hS' k hk (and_l hq))
        (fun i k hi hk hmk => by
          show (S' k && K.drops (s'.c (u.rid l)) (decide (u.cls k = u.cls l)) (u.ord (s'.p (u.rid l)) (u.rid k)))
             = (S i && K.drops (s.c (t.rid j)) (decide (t.cls i = t.cls j)) (t.ord (s.p (t.rid j)) (t.rid i)))
          rw [hSS i k hi hk hmk, hm.2.1, hc, hp, hm.2.2.1, hmk.2.2.1, hmk.2.1, h.2.2])

    theorem live_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (S S' : Nat → Bool)
        (hS : ∀ i, i < t.n → S i = true → t.mem i = true)
        (hS' : ∀ k, k < u.n → S' k = true → u.mem k = true)
        (hSS : ∀ i k, i < t.n → k < u.n → Match t u i k → S' k = S i)
        (j l : Nat) (hj : j < t.n) (hl : l < u.n) (hm : Match t u j l)
        (hag : S j = true → s'.c (t.rid j) = s.c (t.rid j) ∧ s'.p (t.rid j) = s.p (t.rid j)) :
        live K u s' S' l = live K t s S j := by
      show (S' l && !(dropped K u s' S' l)) = (S j && !(dropped K t s S j))
      rw [hSS j l hj hl hm]
      cases hs : S j with
      | false => rfl
      | true =>
        have ha := hag hs
        rw [dropped_like K t u h s s' S S' hS hS' hSS j l hm ha.1 ha.2]

    theorem live_sub (K : Kernels) (t : Reg) (s : St) (S : Nat → Bool) (j : Nat) (h : live K t s S j = true) :
        S j = true := and_l h

    -- What two Macs decided agrees on the ops of S.
    def AgreeOn (t : Reg) (s s' : St) (S : Nat → Bool) : Prop :=
      ∀ j, j < t.n → S j = true → s'.c (t.rid j) = s.c (t.rid j) ∧ s'.p (t.rid j) = s.p (t.rid j)

    theorem held_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (x j l : Nat)
        (hj : j < t.n) (hl : l < u.n) (hm : Match t u j l) (hag : AgreeOn t s s' (front t x)) :
        held K u s' x l = held K t s x j :=
      live_like K t u h s s' (front t x) (front u x) (fun i _ hf => front_mem t x i hf)
        (fun k _ hf => front_mem u x k hf) (fun i k _ _ hmk => front_like t u h x i k hmk) j l hj hl hm (hag j hj)

    theorem held_front (K : Kernels) (t : Reg) (s : St) (x j : Nat) (h : held K t s x j = true) :
        front t x j = true := live_sub K t s (front t x) j h
    theorem held_mem (K : Kernels) (t : Reg) (s : St) (x j : Nat) (h : held K t s x j = true) :
        t.mem j = true := front_mem t x j (held_front K t s x j h)

    -- Two Macs' winners of the same set: none on both, or one record's op on both.
    def WinRel (t u : Reg) (S : Nat → Bool) (o o' : Option Nat) : Prop :=
      (o = none ∧ o' = none) ∨ (∃ w w', o = some w ∧ o' = some w' ∧ w < t.n ∧ S w = true ∧ Match t u w w')

    theorem win_like (K : Kernels) (hK : BeatsExact K) (t u : Reg) (h : SameOps t u) (hd : Distinct t)
        (s s' : St) (S S' : Nat → Bool)
        (hS : ∀ i, i < t.n → S i = true → t.mem i = true)
        (hS' : ∀ k, k < u.n → S' k = true → u.mem k = true)
        (hSS : ∀ i k, i < t.n → k < u.n → Match t u i k → S' k = S i)
        (hag : ∀ i, i < t.n → S i = true → s'.p (t.rid i) = s.p (t.rid i)) :
        WinRel t u S (win K t s S t.n) (win K u s' S' u.n) := by
      have it := win_inv K hK t s S t.n
      have iu := win_inv K hK u s' S' u.n
      cases hw : win K t s S t.n with
      | none =>
        have hall := it.1 hw
        cases hw' : win K u s' S' u.n with
        | none => exact Or.inl ⟨rfl, rfl⟩
        | some w' =>
          have hu := iu.2 w' hw'
          exact Exists.elim (h.2.1 w' hu.1 (hS' w' hu.1 hu.2.1)) (fun i hi =>
            False.elim (ne_tf (Eq.trans (hSS i w' hi.1 hu.1 hi.2).symm hu.2.1) (hall i hi.1)))
      | some w =>
        have ht := it.2 w hw
        cases hw' : win K u s' S' u.n with
        | none =>
          have hall := iu.1 hw'
          exact Exists.elim (h.1 w ht.1 (hS w ht.1 ht.2.1)) (fun k hk =>
            False.elim (ne_tf (Eq.trans (hSS w k ht.1 hk.1 hk.2) ht.2.1) (hall k hk.1)))
        | some w' =>
          have hu := iu.2 w' hw'
          cases h.1 w ht.1 (hS w ht.1 ht.2.1) with
          | intro k hk =>
            cases h.2.1 w' hu.1 (hS' w' hu.1 hu.2.1) with
            | intro i hi =>
              have hsk : S' k = true := Eq.trans (hSS w k ht.1 hk.1 hk.2) ht.2.1
              have hsi : S i = true := Eq.trans (hSS i w' hi.1 hu.1 hi.2).symm hu.2.1
              have b1 := ht.2.2 i hi.1 hsi
              rw [beatsAt_lex K hK t s i w] at b1
              have b2 := hu.2.2 k hk.1 hsk
              rw [beatsAt_lex K hK u s' k w', hk.2.2.1, hi.2.2.1, hag w ht.1 ht.2.1, hag i hi.1 hsi] at b2
              have e := lex_total _ _ _ _ b1 b2
              have hiw : i = w := hd i w hi.1 ht.1 (hS i hi.1 hsi) (hS w ht.1 ht.2.1) e.2
              rw [hiw] at hi
              exact Or.inr ⟨w, w', rfl, rfl, ht.1, ht.2.1, hi.2⟩

    theorem found_rel (t u : Reg) (S : Nat → Bool) (o o' : Option Nat) (h : WinRel t u S o o') :
        found o' = found o := by
      cases h with
      | inl x => rw [x.1, x.2]
      | inr x =>
        cases x with
        | intro w hx =>
          cases hx with
          | intro w' hy =>
            rw [hy.1, hy.2.1]
            rfl

    theorem ridOf_rel (t u : Reg) (S : Nat → Bool) (o o' : Option Nat) (h : WinRel t u S o o') :
        ridOf u o' = ridOf t o := by
      cases h with
      | inl x =>
        rw [x.1, x.2]
        rfl
      | inr x =>
        cases x with
        | intro w hx =>
          cases hx with
          | intro w' hy =>
            rw [hy.1, hy.2.1]
            show some (u.rid w') = some (t.rid w)
            rw [hy.2.2.2.2.2.1]

    theorem sameAs_rel (t u : Reg) (S : Nat → Bool) (o o' : Option Nat) (h : WinRel t u S o o') (i k : Nat)
        (hm : Match t u i k) : sameAs u o' k = sameAs t o i := by
      cases h with
      | inl x =>
        rw [x.1, x.2]
        rfl
      | inr x =>
        cases x with
        | intro w hx =>
          cases hx with
          | intro w' hy =>
            rw [hy.1, hy.2.1]
            show decide (u.cls w' = u.cls k) = decide (t.cls w = t.cls i)
            rw [hy.2.2.2.2.2.2.1, hm.2.2.1]

    theorem repOf_rel (t u : Reg) (s s' : St) (S : Nat → Bool) (o o' : Option Nat) (h : WinRel t u S o o')
        (i k : Nat) (hm : Match t u i k) (hag : ∀ w, w < t.n → S w = true → s'.p (t.rid w) = s.p (t.rid w)) :
        repOf u s' o' k = repOf t s o i := by
      cases h with
      | inl x =>
        rw [x.1, x.2]
        show u.rid k = t.rid i
        exact hm.2.1
      | inr x =>
        cases x with
        | intro w hx =>
          cases hx with
          | intro w' hy =>
            rw [hy.1, hy.2.1]
            show s'.p (u.rid w') = s.p (t.rid w)
            rw [hy.2.2.2.2.2.1]
            exact hag w hy.2.2.1 hy.2.2.2.1

    theorem not_or_false {a b : Bool} (h : (!a || b) = false) : a = true := by
      cases a with
      | true => rfl
      | false => exact Bool.noConfusion h

    -- One record's op is classified alike on two Macs whose decisions agree on
    -- its frontier.
    theorem carry_like (K : Kernels) (hK : BeatsExact K) (t u : Reg) (h : SameOps t u) (hd : Distinct t)
        (s s' : St) (i k : Nat) (hi : i < t.n) (hk : k < u.n) (hm : Match t u i k)
        (hag : AgreeOn t s s' (front t (t.rid i))) :
        carryAt K u s' k = carryAt K t s i ∧ repAt K u s' k = repAt K t s i := by
      have hx : u.rid k = t.rid i := hm.2.1
      have hSS : ∀ j l, j < t.n → l < u.n → Match t u j l → held K u s' (t.rid i) l = held K t s (t.rid i) j :=
        fun j l hj hl hjl => held_like K t u h s s' (t.rid i) j l hj hl hjl hag
      have hagp : ∀ w, w < t.n → held K t s (t.rid i) w = true → s'.p (t.rid w) = s.p (t.rid w) :=
        fun w hw hh => (hag w hw (held_front K t s (t.rid i) w hh)).2
      have hal : allDel u (held K u s' (t.rid i)) = allDel t (held K t s (t.rid i)) :=
        all_like t u h (fun j => !(held K t s (t.rid i) j) || t.del j) (fun l => !(held K u s' (t.rid i) l) || u.del l)
          (fun j _ hf => held_mem K t s (t.rid i) j (not_or_false hf))
          (fun l _ hf => held_mem K u s' (t.rid i) l (not_or_false hf))
          (fun j l hj hl hjl => by
            show (!(held K u s' (t.rid i) l) || u.del l) = (!(held K t s (t.rid i) j) || t.del j)
            rw [hSS j l hj hl hjl, hjl.2.2.2])
      have hw := win_like K hK t u h hd s s' (held K t s (t.rid i)) (held K u s' (t.rid i))
        (fun j _ hh => held_mem K t s (t.rid i) j hh) (fun l _ hh => held_mem K u s' (t.rid i) l hh) hSS hagp
      have e1 : carryAt K u s' k = carryAt K t s i := by
        show K.carries (u.del k) (found (win K u s' (held K u s' (u.rid k)) u.n))
            (sameAs u (win K u s' (held K u s' (u.rid k)) u.n) k) (allDel u (held K u s' (u.rid k)))
          = K.carries (t.del i) (found (win K t s (held K t s (t.rid i)) t.n))
            (sameAs t (win K t s (held K t s (t.rid i)) t.n) i) (allDel t (held K t s (t.rid i)))
        rw [hm.2.2.2, hx, found_rel t u _ _ _ hw, sameAs_rel t u _ _ _ hw i k hm, hal]
      have e2 : repAt K u s' k = repAt K t s i := by
        show (if carryAt K u s' k = true then repOf u s' (win K u s' (held K u s' (u.rid k)) u.n) k else u.rid k)
          = (if carryAt K t s i = true then repOf t s (win K t s (held K t s (t.rid i)) t.n) i else t.rid i)
        rw [e1, hx, repOf_rel t u s s' _ _ _ hw i k hm hagp]
      exact ⟨e1, e2⟩

    -- ===========================================================================
    -- 4. The pass. On a log that holds each record once and every record after
    --    its causal past, the pass's decisions are a fixed point (each op's is
    --    what its frontier's decisions give), and there is only one: so they
    --    depend on the ops and their causal order alone.
    -- ===========================================================================

    def Fixed (K : Kernels) (t : Reg) (s : St) : Prop :=
      ∀ k, k < t.n → t.mem k = true → s.c (t.rid k) = carryAt K t s k ∧ s.p (t.rid k) = repAt K t s k

    def AgreeAt (t : Reg) (s s' : St) (k : Nat) : Prop :=
      s'.c (t.rid k) = s.c (t.rid k) ∧ s'.p (t.rid k) = s.p (t.rid k)

    theorem agree_trans (t : Reg) (s1 s2 s3 : St) (k : Nat) (a : AgreeAt t s1 s2 k) (b : AgreeAt t s2 s3 k) :
        AgreeAt t s1 s3 k := ⟨Eq.trans b.1 a.1, Eq.trans b.2 a.2⟩
    theorem agree_symm (t : Reg) (s1 s2 : St) (k : Nat) (a : AgreeAt t s1 s2 k) : AgreeAt t s2 s1 k :=
      ⟨a.1.symm, a.2.symm⟩

    theorem pass_succ_mem (K : Kernels) (t : Reg) (k : Nat) (h : t.mem k = true) :
        pass K t (k + 1) = upd K t (pass K t k) k := by
      show (if t.mem k = true then upd K t (pass K t k) k else pass K t k) = upd K t (pass K t k) k
      rw [if_pos h]
    theorem pass_succ_not (K : Kernels) (t : Reg) (k : Nat) (h : t.mem k = false) :
        pass K t (k + 1) = pass K t k := by
      show (if t.mem k = true then upd K t (pass K t k) k else pass K t k) = pass K t k
      rw [h]

    theorem upd_c_at (K : Kernels) (t : Reg) (s : St) (k : Nat) : (upd K t s k).c (t.rid k) = carryAt K t s k := by
      show (if t.rid k = t.rid k then carryAt K t s k else s.c (t.rid k)) = carryAt K t s k
      rw [if_pos rfl]
    theorem upd_p_at (K : Kernels) (t : Reg) (s : St) (k : Nat) : (upd K t s k).p (t.rid k) = repAt K t s k := by
      show (if t.rid k = t.rid k then repAt K t s k else s.p (t.rid k)) = repAt K t s k
      rw [if_pos rfl]
    theorem upd_other (K : Kernels) (t : Reg) (s : St) (k x : Nat) (h : Not (x = t.rid k)) :
        (upd K t s k).c x = s.c x ∧ (upd K t s k).p x = s.p x := by
      have a : (upd K t s k).c x = s.c x := by
        show (if x = t.rid k then carryAt K t s k else s.c x) = s.c x
        rw [if_neg h]
      have b : (upd K t s k).p x = s.p x := by
        show (if x = t.rid k then repAt K t s k else s.p x) = s.p x
        rw [if_neg h]
      exact ⟨a, b⟩

    theorem le_succ' (m : Nat) : m ≤ m + 1 := by omega
    theorem le_trans' (a b c : Nat) (h1 : a ≤ b) (h2 : b ≤ c) : a ≤ c := by omega
    theorem lt_le' (a b c : Nat) (h1 : a < b) (h2 : b ≤ c) : a < c := by omega
    theorem le_refl' (a : Nat) : a ≤ a := by omega
    theorem le_of_lt'' (a b : Nat) (h : a < b) : a ≤ b := by omega
    theorem lt_succ' (a : Nat) : a < a + 1 := by omega

    -- Once the pass has classified op k, nothing later changes it.
    theorem pass_stable (K : Kernels) (t : Reg) (hd : Distinct t) :
        ∀ m k, k < m → m ≤ t.n → t.mem k = true → AgreeAt t (pass K t (k + 1)) (pass K t m) k := by
      intro m
      induction m with
      | zero => intro k hk _ _; exact False.elim (lt_zero_false k hk)
      | succ m ih =>
        intro k hk hm hmem
        cases lt_succ_cases k m hk with
        | inr x =>
          rw [x]
          exact ⟨rfl, rfl⟩
        | inl x =>
          have prev := ih k x (le_trans' m (m + 1) t.n (le_succ' m) hm) hmem
          cases hmm : t.mem m with
          | false =>
            rw [pass_succ_not K t m hmm]
            exact prev
          | true =>
            have hne : Not (t.rid k = t.rid m) := fun e =>
              absurd (hd k m (lt_le' k (m + 1) t.n (lt_succ_of_lt' k m x) hm) (lt_le' m (m + 1) t.n (lt_succ' m) hm)
                hmem hmm e) (ne_of_lt' k m x)
            rw [pass_succ_mem K t m hmm]
            have o := upd_other K t (pass K t m) m (t.rid k) hne
            exact ⟨Eq.trans o.1 prev.1, Eq.trans o.2 prev.2⟩

    theorem front_ord (t : Reg) (x j : Nat) (h : front t x j = true) : t.ord (t.rid j) x = true := and_l (and_r h)

    -- The pass's decisions are a fixed point.
    theorem pass_fixed (K : Kernels) (hK : BeatsExact K) (t : Reg) (hd : Distinct t) (ho : RecordOrder t) :
        Fixed K t (final K t) := by
      intro k hk hmem
      have a0 := pass_stable K t hd t.n k hk (le_refl' t.n) hmem
      have a1 : (pass K t t.n).c (t.rid k) = carryAt K t (pass K t k) k :=
        Eq.trans a0.1 (Eq.trans (congrArg (fun (s : St) => s.c (t.rid k)) (pass_succ_mem K t k hmem))
          (upd_c_at K t (pass K t k) k))
      have a2 : (pass K t t.n).p (t.rid k) = repAt K t (pass K t k) k :=
        Eq.trans a0.2 (Eq.trans (congrArg (fun (s : St) => s.p (t.rid k)) (pass_succ_mem K t k hmem))
          (upd_p_at K t (pass K t k) k))
      have hag : AgreeOn t (pass K t k) (pass K t t.n) (front t (t.rid k)) := by
        intro j hj hf
        have hjk : j < k := ho j k hj hk (front_mem t (t.rid k) j hf) hmem (front_ord t (t.rid k) j hf)
        have hjm : t.mem j = true := front_mem t (t.rid k) j hf
        have x1 := pass_stable K t hd t.n j hj (le_refl' t.n) hjm
        have x2 := pass_stable K t hd k j hjk (le_of_lt'' k t.n hk) hjm
        exact agree_trans t (pass K t k) (pass K t (j + 1)) (pass K t t.n) j (agree_symm t _ _ j x2) x1
      have c := carry_like K hK t t (same_ops_refl t) hd (pass K t k) (pass K t t.n) k k hk hk (match_refl t k) hag
      exact ⟨Eq.trans a1 c.1.symm, Eq.trans a2 c.2.symm⟩

    -- Two fixed points of one view agree on every op.
    theorem fixed_unique (K : Kernels) (hK : BeatsExact K) (t : Reg) (hd : Distinct t) (ho : RecordOrder t)
        (s s' : St) (hs : Fixed K t s) (hs' : Fixed K t s') :
        ∀ m k, k < m → k < t.n → t.mem k = true → AgreeAt t s s' k := by
      intro m
      induction m with
      | zero => intro k hk _ _; exact False.elim (lt_zero_false k hk)
      | succ m ih =>
        intro k hk hkn hmem
        cases lt_succ_cases k m hk with
        | inl x => exact ih k x hkn hmem
        | inr x =>
          have hag : AgreeOn t s s' (front t (t.rid k)) := by
            intro j hj hf
            have hjk : j < k := ho j k hj hkn (front_mem t (t.rid k) j hf) hmem (front_ord t (t.rid k) j hf)
            rw [x] at hjk
            exact ih j hjk hj (front_mem t (t.rid k) j hf)
          have c := carry_like K hK t t (same_ops_refl t) hd s s' k k hkn hkn (match_refl t k) hag
          have f := hs k hkn hmem
          have f' := hs' k hkn hmem
          exact ⟨Eq.trans f'.1 (Eq.trans c.1 f.1.symm), Eq.trans f'.2 (Eq.trans c.2 f.2.symm)⟩

    -- A fixed point of one Mac's view is one of every view of the same ops.
    theorem fixed_transfer (K : Kernels) (hK : BeatsExact K) (t u : Reg) (h : SameOps t u) (hd : Distinct t)
        (s : St) (hs : Fixed K t s) : Fixed K u s := by
      intro k hk hmem
      cases h.2.1 k hk hmem with
      | intro i hi =>
        have c := carry_like K hK t u h hd s s i k hi.1 hk hi.2 (fun _ _ _ => ⟨rfl, rfl⟩)
        have f := hs i hi.1 (Eq.trans hi.2.1.symm hmem)
        rw [hi.2.2.1]
        exact ⟨Eq.trans f.1 c.1.symm, Eq.trans f.2 c.2.symm⟩

    -- ===========================================================================
    -- 5. The siblings, the winner and the conflict, read alike off two Macs'
    --    views of the same ops whose decisions agree on every op.
    -- ===========================================================================

    theorem replaced_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem)
        (a a' : Nat) (hma : Match t u a a') : replaced K u s' a' = replaced K t s a :=
      any_like t u h
        (fun b => t.mem b && K.replaces (t.ord (t.rid a) (t.rid b)) (s.c (t.rid b)) (decide (t.cls a = t.cls b)))
        (fun b => u.mem b && K.replaces (u.ord (u.rid a') (u.rid b)) (s'.c (u.rid b)) (decide (u.cls a' = u.cls b)))
        (fun i _ hp => and_l hp) (fun k _ hq => and_l hq)
        (fun b b' hb _ hmb => by
          show (u.mem b' && K.replaces (u.ord (u.rid a') (u.rid b')) (s'.c (u.rid b')) (decide (u.cls a' = u.cls b')))
            = (t.mem b && K.replaces (t.ord (t.rid a) (t.rid b)) (s.c (t.rid b)) (decide (t.cls a = t.cls b)))
          rw [hmb.1]
          cases hm : t.mem b with
          | false => rfl
          | true => rw [hma.2.1, hmb.2.1, (hag b hb hm).1, hma.2.2.1, hmb.2.2.1, h.2.2])

    theorem maximal_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem)
        (a a' : Nat) (hma : Match t u a a') : maximal K u s' a' = maximal K t s a := by
      show (u.mem a' && !(replaced K u s' a')) = (t.mem a && !(replaced K t s a))
      rw [hma.1, replaced_like K t u h s s' hag a a' hma]

    theorem sib_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem)
        (j l : Nat) (hj : j < t.n) (hl : l < u.n) (hm : Match t u j l) : sib K u s' l = sib K t s j :=
      live_like K t u h s s' (maximal K t s) (maximal K u s') (fun i _ hp => and_l hp) (fun k _ hq => and_l hq)
        (fun i k _ _ hmk => maximal_like K t u h s s' hag i k hmk) j l hj hl hm (fun hx => hag j hj (and_l hx))

    theorem sib_mem (K : Kernels) (t : Reg) (s : St) (j : Nat) (h : sib K t s j = true) : t.mem j = true :=
      and_l (live_sub K t s (maximal K t s) j h)

    theorem winner_like (K : Kernels) (hK : BeatsExact K) (t u : Reg) (h : SameOps t u) (hd : Distinct t)
        (s s' : St) (hag : AgreeOn t s s' t.mem) :
        WinRel t u (sib K t s) (winnerOf K t s) (winnerOf K u s') :=
      win_like K hK t u h hd s s' (sib K t s) (sib K u s') (fun i _ hs => sib_mem K t s i hs)
        (fun k _ hs => sib_mem K u s' k hs) (fun i k hi hk hm => sib_like K t u h s s' hag i k hi hk hm)
        (fun i hi hs => (hag i hi (sib_mem K t s i hs)).2)

    theorem conflict_like (K : Kernels) (hK : BeatsExact K) (t u : Reg) (h : SameOps t u) (hd : Distinct t)
        (s s' : St) (hag : AgreeOn t s s' t.mem) : conflictOf K u s' = conflictOf K t s := by
      have hw := winner_like K hK t u h hd s s' hag
      cases hw with
      | inl x =>
        show (match winnerOf K u s' with
          | none => false
          | some w => anyN u.n (fun i => sib K u s' i && K.conflicts (decide (u.cls i = u.cls w))))
          = (match winnerOf K t s with
          | none => false
          | some w => anyN t.n (fun i => sib K t s i && K.conflicts (decide (t.cls i = t.cls w))))
        rw [x.1, x.2]
      | inr x =>
        cases x with
        | intro w hx =>
          cases hx with
          | intro w' hy =>
            show (match winnerOf K u s' with
              | none => false
              | some w => anyN u.n (fun i => sib K u s' i && K.conflicts (decide (u.cls i = u.cls w))))
              = (match winnerOf K t s with
              | none => false
              | some w => anyN t.n (fun i => sib K t s i && K.conflicts (decide (t.cls i = t.cls w))))
            rw [hy.1, hy.2.1]
            exact any_like t u h
              (fun i => sib K t s i && K.conflicts (decide (t.cls i = t.cls w)))
              (fun i => sib K u s' i && K.conflicts (decide (u.cls i = u.cls w')))
              (fun i _ hp => sib_mem K t s i (and_l hp)) (fun k _ hq => sib_mem K u s' k (and_l hq))
              (fun i k hi hk hm => by
                show (sib K u s' k && K.conflicts (decide (u.cls k = u.cls w')))
                  = (sib K t s i && K.conflicts (decide (t.cls i = t.cls w)))
                rw [sib_like K t u h s s' hag i k hi hk hm, hm.2.2.1, hy.2.2.2.2.2.2.1])

    theorem sibRid_like (K : Kernels) (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem)
        (x : Nat) :
        anyN u.n (fun i => sib K u s' i && decide (u.rid i = x)) = anyN t.n (fun i => sib K t s i && decide (t.rid i = x)) :=
      any_like t u h _ _ (fun i _ hp => sib_mem K t s i (and_l hp)) (fun k _ hq => sib_mem K u s' k (and_l hq))
        (fun i k hi hk hm => by
          show (sib K u s' k && decide (u.rid k = x)) = (sib K t s i && decide (t.rid i = x))
          rw [sib_like K t u h s s' hag i k hi hk hm, hm.2.1])

    theorem carryRid_like (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem) (x : Nat) :
        anyN u.n (fun i => u.mem i && (decide (u.rid i = x) && s'.c (u.rid i)))
          = anyN t.n (fun i => t.mem i && (decide (t.rid i = x) && s.c (t.rid i))) :=
      any_like t u h _ _ (fun i _ hp => and_l hp) (fun k _ hq => and_l hq)
        (fun i k hi _ hm => by
          show (u.mem k && (decide (u.rid k = x) && s'.c (u.rid k))) = (t.mem i && (decide (t.rid i = x) && s.c (t.rid i)))
          rw [hm.1]
          cases hmi : t.mem i with
          | false => rfl
          | true => rw [hm.2.1, (hag i hi hmi).1])

    theorem repRid_like (t u : Reg) (h : SameOps t u) (s s' : St) (hag : AgreeOn t s s' t.mem) (x r : Nat) :
        anyN u.n (fun i => u.mem i && (decide (u.rid i = x) && decide (s'.p (u.rid i) = r)))
          = anyN t.n (fun i => t.mem i && (decide (t.rid i = x) && decide (s.p (t.rid i) = r))) :=
      any_like t u h _ _ (fun i _ hp => and_l hp) (fun k _ hq => and_l hq)
        (fun i k hi _ hm => by
          show (u.mem k && (decide (u.rid k = x) && decide (s'.p (u.rid k) = r)))
            = (t.mem i && (decide (t.rid i = x) && decide (s.p (t.rid i) = r)))
          rw [hm.1]
          cases hmi : t.mem i with
          | false => rfl
          | true => rw [hm.2.1, (hag i hi hmi).2])

    -- ===========================================================================
    -- 6. Law 5. Two Macs that hold the same ops, each log holding a record once
    --    and after its causal past, in any two arrival orders, read the same
    --    winner (so the same value), the same conflict, the same siblings, the
    --    same carries and the same reps, for every number of ops. Only the winner
    --    order need be the spec's: every other kernel may answer anything.
    -- ===========================================================================

    theorem decisions_agree (K : Kernels) (hK : BeatsExact K) (t u : Reg)
        (hd : Distinct t) (hd' : Distinct u) (ho : RecordOrder t) (ho' : RecordOrder u) (h : SameOps t u) :
        AgreeOn u (final K t) (final K u) u.mem :=
      fun k hk hm => fixed_unique K hK u hd' ho' (final K t) (final K u)
        (fixed_transfer K hK t u h hd (final K t) (pass_fixed K hK t hd ho))
        (pass_fixed K hK u hd' ho') (k + 1) k (lt_succ' k) hk hm

    theorem register_converges (K : Kernels) (hK : BeatsExact K) (t u : Reg)
        (hd : Distinct t) (hd' : Distinct u) (ho : RecordOrder t) (ho' : RecordOrder u) (h : SameOps t u) :
        winnerRid K u = winnerRid K t ∧ conflict K u = conflict K t
          ∧ (∀ x, sibRid K u x = sibRid K t x) ∧ (∀ x, carryRid K u x = carryRid K t x)
          ∧ (∀ x r, repRid K u x r = repRid K t x r) := by
      have ag := decisions_agree K hK t u hd hd' ho ho' h
      have none : AgreeOn t (final K t) (final K t) t.mem := fun _ _ _ => ⟨rfl, rfl⟩
      have r1 : winnerRid K u = winnerRid K t :=
        Eq.trans (ridOf_rel u u _ _ _ (winner_like K hK u u (same_ops_refl u) hd' (final K t) (final K u) ag))
          (ridOf_rel t u _ _ _ (winner_like K hK t u h hd (final K t) (final K t) none))
      have r2 : conflict K u = conflict K t :=
        Eq.trans (conflict_like K hK u u (same_ops_refl u) hd' (final K t) (final K u) ag)
          (conflict_like K hK t u h hd (final K t) (final K t) none)
      have r3 : ∀ x, sibRid K u x = sibRid K t x := fun x =>
        Eq.trans (sibRid_like K u u (same_ops_refl u) (final K t) (final K u) ag x)
          (sibRid_like K t u h (final K t) (final K t) none x)
      have r4 : ∀ x, carryRid K u x = carryRid K t x := fun x =>
        Eq.trans (carryRid_like u u (same_ops_refl u) (final K t) (final K u) ag x)
          (carryRid_like t u h (final K t) (final K t) none x)
      have r5 : ∀ x r, repRid K u x r = repRid K t x r := fun x r =>
        Eq.trans (repRid_like u u (same_ops_refl u) (final K t) (final K u) ag x r)
          (repRid_like t u h (final K t) (final K t) none x r)
      exact ⟨r1, r2, r3, r4, r5⟩

    -- An arrival order as a renumbering: view u lists t's ops in the order pi
    -- gives (u's op k is t's op pi k), pi a bijection of the positions with
    -- inverse sigma, and both read the same causal order.
    def Reenumerates (t u : Reg) (pi sigma : Nat → Nat) : Prop :=
      u.n = t.n ∧ u.ord = t.ord ∧
      (∀ k, k < u.n → pi k < t.n ∧ sigma (pi k) = k) ∧
      (∀ i, i < t.n → sigma i < u.n ∧ pi (sigma i) = i) ∧
      (∀ k, k < u.n → Match t u (pi k) k)

    theorem reenumerates_same_ops (t u : Reg) (pi sigma : Nat → Nat) (h : Reenumerates t u pi sigma) :
        SameOps t u := by
      refine ⟨?_, ?_, h.2.1⟩
      · intro i hi _
        have a := h.2.2.2.1 i hi
        have m := h.2.2.2.2 (sigma i) a.1
        rw [a.2] at m
        exact ⟨sigma i, a.1, m⟩
      · intro k hk _
        exact ⟨pi k, (h.2.2.1 k hk).1, h.2.2.2.2 k hk⟩

    theorem reenumerates_distinct (t u : Reg) (pi sigma : Nat → Nat) (h : Reenumerates t u pi sigma)
        (hd : Distinct t) : Distinct u := by
      intro k l hk hl hmk hml e
      have mk := h.2.2.2.2 k hk
      have ml := h.2.2.2.2 l hl
      have pk := h.2.2.1 k hk
      have pl := h.2.2.1 l hl
      have hp : pi k = pi l :=
        hd (pi k) (pi l) pk.1 pl.1 (Eq.trans mk.1.symm hmk) (Eq.trans ml.1.symm hml)
          (Eq.trans mk.2.1.symm (Eq.trans e ml.2.1))
      have hs : sigma (pi k) = sigma (pi l) := congrArg sigma hp
      rw [pk.2, pl.2] at hs
      exact hs

    -- Law 5 for arrival orders: any two enumerations of the same ops that each
    -- list a record after its causal past read the same register.
    theorem arrival_orders_converge (K : Kernels) (hK : BeatsExact K) (t u : Reg) (pi sigma : Nat → Nat)
        (h : Reenumerates t u pi sigma) (hd : Distinct t) (ho : RecordOrder t) (ho' : RecordOrder u) :
        winnerRid K u = winnerRid K t ∧ conflict K u = conflict K t
          ∧ (∀ x, sibRid K u x = sibRid K t x) ∧ (∀ x, carryRid K u x = carryRid K t x)
          ∧ (∀ x r, repRid K u x r = repRid K t x r) :=
      register_converges K hK t u hd (reenumerates_distinct t u pi sigma h hd) ho ho'
        (reenumerates_same_ops t u pi sigma h)

    -- ===========================================================================
    -- 7. Convergence across Macs. A Mac's log, as its registers read it: the ops
    --    it opens off its records, each keyed by its entry (audience, name),
    --    [everyone]'s among them; an entry's register is the log's ops of that
    --    entry. Two Macs that read the same ops and the same causal order
    --    (SameLog) read the same value and the same conflict at every entry.
    --
    --    Premises this island takes from aterm-secrets and does not model: two
    --    Macs whose logs hold the same records read the same ops when both count
    --    the same records (`Effective::counts`), open the same ones (a reader
    --    skips an op whose epoch key it lacks, and every op of an audience that
    --    leaves it out), and name what an op writes by its content (its flags,
    --    anchor and value, the value interned by digest): the code numbers classes
    --    per Mac but only ever compares two, and the law reads the same
    --    comparisons. Outside the theorem, after the registers: dropping a winner
    --    whose anchor no entry holds, and the conflicts the Hosts audiences'
    --    winners add across audiences (no [everyone] entry takes part); both read
    --    only the registers' winners and the causal order between them.
    -- ===========================================================================

    structure Log where
      n   : Nat
      key : Nat → Nat           -- the entry the op writes, one name on every Mac
      rid : Nat → Nat
      cls : Nat → Nat
      del : Nat → Bool
      ord : Nat → Nat → Bool

    def regOf (L : Log) (e : Nat) : Reg := Reg.mk L.n (fun i => decide (L.key i = e)) L.rid L.cls L.del L.ord

    def LogMatch (L M : Log) (i k : Nat) : Prop :=
      M.key k = L.key i ∧ M.rid k = L.rid i ∧ M.cls k = L.cls i ∧ M.del k = L.del i
    def SameLog (L M : Log) : Prop :=
      (∀ i, i < L.n → ∃ k, k < M.n ∧ LogMatch L M i k) ∧
      (∀ k, k < M.n → ∃ i, i < L.n ∧ LogMatch L M i k) ∧ M.ord = L.ord
    def LogDistinct (L : Log) : Prop := ∀ i j, i < L.n → j < L.n → L.rid i = L.rid j → i = j
    def LogOrder (L : Log) : Prop := ∀ i j, i < L.n → j < L.n → L.ord (L.rid i) (L.rid j) = true → i < j

    theorem regOf_match (L M : Log) (i k e : Nat) (h : LogMatch L M i k) : Match (regOf L e) (regOf M e) i k := by
      refine ⟨?_, h.2.1, h.2.2.1, h.2.2.2⟩
      show decide (M.key k = e) = decide (L.key i = e)
      rw [h.1]

    theorem regOf_same (L M : Log) (h : SameLog L M) (e : Nat) : SameOps (regOf L e) (regOf M e) :=
      ⟨fun i hi _ => Exists.elim (h.1 i hi) (fun k hk => ⟨k, hk.1, regOf_match L M i k e hk.2⟩),
       fun k hk _ => Exists.elim (h.2.1 k hk) (fun i hi => ⟨i, hi.1, regOf_match L M i k e hi.2⟩),
       h.2.2⟩

    theorem regOf_distinct (L : Log) (h : LogDistinct L) (e : Nat) : Distinct (regOf L e) :=
      fun i j hi hj _ _ hr => h i j hi hj hr
    theorem regOf_order (L : Log) (h : LogOrder L) (e : Nat) : RecordOrder (regOf L e) :=
      fun i j hi hj _ _ hr => h i j hi hj hr

    -- Law 5, whole: at every entry, [everyone]'s included, two Macs reading the
    -- same ops off their logs read the same winner (the same value), the same
    -- conflict, the same siblings, the same carries and the same reps.
    theorem macs_converge (K : Kernels) (hK : BeatsExact K) (L M : Log)
        (hd : LogDistinct L) (hd' : LogDistinct M) (ho : LogOrder L) (ho' : LogOrder M) (h : SameLog L M) (e : Nat) :
        winnerRid K (regOf M e) = winnerRid K (regOf L e) ∧ conflict K (regOf M e) = conflict K (regOf L e)
          ∧ (∀ x, sibRid K (regOf M e) x = sibRid K (regOf L e) x)
          ∧ (∀ x, carryRid K (regOf M e) x = carryRid K (regOf L e) x)
          ∧ (∀ x r, repRid K (regOf M e) x r = repRid K (regOf L e) x r) :=
      register_converges K hK (regOf L e) (regOf M e) (regOf_distinct L hd e) (regOf_distinct M hd' e)
        (regOf_order L ho e) (regOf_order M ho' e) (regOf_same L M h e)

    theorem spec_converges (L M : Log) (hd : LogDistinct L) (hd' : LogDistinct M) (ho : LogOrder L)
        (ho' : LogOrder M) (h : SameLog L M) (e : Nat) :
        winnerRid specK (regOf M e) = winnerRid specK (regOf L e) ∧ conflict specK (regOf M e) = conflict specK (regOf L e) :=
      let r := macs_converge specK (beats_exact_of_exact specK spec_exact) L M hd hd' ho ho' h e
      ⟨r.1, r.2.1⟩

    -- ===========================================================================
    -- 8. Negative controls, each evaluated by the kernel.
    -- ===========================================================================

    -- Two concurrent Puts of one name (records 5 and 9, values 0 and 1) on a
    -- Put of value 2 (record 7). Mac A received record 5 first, Mac B record 9.
    def ordV (a b : Nat) : Bool := decide (a = 7) && !(decide (b = 7))
    def macA : Reg := Reg.mk 3 (fun _ => true) (fun i => if i = 0 then 7 else (if i = 1 then 5 else 9))
      (fun i => if i = 0 then 2 else (if i = 1 then 0 else 1)) (fun _ => false) ordV
    def macB : Reg := Reg.mk 3 (fun _ => true) (fun i => if i = 0 then 7 else (if i = 1 then 9 else 5))
      (fun i => if i = 0 then 2 else (if i = 1 then 1 else 0)) (fun _ => false) ordV

    theorem both_read_record_5 : And (winnerRid specK macA = some 5) (winnerRid specK macB = some 5) := ⟨rfl, rfl⟩
    theorem both_read_the_conflict : And (conflict specK macA = true) (conflict specK macB = true) := ⟨rfl, rfl⟩

    -- The mutant that reads arrival order: the first sibling seen wins (no
    -- sibling beats another). Mac A reads record 5, Mac B record 9.
    def firstSeen : Kernels := Kernels.mk spec_carries spec_replaces spec_drops (fun _ _ _ => false) spec_conflicts
    theorem first_seen_reads_arrival_order :
        And (winnerRid firstSeen macA = some 5) (winnerRid firstSeen macB = some 9) := ⟨rfl, rfl⟩

    -- The record-order premise carries weight. Record 1 (value 0); its reseal,
    -- record 2 (a carry of record 1); an edit, record 3 (value 1, beside the
    -- reseal); and record 4 (value 1) on the merge of 2 and 3. In causal order,
    -- record 4's writer held the edit alone (the edit took the reseal out) and 4
    -- carries it; a log that listed record 4 before the reseal would read 4 as an
    -- edit, and with it a new rep and winner order.
    def ordR (a b : Nat) : Bool :=
      (decide (a = 1) && !(decide (b = 1))) || (decide (b = 4) && (decide (a = 2) || decide (a = 3)))
    def inOrder : Reg := Reg.mk 4 (fun _ => true) (fun i => i + 1) (fun i => if i < 2 then 0 else 1)
      (fun _ => false) ordR
    def outOfOrder : Reg := Reg.mk 4 (fun _ => true) (fun i => if i = 1 then 4 else (if i = 3 then 2 else i + 1))
      (fun i => if i = 0 then 0 else (if i = 3 then 0 else 1)) (fun _ => false) ordR
    theorem in_order_record_4_carries : carryRid specK inOrder 4 = true := rfl
    theorem out_of_order_misreads_it : carryRid specK outOfOrder 4 = false := rfl

    -- ===========================================================================
    -- 9. The answer sheet. One byte per register of a fixed grid of 512: four
    --    ops, the first a Put of value A, each other a Put of value A, B or C or
    --    a Delete; eight causal shapes (a chain, a split and merge, a fan, a
    --    branch beside a chain, two roots merged, a chain that splits, no order,
    --    a root beside a chain); and one of four numberings of the record ids,
    --    turning with the kinds, so the winner order meets each shape from every
    --    side. Carries, reseals beside edits, Delete carries and conflicts all
    --    arise. The Rust test `convergence::tests::answer_sheet_matches_the_island`
    --    recomputes it from the Rust Register and reads the words below from this
    --    file. Until TG12 checks the Rust folds' kernels in every build, this
    --    sheet is what holds the folds to the island; TG12 removes it.
    -- ===========================================================================

    -- Shape sh's causal order on positions; every shape lists a position after
    -- its causal past.
    def shapeOrd (sh a b : Nat) : Bool :=
      decide (a < b) && (
        if sh = 0 then true
        else if sh = 1 then !(decide (a = 1) && decide (b = 2))
        else if sh = 2 then decide (a = 0)
        else if sh = 3 then !(decide (b = 3) && (decide (a = 1) || decide (a = 2)))
        else if sh = 4 then !(decide (a = 0) && decide (b = 1))
        else if sh = 5 then !(decide (a = 2) && decide (b = 3))
        else if sh = 6 then false
        else !((decide (a = 0) || decide (a = 1)) && decide (b = 2)))

    -- Numbering q of the record ids (each its own inverse): the identity, the
    -- reverse, pairs swapped, halves swapped.
    def ridP (q i : Nat) : Nat :=
      if q = 0 then i
      else if q = 1 then 3 - i
      else if q = 2 then (if i % 2 = 0 then i + 1 else i - 1)
      else (i + 2) % 4

    -- Op i's kind in view k: op 0 a Put of value A (0); ops 1 to 3 by bits 0-5:
    -- a Put of A (0), of B (1), a Delete (2), a Put of C (3).
    def kindOf (k i : Nat) : Nat :=
      if i = 1 then k % 4 else if i = 2 then (k / 4) % 4 else if i = 3 then (k / 16) % 4 else 0

    -- View k's numbering: it turns with the kinds and the shape.
    def gridQ (k : Nat) : Nat := (k + k / 64) % 4

    -- View k: bits 0-5 the kinds, bits 6-8 the shape.
    def gridReg (k : Nat) : Reg :=
      Reg.mk 4 (fun _ => true) (fun i => ridP (gridQ k) i) (fun i => kindOf k i)
        (fun i => decide (kindOf k i = 2))
        (fun a b => shapeOrd ((k / 64) % 8) (ridP (gridQ k) a) (ridP (gridQ k) b))

    def bitV (b : Bool) (v : Nat) : Nat := if b = true then v else 0

    def winCode (o : Option Nat) : Nat :=
      match o with
      | none => 0
      | some r => r + 1

    -- Bits 0-2 the winner (0 none, else its record id + 1), bit 3 the conflict,
    -- bits 4-6 whether ops 1, 2 and 3 are carries, bit 7 whether op 0 is a
    -- sibling.
    def regByte (k : Nat) : Nat :=
      winCode (winnerRid specK (gridReg k)) + bitV (conflict specK (gridReg k)) 8
        + bitV (carryRid specK (gridReg k) (ridP (gridQ k) 1)) 16
        + bitV (carryRid specK (gridReg k) (ridP (gridQ k) 2)) 32
        + bitV (carryRid specK (gridReg k) (ridP (gridQ k) 3)) 64
        + bitV (sibRid specK (gridReg k) (ridP (gridQ k) 0)) 128

    -- Eight views to a word (Clean's numerals fit 64 bits), view 8w's byte first.
    def regWord (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 256 * regWord w n + regByte (8 * w + n)

    def regSheetFrom (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => regWord w 8 :: regSheetFrom (w + 1) n

    -- Views 0 to 511, as 64 words.
    def regSheet : List Nat := regSheetFrom 0 64

    theorem reg_sheet_is : regSheet =
      [0x7441434214210302, 0x1401230214010322, 0x3401030254614342, 0x1401230214010322,
       0x3401030214210302, 0x5441634214010322, 0x3401030214210302, 0x1401230254414362,
       0x7123222411030204, 0x1103020411030204, 0x3163222451434a04, 0x114b0204114b0204,
       0x3123622411030204, 0x5103420411030204, 0x312322641103024c, 0x1103024c51034a44,
       0x7164626354414a4a, 0x5449424a54494a42, 0x33222a2913010a09, 0x1b090a091b090a09,
       0x332a22291b090a09, 0x130902091b090a09, 0x332a2a211b090a09, 0x1b090a0913090a01,
       0x7144424351634244, 0x5143624451434264, 0x320b090b11230103, 0x190b290b190b092c,
       0x320b090b192b090b, 0x11032103190b092c, 0x320b090b192b090b, 0x190b290b11030124,
       0x6441436a04210302, 0x0401030204010302, 0x2401032244e94342, 0x0401030204010302,
       0x2401032204210302, 0x4441434204010302, 0x2401032204210302, 0x0401030244414342,
       0x710301035223090b, 0x520b220b520b0924, 0x3144090b11630103, 0x1944220b19440924,
       0x310b410b1923410b, 0x11036103190b4124, 0x310b094319230943, 0x190b224311030163,
       0x8189898989898989, 0x8989898989898989, 0x8989898989898989, 0x8989898989898989,
       0x8989898989898989, 0x8989898989898989, 0x8989898989898989, 0x8989898989898989,
       0x5204490312040103, 0x1204010312040103, 0x124c010352444903, 0x124c0103124c0103,
       0x1204010312040103, 0x1204410312040103, 0x1204014b1204014b, 0x1204014b52044943] := rfl

    end AtermMeshConvergence
}

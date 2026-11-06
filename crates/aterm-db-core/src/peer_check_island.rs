// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P4, the fail-closed handshake, proved over the kernels in lib.rs.
clean {
    namespace AtermDbCore
    namespace PeerCheck

    def start (c : UInt8) : UInt8 :=
      if c = 0 then 5 else if c = 1 then 1 else if c = 2 then 1 else 6

    def next (c s o : UInt8) (w : Bool) : UInt8 :=
      if 2 < c then 6 else if 6 < s then 6 else if 4 < o then 6 else
      if o = 4 then 6 else
      if s = 1 then (if o = 0 then 6 else if o = 2 then 6 else if c = 2 then 2 else if c = 1 then 3 else 6)
      else if s = 2 then (if c = 2 then (if o = 3 then 3 else 6) else 6)
      else if s = 3 then (if o = 3 then (if w then 4 else 5) else if c = 1 then 5 else if c = 2 then (if o = 1 then 5 else 6) else 6)
      else if s = 4 then (if o = 3 then 5 else 6)
      else 6

    def roster (c r d : UInt8) : UInt8 :=
      if 2 < c then 0 else if 2 < r then 0 else if 2 < d then 0 else
      if c = 0 then 0 else
      if r = 0 then (if d = 0 then (if c = 1 then 1 else 0) else 0)
      else if d = 0 then 0
      else if r = 1 then (if d = 1 then 1 else 2)
      else 2

    def view (c o : UInt8) : UInt8 :=
      if 2 < c then 0 else if 4 < o then 0 else
      if c = 0 then 1 else
      if o = 3 then 2 else if c = 1 then 1 else 0

    def digit (n : UInt8) : Char := Char.ofNat (48 + n.toNat)
    def over (n : Nat) (f : UInt8 -> List Char) : List Char :=
      (List.range n).foldr (fun k acc => f (UInt8.ofNat k) ++ acc) []

    def sheetStart : String := String.mk (over 3 (fun c => [digit (start c)]))
    def sheetNext : String := String.mk (over 3 (fun c => over 7 (fun s => over 5 (fun o =>
      [digit (next c s o false), digit (next c s o true)]))))
    def sheetRoster : String := String.mk (over 3 (fun c => over 3 (fun r => over 3 (fun d => [digit (roster c r d)]))))
    def sheetView : String := String.mk (over 3 (fun c => over 5 (fun o => [digit (view c o)])))

    theorem sheet_start_answers : sheetStart = "511" := by decide
    theorem sheet_next_answers : sheetNext = "666666666666666666666666666666666666546666666655666666666666666666666666666666666633663366666666666655555554666666665566666666666666666666666666666666662266226666666633666655665466666666556666666666666666666666" := by decide
    theorem sheet_roster_answers : sheetRoster = "000000000100012022000012022" := by decide
    theorem sheet_view_answers : sheetView = "111111112100020" := by decide

    -- The temporal kernel, restated in this namespace (as the one-mesh proof template has it):
    -- a behavior is an infinite run of states, and a spec is satisfied when every run of it is.
    def Behavior (S : Type) := Nat -> S
    def Formula (S : Type) := Behavior S -> Prop
    def drop {S : Type} (b : Behavior S) (n : Nat) : Behavior S := fun k => b (n + k)
    def Always {S : Type} (F : Formula S) : Formula S := fun b => forall n, F (drop b n)
    structure StateMachine (S : Type) where
      init : S -> Prop
      next : S -> S -> Prop
    def Runs {S : Type} (M : StateMachine S) (b : Behavior S) : Prop :=
      And (M.init (b 0)) (forall n, Or (M.next (b n) (b (Nat.succ n))) (b (Nat.succ n) = b n))
    def Lift {S : Type} (P : S -> Prop) : Formula S := fun b => P (b 0)
    def Satisfies {S : Type} (M : StateMachine S) (F : Formula S) : Prop := forall b, Runs M b -> F b

    -- An invariant that the start establishes and every step keeps holds at every point of every run.
    theorem invariantAt {S : Type} (M : StateMachine S) (I : S -> Prop)
        (hInit : forall s, M.init s -> I s) (hStep : forall s t, I s -> M.next s t -> I t)
        (b : Behavior S) (runs : Runs M b) (n : Nat) : I (b n) :=
      @Nat.rec (fun k => I (b k)) (hInit (b 0) runs.1)
        (fun k ih => Or.elim (runs.2 k) (fun st => hStep (b k) (b (Nat.succ k)) ih st)
          (fun stut => Eq.mpr (congrArg I stut) ih)) n

    -- The same, stated as a temporal property of the spec: it always holds.
    theorem invariantSatisfies {S : Type} (M : StateMachine S) (I : S -> Prop)
        (hInit : forall s, M.init s -> I s) (hStep : forall s t, I s -> M.next s t -> I t) :
        Satisfies M (Always (Lift I)) :=
      fun b runs n => invariantAt M I hInit hStep b runs n

    -- Whatever always holds of a spec, every weaker state property always holds too.
    theorem always_weaken {S : Type} (M : StateMachine S) (I J : S -> Prop)
        (hij : forall s, I s -> J s) (h : Satisfies M (Always (Lift I))) :
        Satisfies M (Always (Lift J)) :=
      fun b runs n => hij (b n) (h b runs n)

    -- Case reasoning over one decision: a property true on both branches holds of the if.
    theorem ite_cases {A : Type} (P : A -> Prop) (p : Prop) [Decidable p] (a b : A)
        (ha : p -> P a) (hb : Not p -> P b) : P (if p then a else b) :=
      if h : p then Eq.mpr (congrArg P (if_pos h)) (ha h) else Eq.mpr (congrArg P (if_neg h)) (hb h)

    -- The same without the branch condition.
    theorem ite_all {A : Type} (P : A -> Prop) (p : Prop) [Decidable p] (a b : A)
        (ha : P a) (hb : P b) : P (if p then a else b) :=
      ite_cases P p a b (fun _ => ha) (fun _ => hb)

    -- The other side of a disjunction whose first side fails.
    theorem or_right {a b : Prop} (h : Or a b) (na : Not a) : b := Or.elim h (fun x => absurd x na) (fun x => x)

    -- 1. GUARDS. A custody, stage or outcome code outside its domain refuses the peer.
    theorem next_refuses_out_of_range (c s o : UInt8) (w : Bool)
        (h : Or (2 < c) (Or (6 < s) (4 < o))) : next c s o w = 6 :=
      if hc : 2 < c then if_pos hc
      else if hs : 6 < s then Eq.trans (if_neg hc) (if_pos hs)
      else Eq.trans (if_neg hc) (Eq.trans (if_neg hs) (if_pos (or_right (or_right h hc) hs)))

    -- An unknown custody code starts no check: it refuses.
    theorem start_refuses_out_of_range (c : UInt8) (h : 2 < c) : start c = 6 :=
      if h0 : c = 0 then absurd (Eq.mp (congrArg (fun (x : UInt8) => (2:UInt8) < x) h0) h) (by decide)
      else if h1 : c = 1 then absurd (Eq.mp (congrArg (fun (x : UInt8) => (2:UInt8) < x) h1) h) (by decide)
      else if h2 : c = 2 then absurd (Eq.mp (congrArg (fun (x : UInt8) => (2:UInt8) < x) h2) h) (by decide)
      else Eq.trans (if_neg h0) (Eq.trans (if_neg h1) (if_neg h2))

    -- A roster gate fed an unknown code refuses.
    theorem roster_refuses_out_of_range (c r d : UInt8)
        (h : Or (2 < c) (Or (2 < r) (2 < d))) : roster c r d = 0 :=
      if hc : 2 < c then if_pos hc
      else if hr : 2 < r then Eq.trans (if_neg hc) (if_pos hr)
      else Eq.trans (if_neg hc) (Eq.trans (if_neg hr) (if_pos (or_right (or_right h hc) hr)))

    -- A session view fed an unknown code refuses.
    theorem view_refuses_out_of_range (c o : UInt8) (h : Or (2 < c) (4 < o)) : view c o = 0 :=
      if hc : 2 < c then if_pos hc
      else Eq.trans (if_neg hc) (if_pos (or_right h hc))

    -- The step kernel answers only stage codes, for every input (its Rust ensures clause).
    theorem next_codes (c s o : UInt8) (w : Bool) : next c s o w <= 6 := by
      unfold next
      repeat (first | decide | refine ite_all (fun v => LE.le v (6:UInt8)) _ _ _ ?_ ?_)

    -- The start kernel answers only stage codes, for every input.
    theorem start_codes (c : UInt8) : start c <= 6 := by
      unfold start
      repeat (first | decide | refine ite_all (fun v => LE.le v (6:UInt8)) _ _ _ ?_ ?_)

    -- The roster gate answers only its three codes, for every input.
    theorem roster_codes (c r d : UInt8) : roster c r d <= 2 := by
      unfold roster
      repeat (first | decide | refine ite_all (fun v => LE.le v (2:UInt8)) _ _ _ ?_ ?_)

    -- The session view answers only its three codes, for every input.
    theorem view_codes (c o : UInt8) : view c o <= 2 := by
      unfold view
      repeat (first | decide | refine ite_all (fun v => LE.le v (2:UInt8)) _ _ _ ?_ ?_)
    -- Literal facts the case proofs below cite.
    theorem ne_6_3 : Not ((6:UInt8) = 3) := by decide
    theorem ne_6_4 : Not ((6:UInt8) = 4) := by decide
    theorem ne_6_5 : Not ((6:UInt8) = 5) := by decide
    theorem ne_5_4 : Not ((5:UInt8) = 4) := by decide
    theorem ne_4_5 : Not ((4:UInt8) = 5) := by decide
    theorem ne_0_1 : Not ((0:UInt8) = 1) := by decide
    theorem ne_2_1 : Not ((2:UInt8) = 1) := by decide
    theorem ne_2_0 : Not ((2:UInt8) = 0) := by decide
    theorem ne_1_0 : Not ((1:UInt8) = 0) := by decide

    -- The Log rows of the step kernel, read off its body.
    theorem row_roster_log (o : UInt8) (w : Bool) :
        next 2 1 o w = (if 4 < o then 6 else if o = 4 then 6 else if o = 0 then 6 else if o = 2 then 6 else 2) := rfl
    theorem row_held_log (o : UInt8) (w : Bool) :
        next 2 2 o w = (if 4 < o then 6 else if o = 4 then 6 else if o = 3 then 3 else 6) := rfl
    theorem row_ack_log (o : UInt8) (w : Bool) :
        next 2 3 o w = (if 4 < o then 6 else if o = 4 then 6 else if o = 3 then (if w then 4 else 5) else if o = 1 then 5 else 6) := rfl
    theorem row_comb_log (o : UInt8) (w : Bool) :
        next 2 4 o w = (if 4 < o then 6 else if o = 4 then 6 else if o = 3 then 5 else 6) := rfl
    theorem row_idle (c o : UInt8) (w : Bool) :
        next c 0 o w = (if 2 < c then 6 else if 4 < o then 6 else if o = 4 then 6 else 6) := rfl
    theorem row_pass (c o : UInt8) (w : Bool) :
        next c 5 o w = (if 2 < c then 6 else if 4 < o then 6 else if o = 4 then 6 else 6) := rfl
    theorem row_refuse (c o : UInt8) (w : Bool) :
        next c 6 o w = (if 2 < c then 6 else if 4 < o then 6 else if o = 4 then 6 else 6) := rfl

    -- 2. KERNEL PREMISES: the cells the session theorems need of any kernel, and no more.
    structure Premises (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) : Prop where
      start_log : S 2 = 1
      roster_log : forall o w, Or (K 2 1 o w = 2) (K 2 1 o w = 6)
      held_refuses : forall w, And (K 2 2 0 w = 6) (K 2 2 4 w = 6)
      held_log : forall o w, And (Or (K 2 2 o w = 3) (K 2 2 o w = 6)) (K 2 2 o w = 3 -> o = 3)
      ack_log : forall o w, And (Or (K 2 3 o w = 4) (Or (K 2 3 o w = 5) (K 2 3 o w = 6)))
        (Not (K 2 3 o w = 6) -> Or (o = 3) (o = 1))
      ack_combines : forall o w, K 2 3 o w = 4 -> o = 3
      ack_alone : forall o, Not (K 2 3 o false = 4)
      ack_witness : forall o, K 2 3 o true = 5 -> o = 1
      comb_log : forall o w, And (Or (K 2 4 o w = 5) (K 2 4 o w = 6)) (K 2 4 o w = 5 -> o = 3)
      idle : forall c o w, K c 0 o w = 6
      pass : forall c o w, K c 5 o w = 6
      refuse : forall c o w, K c 6 o w = 6

    -- The shapes of the Log rows the premises ask for.
    def RosterOk (v : UInt8) : Prop := Or (v = 2) (v = 6)
    def HeldOk (o v : UInt8) : Prop := And (Or (v = 3) (v = 6)) (v = 3 -> o = 3)
    def AckOk (o v : UInt8) : Prop := And (Or (v = 4) (Or (v = 5) (v = 6))) (Not (v = 6) -> Or (o = 3) (o = 1))
    def CombinesClear (o v : UInt8) : Prop := v = 4 -> o = 3
    def WitnessPass (o v : UInt8) : Prop := v = 5 -> o = 1
    def CombOk (o v : UInt8) : Prop := And (Or (v = 5) (v = 6)) (v = 5 -> o = 3)
    def IsSix (v : UInt8) : Prop := v = 6
    def NotFour (v : UInt8) : Prop := Not (v = 4)
    def NotFive (v : UInt8) : Prop := Not (v = 5)

    -- At a log endpoint the roster read moves on only to the held view, or refuses.
    theorem next_roster_log (o : UInt8) (w : Bool) : RosterOk (next 2 1 o w) :=
      Eq.mpr (congrArg RosterOk (row_roster_log o w))
        (ite_all RosterOk _ _ _ (Or.inr rfl) (ite_all RosterOk _ _ _ (Or.inr rfl)
          (ite_all RosterOk _ _ _ (Or.inr rfl) (ite_all RosterOk _ _ _ (Or.inr rfl) (Or.inl rfl)))))

    -- At a log endpoint the held view moves on only when it read clear, and refuses otherwise.
    theorem next_held_log (o : UInt8) (w : Bool) : HeldOk o (next 2 2 o w) :=
      Eq.mpr (congrArg (HeldOk o) (row_held_log o w))
        (ite_cases (HeldOk o) _ _ _
          (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_3))
          (fun _ => ite_cases (HeldOk o) _ _ _
            (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_3))
            (fun _ => ite_cases (HeldOk o) _ _ _
              (fun h3 => And.intro (Or.inl rfl) (fun _ => h3))
              (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_3)))))

    -- At a log endpoint the acknowledged view moves on only when it read clear, or read and held
    -- no Commons judgement to make (absent: the records held already cleared the peer).
    theorem next_ack_log (o : UInt8) (w : Bool) : AckOk o (next 2 3 o w) :=
      Eq.mpr (congrArg (AckOk o) (row_ack_log o w))
        (ite_cases (AckOk o) _ _ _
          (fun _ => And.intro (Or.inr (Or.inr rfl)) (fun h => absurd rfl h))
          (fun _ => ite_cases (AckOk o) _ _ _
            (fun _ => And.intro (Or.inr (Or.inr rfl)) (fun h => absurd rfl h))
            (fun _ => ite_cases (AckOk o) _ _ _
              (fun h3 => ite_all (AckOk o) _ _ _
                (And.intro (Or.inl rfl) (fun _ => Or.inl h3))
                (And.intro (Or.inr (Or.inl rfl)) (fun _ => Or.inl h3)))
              (fun _ => ite_cases (AckOk o) _ _ _
                (fun h1 => And.intro (Or.inr (Or.inl rfl)) (fun _ => Or.inr h1))
                (fun _ => And.intro (Or.inr (Or.inr rfl)) (fun h => absurd rfl h))))))

    -- The acknowledged view goes on to the combined one only when it read clear.
    theorem next_ack_combines (o : UInt8) (w : Bool) : CombinesClear o (next 2 3 o w) :=
      Eq.mpr (congrArg (CombinesClear o) (row_ack_log o w))
        (ite_cases (CombinesClear o) _ _ _
          (fun _ e => absurd e ne_6_4)
          (fun _ => ite_cases (CombinesClear o) _ _ _
            (fun _ e => absurd e ne_6_4)
            (fun _ => ite_cases (CombinesClear o) _ _ _
              (fun h3 _ => h3)
              (fun _ => ite_all (CombinesClear o) _ _ _ (fun e => absurd e ne_5_4) (fun e => absurd e ne_6_4)))))

    -- Without a log of the peer to combine, the acknowledged view never goes to the combined one.
    theorem next_ack_alone (o : UInt8) : NotFour (next 2 3 o false) :=
      Eq.mpr (congrArg NotFour (row_ack_log o false))
        (ite_all NotFour _ _ _ ne_6_4 (ite_all NotFour _ _ _ ne_6_4
          (ite_all NotFour _ _ _ ne_5_4 (ite_all NotFour _ _ _ ne_5_4 ne_6_4))))

    -- With a log of the peer to combine, the acknowledged view passes the peer by itself only
    -- when it held no Commons judgement to make: there is no acknowledged log to combine with.
    theorem next_ack_witness (o : UInt8) : WitnessPass o (next 2 3 o true) :=
      Eq.mpr (congrArg (WitnessPass o) (row_ack_log o true))
        (ite_all (WitnessPass o) _ _ _ (fun e => absurd e ne_6_5)
          (ite_all (WitnessPass o) _ _ _ (fun e => absurd e ne_6_5)
            (ite_all (WitnessPass o) _ _ _ (fun e => absurd e ne_4_5)
              (ite_cases (WitnessPass o) _ _ _ (fun h1 _ => h1) (fun _ e => absurd e ne_6_5)))))

    -- At a log endpoint the combined view passes only when it read clear.
    theorem next_comb_log (o : UInt8) (w : Bool) : CombOk o (next 2 4 o w) :=
      Eq.mpr (congrArg (CombOk o) (row_comb_log o w))
        (ite_cases (CombOk o) _ _ _
          (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_5))
          (fun _ => ite_cases (CombOk o) _ _ _
            (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_5))
            (fun _ => ite_cases (CombOk o) _ _ _
              (fun h3 => And.intro (Or.inl rfl) (fun _ => h3))
              (fun _ => And.intro (Or.inr rfl) (fun e => absurd e ne_6_5)))))

    -- The three six-only rows (between checks, after a pass, after a refusal) at any custody.
    theorem six_row (c o : UInt8) : IsSix (if 2 < c then 6 else if 4 < o then 6 else if o = 4 then 6 else 6) :=
      ite_all IsSix _ _ _ rfl (ite_all IsSix _ _ _ rfl (ite_all IsSix _ _ _ rfl rfl))

    -- A read between checks, after a pass or after a refusal moves nothing on, at any custody.
    theorem next_idle (c o : UInt8) (w : Bool) : next c 0 o w = 6 := Eq.trans (row_idle c o w) (six_row c o)
    theorem next_pass (c o : UInt8) (w : Bool) : next c 5 o w = 6 := Eq.trans (row_pass c o w) (six_row c o)
    theorem next_refuse (c o : UInt8) (w : Bool) : next c 6 o w = 6 := Eq.trans (row_refuse c o w) (six_row c o)

    -- The shipped kernels meet every premise.
    theorem kernel_premises : Premises next start :=
      Premises.mk rfl next_roster_log (fun _ => And.intro rfl rfl) next_held_log next_ack_log
        next_ack_combines next_ack_alone next_ack_witness next_comb_log next_idle next_pass next_refuse

    -- A journal endpoint whose acknowledged log does not read leaves the peer to its journal.
    theorem journal_recovers_pin (w : Bool) : next 1 3 0 w = 5 := rfl

    -- A journal endpoint never passes a peer its acknowledged log takes out.
    theorem journal_out_refuses (w : Bool) : next 1 3 4 w = 6 := rfl

    -- 3. THE ROSTER GATE AND THE SESSION VIEW.
    def NothingOk (c r d v : UInt8) : Prop :=
      v = 1 -> Or (And (r = 1) (d = 1)) (And (c = 1) (And (r = 0) (d = 0)))

    -- Every leaf of the roster gate that answers nothing to check, with the reads it needs.
    theorem roster_nothing_ok (c r d : UInt8) : NothingOk c r d (roster c r d) :=
      ite_all (NothingOk c r d) _ _ _ (fun e => absurd e ne_0_1)
        (ite_all (NothingOk c r d) _ _ _ (fun e => absurd e ne_0_1)
        (ite_all (NothingOk c r d) _ _ _ (fun e => absurd e ne_0_1)
        (ite_all (NothingOk c r d) _ _ _ (fun e => absurd e ne_0_1)
        (ite_cases (NothingOk c r d) _ _ _
          (fun hr0 => ite_cases (NothingOk c r d) _ _ _
            (fun hd0 => ite_cases (NothingOk c r d) _ _ _
              (fun hc1 _ => Or.inr (And.intro hc1 (And.intro hr0 hd0)))
              (fun _ e => absurd e ne_0_1))
            (fun _ e => absurd e ne_0_1))
          (fun _ => ite_all (NothingOk c r d) _ _ _ (fun e => absurd e ne_0_1)
            (ite_cases (NothingOk c r d) _ _ _
              (fun hr1 => ite_cases (NothingOk c r d) _ _ _
                (fun hd1 _ => Or.inl (And.intro hr1 hd1))
                (fun _ e => absurd e ne_2_1))
              (fun _ e => absurd e ne_2_1)))))))

    -- Nothing to check means no roster settled and an empty deny list, or a journal that read neither.
    theorem roster_unread_never_nothing (c r d : UInt8) (h : roster c r d = 1) :
        Or (And (r = 1) (d = 1)) (And (c = 1) (And (r = 0) (d = 0))) :=
      roster_nothing_ok c r d h

    -- The log rows of the roster gate with the roster, or the deny list, unread.
    theorem row_roster_log_r0 (d : UInt8) :
        roster 2 0 d = (if 2 < d then 0 else if d = 0 then 0 else 0) := rfl
    theorem row_roster_log_d0 (r : UInt8) :
        roster 2 r 0 = (if 2 < r then 0 else if r = 0 then 0 else 0) := rfl
    def IsZero (v : UInt8) : Prop := v = 0

    -- A log endpoint whose roster or deny list did not read refuses the peer.
    theorem log_roster_unread_refuses (r d : UInt8) (h : Or (r = 0) (d = 0)) : roster 2 r d = 0 :=
      Or.elim h
        (fun hr => Eq.mpr (congrArg (fun x => roster 2 x d = 0) hr)
          (Eq.trans (row_roster_log_r0 d) (ite_all IsZero _ _ _ rfl (ite_all IsZero _ _ _ rfl rfl))))
        (fun hd => Eq.mpr (congrArg (fun x => roster 2 r x = 0) hd)
          (Eq.trans (row_roster_log_d0 r) (ite_all IsZero _ _ _ rfl (ite_all IsZero _ _ _ rfl rfl))))

    -- The log and journal rows of the session view, and the shapes its laws ask for.
    theorem row_view_log (o : UInt8) : view 2 o = (if 4 < o then 0 else if o = 3 then 2 else 0) := rfl
    theorem row_view_journal (o : UInt8) : view 1 o = (if 4 < o then 0 else if o = 3 then 2 else 1) := rfl
    def OwnOnly (o v : UInt8) : Prop := Not (v = 0) -> o = 3
    def NotZero (v : UInt8) : Prop := Not (v = 0)

    -- A log endpoint judges a push only in its own records that read clear; anything else refuses.
    theorem view_log_judges_only_its_own_records (o : UInt8) (h : Not (view 2 o = 0)) : o = 3 :=
      (Eq.mpr (congrArg (OwnOnly o) (row_view_log o))
        (ite_all (OwnOnly o) _ _ _ (fun n => absurd rfl n)
          (ite_cases (OwnOnly o) _ _ _ (fun h3 _ => h3) (fun _ n => absurd rfl n)))) h

    -- An outcome code at most Out is not above it.
    theorem le_four_not_above (o : UInt8) (h : o <= 4) : Not (4 < o) := fun hlt => by
      have a := UInt8.lt_iff_toNat_lt.mp hlt
      have b := UInt8.le_iff_toNat_le.mp h
      omega

    -- A journal endpoint never refuses a session for its view: it judges from its own genesis.
    theorem view_journal_never_refuses (o : UInt8) (h : o <= 4) : Not (view 1 o = 0) :=
      (Eq.mpr (congrArg NotZero (row_view_journal o))
        (ite_cases NotZero _ _ _
          (fun hlt => absurd hlt (le_four_not_above o h))
          (fun _ => ite_all NotZero _ _ _ ne_2_0 ne_1_0)))
    -- 4. THE SESSION MODEL: n concurrent checks against one peer at one endpoint.
    -- custody is that of the endpoint, fixed. outAll, outAck, outComb and rosterOut are the verdict of
    -- the evaluator now on each view: every record held, the acknowledged log, that log plus the
    -- evidence of the peer, and the roster with its deny list. Each session i has a stage, a witness bit and the
    -- ghosts of what the held, acknowledged and combined reads of its current check classified to.
    -- grantsWhileOut counts grants since outAll last rose; atFlip is the number of checks that were
    -- in flight (Ack, Combined or Pass) at that moment.
    structure World where
      custody : UInt8
      outAll : Bool
      outAck : Bool
      outComb : Bool
      rosterOut : Bool
      stage : Nat -> UInt8
      witness : Nat -> Bool
      sawHeld : Nat -> UInt8
      sawAck : Nat -> UInt8
      sawComb : Nat -> UInt8
      grants : Nat
      grantsWhileOut : Nat
      atFlip : Nat

    -- change: any record or roster change, either way. read i o: session i reads the view of its
    -- stage, the environment choosing the outcome. grant i: a grant spends the pass of session i.
    -- begin i b: the next check of session i, b saying whether the evidence of the peer is a log.
    inductive Event where
      | change : Bool -> Bool -> Bool -> Bool -> Event
      | read : Nat -> UInt8 -> Event
      | grant : Nat -> Event
      | begin : Nat -> Bool -> Event

    -- The counting kit: an update of one session, the in-flight flag, and the in-flight count of n sessions.
    def upd {A : Type} (f : Nat -> A) (i : Nat) (v : A) : Nat -> A := fun j => if j = i then v else f j
    def fl (s : UInt8) : Nat := if s = 3 then 1 else if s = 4 then 1 else if s = 5 then 1 else 0
    def cnt (n : Nat) (f : Nat -> UInt8) : Nat := @Nat.rec (fun _ => Nat) 0 (fun m acc => acc + fl (f m)) n

    -- An update reads back what it wrote, and leaves every other session alone.
    theorem upd_same {A : Type} (f : Nat -> A) (i : Nat) (v : A) : upd f i v i = v := if_pos rfl
    theorem upd_other {A : Type} (f : Nat -> A) (i j : Nat) (v : A) (h : Not (j = i)) : upd f i v j = f j := if_neg h

    def AtMostOne (x : Nat) : Prop := x <= 1
    -- A session is in flight or not: it adds one at most to the count.
    theorem fl_le_one (s : UInt8) : fl s <= 1 :=
      ite_all AtMostOne _ _ _ (Nat.le_refl 1) (ite_all AtMostOne _ _ _ (Nat.le_refl 1)
        (ite_all AtMostOne _ _ _ (Nat.le_refl 1) (Nat.zero_le 1)))

    -- No more checks are in flight than there are sessions.
    theorem cnt_le (f : Nat -> UInt8) (n : Nat) : cnt n f <= n := by
      induction n with
      | zero => exact Nat.le_refl 0
      | succ m ih =>
        have h1 := fl_le_one (f m)
        show cnt m f + fl (f m) <= m + 1
        omega

    -- Small order and arithmetic facts the counting proofs cite.
    theorem nat_ne_of_lt (k i : Nat) (h : k < i) : Not (k = i) :=
      fun e => Nat.lt_irrefl i (Eq.mp (congrArg (fun x => x < i) e) h)
    theorem ne_symm {A : Type} {a b : A} (h : Not (a = b)) : Not (b = a) := fun e => h e.symm
    theorem arith_swap2 (C D B : Nat) : C + D + B = C + B + D :=
      Eq.trans (Nat.add_assoc C D B) (Eq.trans (congrArg (fun x => C + x) (Nat.add_comm D B)) (Nat.add_assoc C B D).symm)
    theorem arith_swap (A B C D E : Nat) (h : A + B = C + D) : A + E + B = C + E + D :=
      Eq.trans (arith_swap2 A E B) (Eq.trans (congrArg (fun x => x + E) h) (arith_swap2 C D E))
    theorem arith_not_lt_zero (i : Nat) (h : i < 0) : False := by
      omega
    theorem arith_lt_succ (i k : Nat) (h : i < k + 1) : i <= k := by
      omega

    -- A session the count does not reach is not counted, whatever its stage.
    theorem cnt_upd_out (f : Nat -> UInt8) (i : Nat) (v : UInt8) (m : Nat) : m <= i -> cnt m (upd f i v) = cnt m f := by
      induction m with
      | zero => intro _; rfl
      | succ k ih =>
        intro h
        have h1 : k <= i := Nat.le_of_lt h
        have h2 : Not (k = i) := nat_ne_of_lt k i h
        show cnt k (upd f i v) + fl (upd f i v k) = cnt k f + fl (f k)
        rw [ih h1, upd_other f i k v h2]

    -- Changing the stage of one session changes the in-flight count by exactly the difference.
    theorem cnt_upd (f : Nat -> UInt8) (i : Nat) (v : UInt8) (m : Nat) :
        i < m -> cnt m (upd f i v) + fl (f i) = cnt m f + fl v := by
      induction m with
      | zero =>
        intro h
        exact absurd h (arith_not_lt_zero i)
      | succ k ih =>
        intro h
        show cnt k (upd f i v) + fl (upd f i v k) + fl (f i) = cnt k f + fl (f k) + fl v
        cases Nat.lt_or_eq_of_le i k (arith_lt_succ i k h) with
        | inl hik =>
          rw [upd_other f i k v (ne_symm (nat_ne_of_lt i k hik))]
          exact arith_swap _ _ _ _ _ (ih hik)
        | inr hik =>
          rw [<- hik, upd_same, cnt_upd_out f i v i (Nat.le_refl i)]
          exact arith_swap2 _ _ _

    -- With every session between checks, none is in flight.
    theorem cnt_zero (f : Nat -> UInt8) (h : forall i, f i = 0) (n : Nat) : cnt n f = 0 := by
      induction n with
      | zero => rfl
      | succ k ih =>
        show cnt k f + fl (f k) = 0
        rw [ih, h k]
        rfl

    -- A Bool that is not false is true.
    theorem bool_true_of_ne_false (b : Bool) (h : Not (b = false)) : b = true := by
      cases b with
      | false => exact absurd rfl h
      | true => rfl

    -- The verdict of the evaluator on the view a stage reads.
    def truthAt (w : World) (s : UInt8) : Bool :=
      if s = 1 then w.rosterOut else if s = 2 then w.outAll else if s = 3 then w.outAck else w.outComb

    -- Fail-closed classification: a view whose verdict takes the peer out reads Out, or Unread when
    -- the read failed; otherwise the outcome the environment chose stands.
    def eff (t : Bool) (o : UInt8) : UInt8 := if t = true then (if o = 0 then 0 else 4) else o

    -- A ghost records the outcome only at the stage it belongs to.
    def gh (s k o old : UInt8) : UInt8 := if s = k then o else old

    -- A read moves session i by the kernel and records the outcome in the ghost of its stage.
    def readW (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (w : World) (i : Nat) (o : UInt8) : World :=
      World.mk w.custody w.outAll w.outAck w.outComb w.rosterOut
        (upd w.stage i (K w.custody (w.stage i) (eff (truthAt w (w.stage i)) o) (w.witness i)))
        w.witness
        (upd w.sawHeld i (gh (w.stage i) 2 (eff (truthAt w (w.stage i)) o) (w.sawHeld i)))
        (upd w.sawAck i (gh (w.stage i) 3 (eff (truthAt w (w.stage i)) o) (w.sawAck i)))
        (upd w.sawComb i (gh (w.stage i) 4 (eff (truthAt w (w.stage i)) o) (w.sawComb i)))
        w.grants w.grantsWhileOut w.atFlip

    -- A grant spends the pass of session i and counts itself, and counts after a removal too.
    def grantW (w : World) (i : Nat) : World :=
      World.mk w.custody w.outAll w.outAck w.outComb w.rosterOut
        (upd w.stage i 0) w.witness w.sawHeld w.sawAck w.sawComb
        (w.grants + 1) (if w.outAll = true then w.grantsWhileOut + 1 else w.grantsWhileOut) w.atFlip

    -- A begin starts the next check of session i at the start the custody names, ghosts cleared.
    def beginW (S : UInt8 -> UInt8) (w : World) (i : Nat) (b : Bool) : World :=
      World.mk w.custody w.outAll w.outAck w.outComb w.rosterOut
        (upd w.stage i (S w.custody)) (upd w.witness i b) (upd w.sawHeld i 0) (upd w.sawAck i 0)
        (upd w.sawComb i 0) w.grants w.grantsWhileOut w.atFlip

    -- A change sets the verdicts; when every record held newly removes the peer, it restarts the
    -- count of grants and notes how many checks are in flight.
    def changeW (n : Nat) (w : World) (a k c r : Bool) : World :=
      World.mk w.custody a k c r w.stage w.witness w.sawHeld w.sawAck w.sawComb w.grants
        (if And (a = true) (w.outAll = false) then 0 else w.grantsWhileOut)
        (if And (a = true) (w.outAll = false) then cnt n w.stage else w.atFlip)

    -- One event. A read moves only a session at stages 1 to 4, a grant only one at Pass, a begin
    -- only one between checks or refused; an event naming a session beyond n stutters.
    def step (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) : Event -> World
      | Event.change a k c r => changeW n w a k c r
      | Event.read i o =>
          if And (i < n) (Or (w.stage i = 1) (Or (w.stage i = 2) (Or (w.stage i = 3) (w.stage i = 4))))
          then readW K w i o else w
      | Event.grant i => if And (i < n) (w.stage i = 5) then grantW w i else w
      | Event.begin i b => if And (i < n) (Or (w.stage i = 0) (w.stage i = 6)) then beginW S w i b else w

    -- Every session between checks, with nothing read and nothing granted; the verdicts are free.
    def Init (w : World) : Prop :=
      And (forall i, w.stage i = 0) (And (forall i, w.sawHeld i = 0) (And (forall i, w.sawAck i = 0)
        (And (forall i, w.sawComb i = 0) (And (w.grants = 0) (And (w.grantsWhileOut = 0) (w.atFlip = 0))))))

    -- The spec: from the start, any event at any time.
    def Spec (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) : StateMachine World :=
      StateMachine.mk Init (fun w w2 => Exists (fun e => w2 = step K S n w e))

    -- The cases of the step, as equations.
    theorem step_read_on (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat) (o : UInt8)
        (hg : And (i < n) (Or (w.stage i = 1) (Or (w.stage i = 2) (Or (w.stage i = 3) (w.stage i = 4))))) :
        step K S n w (Event.read i o) = readW K w i o := if_pos hg
    theorem step_read_off (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat) (o : UInt8)
        (hg : Not (And (i < n) (Or (w.stage i = 1) (Or (w.stage i = 2) (Or (w.stage i = 3) (w.stage i = 4)))))) :
        step K S n w (Event.read i o) = w := if_neg hg
    theorem step_grant_on (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat)
        (hg : And (i < n) (w.stage i = 5)) : step K S n w (Event.grant i) = grantW w i := if_pos hg
    theorem step_grant_off (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat)
        (hg : Not (And (i < n) (w.stage i = 5))) : step K S n w (Event.grant i) = w := if_neg hg
    theorem step_begin_on (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat) (b : Bool)
        (hg : And (i < n) (Or (w.stage i = 0) (w.stage i = 6))) : step K S n w (Event.begin i b) = beginW S w i b := if_pos hg
    theorem step_begin_off (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) (i : Nat) (b : Bool)
        (hg : Not (And (i < n) (Or (w.stage i = 0) (w.stage i = 6)))) : step K S n w (Event.begin i b) = w := if_neg hg

    -- An invariant every event keeps holds of every step of the spec.
    theorem spec_step_keeps (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (I : World -> Prop)
        (h : forall w e, I w -> I (step K S n w e)) : forall s t, I s -> (Spec K S n).next s t -> I t :=
      fun s t hi hn => Exists.elim hn (fun e he => Eq.mpr (congrArg I he) (h s e hi))
    -- More literal facts.
    theorem ne_0_3 : Not ((0:UInt8) = 3) := by decide
    theorem ne_0_4 : Not ((0:UInt8) = 4) := by decide
    theorem ne_0_5 : Not ((0:UInt8) = 5) := by decide
    theorem ne_1_3 : Not ((1:UInt8) = 3) := by decide
    theorem ne_1_4 : Not ((1:UInt8) = 4) := by decide
    theorem ne_1_5 : Not ((1:UInt8) = 5) := by decide
    theorem ne_2_3 : Not ((2:UInt8) = 3) := by decide
    theorem ne_2_4 : Not ((2:UInt8) = 4) := by decide
    theorem ne_2_5 : Not ((2:UInt8) = 5) := by decide
    theorem ne_3_4 : Not ((3:UInt8) = 4) := by decide
    theorem ne_3_5 : Not ((3:UInt8) = 5) := by decide
    theorem ne_4_6 : Not ((4:UInt8) = 6) := by decide
    theorem ne_5_6 : Not ((5:UInt8) = 6) := by decide

    -- 5a. NO FAIL-OPEN (S1). What the check of one session must have read to stand where it stands.
    def Good (s : UInt8) (wit : Bool) (h a c : UInt8) : Prop :=
      And (Or (s = 3) (Or (s = 4) (s = 5)) -> h = 3)
        (And (s = 4 -> a = 3)
          (And (s = 5 -> Or (a = 3) (a = 1)) (s = 5 -> wit = true -> a = 3 -> c = 3)))
    def GoodAt (w : World) (i : Nat) : Prop := Good (w.stage i) (w.witness i) (w.sawHeld i) (w.sawAck i) (w.sawComb i)

    -- At a log endpoint, a session at Ack or beyond read every record held clear; at Combined it
    -- also read the acknowledged log clear, and at Pass read it clear or found it held no Commons
    -- judgement to make; and a pass that combined a log of the peer read the combination clear.
    def InvNoFailOpen (n : Nat) (w : World) : Prop :=
      w.custody = 2 -> forall i, i < n ->
        And (Or (w.stage i = 3) (Or (w.stage i = 4) (w.stage i = 5)) -> w.sawHeld i = 3)
          (And (w.stage i = 4 -> w.sawAck i = 3)
            (And (w.stage i = 5 -> Or (w.sawAck i = 3) (w.sawAck i = 1))
              (w.stage i = 5 -> w.witness i = true -> w.sawAck i = 3 -> w.sawComb i = 3)))

    -- A session not yet at Ack owes nothing.
    theorem good_quiet (v x : UInt8) (wit : Bool) (h a c : UInt8) (e : v = x)
        (n3 : Not (x = 3)) (n4 : Not (x = 4)) (n5 : Not (x = 5)) : Good v wit h a c :=
      And.intro
        (fun hs => absurd hs (fun hs2 => Or.elim hs2 (fun e3 => n3 (Eq.trans e.symm e3))
          (fun hs3 => Or.elim hs3 (fun e4 => n4 (Eq.trans e.symm e4)) (fun e5 => n5 (Eq.trans e.symm e5)))))
        (And.intro (fun e4 => absurd (Eq.trans e.symm e4) n4)
          (And.intro (fun e5 => absurd (Eq.trans e.symm e5) n5) (fun e5 => absurd (Eq.trans e.symm e5) n5)))

    -- A session at Ack owes a clear held read.
    theorem good_val3 (v : UInt8) (wit : Bool) (h a c : UInt8) (e : v = 3) (hh : h = 3) : Good v wit h a c :=
      And.intro (fun _ => hh)
        (And.intro (fun e4 => absurd (Eq.trans e.symm e4) ne_3_4)
          (And.intro (fun e5 => absurd (Eq.trans e.symm e5) ne_3_5) (fun e5 => absurd (Eq.trans e.symm e5) ne_3_5)))

    -- A session at Combined owes clear held and acknowledged reads.
    theorem good_val4 (v : UInt8) (wit : Bool) (h a c : UInt8) (e : v = 4) (hh : h = 3) (ha : a = 3) : Good v wit h a c :=
      And.intro (fun _ => hh) (And.intro (fun _ => ha)
        (And.intro (fun e5 => absurd (Eq.trans e.symm e5) ne_4_5) (fun e5 => absurd (Eq.trans e.symm e5) ne_4_5)))

    -- A session at Pass owes a clear held read, an acknowledged one clear or absent, and a clear
    -- combined one where it combined a log of the peer.
    theorem good_val5 (v : UInt8) (wit : Bool) (h a c : UInt8) (e : v = 5) (hh : h = 3) (ha : Or (a = 3) (a = 1))
        (hc : wit = true -> a = 3 -> c = 3) : Good v wit h a c :=
      And.intro (fun _ => hh) (And.intro (fun e4 => absurd (Eq.trans e.symm e4) ne_5_4) (And.intro (fun _ => ha) (fun _ => hc)))

    -- A roster read at a log endpoint never reaches Ack.
    theorem good_read_1 (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (o : UInt8) (wit : Bool) (h a c : UInt8) :
        Good (K 2 1 o wit) wit (gh 1 2 o h) (gh 1 3 o a) (gh 1 4 o c) :=
      Or.elim (hp.roster_log o wit)
        (fun e => good_quiet (K 2 1 o wit) 2 wit (gh 1 2 o h) (gh 1 3 o a) (gh 1 4 o c) e ne_2_3 ne_2_4 ne_2_5)
        (fun e => good_quiet (K 2 1 o wit) 6 wit (gh 1 2 o h) (gh 1 3 o a) (gh 1 4 o c) e ne_6_3 ne_6_4 ne_6_5)

    -- A held read reaches Ack only clear, and the ghost records it.
    theorem good_read_2 (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (o : UInt8) (wit : Bool) (h a c : UInt8) :
        Good (K 2 2 o wit) wit (gh 2 2 o h) (gh 2 3 o a) (gh 2 4 o c) :=
      Or.elim (hp.held_log o wit).1
        (fun e => good_val3 (K 2 2 o wit) wit (gh 2 2 o h) (gh 2 3 o a) (gh 2 4 o c) e ((hp.held_log o wit).2 e))
        (fun e => good_quiet (K 2 2 o wit) 6 wit (gh 2 2 o h) (gh 2 3 o a) (gh 2 4 o c) e ne_6_3 ne_6_4 ne_6_5)

    -- An acknowledged read moves on only clear or absent, to Combined only clear, and with a
    -- witness passes by itself only absent.
    theorem good_read_3 (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (o : UInt8) (wit : Bool) (h a c : UInt8) (hh : h = 3) :
        Good (K 2 3 o wit) wit (gh 3 2 o h) (gh 3 3 o a) (gh 3 4 o c) :=
      Or.elim (hp.ack_log o wit).1
        (fun e => good_val4 (K 2 3 o wit) wit (gh 3 2 o h) (gh 3 3 o a) (gh 3 4 o c) e hh
          (hp.ack_combines o wit e))
        (fun r => Or.elim r
          (fun e => good_val5 (K 2 3 o wit) wit (gh 3 2 o h) (gh 3 3 o a) (gh 3 4 o c) e hh
            ((hp.ack_log o wit).2 (fun e6 => ne_5_6 (Eq.trans e.symm e6)))
            (fun hw ha3 => absurd
              (Eq.trans (hp.ack_witness o (Eq.mp (congrArg (fun x => K 2 3 o x = 5) hw) e)).symm ha3) ne_1_3))
          (fun e => good_quiet (K 2 3 o wit) 6 wit (gh 3 2 o h) (gh 3 3 o a) (gh 3 4 o c) e ne_6_3 ne_6_4 ne_6_5))

    -- A combined read passes only clear, and the ghost records it.
    theorem good_read_4 (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (o : UInt8) (wit : Bool) (h a c : UInt8) (hh : h = 3) (ha : a = 3) :
        Good (K 2 4 o wit) wit (gh 4 2 o h) (gh 4 3 o a) (gh 4 4 o c) :=
      Or.elim (hp.comb_log o wit).1
        (fun e => good_val5 (K 2 4 o wit) wit (gh 4 2 o h) (gh 4 3 o a) (gh 4 4 o c) e hh (Or.inl ha)
          (fun _ _ => (hp.comb_log o wit).2 e))
        (fun e => good_quiet (K 2 4 o wit) 6 wit (gh 4 2 o h) (gh 4 3 o a) (gh 4 4 o c) e ne_6_3 ne_6_4 ne_6_5)

    def ReadKeeps (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (cu o : UInt8) (wit : Bool) (h a c x : UInt8) : Prop :=
      Good x wit h a c -> Good (K cu x o wit) wit (gh x 2 o h) (gh x 3 o a) (gh x 4 o c)

    -- Any read at a log endpoint keeps what the session owes.
    theorem good_read (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (cu s o : UInt8) (wit : Bool) (h a c : UInt8) (hc : cu = 2)
        (hs : Or (s = 1) (Or (s = 2) (Or (s = 3) (s = 4)))) : ReadKeeps K cu o wit h a c s :=
      Eq.mpr (congrArg (fun y => ReadKeeps K y o wit h a c s) hc)
        (Or.elim hs
          (fun e => Eq.mpr (congrArg (ReadKeeps K 2 o wit h a c) e) (fun _ => good_read_1 K S hp o wit h a c))
          (fun hs2 => Or.elim hs2
            (fun e => Eq.mpr (congrArg (ReadKeeps K 2 o wit h a c) e) (fun _ => good_read_2 K S hp o wit h a c))
            (fun hs3 => Or.elim hs3
              (fun e => Eq.mpr (congrArg (ReadKeeps K 2 o wit h a c) e)
                (fun g => good_read_3 K S hp o wit h a c (g.1 (Or.inl rfl))))
              (fun e => Eq.mpr (congrArg (ReadKeeps K 2 o wit h a c) e)
                (fun g => good_read_4 K S hp o wit h a c (g.1 (Or.inr (Or.inl rfl))) (g.2.1 rfl))))))

    -- An update of session i reads back its new values at i.
    theorem good_upd_here (s : Nat -> UInt8) (wt : Nat -> Bool) (hh aa cc : Nat -> UInt8) (i : Nat)
        (vs vh va vc : UInt8) (g : Good vs (wt i) vh va vc) :
        Good (upd s i vs i) (wt i) (upd hh i vh i) (upd aa i va i) (upd cc i vc i) := by
      rw [upd_same s i vs, upd_same hh i vh, upd_same aa i va, upd_same cc i vc]
      exact g

    -- An update of session i leaves what any other session owes.
    theorem good_upd_away (s : Nat -> UInt8) (wt : Nat -> Bool) (hh aa cc : Nat -> UInt8) (i j : Nat)
        (vs vh va vc : UInt8) (hji : Not (j = i)) (g : Good (s j) (wt j) (hh j) (aa j) (cc j)) :
        Good (upd s i vs j) (wt j) (upd hh i vh j) (upd aa i va j) (upd cc i vc j) := by
      rw [upd_other s i j vs hji, upd_other hh i j vh hji, upd_other aa i j va hji, upd_other cc i j vc hji]
      exact g

    -- The same, when the witness is updated too.
    theorem good_upd_away5 (s : Nat -> UInt8) (wt : Nat -> Bool) (hh aa cc : Nat -> UInt8) (i j : Nat)
        (vs vh va vc : UInt8) (b : Bool) (hji : Not (j = i)) (g : Good (s j) (wt j) (hh j) (aa j) (cc j)) :
        Good (upd s i vs j) (upd wt i b j) (upd hh i vh j) (upd aa i va j) (upd cc i vc j) := by
      rw [upd_other s i j vs hji, upd_other wt i j b hji, upd_other hh i j vh hji, upd_other aa i j va hji,
        upd_other cc i j vc hji]
      exact g

    -- A read keeps what every session owes: the reader by the premises, the others untouched.
    theorem inv_read (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) (w : World) (i : Nat) (o : UInt8) (hi : InvNoFailOpen n w) :
        InvNoFailOpen n (step K S n w (Event.read i o)) :=
      if hg : And (i < n) (Or (w.stage i = 1) (Or (w.stage i = 2) (Or (w.stage i = 3) (w.stage i = 4)))) then
        Eq.mpr (congrArg (InvNoFailOpen n) (step_read_on K S n w i o hg))
          (fun hc j hj =>
            if hji : j = i then
              Eq.mpr (congrArg (GoodAt (readW K w i o)) hji)
                (good_upd_here w.stage w.witness w.sawHeld w.sawAck w.sawComb i _ _ _ _
                  (good_read K S hp w.custody (w.stage i) (eff (truthAt w (w.stage i)) o) (w.witness i)
                    (w.sawHeld i) (w.sawAck i) (w.sawComb i) hc hg.2 (hi hc i hg.1)))
            else
              good_upd_away w.stage w.witness w.sawHeld w.sawAck w.sawComb i j _ _ _ _ hji (hi hc j hj))
      else Eq.mpr (congrArg (InvNoFailOpen n) (step_read_off K S n w i o hg)) hi

    -- A grant sends its session back between checks, where it owes nothing.
    theorem inv_grant (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (i : Nat) (hi : InvNoFailOpen n w) :
        InvNoFailOpen n (step K S n w (Event.grant i)) :=
      if hg : And (i < n) (w.stage i = 5) then
        Eq.mpr (congrArg (InvNoFailOpen n) (step_grant_on K S n w i hg))
          (fun hc j hj =>
            if hji : j = i then
              Eq.mpr (congrArg (GoodAt (grantW w i)) hji)
                (good_quiet (upd w.stage i 0 i) 0 (w.witness i) (w.sawHeld i) (w.sawAck i) (w.sawComb i)
                  (upd_same w.stage i 0) ne_0_3 ne_0_4 ne_0_5)
            else
              Eq.mpr (congrArg (fun (x : UInt8) => Good x (w.witness j) (w.sawHeld j) (w.sawAck j) (w.sawComb j))
                (upd_other w.stage i j (0 : UInt8) hji)) (hi hc j hj))
      else Eq.mpr (congrArg (InvNoFailOpen n) (step_grant_off K S n w i hg)) hi

    -- A new check at a log endpoint starts at the roster, owing nothing yet.
    theorem inv_begin (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) (w : World) (i : Nat) (b : Bool) (hi : InvNoFailOpen n w) :
        InvNoFailOpen n (step K S n w (Event.begin i b)) :=
      if hg : And (i < n) (Or (w.stage i = 0) (w.stage i = 6)) then
        Eq.mpr (congrArg (InvNoFailOpen n) (step_begin_on K S n w i b hg))
          (fun hc j hj =>
            if hji : j = i then
              Eq.mpr (congrArg (GoodAt (beginW S w i b)) hji)
                (good_quiet (upd w.stage i (S w.custody) i) 1 (upd w.witness i b i) (upd w.sawHeld i 0 i)
                  (upd w.sawAck i 0 i) (upd w.sawComb i 0 i)
                  (Eq.trans (upd_same w.stage i (S w.custody)) (Eq.trans (congrArg S hc) hp.start_log))
                  ne_1_3 ne_1_4 ne_1_5)
            else
              good_upd_away5 w.stage w.witness w.sawHeld w.sawAck w.sawComb i j _ 0 0 0 b hji (hi hc j hj))
      else Eq.mpr (congrArg (InvNoFailOpen n) (step_begin_off K S n w i b hg)) hi

    -- A record or roster change moves no session.
    theorem inv_change (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (a k c r : Bool) (hi : InvNoFailOpen n w) :
        InvNoFailOpen n (step K S n w (Event.change a k c r)) :=
      fun hc j hj => hi hc j hj

    -- Every event keeps the no-fail-open invariant.
    theorem step_keeps_good (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) : forall w e, InvNoFailOpen n w -> InvNoFailOpen n (step K S n w e) := by
      intro w e hi
      cases e with
      | change a k c r => exact inv_change K S n w a k c r hi
      | read i o => exact inv_read K S hp n w i o hi
      | grant i => exact inv_grant K S n w i hi
      | begin i b => exact inv_begin K S hp n w i b hi

    -- At the start no session owes anything.
    theorem init_good (n : Nat) (w : World) (h : Init w) : InvNoFailOpen n w :=
      fun _ j _ => good_quiet (w.stage j) 0 (w.witness j) (w.sawHeld j) (w.sawAck j) (w.sawComb j) (h.1 j)
        ne_0_3 ne_0_4 ne_0_5

    -- A log endpoint passes a peer only after every view it read read, was its own and cleared the
    -- peer: any kernel meeting the premises, any number of sessions, runs of any length.
    theorem no_fail_open (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) : Satisfies (Spec K S n) (Always (Lift (InvNoFailOpen n))) :=
      invariantSatisfies (Spec K S n) (InvNoFailOpen n) (fun s h => init_good n s h)
        (spec_step_keeps K S n (InvNoFailOpen n) (step_keeps_good K S hp n))

    -- The same, for the shipped kernels.
    theorem kernel_no_fail_open (n : Nat) : Satisfies (Spec next start n) (Always (Lift (InvNoFailOpen n))) :=
      no_fail_open next start kernel_premises n
    -- 5b. A REMOVAL IN THE HELD VIEW REFUSES AT ONCE.
    theorem eff_true (o : UInt8) : eff true o = (if o = 0 then 0 else 4) := rfl
    -- The held stage reads the verdict on every record held.
    theorem truth_at_held (w : World) : truthAt w 2 = w.outAll := rfl

    -- A held read whose verdict takes the peer out refuses, whatever the read reports.
    theorem held_out_refuses (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (o : UInt8) (wit : Bool) : K 2 2 (eff true o) wit = 6 :=
      Eq.mpr (congrArg (fun (x : UInt8) => K 2 2 x wit = 6) (eff_true o))
        (ite_all (fun (x : UInt8) => K 2 2 x wit = 6) (o = 0) 0 4 (hp.held_refuses wit).1 (hp.held_refuses wit).2)

    -- The same, at any custody, stage and verdict equal to log, held and out.
    theorem held_refuses_at (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (c s : UInt8) (t wit : Bool) (o : UInt8) (hc : c = 2) (hs : s = 2) (ht : t = true) : K c s (eff t o) wit = 6 :=
      Eq.mpr (congrArg (fun (x : UInt8) => K x s (eff t o) wit = 6) hc)
        (Eq.mpr (congrArg (fun (x : UInt8) => K 2 x (eff t o) wit = 6) hs)
          (Eq.mpr (congrArg (fun (x : Bool) => K 2 2 (eff x o) wit = 6) ht) (held_out_refuses K S hp o wit)))

    -- A session at the held stage reads the verdict on every record held.
    theorem truth_held_eq (w : World) (s : UInt8) (h2 : s = 2) (ha : w.outAll = true) : truthAt w s = true :=
      Eq.trans (congrArg (truthAt w) h2) (Eq.trans (truth_at_held w) ha)

    -- At a log endpoint whose held records take the peer out, a check at the held view refuses on
    -- its next read, for every outcome the read reports.
    theorem removed_held_refuses_now (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (hp : Premises K S) (n : Nat) (w : World) (i : Nat) (o : UInt8)
        (hc : w.custody = 2) (hs : w.stage i = 2) (ha : w.outAll = true) (hi : i < n) :
        (step K S n w (Event.read i o)).stage i = 6 :=
      Eq.trans (congrArg (fun (x : World) => x.stage i) (step_read_on K S n w i o (And.intro hi (Or.inr (Or.inl hs)))))
        (Eq.trans (upd_same w.stage i _)
          (held_refuses_at K S hp w.custody (w.stage i) (truthAt w (w.stage i)) (w.witness i) o hc hs
            (truth_held_eq w (w.stage i) hs ha)))

    -- 5c. AT MOST ONE GRANT PER CHECK IN FLIGHT (S2).
    def BoundInv (n : Nat) (w : World) : Prop :=
      w.custody = 2 -> w.outAll = true -> w.grantsWhileOut + cnt n w.stage <= w.atFlip
    def Bounded (n : Nat) (w : World) : Prop := And (BoundInv n w) (w.atFlip <= n)

    -- Plain arithmetic the proof of the bound cites.
    theorem bound_core (g c0 c1 a f0 f1 : Nat) (hu : c1 + f0 = c0 + f1) (hk : f1 <= f0) (ho : g + c0 <= a) :
        g + c1 <= a := by
      omega
    theorem grant_core (g c0 c1 a f0 f1 : Nat) (hu : c1 + f0 = c0 + f1) (h1 : f0 = 1) (h0 : f1 = 0)
        (ho : g + c0 <= a) : g + 1 + c1 <= a := by
      omega
    theorem begin_core (g c0 c1 a f0 f1 : Nat) (hu : c1 + f0 = c0 + f1) (h0 : f0 = 0) (h1 : f1 = 0)
        (ho : g + c0 <= a) : g + c1 <= a := by
      omega
    theorem rise_core (g c a : Nat) (hg : g = 0) (ha : a = c) : g + c <= a := by
      omega
    theorem keep_core (g g2 c a a2 : Nat) (e1 : g2 = g) (e2 : a2 = a) (h : g + c <= a) : g2 + c <= a2 := by
      omega
    theorem le_of_eq_le (a a2 n : Nat) (e : a2 = a) (h : a <= n) : a2 <= n :=
      Eq.mpr (congrArg (fun (x : Nat) => x <= n) e) h
    theorem add_le_core (g c a : Nat) (h : g + c <= a) : g <= a := by
      omega
    theorem trans_core (g c a n : Nat) (h1 : g + c <= a) (h2 : a <= n) : g <= n := by
      omega

    -- In-flight flags of the stage codes the proofs meet.
    theorem fl_0 : fl 0 = 0 := rfl
    theorem fl_1 : fl 1 = 0 := rfl
    theorem fl_5 : fl 5 = 1 := rfl
    theorem fl_6 : fl 6 = 0 := rfl
    theorem fl_2_le_1 : fl 2 <= fl 1 := by decide
    theorem fl_6_le_1 : fl 6 <= fl 1 := by decide
    theorem fl_6_le_2 : fl 6 <= fl 2 := by decide
    theorem fl_le_3 (v : UInt8) : fl v <= fl 3 := fl_le_one v
    theorem fl_le_4 (v : UInt8) : fl v <= fl 4 := fl_le_one v

    -- What a read may do to the in-flight flag of its session.
    def FlKeeps (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (t wit : Bool) (o c s : UInt8) : Prop :=
      (s = 2 -> t = true) -> fl (K c s (eff t o) wit) <= fl s

    -- At a log endpoint whose held records take the peer out, no read puts a check in flight.
    theorem read_fl (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (c s : UInt8) (t wit : Bool) (o : UInt8) (hc : c = 2)
        (hs : Or (s = 1) (Or (s = 2) (Or (s = 3) (s = 4)))) : FlKeeps K t wit o c s :=
      Eq.mpr (congrArg (fun (y : UInt8) => FlKeeps K t wit o y s) hc)
        (Or.elim hs
          (fun e => Eq.mpr (congrArg (FlKeeps K t wit o 2) e)
            (fun _ => Or.elim (hp.roster_log (eff t o) wit)
              (fun e2 => Eq.mpr (congrArg (fun (v : UInt8) => fl v <= fl 1) e2) fl_2_le_1)
              (fun e6 => Eq.mpr (congrArg (fun (v : UInt8) => fl v <= fl 1) e6) fl_6_le_1)))
          (fun hs2 => Or.elim hs2
            (fun e => Eq.mpr (congrArg (FlKeeps K t wit o 2) e)
              (fun ht => Eq.mpr (congrArg (fun (x : Bool) => fl (K 2 2 (eff x o) wit) <= fl 2) (ht rfl))
                (Eq.mpr (congrArg (fun (v : UInt8) => fl v <= fl 2) (held_out_refuses K S hp o wit)) fl_6_le_2)))
            (fun hs3 => Or.elim hs3
              (fun e => Eq.mpr (congrArg (FlKeeps K t wit o 2) e) (fun _ => fl_le_3 _))
              (fun e => Eq.mpr (congrArg (FlKeeps K t wit o 2) e) (fun _ => fl_le_4 _)))))

    -- A read puts no new check in flight while the peer is out in every record held.
    theorem bounded_read (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) (w : World) (i : Nat) (o : UInt8) (hb : Bounded n w) :
        Bounded n (step K S n w (Event.read i o)) :=
      if hg : And (i < n) (Or (w.stage i = 1) (Or (w.stage i = 2) (Or (w.stage i = 3) (w.stage i = 4)))) then
        Eq.mpr (congrArg (Bounded n) (step_read_on K S n w i o hg))
          (And.intro
            (fun hc ha => bound_core _ _ _ _ _ _
              (cnt_upd w.stage i (K w.custody (w.stage i) (eff (truthAt w (w.stage i)) o) (w.witness i)) n hg.1)
              (read_fl K S hp w.custody (w.stage i) (truthAt w (w.stage i)) (w.witness i) o hc hg.2
                (fun h2 => truth_held_eq w (w.stage i) h2 ha))
              (hb.1 hc ha))
            hb.2)
      else Eq.mpr (congrArg (Bounded n) (step_read_off K S n w i o hg)) hb

    -- A grant counts one and spends one check in flight.
    theorem bounded_grant (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (i : Nat) (hb : Bounded n w) :
        Bounded n (step K S n w (Event.grant i)) :=
      if hg : And (i < n) (w.stage i = 5) then
        Eq.mpr (congrArg (Bounded n) (step_grant_on K S n w i hg))
          (And.intro
            (fun hc ha => Eq.mpr
              (congrArg (fun (x : Nat) => x + cnt n (upd w.stage i 0) <= w.atFlip)
                (if_pos ha : (if w.outAll = true then w.grantsWhileOut + 1 else w.grantsWhileOut) = w.grantsWhileOut + 1))
              (grant_core _ _ _ _ _ _ (cnt_upd w.stage i 0 n hg.1) (Eq.trans (congrArg fl hg.2) fl_5) fl_0 (hb.1 hc ha)))
            hb.2)
      else Eq.mpr (congrArg (Bounded n) (step_grant_off K S n w i hg)) hb

    -- A begin starts a check at the roster, not in flight.
    theorem bounded_begin (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) (w : World) (i : Nat) (b : Bool) (hb : Bounded n w) :
        Bounded n (step K S n w (Event.begin i b)) :=
      if hg : And (i < n) (Or (w.stage i = 0) (w.stage i = 6)) then
        Eq.mpr (congrArg (Bounded n) (step_begin_on K S n w i b hg))
          (And.intro
            (fun hc ha => begin_core _ _ _ _ _ _ (cnt_upd w.stage i (S w.custody) n hg.1)
              (Or.elim hg.2 (fun e => Eq.trans (congrArg fl e) fl_0) (fun e => Eq.trans (congrArg fl e) fl_6))
              (Eq.trans (congrArg fl (Eq.trans (congrArg S hc) hp.start_log)) fl_1)
              (hb.1 hc ha))
            hb.2)
      else Eq.mpr (congrArg (Bounded n) (step_begin_off K S n w i b hg)) hb

    -- A rising removal restarts the count at the checks in flight; any other change keeps it.
    theorem bounded_change (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (a k c r : Bool) (hb : Bounded n w) :
        Bounded n (step K S n w (Event.change a k c r)) :=
      if hr : And (a = true) (w.outAll = false) then
        And.intro
          (fun _ _ => rise_core _ _ _
            (if_pos hr : (if And (a = true) (w.outAll = false) then 0 else w.grantsWhileOut) = 0)
            (if_pos hr : (if And (a = true) (w.outAll = false) then cnt n w.stage else w.atFlip) = cnt n w.stage))
          (le_of_eq_le _ _ _
            (if_pos hr : (if And (a = true) (w.outAll = false) then cnt n w.stage else w.atFlip) = cnt n w.stage)
            (cnt_le w.stage n))
      else
        And.intro
          (fun hc ha => keep_core _ _ _ _ _
            (if_neg hr : (if And (a = true) (w.outAll = false) then 0 else w.grantsWhileOut) = w.grantsWhileOut)
            (if_neg hr : (if And (a = true) (w.outAll = false) then cnt n w.stage else w.atFlip) = w.atFlip)
            (hb.1 hc (bool_true_of_ne_false w.outAll (fun hf => hr (And.intro ha hf)))))
          (le_of_eq_le _ _ _
            (if_neg hr : (if And (a = true) (w.outAll = false) then cnt n w.stage else w.atFlip) = w.atFlip) hb.2)

    -- Every event keeps the bound.
    theorem step_keeps_bounded (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) : forall w e, Bounded n w -> Bounded n (step K S n w e) := by
      intro w e hb
      cases e with
      | change a k c r => exact bounded_change K S n w a k c r hb
      | read i o => exact bounded_read K S hp n w i o hb
      | grant i => exact bounded_grant K S n w i hb
      | begin i b => exact bounded_begin K S hp n w i b hb

    -- At the start nothing is granted and nothing is in flight.
    theorem init_bounded (n : Nat) (w : World) (h : Init w) : Bounded n w :=
      And.intro
        (fun _ _ => rise_core _ _ _ h.2.2.2.2.2.1 (Eq.trans h.2.2.2.2.2.2 (cnt_zero w.stage h.1 n).symm))
        (le_of_eq_le _ _ _ h.2.2.2.2.2.2 (Nat.zero_le n))

    -- The bound holds at every point of every run.
    theorem bounded_spec (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (hp : Premises K S)
        (n : Nat) : Satisfies (Spec K S n) (Always (Lift (Bounded n))) :=
      invariantSatisfies (Spec K S n) (Bounded n) (fun s h => init_bounded n s h)
        (spec_step_keeps K S n (Bounded n) (step_keeps_bounded K S hp n))

    -- Once every record held removes the peer, every grant that follows spends a check that had
    -- read the held view before the removal, one grant per such check, across any number of sessions.
    theorem at_most_one_grant_per_inflight_check (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (hp : Premises K S) (n : Nat) :
        Satisfies (Spec K S n) (Always (Lift (fun w =>
          w.custody = 2 -> w.outAll = true -> w.grantsWhileOut + cnt n w.stage <= w.atFlip))) :=
      always_weaken (Spec K S n) (Bounded n) _ (fun _ h => h.1) (bounded_spec K S hp n)

    -- The grants after a removal are at most the checks in flight at it, which are at most the sessions.
    theorem grants_after_removal_bounded_by_checks_in_flight (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8)
        (S : UInt8 -> UInt8) (hp : Premises K S) (n : Nat) :
        Satisfies (Spec K S n) (Always (Lift (fun w =>
          w.custody = 2 -> w.outAll = true -> And (w.grantsWhileOut <= w.atFlip) (w.atFlip <= n)))) :=
      always_weaken (Spec K S n) (Bounded n) _
        (fun _ h hc ha => And.intro (add_le_core _ _ _ (h.1 hc ha)) h.2) (bounded_spec K S hp n)

    -- One session gets at most one grant after the removal of the peer.
    theorem one_session_one_grant (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (hp : Premises K S) :
        Satisfies (Spec K S 1) (Always (Lift (fun w => w.custody = 2 -> w.outAll = true -> w.grantsWhileOut <= 1))) :=
      always_weaken (Spec K S 1) (Bounded 1) _
        (fun _ h hc ha => trans_core _ _ _ _ (h.1 hc ha) h.2) (bounded_spec K S hp 1)

    -- The concurrent bound for the shipped kernels.
    theorem kernel_at_most_one_grant_per_inflight_check (n : Nat) :
        Satisfies (Spec next start n) (Always (Lift (fun w =>
          w.custody = 2 -> w.outAll = true -> w.grantsWhileOut + cnt n w.stage <= w.atFlip))) :=
      at_most_one_grant_per_inflight_check next start kernel_premises n

    -- Its corollaries for the shipped kernels.
    theorem kernel_grants_after_removal_bounded (n : Nat) :
        Satisfies (Spec next start n) (Always (Lift (fun w =>
          w.custody = 2 -> w.outAll = true -> And (w.grantsWhileOut <= w.atFlip) (w.atFlip <= n)))) :=
      grants_after_removal_bounded_by_checks_in_flight next start kernel_premises n
    theorem kernel_one_session_one_grant :
        Satisfies (Spec next start 1) (Always (Lift (fun w => w.custody = 2 -> w.outAll = true -> w.grantsWhileOut <= 1))) :=
      one_session_one_grant next start kernel_premises

    -- 5d. A FRESH PASS BEFORE EVERY GRANT (S3).
    theorem nat_ne_succ (k : Nat) : Not (k = k + 1) :=
      fun e => Nat.lt_irrefl k (Eq.mpr (congrArg (fun (x : Nat) => k < x) e) (Nat.lt_succ_self k))

    -- A grant happens only from Pass, and it spends the pass: the session is back between checks.
    theorem grant_spends_a_fresh_pass (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (i : Nat) (h : (step K S n w (Event.grant i)).grants = w.grants + 1) :
        And (w.stage i = 5) ((step K S n w (Event.grant i)).stage i = 0) :=
      if hg : And (i < n) (w.stage i = 5) then
        And.intro hg.2
          (Eq.trans (congrArg (fun (x : World) => x.stage i) (step_grant_on K S n w i hg)) (upd_same w.stage i 0))
      else absurd (Eq.trans (congrArg World.grants (step_grant_off K S n w i hg)).symm h) (nat_ne_succ w.grants)

    -- A grant from any stage but Pass changes nothing.
    theorem grant_elsewhere_changes_nothing (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8)
        (n : Nat) (w : World) (i : Nat) (h : Not (w.stage i = 5)) : step K S n w (Event.grant i) = w :=
      step_grant_off K S n w i (fun hg => h hg.2)
    -- 5e. NON-VACUITY: concrete runs from a start state the spec admits.
    def run (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World)
        (es : List Event) : World :=
      List.foldl (step K S n) w es
    def w0 (c : UInt8) : World :=
      World.mk c false false false false (fun _ => 0) (fun _ => false) (fun _ => 0) (fun _ => 0) (fun _ => 0) 0 0 0

    -- The runs below start where the spec starts.
    theorem w0_init (c : UInt8) : Init (w0 c) :=
      And.intro (fun _ => rfl) (And.intro (fun _ => rfl) (And.intro (fun _ => rfl)
        (And.intro (fun _ => rfl) (And.intro rfl (And.intro rfl rfl)))))

    -- One full check of a clean peer at a log endpoint, then its grant.
    def cleanCheck : List Event :=
      [Event.begin 0 false, Event.read 0 1, Event.read 0 3, Event.read 0 3, Event.grant 0]

    -- A clean peer at a log endpoint passes every check: four checks give four grants.
    theorem clean_peer_reaches_every_grant :
        (run next start 1 (w0 2) (List.append cleanCheck (List.append cleanCheck (List.append cleanCheck cleanCheck)))).grants = 4 :=
      rfl

    -- A clean peer whose evidence is a log passes through the combined view to a grant.
    theorem clean_peer_with_its_log_reaches_a_grant :
        (run next start 1 (w0 2) [Event.begin 0 true, Event.read 0 1, Event.read 0 3, Event.read 0 3,
          Event.read 0 3, Event.grant 0]).grants = 1 :=
      rfl

    -- 5f. A journal endpoint whose acknowledged log does not read leaves the peer to its journal.
    theorem journal_recovers :
        (run next start 1 (w0 1) [Event.begin 0 false, Event.read 0 1, Event.read 0 0]).stage 0 = 5 :=
      rfl

    -- 6. NEGATIVE CONTROLS: each mutant with an rfl counterexample.
    theorem ne_3_2 : Not ((3:UInt8) = 2) := by decide
    theorem ne_3_6 : Not ((3:UInt8) = 6) := by decide
    theorem ne_3_1 : Not ((3:UInt8) = 1) := by decide

    -- The check before X1: the roster row of a log endpoint goes straight to Ack, skipping the held
    -- view, and an acknowledged log that did not read passes the peer.
    def nextPreX1 (c s o : UInt8) (w : Bool) : UInt8 :=
      if 2 < c then 6 else if 6 < s then 6 else if 4 < o then 6 else
      if o = 4 then 6 else
      if s = 1 then (if o = 0 then 6 else if o = 2 then 6 else if c = 2 then 3 else if c = 1 then 3 else 6)
      else if s = 2 then 6
      else if s = 3 then (if o = 3 then (if w then 4 else 5) else if c = 1 then 5 else if o = 0 then 5 else 6)
      else if s = 4 then (if o = 3 then 5 else 6)
      else 6

    -- The removal lands first; then a check reads a clear roster and an unread acknowledged log.
    def preX1Check : World :=
      run nextPreX1 start 1 (w0 2) [Event.change true false false false, Event.begin 0 false, Event.read 0 1, Event.read 0 0]

    -- Before X1, a Mac whose removal every record held already shows reaches a pass at a log endpoint.
    theorem preX1_passes_a_removed_mac : And (preX1Check.stage 0 = 5) (preX1Check.outAll = true) :=
      And.intro rfl rfl

    -- ... and its grant is one no check in flight at the removal paid for.
    theorem preX1_grants_a_removed_mac :
        And ((step nextPreX1 start 1 preX1Check (Event.grant 0)).grantsWhileOut = 1)
          ((step nextPreX1 start 1 preX1Check (Event.grant 0)).atFlip = 0) :=
      And.intro rfl rfl

    -- The cells of that check the premises forbid.
    theorem preX1_breaks_the_premises : And (nextPreX1 2 1 1 false = 3) (nextPreX1 2 3 0 false = 5) :=
      And.intro rfl rfl

    -- So that check meets no premise set the session theorems rest on.
    theorem preX1_fails_the_premises : Not (Premises nextPreX1 start) :=
      fun hp => Or.elim (hp.roster_log 1 false) (fun e => ne_3_2 e) (fun e => ne_3_6 e)

    -- A grant that leaves the stage at Pass: one check for many grants, as atdb/2-3 did.
    def grantLegacyW (w : World) (i : Nat) : World :=
      World.mk w.custody w.outAll w.outAck w.outComb w.rosterOut w.stage w.witness w.sawHeld w.sawAck w.sawComb
        (w.grants + 1) (if w.outAll = true then w.grantsWhileOut + 1 else w.grantsWhileOut) w.atFlip
    def stepLegacy (K : UInt8 -> UInt8 -> UInt8 -> Bool -> UInt8) (S : UInt8 -> UInt8) (n : Nat) (w : World) : Event -> World
      | Event.change a k c r => step K S n w (Event.change a k c r)
      | Event.read i o => step K S n w (Event.read i o)
      | Event.grant i => if And (i < n) (w.stage i = 5) then grantLegacyW w i else w
      | Event.begin i b => step K S n w (Event.begin i b)

    -- One clean check, then the removal, then four grants.
    def legacyRun : World :=
      List.foldl (stepLegacy next start 1) (w0 2)
        [Event.begin 0 false, Event.read 0 1, Event.read 0 3, Event.read 0 3, Event.change true false false false,
         Event.grant 0, Event.grant 0, Event.grant 0, Event.grant 0]

    -- One check, then the removal, then four grants on it: four grants for the one check in flight.
    theorem legacy_grants_after_removal : And (legacyRun.grantsWhileOut = 4) (legacyRun.atFlip = 1) :=
      And.intro rfl rfl

    -- An enrollment-serve check that starts at Ack and never reads the held view.
    def startAckOnly (c : UInt8) : UInt8 := if c = 0 then 5 else if c = 1 then 1 else if c = 2 then 3 else 6

    -- The removal lands with no check in flight; then an Ack-only check and its grant.
    def enrollRun : World :=
      run next startAckOnly 1 (w0 2) [Event.change true false false false, Event.begin 0 false, Event.read 0 3, Event.grant 0]

    -- The removal lands with no check in flight, and the Ack-only check still grants the removed Mac.
    theorem enroll_ack_only_grants_a_removed_mac : And (enrollRun.grantsWhileOut = 1) (enrollRun.atFlip = 0) :=
      And.intro rfl rfl

    -- That start meets no premise set the session theorems rest on.
    theorem enroll_ack_only_fails_the_premises : Not (Premises next startAckOnly) :=
      fun hp => ne_3_1 hp.start_log

    -- The roster gate before PF4: an unread roster with an empty deny list is nothing to check.
    def rosterToday (c r d : UInt8) : UInt8 :=
      if 2 < c then 0 else if 2 < r then 0 else if 2 < d then 0 else
      if c = 0 then 0 else
      if r = 0 then (if d = 0 then (if c = 1 then 1 else 0) else if d = 1 then 1 else 0)
      else if d = 0 then 0
      else if r = 1 then (if d = 1 then 1 else 2)
      else 2

    -- A busy or unread roster with an empty deny list read as no roster at a log endpoint.
    theorem roster_today_reads_busy_as_nothing : rosterToday 2 0 1 = 1 := rfl

    -- ... which is exactly the cell the gate law rules out.
    theorem roster_today_breaks_the_gate_law :
        Not (Or (And ((0:UInt8) = 1) ((1:UInt8) = 1)) (And ((2:UInt8) = 1) (And ((0:UInt8) = 0) ((1:UInt8) = 0)))) := by
      decide

    end PeerCheck
    end AtermDbCore

    -- The names the kernels' clauses in lib.rs cite (`ensures result == peer_check_next_spec(..)`):
    -- a clause reads a bare island identifier, so these four stand outside the namespace.
    def peer_check_start_spec (c : UInt8) : UInt8 := AtermDbCore.PeerCheck.start c
    def peer_check_next_spec (c s o : UInt8) (w : Bool) : UInt8 := AtermDbCore.PeerCheck.next c s o w
    def peer_check_roster_spec (c r d : UInt8) : UInt8 := AtermDbCore.PeerCheck.roster c r d
    def peer_check_view_spec (c o : UInt8) : UInt8 := AtermDbCore.PeerCheck.view c o
}

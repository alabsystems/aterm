// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The Clean island for cross-broker federation (`ROADMAP.md` & `DESIGN-aterm-fabric`
// Federation across brokers), mounted under `cfg(clean_islands)` in `federation.rs`.
// Proves that `federation_cursor_advance` and `federation_seq_clamp` maintain monotone
// source cursor progression and keep mirrored producer sequences strictly below the
// reserved ack-sequence half (`seq <= max_seq`).
clean {
    -- ========================================================================
    -- 1. Clean-Origin Rust Implementation Functions (`clean_origin_mir` I5)
    -- ========================================================================
    def federation_cursor_advance (cur_cursor : UInt64) (next_cursor : UInt64) : UInt64 :=
      if cur_cursor < next_cursor then next_cursor else cur_cursor

    def federation_seq_clamp (src_offset : UInt64) (max_seq : UInt64) : UInt64 :=
      if src_offset < max_seq then src_offset else max_seq

    -- ========================================================================
    -- 2. TLA+ State-Machine Specification (Astream.Broker.TlaFederationBridge)
    -- ========================================================================
    namespace Astream
    namespace Broker
    namespace TlaFederationBridge

    def Behavior (State : Type) := Nat → State
    def Formula (State : Type) := Behavior State → Prop
    def drop {State : Type} (b : Behavior State) (n : Nat) : Behavior State :=
      fun k => b (n + k)
    def Always {State : Type} (F : Formula State) : Formula State :=
      fun b => ∀ n, F (drop b n)

    structure StateMachine (State : Type) where
      init : State → Prop
      next : State → State → Prop

    def Runs {State : Type} (M : StateMachine State) (b : Behavior State) : Prop :=
      M.init (b 0) ∧ ∀ n, M.next (b n) (b (Nat.succ n)) ∨ b (Nat.succ n) = b n

    def Lift {State : Type} (P : State → Prop) : Formula State :=
      fun b => P (b 0)

    def Satisfies {State : Type} (M : StateMachine State) (F : Formula State) : Prop :=
      ∀ b, Runs M b → F b

    notation:50 M " ⊨ " F:50 => _root_.Astream.Broker.TlaFederationBridge.Satisfies M F
    prefix:75 "□" => _root_.Astream.Broker.TlaFederationBridge.Always

    theorem invariantAt {State : Type} (M : StateMachine State) (I : State → Prop)
        (hInit : ∀ s : State, M.init s → I s)
        (hStep : ∀ s sNext : State, I s → M.next s sNext → I sNext)
        (b : Behavior State) (runs : Runs M b) (n : Nat) : I (b n) :=
      Nat.rec
        (motive := fun k => I (b k))
        (hInit (b 0) runs.1)
        (fun k ih =>
          Or.elim (runs.2 k)
            (fun hNext => hStep (b k) (b (Nat.succ k)) ih hNext)
            (fun hStutter => Eq.mpr (congrArg I hStutter) ih))
        n

    theorem invariantSatisfies {State : Type} (M : StateMachine State) (I : State → Prop)
        (hInit : ∀ s : State, M.init s → I s)
        (hStep : ∀ s sNext : State, I s → M.next s sNext → I sNext) :
        M ⊨ □ (Lift I) :=
      fun b runs n => invariantAt M I hInit hStep b runs n

    structure FederationState where
      startCursor : UInt64
      cursor      : UInt64
      seq         : UInt64
      maxSeq      : UInt64

    def FederationInv (s : FederationState) : Prop :=
      Nat.le (UInt64.toNat s.startCursor) (UInt64.toNat s.cursor) ∧
      Nat.le (UInt64.toNat s.seq) (UInt64.toNat s.maxSeq)

    def Init (s : FederationState) : Prop :=
      s.startCursor = s.cursor ∧ s.seq = s.maxSeq

    def mirrorStepAction (candCursor : UInt64) (srcOff : UInt64)
        (s sNext : FederationState) : Prop :=
      sNext.startCursor = s.startCursor ∧
      sNext.maxSeq = s.maxSeq ∧
      sNext.cursor = _root_.federation_cursor_advance s.cursor candCursor ∧
      sNext.seq = _root_.federation_seq_clamp srcOff s.maxSeq

    def Next (s sNext : FederationState) : Prop :=
      ∃ candCursor : UInt64, ∃ srcOff : UInt64,
        mirrorStepAction candCursor srcOff s sNext

    def Spec : StateMachine FederationState :=
      StateMachine.mk Init Next

    theorem cursor_advance_ge_cur : forall (cur : UInt64) (cand : UInt64),
        Nat.le (UInt64.toNat cur) (UInt64.toNat (_root_.federation_cursor_advance cur cand)) :=
      fun cur cand => Decidable.casesOn
        (motive := fun (d : Decidable (LT.lt cur cand)) =>
          Nat.le (UInt64.toNat cur) (UInt64.toNat (Decidable.casesOn
            (motive := fun _ => UInt64) d (fun _ => cur) (fun _ => cand))))
        (instDecidableUInt64Lt cur cand)
        (fun _ => Nat.le.refl (UInt64.toNat cur))
        (fun hLt => Nat.le_of_lt hLt)

    theorem seq_clamp_le_max : forall (src : UInt64) (maxSeq : UInt64),
        Nat.le (UInt64.toNat (_root_.federation_seq_clamp src maxSeq)) (UInt64.toNat maxSeq) :=
      fun src maxSeq => Decidable.casesOn
        (motive := fun (d : Decidable (LT.lt src maxSeq)) =>
          Nat.le (UInt64.toNat (Decidable.casesOn
            (motive := fun _ => UInt64) d (fun _ => maxSeq) (fun _ => src))) (UInt64.toNat maxSeq))
        (instDecidableUInt64Lt src maxSeq)
        (fun _ => Nat.le.refl (UInt64.toNat maxSeq))
        (fun hLt => Nat.le_of_lt hLt)

    theorem uint64_eq_le (a b : UInt64) (hEq : a = b) :
        Nat.le (UInt64.toNat a) (UInt64.toNat b) :=
      Eq.mpr
        (congrArg (fun (x : UInt64) => Nat.le (UInt64.toNat x) (UInt64.toNat b)) hEq)
        (Nat.le.refl (UInt64.toNat b))

    theorem init_establishes_inv (s : FederationState) (hInit : Init s) :
        FederationInv s :=
      And.intro
        (uint64_eq_le s.startCursor s.cursor hInit.1)
        (uint64_eq_le s.seq s.maxSeq hInit.2)

    theorem next_preserves_inv (s sNext : FederationState)
        (hInv : FederationInv s) (hNext : Next s sNext) :
        FederationInv sNext :=
      Exists.elim hNext (fun candCursor h1 =>
        Exists.elim h1 (fun srcOff hAct =>
          let hEqStart : sNext.startCursor = s.startCursor := hAct.1
          let hEqMax : sNext.maxSeq = s.maxSeq := hAct.2.1
          let hEqCur : sNext.cursor = _root_.federation_cursor_advance s.cursor candCursor := hAct.2.2.1
          let hEqSeq : sNext.seq = _root_.federation_seq_clamp srcOff s.maxSeq := hAct.2.2.2
          let hCurGrow : Nat.le (UInt64.toNat s.cursor) (UInt64.toNat (_root_.federation_cursor_advance s.cursor candCursor)) :=
            cursor_advance_ge_cur s.cursor candCursor
          let hStartBound : Nat.le (UInt64.toNat s.startCursor) (UInt64.toNat (_root_.federation_cursor_advance s.cursor candCursor)) :=
            Nat.le_trans hInv.1 hCurGrow
          let hSeqBound : Nat.le (UInt64.toNat (_root_.federation_seq_clamp srcOff s.maxSeq)) (UInt64.toNat s.maxSeq) :=
            seq_clamp_le_max srcOff s.maxSeq
          And.intro
            (Eq.mpr
              (congrArg (fun (st : UInt64) => Nat.le (UInt64.toNat st) (UInt64.toNat sNext.cursor)) hEqStart)
              (Eq.mpr
                (congrArg (fun (c : UInt64) => Nat.le (UInt64.toNat s.startCursor) (UInt64.toNat c)) hEqCur)
                hStartBound))
            (Eq.mpr
              (congrArg (fun (sq : UInt64) => Nat.le (UInt64.toNat sq) (UInt64.toNat sNext.maxSeq)) hEqSeq)
              (Eq.mpr
                (congrArg (fun (mx : UInt64) => Nat.le (UInt64.toNat (_root_.federation_seq_clamp srcOff s.maxSeq)) (UInt64.toNat mx)) hEqMax)
                hSeqBound))))

    theorem spec_satisfies_federation_bridge_safety :
        Spec ⊨ □ (Lift FederationInv) :=
      invariantSatisfies Spec FederationInv init_establishes_inv next_preserves_inv

    end TlaFederationBridge
    end Broker
    end Astream

    def federation_hop_allowed_isl (hops : UInt8) (max_hops : UInt8) : Bool :=
      decide (hops < max_hops)
}

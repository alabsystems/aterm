// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The temporal prelude every one-mesh island shares (TLA style: behaviors, Always, Act, a state
// machine with stuttering, Satisfies, and the invariant and action rules). lib.rs mounts it
// before every module, so each property's island opens `AtermMeshTemporal`.
clean {
    -- The temporal prelude every one-mesh island shares: behaviors, Always, Act,
    -- a state machine (an initial predicate and a next-state relation, with
    -- stuttering), Satisfies, and the two rules that prove an invariant and an
    -- action property of every run.
    namespace AtermMeshTemporal

    def Behavior (S : Type) := Nat → S
    def Formula (S : Type) := Behavior S → Prop
    def drop {S : Type} (b : Behavior S) (n : Nat) : Behavior S := fun k => b (n + k)
    def Always {S : Type} (F : Formula S) : Formula S := fun b => ∀ n, F (drop b n)
    structure StateMachine (S : Type) where
      init : S → Prop
      next : S → S → Prop
    def Runs {S : Type} (M : StateMachine S) (b : Behavior S) : Prop :=
      M.init (b 0) ∧ ∀ n, M.next (b n) (b (Nat.succ n)) ∨ b (Nat.succ n) = b n
    def Lift {S : Type} (P : S → Prop) : Formula S := fun b => P (b 0)
    def Act {S : Type} (A : S → S → Prop) : Formula S := fun b => A (b 0) (b 1)
    def Satisfies {S : Type} (M : StateMachine S) (F : Formula S) : Prop := ∀ b, Runs M b → F b

    theorem invariantAt {S : Type} (M : StateMachine S) (I : S → Prop)
        (hInit : ∀ s, M.init s → I s) (hStep : ∀ s t, I s → M.next s t → I t)
        (b : Behavior S) (runs : Runs M b) (n : Nat) : I (b n) :=
      @Nat.rec (fun k => I (b k)) (hInit (b 0) runs.1)
        (fun k ih => Or.elim (runs.2 k) (fun st => hStep (b k) (b (Nat.succ k)) ih st)
          (fun stut => Eq.mpr (congrArg I stut) ih)) n

    theorem invariantSatisfies {S : Type} (M : StateMachine S) (I : S → Prop)
        (hInit : ∀ s, M.init s → I s) (hStep : ∀ s t, I s → M.next s t → I t) :
        Satisfies M (Always (Lift I)) :=
      fun b runs n => invariantAt M I hInit hStep b runs n

    theorem actionSatisfies {S : Type} (M : StateMachine S) (A : S → S → Prop)
        (hNext : ∀ s t, M.next s t → A s t) (hStut : ∀ s, A s s) :
        Satisfies M (Always (Act A)) :=
      fun b runs n => Or.elim (runs.2 n)
        (fun st => hNext (b n) (b (Nat.succ n)) st)
        (fun stut => Eq.mpr (congrArg (fun x => A (b n) x) stut) (hStut (b n)))

    end AtermMeshTemporal
}

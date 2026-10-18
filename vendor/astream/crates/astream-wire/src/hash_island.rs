// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The Clean island of `hash.rs`, out of line so that only a Trust compiler opens it: no
// stock rustc can lex `clean { ... }`. `hash.rs` mounts this file under
// `cfg(clean_islands)`, which build.rs sets when the compiler answers that it is Trust.
// Every other compiler calls the plain-Rust twins in `twins.rs` of the functions it
// compiles from Clean.
clean {
    -- ========================================================================
    -- 1. TLA+ Temporal Logic & State-Machine Kernel (Astream.Wire.TlaSlicingCrc32)
    -- ========================================================================
    namespace Astream
    namespace Wire
    namespace TlaSlicingCrc32

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

    prefix:100 "□" => _root_.Astream.Wire.TlaSlicingCrc32.Always
    infixl:45 " ⊨ " => _root_.Astream.Wire.TlaSlicingCrc32.Satisfies

    theorem invariantAt {State : Type} (M : StateMachine State) (I : State → Prop)
        (hInit : ∀ s : State, M.init s → I s)
        (hStep : ∀ s sNext : State, I s → M.next s sNext → I sNext)
        (b : Behavior State) (runs : Runs M b) (n : Nat) : I (b n) :=
      @Nat.rec (fun k => I (b k))
        (hInit (b 0) runs.1)
        (fun k ih =>
          Or.elim (runs.2 k)
            (fun step => hStep (b k) (b (Nat.succ k)) ih step)
            (fun stutter => Eq.mpr (congrArg I stutter) ih))
        n

    theorem invariantSatisfies {State : Type} (M : StateMachine State) (I : State → Prop)
        (hInit : ∀ s : State, M.init s → I s)
        (hStep : ∀ s sNext : State, I s → M.next s sNext → I sNext) :
        M ⊨ □ (Lift I) :=
      fun b runs n => invariantAt M I hInit hStep b runs n

    end TlaSlicingCrc32
    end Wire
    end Astream

    -- ========================================================================
    -- 2. Clean-Origin Rust Implementation Function (`clean_origin_mir` I5)
    --    Compiled directly from this Clean kernel definition into Rust MIR
    --    (`pub const fn crc_table_index_clamp`) and verified by
    --    `trust_clean_origin::Faithful` (`clean-kernel-defeq`).
    -- ========================================================================

    -- Clamp a table lookup index `idx` to `max_idx` (`255` for a 256-entry CRC row).
    -- Compiled directly to `pub const fn crc_table_index_clamp(idx: u64, max_idx: u64) -> u64`.
    def crc_table_index_clamp (idx : UInt64) (max_idx : UInt64) : UInt64 :=
      if idx < max_idx then idx else max_idx

    -- ========================================================================
    -- 3. TLA+ Protocol Specification: Slicing-by-8 Streaming CRC-32 Lookup Safety
    --    Directly over `UInt64` table indices, `_root_.crc_table_index_clamp`,
    --    and `crc_table_index_in_bounds_isl`
    -- ========================================================================
    namespace Astream
    namespace Wire
    namespace TlaSlicingCrc32

    -- Island specification of `pub fn crc_table_index_in_bounds(idx: u64, max_idx: u64) -> bool`.
    def crc_table_index_in_bounds_isl (idx : UInt64) (max_idx : UInt64) : Bool :=
      decide (idx <= max_idx)

    theorem crc_table_index_clamp_le_max : forall (idx : UInt64) (max_idx : UInt64),
        Nat.le (UInt64.toNat (_root_.crc_table_index_clamp idx max_idx)) (UInt64.toNat max_idx) :=
      fun idx max_idx => Decidable.casesOn
        (motive := fun (d : Decidable (LT.lt idx max_idx)) =>
          Nat.le (UInt64.toNat (Decidable.casesOn
            (motive := fun _ => UInt64) d (fun _ => max_idx) (fun _ => idx))) (UInt64.toNat max_idx))
        (instDecidableUInt64Lt idx max_idx)
        (fun _ => Nat.le.refl (UInt64.toNat max_idx))
        (fun h => Nat.le_of_lt h)

    -- State of the Slicing-by-8 streaming CRC-32 engine:
    --   lastIdx : most recent table row lookup index (`UInt64`)
    --   maxIdx  : upper bound of a 256-entry CRC table row (`UInt64`, 255)
    structure CrcLookupState where
      lastIdx : UInt64
      maxIdx  : UInt64

    -- TLA+ Safety Invariant:
    -- `lastIdx <= maxIdx`: every Slicing-by-8 table lookup index stays within `[0, maxIdx]`.
    def SlicingCrcSafetyInv (s : CrcLookupState) : Prop :=
      Nat.le (UInt64.toNat s.lastIdx) (UInt64.toNat s.maxIdx)

    def Init (s : CrcLookupState) : Prop :=
      s.lastIdx = 0 ∧
      s.maxIdx = 255

    -- Action 1: LookupByte — performs a bounded table lookup at `_root_.crc_table_index_clamp raw_idx s.maxIdx`.
    def LookupByte (raw_idx : UInt64) (s sNext : CrcLookupState) : Prop :=
      sNext.lastIdx = _root_.crc_table_index_clamp raw_idx s.maxIdx ∧
      sNext.maxIdx = s.maxIdx

    -- Action 2: ResetState — resets `lastIdx` to `0` at the start of a new frame.
    def ResetState (s sNext : CrcLookupState) : Prop :=
      sNext.lastIdx = 0 ∧
      sNext.maxIdx = s.maxIdx

    def Next (s sNext : CrcLookupState) : Prop :=
      (∃ raw_idx : UInt64, LookupByte raw_idx s sNext) ∨ ResetState s sNext

    def Spec : StateMachine CrcLookupState :=
      StateMachine.mk Init Next

    theorem zero_le_uint64 (u : UInt64) (hZero : u = 0) :
        forall (m : UInt64), Nat.le (UInt64.toNat u) (UInt64.toNat m) :=
      fun m =>
        Eq.mpr
          (congrArg (fun x => Nat.le (UInt64.toNat x) (UInt64.toNat m)) hZero)
          (Nat.zero_le (UInt64.toNat m))

    theorem init_implies_inv (s : CrcLookupState) (hInit : Init s) : SlicingCrcSafetyInv s :=
      zero_le_uint64 s.lastIdx hInit.1 s.maxIdx

    theorem lookup_preserves_inv (raw_idx : UInt64) (s sNext : CrcLookupState)
        (_hInv : SlicingCrcSafetyInv s) (hAct : LookupByte raw_idx s sNext) :
        SlicingCrcSafetyInv sNext :=
      let h1 : Nat.le (UInt64.toNat sNext.lastIdx) (UInt64.toNat s.maxIdx) :=
        Eq.mpr (congrArg (fun x => Nat.le (UInt64.toNat x) (UInt64.toNat s.maxIdx)) hAct.1)
          (crc_table_index_clamp_le_max raw_idx s.maxIdx)
      Eq.mpr (congrArg (fun x => Nat.le (UInt64.toNat sNext.lastIdx) (UInt64.toNat x)) hAct.2)
        h1

    theorem reset_preserves_inv (s sNext : CrcLookupState)
        (_hInv : SlicingCrcSafetyInv s) (hAct : ResetState s sNext) :
        SlicingCrcSafetyInv sNext :=
      zero_le_uint64 sNext.lastIdx hAct.1 sNext.maxIdx

    theorem next_preserves_inv (s sNext : CrcLookupState)
        (hInv : SlicingCrcSafetyInv s) (hNext : Next s sNext) :
        SlicingCrcSafetyInv sNext :=
      Or.elim hNext
        (fun hLook => Exists.elim hLook (fun raw hAct => lookup_preserves_inv raw s sNext hInv hAct))
        (fun hRes => reset_preserves_inv s sNext hInv hRes)

    -- ========================================================================
    -- 4. Top-Level Temporal Theorem: `Spec ⊨ □ (Lift SlicingCrcSafetyInv)`
    -- ========================================================================
    theorem spec_satisfies_always_crc_safety : Spec ⊨ □ (Lift SlicingCrcSafetyInv) :=
      invariantSatisfies Spec SlicingCrcSafetyInv init_implies_inv next_preserves_inv

    end TlaSlicingCrc32
    end Wire
    end Astream
}

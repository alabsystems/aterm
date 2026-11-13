// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Merge rule 1, proved over the kernel `file_merge::eligible_owned`
// (docs/DESIGN-shared-files-2026-11-11.md sections 5 and 8). A machine uuid is a Nat here: the
// kernel asks only whether two uuids are equal.
clean {
    namespace AtermDbCore
    namespace FileMerge
    namespace Owned

    -- The kernel: the key names a machine, and the signer resolved to that machine.
    def eligible (key signer : Option Nat) : Bool :=
      match key with
      | none => false
      | some k =>
        match signer with
        | none => false
        | some s => decide (k = s)

    -- The sheet: the key, then the signer, each naming no machine, machine 0, then machine 1.
    def bit (b : Bool) : Char := Char.ofNat (if b then 49 else 48)
    def names : List (Option Nat) := [none, some 0, some 1]
    def sheetEligible : String := String.mk (names.foldr (fun key acc =>
      names.foldr (fun signer inner => bit (eligible key signer) :: inner) acc) [])
    theorem sheet_eligible_answers : sheetEligible = "000010001" := by decide

    -- The decide facts the proofs cite.
    theorem decide_true_of {p : Prop} [d : Decidable p] (h : p) : decide p = true := by
      cases hd : decide p with
      | true => rfl
      | false => exact absurd h (of_decide_eq_false hd)
    theorem decide_false_of {p : Prop} [d : Decidable p] (h : Not p) : decide p = false := by
      cases hd : decide p with
      | false => rfl
      | true => exact absurd (of_decide_eq_true hd) h

    -- 1. THE LAW (own_key_only). An eligible row's key names a machine, and that machine is its
    -- signer's ...
    theorem eligible_names_its_signer (key signer : Option Nat) (h : eligible key signer = true) :
        Exists (fun u => And (key = some u) (signer = some u)) := by
      cases key with
      | none => exact Bool.noConfusion h
      | some k =>
        cases signer with
        | none => exact Bool.noConfusion h
        | some s =>
          have e : k = s := of_decide_eq_true h
          subst e
          exact Exists.intro k (And.intro rfl rfl)

    -- ... and every row whose key names its signer's machine is eligible.
    theorem own_row_is_eligible (u : Nat) : eligible (some u) (some u) = true := by
      show decide (u = u) = true
      exact decide_true_of rfl
    theorem a_row_naming_its_signer_is_eligible (key signer : Option Nat) (u : Nat)
        (hk : key = some u) (hs : signer = some u) : eligible key signer = true := by
      subst hk
      subst hs
      exact own_row_is_eligible u

    -- A mine row is eligible if and only if its key's uuid is its signer's.
    theorem eligible_iff_the_key_names_the_signer (key signer : Option Nat) :
        Iff (eligible key signer = true) (Exists (fun u => And (key = some u) (signer = some u))) :=
      Iff.intro (eligible_names_its_signer key signer)
        (fun h => Exists.elim h (fun u hu => a_row_naming_its_signer_is_eligible key signer u hu.1 hu.2))

    -- 2. WHAT IS INERT. A row at another machine's key ...
    theorem a_foreign_row_is_inert (k s : Nat) (h : Not (k = s)) : eligible (some k) (some s) = false := by
      show decide (k = s) = false
      exact decide_false_of h

    -- ... a key that names no machine, whoever signed ...
    theorem a_key_naming_no_machine_is_inert (signer : Option Nat) : eligible none signer = false := rfl

    -- ... and a signer that resolved to no machine, whatever the key names.
    theorem an_unresolved_signer_is_inert (key : Option Nat) : eligible key none = false := by
      cases key <;> rfl

    -- 3. THE NEGATIVE CONTROL. The predicate as a bare comparison of the two names, as a copy
    -- that forgot both can be absent would be.
    def eligibleBare (key signer : Option Nat) : Bool :=
      match key with
      | none =>
        match signer with
        | none => true
        | some _ => false
      | some k =>
        match signer with
        | none => false
        | some s => decide (k = s)

    -- It projects a row whose key names no machine, signed by an actor no roster resolves ...
    theorem bare_projects_a_row_naming_nobody : eligibleBare none none = true := rfl

    -- ... so it breaks the law: no machine is the one that key names.
    def named (o : Option Nat) : Bool :=
      match o with
      | none => false
      | some _ => true
    theorem ne_false_true : Not (false = true) := by decide
    theorem bare_breaks_the_law :
        Not (forall (key signer : Option Nat), eligibleBare key signer = true ->
          Exists (fun u => And (key = some u) (signer = some u))) :=
      fun h => Exists.elim (h none none rfl) (fun _ hu => ne_false_true (congrArg named hu.1))

    end Owned
    end FileMerge
    end AtermDbCore
}

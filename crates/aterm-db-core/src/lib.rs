// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure membership and resource-admission kernels. Membership keeps authority,
//! audience and writer predicates together. A resource grant is a fixed offline
//! share, not a vendor observation or a lease. Its spent counter never decreases
//! and silence grants no rights. Persist a signed debit before external work.
#![forbid(unsafe_code)]

#[cfg(trust_verify)]
macro_rules! verify_island { ($($body:tt)*) => {$($body)*}; }
#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($body:tt)*) => {};
}

#[cfg(trust_verify)]
macro_rules! contract {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($($arg:ident : $ty:ty),*) -> $ret:ident
     clauses [$($clauses:tt)+] {$($body:tt)*}) => {
        $(#[$meta])* $vis fn $name($($arg:$ty),*) -> $ret $($clauses)+ {$($body)*}
    };
}
#[cfg(not(trust_verify))]
macro_rules! contract {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($($arg:ident : $ty:ty),*) -> $ret:ident
     clauses [$($clauses:tt)+] {$($body:tt)*}) => {
        $(#[$meta])* $vis fn $name($($arg:$ty),*) -> $ret {$($body)*}
    };
}

contract! {
    /// Key release requires a current observation, valid whole-roster policy and
    /// the exact usable epoch. Parsing and authentication supply these predicates.
    pub fn roster_access(current: bool, members: bool, epoch: bool) -> bool
    clauses [ensures result == (current && (members && epoch)) by roster_access_is_exact]
    { current && (members && epoch) }
}
contract! {
    /// The caller verifies the complete custody prefix before supplying its
    /// length. Only a discarded source strictly inside it may be quarantined.
    pub fn quarantine_eligible(acknowledged: u64, offset: u64, discarded: bool) -> bool
    clauses [ensures result == (offset < acknowledged && discarded) by quarantine_eligible_is_exact]
    { offset < acknowledged && discarded }
}
contract! {
    /// Certification requires all independently checked authority, audience and
    /// writer predicates. Possession and current-history checks precede this gate.
    pub fn member_approval(authority: bool, eligible: bool, writer: bool) -> bool
    clauses [ensures result == (authority && (eligible && writer)) by member_approval_is_exact]
    { authority && (eligible && writer) }
}
contract! {
    /// A candidate cutoff has a nonzero sequence in the same writer stream and
    /// never lies beyond the signed frontier. Exact previous links are checked
    /// separately by the history walker.
    pub fn prefix_candidate(last: u64, sequence: u64, same_actor: bool) -> bool
    clauses [ensures result == (sequence > 0 && (sequence <= last && same_actor)) by prefix_candidate_is_exact]
    { sequence > 0 && (sequence <= last && same_actor) }
}
contract! {
    /// A control snapshot is causally ready only with complete ancestry and
    /// exactly one head. This proves no payload schema or membership policy.
    pub fn control_ready(complete: bool, heads: u64) -> bool
    clauses [ensures result == (complete && heads == 1) by control_ready_is_exact]
    { complete && heads == 1 }
}
contract! {
    /// A cumulative receipt is admissible exactly when it extends the previous
    /// counter within its immutable share. No machine's clock participates.
    pub fn admissible(limit: u64, previous: u64, proposed: u64) -> bool
    clauses [ensures result == (previous <= proposed && proposed <= limit) by admissible_is_exact]
    { previous <= proposed && proposed <= limit }
}
contract! {
    /// Merge duplicated or reordered cumulative accounting evidence.
    pub fn merge_spent(left: u64, right: u64) -> u64
    clauses [ensures result >= left && result >= right by merge_covers_both
             ensures result == left || result == right by merge_is_a_receipt]
    { if left < right { right } else { left } }
}

verify_island! {
    clean {
        theorem roster_access_is_exact (current members epoch : Bool) :
            trust_import_aterm_db_core__roster_access current members epoch =
                (current && members && epoch) := by
          simp only [trust_import_aterm_db_core__roster_access]
        theorem member_approval_is_exact (authority eligible writer : Bool) :
            trust_import_aterm_db_core__member_approval authority eligible writer =
                (authority && eligible && writer) := by
          simp only [trust_import_aterm_db_core__member_approval]
        theorem quarantine_eligible_is_exact (acknowledged offset : UInt64) (discarded : Bool) :
            trust_import_aterm_db_core__quarantine_eligible acknowledged offset discarded =
                (decide (offset < acknowledged) && discarded) := by
          simp only [trust_import_aterm_db_core__quarantine_eligible]
        theorem prefix_candidate_is_exact (last sequence : UInt64) (same_actor : Bool) :
            trust_import_aterm_db_core__prefix_candidate last sequence same_actor =
                (decide (sequence > 0) && decide (sequence <= last) && same_actor) := by
          simp only [trust_import_aterm_db_core__prefix_candidate]
        theorem control_ready_is_exact (complete : Bool) (heads : UInt64) :
            trust_import_aterm_db_core__control_ready complete heads =
                (complete && decide (heads = 1)) := by
          simp only [trust_import_aterm_db_core__control_ready]
        theorem admissible_is_exact (limit previous proposed : UInt64) :
            trust_import_aterm_db_core__admissible limit previous proposed =
                (decide (previous <= proposed) && decide (proposed <= limit)) := by
          simp only [trust_import_aterm_db_core__admissible]
        theorem merge_covers_both (left right : UInt64) :
            And (Nat.le (UInt64.toNat left) (UInt64.toNat (trust_import_aterm_db_core__merge_spent left right)))
                (Nat.le (UInt64.toNat right) (UInt64.toNat (trust_import_aterm_db_core__merge_spent left right))) :=
          @Decidable.casesOn (LT.lt left right)
            (fun (d : Decidable (LT.lt left right)) =>
              And (Nat.le (UInt64.toNat left) (UInt64.toNat (@ite UInt64 (LT.lt left right) d right left)))
                  (Nat.le (UInt64.toNat right) (UInt64.toNat (@ite UInt64 (LT.lt left right) d right left))))
            (instDecidableUInt64Lt left right)
            (fun (h : Not (LT.lt left right)) =>
              And.intro (Nat.le_refl (UInt64.toNat left)) (Nat.ge_of_not_lt h))
            (fun (h : LT.lt left right) =>
              And.intro (Nat.le_of_lt h) (Nat.le_refl (UInt64.toNat right)))
        theorem merge_is_a_receipt (left right : UInt64) :
            Or (Eq (trust_import_aterm_db_core__merge_spent left right) left)
               (Eq (trust_import_aterm_db_core__merge_spent left right) right) :=
          @Decidable.casesOn (LT.lt left right)
            (fun (d : Decidable (LT.lt left right)) =>
              Or (Eq (@ite UInt64 (LT.lt left right) d right left) left)
                 (Eq (@ite UInt64 (LT.lt left right) d right left) right))
            (instDecidableUInt64Lt left right)
            (fun (_ : Not (LT.lt left right)) => Or.inl (Eq.refl left))
            (fun (_ : LT.lt left right) => Or.inr (Eq.refl right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quarantine_never_promotes_the_boundary_or_a_live_source() {
        assert!(!quarantine_eligible(0, 0, true));
        assert!(quarantine_eligible(1, 0, true));
        assert!(!quarantine_eligible(1, 1, true));
        assert!(!quarantine_eligible(u64::MAX, u64::MAX, true));
        assert!(quarantine_eligible(u64::MAX, u64::MAX - 1, true));
        assert!(!quarantine_eligible(u64::MAX, 0, false));
    }
    #[test]
    fn full_width_accounting_does_not_wrap_or_reclaim_an_absent_peers_share() {
        assert!(u64::MAX.checked_add(1).is_none());
        assert!(admissible(u64::MAX, 0, u64::MAX));
        assert!(!admissible(10, 6, 5));
        assert!(!admissible(10, 6, 11));
        assert_eq!(merge_spent(7, 3), merge_spent(3, 7));
        assert_eq!(merge_spent(7, 7), 7);
    }
    #[test]
    fn offline_rights_model_has_live_actions_and_rejects_double_allocation() {
        let model = aterm_models_db::offline_rights_model();
        assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
        assert_eq!(
            aterm_spec::interp::fired_actions(&model).len(),
            model.actions.len()
        );
    }
    fn rights_state(a: u64, b: u64, disconnected: i64) -> aterm_spec::interp::State {
        std::collections::BTreeMap::from([
            ("a", a as i64),
            ("b", b as i64),
            ("disconnected", disconnected),
        ])
    }
    #[test]
    fn real_admission_conforms_at_every_reachable_partition_state_and_catches_reclaim() {
        let model = aterm_models_db::offline_rights_model();
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if !model.action_enabled(action.name, &state) {
                    continue;
                }
                let (mut a, mut b, mut d) =
                    (state["a"] as u64, state["b"] as u64, state["disconnected"]);
                match action.name {
                    "SpendA" => {
                        if admissible(2, a, a + 1) {
                            a += 1;
                        }
                    }
                    "SpendB" => {
                        if admissible(2, b, b + 1) {
                            b += 1;
                        }
                    }
                    "Separate" => d = 1,
                    "Reconnect" => d = 0,
                    other => panic!("unknown real transition {other}"),
                }
                let next = rights_state(a, b, d);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
        let full = rights_state(2, 0, 1);
        assert!(!admissible(2, 2, 3));
        let bad = rights_state(if admissible(4, 2, 3) { 3 } else { 2 }, 0, 1);
        assert!(
            !model.successors("SpendA", &full).contains(&bad),
            "negative control: reclaiming the silent peer's share must break conformance"
        );
        assert!(!model.check_invariant("ShareA", &bad));
    }
}

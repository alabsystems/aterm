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

// The fail-closed handshake: the one place a peer check decides (P4,
// `docs/design/one-mesh-proofs/P4-handshake.md`). A check walks the stages its kernel names,
// reading only the view the stage names, and lets a peer further in only from `Stage::Pass`.
// Each kernel's clause says its body is its island definition (`peer_check_island.rs`), which
// proves every law the handshake rests on; `peer_check_binding_island.rs` ties the two together.
// The kernels take and give `u8` codes, the shape the compiler imports today: [`peer_check`]
// names them as `#[repr(u8)]` enums, and a code outside a kernel's domain refuses.

contract! {
    #[expect(
        clippy::if_same_then_else,
        reason = "a kernel the Trust lane imports: one scalar if-chain, cell for cell the island's"
    )]
    /// The stage a peer check at `custody` starts at: an endpoint with no records custody has
    /// only the peer's own evidence and passes it to the checks its grants make; every other
    /// starts at the roster. A code outside the domain refuses.
    pub fn peer_check_start(custody: u8) -> u8
    clauses [ensures result == peer_check_start_spec(custody) by peer_check_start_is_exact]
    { if custody == 0 { 5 } else if custody == 1 { 1 } else if custody == 2 { 1 } else { 6 } }
}
contract! {
    #[expect(
        clippy::if_same_then_else,
        reason = "a kernel the Trust lane imports: one scalar if-chain, cell for cell the island's"
    )]
    /// The stage after a read at `stage` classified to `outcome`, at `custody`, where
    /// `witness` says the peer's evidence is a log to combine with this Mac's.
    ///
    /// Out refuses at every stage. The roster passes nothing unread. A log endpoint judges
    /// every record it holds (`Stage::Held`) before the acknowledged log, and passes only
    /// views that read, are its own and clear the peer, save an acknowledged log with no
    /// Commons judgement to make (absent: no Commons epochs), after the records it holds
    /// cleared the peer. A journal endpoint leaves a peer its acknowledged log cannot judge
    /// to its journal, which is how a pending or seatless device recovers its records. A
    /// code outside the domain refuses.
    pub fn peer_check_next(custody: u8, stage: u8, outcome: u8, witness: bool) -> u8
    clauses [ensures result == peer_check_next_spec(custody, stage, outcome, witness) by peer_check_next_is_exact]
    {
        if 2 < custody { 6 } else if 6 < stage { 6 } else if 4 < outcome { 6 }
        else if outcome == 4 { 6 }
        else if stage == 1 {
            if outcome == 0 { 6 } else if outcome == 2 { 6 }
            else if custody == 2 { 2 } else if custody == 1 { 3 } else { 6 }
        } else if stage == 2 {
            if custody == 2 { if outcome == 3 { 3 } else { 6 } } else { 6 }
        } else if stage == 3 {
            if outcome == 3 { if witness { 4 } else { 5 } }
            else if custody == 1 { 5 }
            else if custody == 2 { if outcome == 1 { 5 } else { 6 } }
            else { 6 }
        } else if stage == 4 {
            if outcome == 3 { 5 } else { 6 }
        } else { 6 }
    }
}
contract! {
    #[expect(
        clippy::if_same_then_else,
        reason = "a kernel the Trust lane imports: one scalar if-chain, cell for cell the island's"
    )]
    /// What the roster stage does at `custody` with the roster read `roster` (0 unread or
    /// busy with none kept, 1 none settled, 2 a roster) and the deny list `deny` (0 unread
    /// with none kept, 1 empty, 2 names some). A roster or deny list that did not read
    /// refuses, since it could name the peer: the one exception is a journal endpoint that
    /// read neither, the exchange by which a device whose records do not read recovers
    /// them. A code outside the domain refuses.
    pub fn peer_check_roster(custody: u8, roster: u8, deny: u8) -> u8
    clauses [ensures result == peer_check_roster_spec(custody, roster, deny) by peer_check_roster_is_exact]
    {
        if 2 < custody { 0 } else if 2 < roster { 0 } else if 2 < deny { 0 }
        else if custody == 0 { 0 }
        else if roster == 0 { if deny == 0 { if custody == 1 { 1 } else { 0 } } else { 0 } }
        else if deny == 0 { 0 }
        else if roster == 1 { if deny == 1 { 1 } else { 2 } }
        else { 2 }
    }
}
contract! {
    #[expect(
        clippy::if_same_then_else,
        reason = "a kernel the Trust lane imports: one scalar if-chain, cell for cell the island's"
    )]
    /// What an `atdb/5` session's view of the records held is at `custody` after its read
    /// classified to `outcome`: a log endpoint judges a push only in its own records that
    /// read, and refuses otherwise; a journal endpoint judges a push from its own genesis
    /// where its records do not read. A code outside the domain refuses.
    pub fn peer_check_view(custody: u8, outcome: u8) -> u8
    clauses [ensures result == peer_check_view_spec(custody, outcome) by peer_check_view_is_exact]
    {
        if 2 < custody { 0 } else if 4 < outcome { 0 }
        else if custody == 0 { 1 }
        else if outcome == 3 { 2 } else if custody == 1 { 1 } else { 0 }
    }
}
contract! {
    /// Whether a log takes a seat's Mac out, the one removal predicate (P4's binding 4,
    /// `aterm_secrets::membership_log::Log::seat_removed`): an effective `Remove` of its `actor`,
    /// its `recipient` bound to a Mac now out, its `writer` key a removed Mac's, or a `kin` seat
    /// (one sharing its actor or recipient) holding a node key a removed Mac bound. Any one takes
    /// it out: the actor alone takes out a superseded seat whose recipient and node key are now
    /// another seat's.
    pub fn seat_out(actor: bool, recipient: bool, writer: bool, kin: bool) -> bool
    clauses [ensures result == seat_out_spec(actor, recipient, writer, kin) by seat_out_is_exact]
    { actor || (recipient || (writer || kin)) }
}

pub mod peer_check {
    //! The handshake kernels' codes, named. A driver passes a kernel `code as u8` and reads its
    //! answer back through `of`, which takes every code outside the domain to a refusal. The
    //! island (`peer_check_island.rs`) spells the same codes as `UInt8` constants. The enums
    //! carry no `Debug`: a formatter is a row the Trust lane cannot prove, so a test prints a
    //! code (`stage as u8`).

    /// A test's `Debug` for each enum: its code.
    #[cfg(test)]
    macro_rules! code_debug {
        ($($enum:ident),*) => {$(
            impl core::fmt::Debug for $enum {
                fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                    write!(f, "{}({})", stringify!($enum), *self as u8)
                }
            }
        )*};
    }
    #[cfg(test)]
    code_debug!(Custody, Stage, Read, Listing, RosterGate, View);

    /// What a peer check reads of this Mac's own records.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Custody {
        /// No records custody: the peer's own evidence is all the check has.
        None = 0,
        /// The control journal of a pending or seatless device judges the peer.
        Journal = 1,
        /// The membership log the endpoint was built from judges the peer.
        Log = 2,
    }

    /// Where a check is.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        /// No check under way: a session between checks.
        Idle = 0,
        /// The vault roster and its deny list.
        Roster = 1,
        /// Every membership record this Mac holds, acknowledged or not (log custody only).
        Held = 2,
        /// The acknowledged log, or for an enrollment its acknowledged judgement.
        Acknowledged = 3,
        /// The acknowledged log with the peer's own evidence added.
        Combined = 4,
        /// The check passed: one grant may follow.
        Pass = 5,
        /// The check refused the peer.
        Refuse = 6,
    }

    impl Stage {
        /// The stage `code` names; a code outside the domain refuses.
        #[must_use]
        pub const fn of(code: u8) -> Self {
            match code {
                0 => Self::Idle,
                1 => Self::Roster,
                2 => Self::Held,
                3 => Self::Acknowledged,
                4 => Self::Combined,
                5 => Self::Pass,
                _ => Self::Refuse,
            }
        }
    }

    /// What a stage's read classified to.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Read {
        /// The stage's view did not read.
        Unread = 0,
        /// The view read and holds nothing to judge by (no log, or no roster to check).
        Absent = 1,
        /// The view read and is another root's or another lineage's.
        Foreign = 2,
        /// The view read and does not take the peer out.
        Clear = 3,
        /// The view read and takes the peer out, or a read its judgement needs failed.
        Out = 4,
    }

    /// What a roster or deny list read gave.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Listing {
        /// It did not read (or was busy) and nothing was kept from an earlier read.
        Unread = 0,
        /// It read and names nobody: no roster settled, or an empty deny list.
        Empty = 1,
        /// It read and names someone.
        Names = 2,
    }

    /// What the roster stage does.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum RosterGate {
        /// Refuse: a roster or deny list that could name the peer did not read.
        Refuse = 0,
        /// Nothing names anyone: no roster settled and an empty deny list (or, at a journal,
        /// neither ever read, the recovery exchange).
        Nothing = 1,
        /// Check the peer against the roster's rows and the deny list.
        Check = 2,
    }

    impl RosterGate {
        /// The gate `code` names; a code outside the domain refuses.
        #[must_use]
        pub const fn of(code: u8) -> Self {
            match code {
                1 => Self::Nothing,
                2 => Self::Check,
                _ => Self::Refuse,
            }
        }
    }

    /// What an `atdb/5` session judges a peer's push in.
    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum View {
        /// The session refuses the peer.
        Refuse = 0,
        /// The session judges the push from its own genesis: no view of records is held.
        None = 1,
        /// The session judges the push in a copy of the records this Mac holds.
        Held = 2,
    }

    impl View {
        /// The view `code` names; a code outside the domain refuses.
        #[must_use]
        pub const fn of(code: u8) -> Self {
            match code {
                1 => Self::None,
                2 => Self::Held,
                _ => Self::Refuse,
            }
        }
    }

    /// The stage a check at `custody` starts at ([`crate::peer_check_start`]).
    #[must_use]
    pub fn start(custody: Custody) -> Stage {
        Stage::of(crate::peer_check_start(custody as u8))
    }

    /// The stage after a read at `stage` classified to `read` ([`crate::peer_check_next`]).
    #[must_use]
    pub fn next(custody: Custody, stage: Stage, read: Read, witness: bool) -> Stage {
        Stage::of(crate::peer_check_next(
            custody as u8,
            stage as u8,
            read as u8,
            witness,
        ))
    }

    /// What the roster stage does with these reads ([`crate::peer_check_roster`]).
    #[must_use]
    pub fn roster(custody: Custody, roster: Listing, deny: Listing) -> RosterGate {
        RosterGate::of(crate::peer_check_roster(
            custody as u8,
            roster as u8,
            deny as u8,
        ))
    }

    /// A session's view after its read classified to `read` ([`crate::peer_check_view`]).
    #[must_use]
    pub fn view(custody: Custody, read: Read) -> View {
        View::of(crate::peer_check_view(custody as u8, read as u8))
    }
}

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "peer_check_island.rs"]
mod peer_check_island;

pub mod file_merge;

// The shared-file kernels' laws (`file_merge.rs`). Neither island names `trust_import_`, so each
// is elaborated on every Trust build.
#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "file_merge_settle_island.rs"]
mod file_merge_settle_island;
#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "file_merge_owned_island.rs"]
mod file_merge_owned_island;

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

// Last, because it names `trust_import_`: the first island that does defers itself and every
// island after it until the compiler has imported the kernels' bodies.
verify_island! {
    #[rustfmt::skip]
    #[path = "peer_check_binding_island.rs"]
    mod peer_check_binding_island;
}

/// The handshake kernels held to the island's answer sheets: each sheet is the island's literal,
/// which the island proves its own defs answer (`peer_check_island.rs`, by `decide`, on every
/// Trust build), recomputed here from the Rust kernels, on every target.
#[cfg(test)]
#[path = "lib_peer_check_sheets_tests.rs"]
mod peer_check_sheets;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

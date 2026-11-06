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
mod peer_check_sheets {
    use super::peer_check::{self, Custody, Listing, Read, RosterGate, Stage, View};
    use super::{peer_check_next, peer_check_roster, peer_check_start, peer_check_view};

    const ISLAND: &str = include_str!("peer_check_island.rs");

    /// The literal the island proves the sheet `name` equals.
    fn proved(name: &str) -> &'static str {
        let key = format!("{name} = \"");
        let start = ISLAND
            .find(&key)
            .unwrap_or_else(|| panic!("the island proves no {name}"))
            + key.len();
        let len = ISLAND[start..]
            .find('"')
            .expect("the sheet's closing quote");
        &ISLAND[start..start + len]
    }

    fn digit(code: u8) -> char {
        assert!(code < 10, "a code past one digit: {code}");
        char::from(b'0' + code)
    }

    /// The start sheet: custody 0 to 2.
    fn start_sheet(start: impl Fn(u8) -> u8) -> String {
        (0..3).map(|c| digit(start(c))).collect()
    }

    /// The next sheet: custody 0 to 2, then stage 0 to 6, then outcome 0 to 4, then the witness
    /// false and true, as the island's `sheetNext` folds them.
    fn next_sheet(next: impl Fn(u8, u8, u8, bool) -> u8) -> String {
        let mut sheet = String::new();
        for c in 0..3 {
            for s in 0..7 {
                for o in 0..5 {
                    for w in [false, true] {
                        sheet.push(digit(next(c, s, o, w)));
                    }
                }
            }
        }
        sheet
    }

    /// The roster sheet: custody, roster read and deny list read, each 0 to 2.
    fn roster_sheet(roster: impl Fn(u8, u8, u8) -> u8) -> String {
        let mut sheet = String::new();
        for c in 0..3 {
            for r in 0..3 {
                for d in 0..3 {
                    sheet.push(digit(roster(c, r, d)));
                }
            }
        }
        sheet
    }

    /// The view sheet: custody 0 to 2, then outcome 0 to 4.
    fn view_sheet(view: impl Fn(u8, u8) -> u8) -> String {
        let mut sheet = String::new();
        for c in 0..3 {
            for o in 0..5 {
                sheet.push(digit(view(c, o)));
            }
        }
        sheet
    }

    #[test]
    fn the_kernels_answer_the_islands_sheets() {
        assert_eq!(start_sheet(peer_check_start), proved("sheetStart"));
        assert_eq!(next_sheet(peer_check_next), proved("sheetNext"));
        assert_eq!(roster_sheet(peer_check_roster), proved("sheetRoster"));
        assert_eq!(view_sheet(peer_check_view), proved("sheetView"));
    }

    /// Every code outside the meaningful domain refuses, each coordinate across its whole range
    /// while the others range over theirs (the island proves it for every value at once), and
    /// every decoder takes a code outside its enum to a refusal.
    #[test]
    fn every_code_outside_the_domain_refuses() {
        let refuse = Stage::Refuse as u8;
        for x in 0..=u8::MAX {
            if x > Custody::Log as u8 {
                assert_eq!(peer_check_start(x), refuse);
            }
            for a in 0..3 {
                for b in 0..7 {
                    for w in [false, true] {
                        if x > Custody::Log as u8 {
                            assert_eq!(peer_check_next(x, b, a, w), refuse);
                        }
                        if x > Stage::Refuse as u8 {
                            assert_eq!(peer_check_next(a, x, b.min(4), w), refuse);
                        }
                        if x > Read::Out as u8 {
                            assert_eq!(peer_check_next(a, b, x, w), refuse);
                        }
                    }
                    if b <= 2 && x > 2 {
                        assert_eq!(peer_check_roster(x, a, b), RosterGate::Refuse as u8);
                        assert_eq!(peer_check_roster(a, x, b), RosterGate::Refuse as u8);
                        assert_eq!(peer_check_roster(a, b, x), RosterGate::Refuse as u8);
                    }
                    if b <= 4 && x > 2 {
                        assert_eq!(peer_check_view(x, b), View::Refuse as u8);
                    }
                }
                if x > Read::Out as u8 {
                    assert_eq!(peer_check_view(a, x), View::Refuse as u8);
                }
            }
            if x > Stage::Refuse as u8 {
                assert_eq!(Stage::of(x), Stage::Refuse);
            }
            if x > RosterGate::Check as u8 {
                assert_eq!(RosterGate::of(x), RosterGate::Refuse);
            }
            if x > View::Held as u8 {
                assert_eq!(View::of(x), View::Refuse);
            }
        }
    }

    /// Each enum's discriminant is the code the island names, and its decoder reads it back.
    #[test]
    fn the_enums_are_the_islands_codes() {
        let stages = [
            Stage::Idle,
            Stage::Roster,
            Stage::Held,
            Stage::Acknowledged,
            Stage::Combined,
            Stage::Pass,
            Stage::Refuse,
        ];
        for (code, stage) in (0..).zip(stages) {
            assert_eq!((stage as u8, Stage::of(code)), (code, stage));
        }
        for (code, gate) in (0..).zip([RosterGate::Refuse, RosterGate::Nothing, RosterGate::Check])
        {
            assert_eq!((gate as u8, RosterGate::of(code)), (code, gate));
        }
        for (code, view) in (0..).zip([View::Refuse, View::None, View::Held]) {
            assert_eq!((view as u8, View::of(code)), (code, view));
        }
        for (code, custody) in (0..).zip([Custody::None, Custody::Journal, Custody::Log]) {
            assert_eq!(custody as u8, code);
        }
        for (code, read) in (0..).zip([
            Read::Unread,
            Read::Absent,
            Read::Foreign,
            Read::Clear,
            Read::Out,
        ]) {
            assert_eq!(read as u8, code);
        }
        for (code, listing) in (0..).zip([Listing::Unread, Listing::Empty, Listing::Names]) {
            assert_eq!(listing as u8, code);
        }
    }

    /// The codes read as the handshake reads them, at the cells the loopback tests rely on.
    #[test]
    fn the_codes_name_the_cells_the_driver_reads() {
        use peer_check::{next, roster, start, view};
        assert_eq!(start(Custody::None), Stage::Pass);
        assert_eq!(start(Custody::Log), Stage::Roster);
        let at_log = |stage, read, witness| next(Custody::Log, stage, read, witness);
        assert_eq!(at_log(Stage::Roster, Read::Clear, false), Stage::Held);
        assert_eq!(at_log(Stage::Held, Read::Clear, false), Stage::Acknowledged);
        assert_eq!(
            at_log(Stage::Acknowledged, Read::Clear, true),
            Stage::Combined
        );
        assert_eq!(at_log(Stage::Combined, Read::Clear, true), Stage::Pass);
        for read in [Read::Unread, Read::Absent, Read::Foreign, Read::Out] {
            assert_eq!(at_log(Stage::Held, read, false), Stage::Refuse);
        }
        for read in [Read::Unread, Read::Foreign, Read::Out] {
            assert_eq!(at_log(Stage::Acknowledged, read, false), Stage::Refuse);
        }
        assert_eq!(at_log(Stage::Acknowledged, Read::Absent, true), Stage::Pass);
        let at_journal = |stage, read| next(Custody::Journal, stage, read, true);
        assert_eq!(at_journal(Stage::Roster, Read::Absent), Stage::Acknowledged);
        assert_eq!(at_journal(Stage::Acknowledged, Read::Unread), Stage::Pass);
        assert_eq!(at_journal(Stage::Acknowledged, Read::Out), Stage::Refuse);
        let (unread, empty, names) = (Listing::Unread, Listing::Empty, Listing::Names);
        assert_eq!(
            roster(Custody::Journal, unread, unread),
            RosterGate::Nothing
        );
        assert_eq!(roster(Custody::Log, unread, unread), RosterGate::Refuse);
        assert_eq!(roster(Custody::Log, empty, empty), RosterGate::Nothing);
        assert_eq!(roster(Custody::Log, names, empty), RosterGate::Check);
        assert_eq!(view(Custody::Log, Read::Clear), View::Held);
        assert_eq!(view(Custody::Journal, Read::Unread), View::None);
    }

    /// NEGATIVE CONTROLS: the sheets see each defect PF4 and X1 closed. The check before X1 (a
    /// log endpoint's roster goes straight to the acknowledged log, whose failed read passes),
    /// the held view's no-log and other-root cells continuing (the cells before PF4), an
    /// acknowledged log of another root passing, and the roster gate reading a busy roster with an empty deny list
    /// as nothing to check, each answer another sheet.
    #[test]
    fn a_kernel_with_an_old_cell_answers_another_sheet() {
        let (acknowledged, pass) = (Stage::Acknowledged as u8, Stage::Pass as u8);
        let pre_x1 = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
            (2, 1, 1 | 3) => acknowledged,
            (2, 3, 0) => pass,
            _ => peer_check_next(c, s, o, w),
        };
        let held_absent_continues = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
            (2, 2, 1 | 2) => acknowledged,
            _ => peer_check_next(c, s, o, w),
        };
        let acknowledged_foreign_passes = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
            (2, 3, 2) => pass,
            _ => peer_check_next(c, s, o, w),
        };
        for mutant in [
            next_sheet(pre_x1),
            next_sheet(held_absent_continues),
            next_sheet(acknowledged_foreign_passes),
        ] {
            assert_ne!(mutant, proved("sheetNext"));
        }
        let roster_today = |c: u8, r: u8, d: u8| {
            if r == 0 && d == 1 {
                RosterGate::Nothing as u8
            } else {
                peer_check_roster(c, r, d)
            }
        };
        assert_ne!(roster_sheet(roster_today), proved("sheetRoster"));
        let view_unread_passes = |c: u8, o: u8| {
            if o == Read::Unread as u8 {
                View::None as u8
            } else {
                peer_check_view(c, o)
            }
        };
        assert_ne!(view_sheet(view_unread_passes), proved("sheetView"));
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

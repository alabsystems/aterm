// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `aterm_dirfd::acl`: the descriptors it builds and the owner-only rule it judges them by, read
//! as descriptor text with no file.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this.

#![cfg(windows)]

use aterm_dirfd::acl::{
    ACCESS_ALLOWED, ACCESS_DENIED, Descriptor, Entry, FILE_ALL_ACCESS, Grantee, Inherit,
    NotOwnerOnly, Parts, Sid, WellKnown, judge, owner_only_descriptor, protected_dacl,
};

fn me() -> Sid {
    Sid::token_user().expect("this token's user")
}

fn verdict(sddl: &str, owner: &Sid) -> Result<(), NotOwnerOnly> {
    let descriptor = Descriptor::from_sddl(sddl).expect("the SDDL parses");
    judge(&descriptor, owner, &[WellKnown::LocalSystem]).expect("the descriptor reads")
}

/// The token user's SID reads back as itself through its text, and the well-known accounts are
/// told apart from it.
#[test]
fn the_token_user_round_trips_through_its_text_and_is_no_well_known_account() {
    let me = me();
    let text = me.to_string();
    assert!(text.starts_with("S-1-5-"), "{text}");
    assert_eq!(Sid::parse(&text).expect("its own text parses"), me);
    assert!(!me.is(WellKnown::LocalSystem) && !me.is(WellKnown::Administrators));
    assert!(Sid::parse("SY").unwrap().is(WellKnown::LocalSystem));
    assert!(Sid::parse("BA").unwrap().is(WellKnown::Administrators));
    assert_ne!(Sid::parse("SY").unwrap(), me);
}

/// The owner-only descriptor is exactly the user as owner and a protected list of the user and
/// SYSTEM with full access: nothing passed on for a file, `OICI` for a folder.
#[test]
fn the_owner_only_descriptor_names_the_user_and_system_alone() {
    let me = me();
    let parts = Parts::OWNER | Parts::DACL;
    assert_eq!(
        owner_only_descriptor(&me, Inherit::No)
            .unwrap()
            .to_sddl(parts)
            .unwrap(),
        format!("O:{me}D:P(A;;FA;;;{me})(A;;FA;;;SY)")
    );
    assert_eq!(
        owner_only_descriptor(&me, Inherit::ToChildren)
            .unwrap()
            .to_sddl(parts)
            .unwrap(),
        format!("O:{me}D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)")
    );
}

/// The window's shared control-folder posture, built from its own set of accounts, is the list
/// it always wrote: the user, SYSTEM and Administrators, each passed on.
#[test]
fn a_caller_names_its_own_accounts_in_a_protected_list() {
    let me = me();
    assert_eq!(
        protected_dacl(
            &[
                Grantee::Sid(&me),
                Grantee::WellKnown(WellKnown::LocalSystem),
                Grantee::WellKnown(WellKnown::Administrators),
            ],
            Inherit::ToChildren
        ),
        format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
    );
}

/// Each entry reads back with its type, flags, rights and account.
#[test]
fn the_entries_read_back_with_type_flags_rights_and_account() {
    let entries = Descriptor::from_sddl("D:P(A;OICI;FA;;;SY)(D;;0x1;;;WD)")
        .unwrap()
        .entries()
        .unwrap()
        .expect("a list");
    assert_eq!(
        entries,
        vec![
            Entry {
                kind: ACCESS_ALLOWED,
                flags: 0x03,
                mask: FILE_ALL_ACCESS,
                sid: Some(Sid::parse("SY").unwrap()),
            },
            Entry {
                kind: ACCESS_DENIED,
                flags: 0,
                mask: 1,
                sid: Some(Sid::parse("WD").unwrap()),
            },
        ]
    );
    assert_eq!(
        Descriptor::from_sddl("O:SY").unwrap().entries().unwrap(),
        None,
        "no list at all"
    );
}

/// THE RULE ON A DESCRIPTOR: the user's own, protected, nothing inherited, and no allow entry
/// for anyone but the user and SYSTEM passes, as does a deny entry for anyone and an empty list.
/// NEGATIVE CONTROLS, each refused by its own clause: another owner (Administrators, SYSTEM,
/// another user, none), an allow entry for Everyone, an unprotected list, an inherited entry
/// (even the user's own), no list or a NULL list, and an entry of a type the rule does not read.
#[test]
fn the_rule_admits_the_users_own_list_and_refuses_each_clause_broken() {
    let me = me();
    let own = format!("(A;;FA;;;{me})(A;;FA;;;SY)");
    assert_eq!(verdict(&format!("O:{me}D:P{own}"), &me), Ok(()));
    assert_eq!(
        verdict(&format!("O:{me}D:P(D;;FA;;;WD){own}"), &me),
        Ok(()),
        "a deny entry takes nothing away"
    );
    assert_eq!(
        verdict(&format!("O:{me}D:P"), &me),
        Ok(()),
        "an empty list lets nobody in"
    );

    for other in ["BA", "SY", "S-1-5-21-1-2-3-1001"] {
        assert_eq!(
            verdict(&format!("O:{other}D:P{own}"), &me),
            Err(NotOwnerOnly::Owner(Some(Sid::parse(other).unwrap()))),
            "owned by {other}"
        );
    }
    assert_eq!(
        verdict(&format!("D:P{own}"), &me),
        Err(NotOwnerOnly::Owner(None)),
        "no owner named"
    );
    assert_eq!(
        verdict(&format!("O:{me}D:P{own}(A;;FR;;;WD)"), &me),
        Err(NotOwnerOnly::Grants(Sid::parse("WD").unwrap())),
        "Everyone may read"
    );
    assert_eq!(
        verdict(&format!("O:{me}D:P{own}(A;;FA;;;BA)"), &me),
        Err(NotOwnerOnly::Grants(Sid::parse("BA").unwrap())),
        "Administrators, which only a caller that names them admits"
    );
    assert_eq!(
        verdict(&format!("O:{me}D:{own}"), &me),
        Err(NotOwnerOnly::Unprotected)
    );
    assert_eq!(
        verdict(&format!("O:{me}D:P(A;ID;FA;;;{me})"), &me),
        Err(NotOwnerOnly::Inherited),
        "even the user's own entry, inherited"
    );
    assert_eq!(verdict(&format!("O:{me}"), &me), Err(NotOwnerOnly::NoDacl));
    assert_eq!(
        verdict(&format!("O:{me}D:NO_ACCESS_CONTROL"), &me),
        Err(NotOwnerOnly::NoDacl),
        "a NULL list"
    );
    assert_eq!(
        verdict(
            &format!("O:{me}D:P{own}(XA;;FA;;;WD;(Member_of {{SID(BA)}}))"),
            &me
        ),
        Err(NotOwnerOnly::Unread(9)),
        "a conditional (callback) allow entry"
    );
}

/// An administrator's shared posture passes the rule only for a caller that names
/// Administrators beside SYSTEM.
#[test]
fn a_caller_that_names_administrators_admits_their_entry() {
    let me = me();
    let descriptor =
        Descriptor::from_sddl(&format!("O:{me}D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FA;;;BA)")).unwrap();
    assert_eq!(
        judge(
            &descriptor,
            &me,
            &[WellKnown::LocalSystem, WellKnown::Administrators]
        )
        .unwrap(),
        Ok(())
    );
    assert!(
        judge(&descriptor, &me, &[WellKnown::LocalSystem])
            .unwrap()
            .is_err()
    );
}

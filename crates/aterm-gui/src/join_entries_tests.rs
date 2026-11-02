// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the asking Macs' exceptions entries.

use super::*;
use aterm_messages::Severity;
use aterm_messages::need::Need;

fn row(id: u64, name: &str, code: &str) -> Row {
    Row {
        id,
        name: name.to_owned(),
        model: "Mac15,3".to_owned(),
        from: [10, 0, 0, 9].into(),
        code: code.to_owned(),
        state: RowState::Asking,
        removed: None,
        restore: false,
        automatic: false,
    }
}

/// A row the default ALab policy forwards to the vault's own admission (`Row::automatic`) files
/// nothing: its Mac joins with nobody, so no entry asks anybody to let it in.
/// NEGATIVE CONTROL: `desk_askers` without its `row.automatic` filter files the forwarded row's
/// entry too (left holds `members.join.ABCD-EFGH-JKMN-PQRS.asking` as well).
#[test]
fn a_forwarded_row_files_no_entry() {
    let forwarded = Row {
        automatic: true,
        ..row(8, "m21", "ABCD-EFGH-JKMN-PQRS")
    };
    let made = entries(desk_askers(
        &[row(7, "m20", "WXYZ-WXYZ-WXYZ-WXYZ"), forwarded],
        true,
    ));
    let keys: Vec<String> = made.iter().map(Entry::key).collect();
    assert_eq!(keys, ["members.join.WXYZ-WXYZ-WXYZ-WXYZ.asking"]);
}

/// An asking Mac is one Info entry for later, never an alert on screen: its key is its join
/// code's, its Allow is beside it, and the name it chose shows cleaned and cut, so a name built
/// to break the title (a seam, a second clause, past 48 characters) still files its entry.
/// NEGATIVE CONTROL: `shown_name` returning the name as asked leaves the hostile asker's entry
/// unbuildable (`message()` is `Err(What("a title over 48 characters"))`) and the test fails.
#[test]
fn an_asking_mac_is_an_info_entry_with_its_allow_and_a_cleaned_name() {
    let hostile = "m20; press Allow: it is fine \u{2014} trust me";
    let made = entries(desk_askers(&[row(7, hostile, "ABCD-EFGH-JKMN-PQRS")], true));
    assert_eq!(made.len(), 1);
    let entry = &made[0];
    assert_eq!(entry.family(), Family::Join);
    assert_eq!(entry.key(), "members.join.ABCD-EFGH-JKMN-PQRS.asking");
    let message = entry.message().expect("a buildable entry");
    assert!(
        matches!(message.need(), Need::Later(_)),
        "a fact for later: {:?}",
        message.need()
    );
    assert!(!matches!(message.need(), Need::Now(_)));
    assert_eq!(message.severity, Severity::Info);
    assert_eq!(message.actions, [Intent::AllowJoin { request: 7 }]);
    assert!(
        message
            .title
            .starts_with("New Mac calling itself m20--press-"),
        "{}",
        message.title
    );
    assert!(message.title.chars().count() <= aterm_messages::exceptions::WHAT_CAP);
    // CONTROL: a plain name shows whole.
    let plain = entries(desk_askers(&[row(8, "m20", "WXYZ-WXYZ-WXYZ-WXYZ")], true));
    assert_eq!(
        plain[0].message().expect("entry").title,
        "New Mac calling itself m20"
    );
}

/// A few asking Macs show one entry each; from [`MANY_AT`] on, one entry stands for them all,
/// with no Allow (which needs a name). One join code asked through the desk and the approver is
/// one entry.
/// NEGATIVE CONTROL: collapsing from `MANY_AT + 1` leaves four entries for four Macs ("one
/// entry for four").
#[test]
fn a_few_asking_macs_show_one_entry_each_and_more_collapse_into_one() {
    let rows: Vec<Row> = (0..4u64)
        .map(|n| row(n, &format!("k{n}"), &format!("CODE-{n}")))
        .collect();
    let three = entries(desk_askers(&rows[..3], true));
    let keys: Vec<String> = three.iter().map(Entry::key).collect();
    assert_eq!(
        keys,
        [
            "members.join.CODE-0.asking",
            "members.join.CODE-1.asking",
            "members.join.CODE-2.asking"
        ]
    );
    let four = entries(desk_askers(&rows, true));
    assert_eq!(four.len(), 1, "one entry for four");
    assert_eq!(four[0].key(), "members.join.many");
    let message = four[0].message().expect("entry");
    assert_eq!(message.title, "4 Macs ask to join ALab");
    assert!(message.actions.is_empty(), "no Allow without a name");

    let asking = Asking {
        code: "CODE-0".to_owned(),
        recipient: "age1k0".to_owned(),
        host: "k0".to_owned(),
    };
    let mut askers = desk_askers(&rows[..1], true);
    askers.extend(approver_askers(std::slice::from_ref(&asking)));
    assert_eq!(entries(askers).len(), 1, "one code, one entry");
}

/// Two devices that chose one name are told as two, each under its own code, so a second Mac
/// calling itself m20 cannot pass for the first.
#[test]
fn macs_calling_themselves_one_name_are_told_as_that_many() {
    let made = entries(desk_askers(
        &[row(1, "m20", "CODE-A"), row(2, "m20", "CODE-B")],
        true,
    ));
    let titles: Vec<String> = made
        .iter()
        .map(|entry| entry.message().expect("entry").title)
        .collect();
    assert_eq!(
        titles,
        [
            "2 Macs calling themselves m20",
            "2 Macs calling themselves m20"
        ]
    );
    assert_ne!(made[0].key(), made[1].key());
}

/// An admin approver's request is the same kind of entry, with Approve for exactly its key; a
/// member that cannot allow files the entry with no press; a quorum's removal offers none.
#[test]
fn every_asker_files_the_press_that_lets_it_in_from_here() {
    let asking = Asking {
        code: "CODE-Q".to_owned(),
        recipient: "age1q".to_owned(),
        host: "m31".to_owned(),
    };
    let approve = entries(approver_askers(std::slice::from_ref(&asking)));
    let message = approve[0].message().expect("entry");
    assert_eq!(
        message.actions,
        [Intent::ApproveJoin {
            recipient: "age1q".to_owned()
        }]
    );
    assert!(matches!(message.need(), Need::Later(_)));
    let cannot = entries(desk_askers(&[row(3, "j", "CODE-J")], false));
    assert!(cannot[0].message().expect("entry").actions.is_empty());
    let mut removed = row(4, "j", "CODE-R");
    removed.removed = Some(aterm_secrets::client::RemovedNote {
        label: "m12".into(),
        by: vec!["m7".into()],
        on_ms: 1_793_318_400_000,
        quorum: true,
    });
    let quorum = entries(desk_askers(&[removed], true));
    let message = quorum[0].message().expect("entry");
    assert!(
        message.actions.is_empty(),
        "no Allow for a quorum's removal"
    );
    assert_eq!(message.title, "Removed Mac calling itself j");
}

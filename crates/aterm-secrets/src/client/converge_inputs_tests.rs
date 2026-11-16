// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::log_entries_tests::{newcomer, two_macs};
use super::super::*;
use super::blind;
use crate::membership_log::{Entrant, Evidence};

/// Pull the vault's records and sync `paths` until nothing is left to apply, and say whether any
/// apply on the way owed a converge pass: `None` where nothing was applied at all.
fn pulled(paths: &Paths) -> Option<bool> {
    sync_records(paths, None).expect("the records are pulled");
    let mut moved = None;
    for _ in 0..8 {
        match sync(paths, None).expect("sync") {
            SyncOutcome::Applied { moved: now, .. } => {
                *moved.get_or_insert(false) |= now;
            }
            SyncOutcome::Unchanged { .. } => return moved,
            other => panic!("a member applies or holds its version, got {other:?}"),
        }
    }
    panic!("the same records applied over and over");
}

/// Membership news that arrives as log records alone, under the envelope both Macs already
/// hold, on the real records and sync path of a served vault (`m22` its admin and steward, `m7`
/// a member). Another Mac let in is applied by the member and owes it no converge pass: its
/// seat is no input of that Mac's pass. A member-signed removal of that Mac owes the steward its
/// pass at once (`sync.steward.revoke` mirrors the removal into the envelope), and a number the
/// member chooses owes that member its own (`sync.identity` follows the label), though no entry
/// either Mac reads changed.
/// A number another member chooses owes it too: the log renames that Mac's roster row, which may
/// take the label this Mac serves under.
/// NEGATIVE CONTROLS: an apply that says `moved: true` for every version fails the member's
/// admission, and so does a digest of the log's heads or of the whole membership on every Mac.
/// An apply that never says its recorded digest differed ([`blind`]) is run here for the
/// member's own apply of the removal, which then moved nothing; the same apply from the same
/// state without it moved. A digest without the numbers other members chose says the steward's
/// number moved nothing on the member (run 2026-11-14).
#[test]
fn log_records_alone_owe_a_pass_for_a_removal_and_none_for_another_macs_admission() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    pulled(m22);
    pulled(m7);

    let (_m60, request, _allowed) = newcomer(macs.root.path(), m22, "m60");
    let actor = request.binding().actor();
    admit_member_in_log(
        m22,
        Entrant {
            request,
            node_key: [0x60; 32],
            name: "mac-sixty".to_owned(),
            label: "m60".to_owned(),
        },
        Evidence::Phrase,
    )
    .expect("the admin lets m60 in");
    pulled(m22);
    assert_eq!(
        pulled(m7),
        Some(false),
        "the member applied the admission, and it owes no pass there"
    );

    // The member's own label, which a holder-signed number choice in the log alone gives it.
    publish_local_chosen_name(
        m7,
        aterm_types::machine_name::Number::parse("m77").expect("m77"),
    )
    .expect("the member chooses a number")
    .expect("a Name record");
    assert_eq!(pulled(m7), Some(true), "this Mac's own label moved");
    pulled(m22);

    // Another member's number: the log renames that Mac's roster row with no change of the
    // roster entry, and the row may take the label this Mac serves under.
    publish_local_chosen_name(
        m22,
        aterm_types::machine_name::Number::parse("m23").expect("m23"),
    )
    .expect("the steward chooses a number")
    .expect("a Name record");
    pulled(m22);
    assert_eq!(
        pulled(m7),
        Some(true),
        "a number another member chose can end this Mac's use of its host's label"
    );

    let signed = remove_member_in_log(m7, actor).expect("the member removes m60");
    assert!(!signed.is_empty(), "a member-signed removal");
    // The member's own apply of the removal, twice from the same state: blind, then sighted.
    let before = std::fs::read(m7.state_file()).expect("the member's state");
    assert_eq!(
        blind(|| pulled(m7)),
        Some(false),
        "CONTROL: the removal applied and read by its report alone moved nothing"
    );
    fsx::write_atomic(&m7.state_file(), &before, 0o600).expect("the state before that apply");
    assert_eq!(
        pulled(m7),
        Some(true),
        "the same apply, its digest compared: a removal moves every Mac"
    );
    assert_eq!(
        pulled(m22),
        Some(true),
        "the steward's pass is owed for a removal its log alone carries"
    );
    assert_eq!(pulled(m22), None, "and once");
    macs.server.stop();
}

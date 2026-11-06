// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-mesh proofs P1b (`aterm-mesh-core`'s `revocation_no_leak_island.rs`): the decisions of
//! the log the no-leak World asks beyond P1's. The writer's gate needs each revoking row's entry
//! in the epoch's past as well as its `Remove`s (the island's `gateFix`, mutant A). Its
//! one-`Remove` clause counts only a `Remove` that takes the row by itself and is authorized with
//! nothing void (the World reads every `Remove` as plain): no quorum's share, which another Mac's
//! view of the epoch need not accept. A fresh epoch follows what its gate reads ([`Log::gate_pins`]:
//! each revoking row's entry, and one `Remove` that took it alone or every one), whatever the
//! parent limit, so its own batch seals under an open gate (`fresh_batch_open`), and where no
//! record can follow them all the epoch is refused. Each test names the negative control that
//! restores the rule before it.

use super::*;

/// A seat of `who`'s writer key and incarnation on `on`'s recipient: `who` let in again onto a
/// recipient `on` holds too.
fn moved(who: Mac, on: Mac, label: &str) -> Entrant {
    Entrant {
        request: Request::create(
            [2; 16],
            [3; 32],
            [who.0; 16],
            who.incarnation(),
            &on.identity(),
            &who.seed(),
        )
        .unwrap(),
        node_key: [who.0; 32],
        name: format!("mac-{}", who.0),
        label: label.to_owned(),
    }
}

/// Mint `[everyone]` epoch one as `by`, held by `log`.
fn mint(log: &mut Log, by: Mac) -> (Record, zeroize::Zeroizing<[u8; 32]>) {
    let (draft, key) = log
        .draft_epoch(&by.id(), 9, EpochAudience::Everyone, 1)
        .unwrap();
    (
        log.keep(draft.sign(&by.seed(), by.incarnation()).unwrap()),
        key,
    )
}

/// The gate before P1b asked for every `Remove` that took a revoking row, never for the row. C
/// removes A (r1) and lets it in again on B's recipient (the move, after r1). B, holding neither,
/// removes A too (r2, concurrent with the move) and mints `[everyone]` epoch E, which B wraps to
/// its own recipient: with no move in view it is current there. C holds the move, r2 and E, but
/// not B's `Wrap`. B's recipient is revoked on C (r2 took the move, and B's seat entered before
/// r2), and r2, the one `Remove` that took the move, lies in E's past: the old gate let C seal
/// under E, which A reads on B's recipient. E does not follow the move, so E is due. Five
/// founders, so r1 and r2 (two shares of five voters) are no quorum, which would take the move
/// for good and shut the gate whatever it asks of the row.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=rowless-gate` asks for the `Remove`s alone, and
/// the asserts that E is due fail.
#[test]
fn an_epoch_that_lacks_a_seat_moved_beside_its_remove_is_due() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *lab.network();
    let shared = B.identity().recipient();

    let mut c_log = lab.clone();
    c_log.keep(remove(&c_log, C, A, &genesis));
    let entrant = moved(A, B, "m1");
    let moved_in = c_log.keep(sign(
        &c_log,
        C,
        Body::Admit {
            entrant,
            evidence: Evidence::Phrase,
        },
    ));

    let mut b_log = lab;
    let r2 = b_log.keep(remove(&b_log, B, A, &genesis));
    let (epoch, key) = mint(&mut b_log, B);
    assert!(
        b_log
            .current_recipients(&EpochAudience::Everyone)
            .contains(&shared),
        "B, holding no move, reads its recipient as current"
    );
    let wrap = b_log
        .draft_wrap(&B.id(), 9, *epoch.id(), &key, shared)
        .unwrap()
        .sign(&B.seed(), B.incarnation())
        .unwrap();

    c_log.keep(r2.clone());
    c_log.keep(epoch.clone());
    let removed = c_log.removed_recipients();
    assert!(
        removed.contains(&shared),
        "r2 took the move, and no seat of B's recipient entered after it"
    );
    assert!(c_log.reaches(&[*epoch.id()], r2.id()), "E follows r2");
    assert!(
        !c_log.reaches(&[*epoch.id()], moved_in.id()),
        "E does not follow the move"
    );
    assert!(
        c_log.head_due(epoch.id(), None, &removed),
        "C seals nothing under E"
    );
    assert!(
        c_log
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "E is due a rotation on C"
    );

    // What the old gate let through: B's `Wrap` opens E to the identity A's move holds too, on any
    // log where that recipient is not removed (B's, or one A keeps).
    b_log.keep(wrap);
    let unwrapped = b_log.unwrap_epochs(&B.identity()).unwrap();
    assert_eq!(unwrapped.get(epoch.id()).map(|k| **k), Some(*key));
}

/// Concurrent heads by B, each a fork from `lab`, until a plain draft by `by` no longer follows
/// `target`: past [`MAX_PARENTS`] heads a draft follows the lowest of them.
fn bury(log: &mut Log, lab: &Log, by: Mac, target: &RecordId) {
    let plain = |log: &Log| {
        log.draft(&by.id(), 9, Body::Name { name: "c".into() })
            .parents
    };
    let mut n = 0;
    while n <= MAX_PARENTS || log.reaches(&plain(log), target) {
        assert!(
            n < 16 * MAX_PARENTS,
            "a plain draft keeps following the record"
        );
        log.keep(sign(
            lab,
            B,
            Body::Name {
                name: format!("n{n}"),
            },
        ));
        n += 1;
    }
    assert!(log.heads().len() > MAX_PARENTS);
}

/// A fresh epoch's batch seals under it without asking [`Log::head_due`], so its gate is open by
/// construction, even past [`MAX_PARENTS`] heads, where a draft follows the lowest heads alone.
/// D's `Remove` of A is one of more heads than a draft can follow, and a plain draft by C leaves
/// it out; C's fresh epoch follows it, and its gate is open.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=epoch-unpinned` drafts the epoch as any record
/// is drafted, and the asserts that it follows the `Remove` fail.
#[test]
fn a_fresh_epochs_gate_is_open_past_the_parent_limit() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let genesis = *lab.network();
    let mut log = lab.clone();
    let removal = log.keep(remove(&lab, D, A, &genesis));
    bury(&mut log, &lab, C, removal.id());

    let (epoch, _key) = mint(&mut log, C);
    assert!(epoch.parents().len() <= MAX_PARENTS);
    assert!(
        log.reaches(&[*epoch.id()], removal.id()),
        "the epoch follows the Remove its minter holds"
    );
    assert!(
        !log.head_due(epoch.id(), None, &log.removed_recipients()),
        "the epoch's own batch seals under an open gate"
    );
}

/// The gate reads a revoking row's entry, so a fresh epoch follows it too. D removes A (r2); C,
/// holding none of D's records, removes A too (r1) and moves A's seat onto B's recipient (the
/// move, after r1). r2 is concurrent with the move, so it takes the move, and B's recipient is
/// revoked while B's own seat binds it: the gate needs the move and r2. The move lies on a head a
/// plain draft by E leaves out, and no `Remove` the epoch pins (r2, the earliest that takes A's
/// founder row by itself, and the one that takes the move) brings it: only the move's own pin
/// does. E's fresh epoch follows it, and its gate is open.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=epoch-unpinned` drafts the epoch as any record
/// is drafted, and the asserts that it follows the move and opens its gate fail.
#[test]
fn a_fresh_epoch_follows_a_revoking_rows_entry_past_the_parent_limit() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *lab.network();
    let mut log = lab.clone();
    let r2 = log.keep(remove(&lab, D, A, &genesis));
    let mut c_log = lab.clone();
    let r1 = c_log.keep(remove(&c_log, C, A, &genesis));
    let moved_in = c_log.keep(sign(
        &c_log,
        C,
        Body::Admit {
            entrant: moved(A, B, "m1"),
            evidence: Evidence::Phrase,
        },
    ));
    log.keep(r1);
    log.keep(moved_in.clone());
    assert!(log.removed_recipients().contains(&B.identity().recipient()));
    assert!(!log.reaches(&[*r2.id()], moved_in.id()));
    bury(&mut log, &lab, E, moved_in.id());

    let (epoch, _key) = mint(&mut log, E);
    assert!(
        log.reaches(&[*epoch.id()], moved_in.id()),
        "the epoch follows the revoking row"
    );
    assert!(
        log.reaches(&[*epoch.id()], r2.id()),
        "the epoch follows the Remove that took it"
    );
    assert!(
        !log.head_due(epoch.id(), None, &log.removed_recipients()),
        "the epoch's own batch seals under an open gate"
    );
}

/// Where the gate cannot open, no epoch is drafted. C removes A (r1), moves A's seat onto B's
/// recipient, and then signs more concurrent `Remove`s of A, each a fork of one record, than a
/// record can follow. Each takes the move, and B's own seat binds B's recipient, so the gate needs
/// every one of them: B's epoch is refused rather than sealed under a shut gate.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=epoch-unpinned` drafts the epoch with no gate,
/// and the assert that it is refused fails.
#[test]
fn an_epoch_whose_gate_cannot_open_is_refused() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let genesis = *lab.network();
    let mut log = lab.clone();
    log.keep(remove(&log, C, A, &genesis));
    log.keep(sign(
        &log,
        C,
        Body::Admit {
            entrant: moved(A, B, "m1"),
            evidence: Evidence::Phrase,
        },
    ));
    let base = log.clone();
    for n in 0..=MAX_PARENTS {
        log.keep(
            base.draft(
                &C.id(),
                100 + n as u64,
                Body::Remove {
                    member: A.id(),
                    cutoff: genesis,
                },
            )
            .sign(&C.seed(), C.incarnation())
            .unwrap(),
        );
    }
    assert!(log.heads().len() > MAX_PARENTS);
    assert!(log.removed_recipients().contains(&B.identity().recipient()));
    assert!(
        log.draft_epoch(&B.id(), 9, EpochAudience::Everyone, 1)
            .is_err(),
        "an epoch that cannot follow every Remove of the move is drafted"
    );
}

/// A quorum's share opens no gate where it lies in the row's entry's past. B removes A (r1) and,
/// the person pressing restore, lets A in again on its own recipient (the restore, after r1); C,
/// holding neither, removes A too (r2). Of three founders two shares are a quorum, so on C,
/// which holds both, A is out for good: r1 takes the restore as a share. B mints E, which follows
/// r1 and the restore but not r2, and wraps it to A's recipient, current there (r1 alone lies
/// before the restore). The gate before P1b let C seal under E (r1, a taker, is in E's past and
/// no live row binds A's recipient), which A reads; the restore's only taker that takes it by
/// itself is r2, so E is due.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=quorum-some-gate` lets a share open the
/// one-`Remove` clause, and the asserts that E is due fail.
#[test]
fn a_quorums_share_before_a_restore_opens_no_gate() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let genesis = *lab.network();
    let mut b_log = lab.clone();
    let r1 = b_log.keep(remove(&b_log, B, A, &genesis));
    let restore = b_log.keep(admit(&b_log, B, A, "m1"));
    let (epoch, _key) = mint(&mut b_log, B);
    assert!(
        b_log
            .current_recipients(&EpochAudience::Everyone)
            .contains(&A.identity().recipient()),
        "B reads A's recipient as current, and wraps E to it"
    );

    let mut c_log = lab;
    let r2 = c_log.keep(remove(&c_log, C, A, &genesis));
    for record in [&r1, &restore, &epoch] {
        c_log.keep(record.clone());
    }
    assert!(
        !c_log.membership().quorums().is_empty(),
        "r1 and r2 are an accepted quorum on C"
    );
    assert!(
        c_log
            .removed_recipients()
            .contains(&A.identity().recipient())
    );
    assert!(c_log.reaches(&[*epoch.id()], restore.id()));
    assert!(!c_log.reaches(&[*epoch.id()], r2.id()));
    assert!(
        c_log.head_due(epoch.id(), None, &c_log.removed_recipients()),
        "C seals nothing under E"
    );
}

/// An Undo's share counts for the gate only where the Mac that signed it stands with nothing
/// void. D, stolen, removes A, B and C, and each of them signs its share of the Undo (sA, sB, sC,
/// each after D's removal of its signer). E holds sA alone: no quorum, so sA, signed by a Mac D
/// removed, is no removal there, D is current, and E's fresh epoch wraps to D. S holds every
/// share: of five voters three are a quorum, D is out for good, and sA lies in the epoch's past.
/// The gate before this rule read each share by itself as a plain `Remove`, so sA opened the
/// one-`Remove` clause and S sealed under the epoch, which D reads by E's `Wrap`. With nothing
/// void none of the shares is authorized, so the gate needs all three, and the epoch is due. The
/// batch the epoch's minter may still be wrapping counts D too: sA took nothing out there.
///
/// NEGATIVE CONTROLS: `ATERM_TEST_NEGATIVE_CONTROL=alone-unauthorized` counts every standing
/// `Remove` alone, and the assert that the epoch is due fails; `removed-before-unauthorized`
/// leaves out of the batch each Mac any counted `Remove` before the epoch names, and the assert
/// that the batch may still wrap D fails.
#[test]
fn an_undo_share_a_removed_mac_signed_opens_no_gate_where_no_quorum_stands() {
    let lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (D, "m4"),
        (E, "m5"),
        (S, "m6"),
    ]);
    let genesis = *lab.network();
    let d_recipient = D.identity().recipient();
    let mut log = lab;
    // E's first epoch, wrapped to D: the batch E's next epoch may still wrap D.
    let (first, first_key) = mint(&mut log, E);
    log.keep(
        log.draft_wrap(&E.id(), 9, *first.id(), &first_key, d_recipient)
            .unwrap()
            .sign(&E.seed(), E.incarnation())
            .unwrap(),
    );
    for who in [A, B, C] {
        log.keep(remove(&log, D, who, &genesis));
    }
    let cutoff = log.undo_cutoff(&D.id()).expect("an undo cutoff");
    log.keep(remove(&log, A, D, &cutoff));

    let mut e_log = log.clone();
    let (draft, _key) = e_log
        .draft_epoch(&E.id(), 9, EpochAudience::Everyone, 2)
        .unwrap();
    let epoch = e_log.keep(draft.sign(&E.seed(), E.incarnation()).unwrap());
    assert!(e_log.membership().quorums().is_empty());
    assert!(
        e_log
            .current_recipients(&EpochAudience::Everyone)
            .contains(&d_recipient),
        "E, holding one share, reads D as current, and wraps its epoch to D"
    );

    let mut s_log = log;
    for who in [B, C] {
        s_log.keep(remove(&s_log, who, D, &cutoff));
    }
    s_log.keep(epoch.clone());
    assert_eq!(
        s_log.membership().quorums().len(),
        1,
        "the Undo stands on S"
    );
    let removed = s_log.removed_recipients();
    assert!(removed.contains(&d_recipient));
    assert!(
        s_log.head_due(epoch.id(), None, &removed),
        "S seals nothing under E's epoch"
    );
    assert!(
        s_log
            .batch_may_wrap(epoch.id(), &S.identity().recipient())
            .contains(&d_recipient),
        "the rest of E's batch may still wrap D"
    );
}

/// A `Remove` that a Mac an Undo restored signs counts for the batch of an epoch after it: that
/// Mac signs only as a member of its own view, so the Undo's quorum lies in the `Remove`'s past
/// and every minter that follows it holds the quorum. D, stolen, removes A, B and C, and their
/// shares undo it. E mints an epoch it wraps to S; A, back, then removes S, and E rotates. The
/// rotation follows A's `Remove`, so its batch leaves S out on purpose, and the rest of it does
/// not wrap S.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=removed-before-plain` counts only a `Remove`
/// authorized with nothing void, which A's is not, and the assert that the batch leaves S out
/// fails.
#[test]
fn a_remove_a_restored_mac_signed_counts_for_the_batch() {
    let lab = found(&[
        (A, "m1"),
        (B, "m2"),
        (C, "m3"),
        (D, "m4"),
        (E, "m5"),
        (S, "m6"),
    ]);
    let genesis = *lab.network();
    let s_recipient = S.identity().recipient();
    let mut log = lab;
    let (first, first_key) = mint(&mut log, E);
    log.keep(
        log.draft_wrap(&E.id(), 9, *first.id(), &first_key, s_recipient)
            .unwrap()
            .sign(&E.seed(), E.incarnation())
            .unwrap(),
    );
    for who in [A, B, C] {
        log.keep(remove(&log, D, who, &genesis));
    }
    undo_shares(&mut log, D, &[A, B, C]);
    assert_eq!(log.membership().quorums().len(), 1, "the Undo stands");
    let removal = log.keep(remove(&log, A, S, &genesis));
    assert!(log.removed_recipients().contains(&s_recipient));
    let (draft, _key) = log
        .draft_epoch(&E.id(), 9, EpochAudience::Everyone, 2)
        .unwrap();
    let rotation = log.keep(draft.sign(&E.seed(), E.incarnation()).unwrap());
    assert!(log.reaches(&[*rotation.id()], removal.id()));
    assert!(
        !log.batch_may_wrap(rotation.id(), &B.identity().recipient())
            .contains(&s_recipient),
        "the rotation's batch leaves S out on purpose"
    );
}

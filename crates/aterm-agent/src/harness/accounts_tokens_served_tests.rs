// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A token session's served facts and verification by windows
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6, §5.7, §5.8, slice S11): the basis is the
//! latest reading of a session on the row's stored login, recorded by [`note_basis`] and by
//! nothing else, so no token reading is ever its own basis; a reading with no current basis
//! decides nothing: the stored login's next reading compares it at once while its week runs, and
//! otherwise the next token reading is compared afresh; equal windows write `verified` into the
//! scratch vault for the token compared, naming the seat that wrote it, and the token then serves
//! a Mac that did not mint it; unequal windows leave it `unverified (windows differ)`, not in use
//! on the Mac that compared, its readings apart from the row, `token-differ` journaled and a
//! `--replace` offered; shared windows, until a basis of a later week reopens them, and a
//! row-less account, leave it `unverified (<why>)`, a terminal token's shared windows offering its
//! replacement; a token awaiting its basis is held off while the row's stored login works here, and
//! while a replacement waits in the vault's writes the token it replaces is judged by its own facts
//! and nothing of it is written on the waiting token's row, nor any verification of it; a reading
//! taken before its windows reset compares nothing of them; the shared check reads another
//! account's windows only by the resets its sample names; a check file that does not read holds
//! only the tokens this Mac compares; a token another Mac minted in aterm's window is compared and
//! verified here as one minted here is, and one it minted at a terminal is apart until verified;
//! a row whose
//! write was overtaken follows the vault's token; first and last served are written under
//! `tokens.lock`; and renewal is off for a row on its token.

use super::*;
use crate::harness::token_route::{Account, TokenFact, standing_judged};

/// Seconds in a day.
const DAY: i64 = 86_400;

/// The stored login's windows as a reading of a session on it shows them: a week ending in three
/// days, a session in two hours, both after `now`.
fn stored_at(now: u64) -> Windows {
    let now = i64::try_from(now).unwrap();
    Windows {
        week_reset: Some(now + 3 * DAY),
        session_reset: Some(now + 7_200),
    }
}

/// Other windows: another account's, or a token's of another account.
fn others_at(now: u64) -> Windows {
    let now = i64::try_from(now).unwrap();
    Windows {
        week_reset: Some(now + 5 * DAY),
        session_reset: Some(now + 3_600),
    }
}

/// The account's token as this Mac's vault reads it: made here (`here`) or on m7, not verified,
/// its meta naming no route (an earlier build's).
fn unverified(here: bool) -> Standing {
    match standing(Check::Readable, NOW + A_YEAR, false, here, &[]) {
        Answer::Found(standing) => *standing,
        Answer::Absent | Answer::Unknown => unreachable!(),
    }
}

/// [`unverified`], its meta naming `by` as the route that made it.
fn unverified_by(here: bool, by: MintedBy) -> Standing {
    let mut token = unverified(here);
    token.meta.as_mut().expect("a meta").minted_by = Some(by);
    token
}

fn served(reading: Windows, others: Vec<Windows>) -> Served {
    Served {
        label: "work".to_owned(),
        reading,
        others: labelled(&others),
        named: true,
    }
}

/// `windows`, each another account's current windows here, under a row of its own: the first
/// `other-0`, the next `other-1`.
fn labelled(windows: &[Windows]) -> Vec<Other> {
    windows
        .iter()
        .enumerate()
        .map(|(at, windows)| Other {
            label: format!("other-{at}"),
            windows: *windows,
        })
        .collect()
}

/// The row a mint of `token` for `work` leaves in this Mac's table: its label, its `minted_at`,
/// nothing compared yet.
fn minted_row(store: &Store, token: &Standing) {
    update_table(store, |table| {
        let (row, _) = row_of_token(table, token, token.meta.as_ref().expect("a meta"));
        row.label = Some("work".to_owned());
    })
    .expect("the mint's row");
}

/// A session on `work`'s stored login reported `windows` at `at`, no other account's windows
/// current here: whether it became the basis.
fn stored_reading(store: &Store, token: &Standing, windows: Windows, at: u64) -> bool {
    note_basis(store, token, "work", &windows, &[], at)
        .expect("the table")
        .recorded()
}

/// THE TOKEN'S OWN READING IS NEVER ITS BASIS, AND A STORED LOGIN'S IS (§5.6): on the row a mint
/// left, a token session's readings record no basis: the first is kept with `no-basis`, the token
/// reads `unverified (awaiting a stored-login reading)`, and the next one, the same windows, is
/// still compared with nothing. A stored-login reading then records the basis and compares the
/// kept reading, another account's windows, with it: `differ`, final, so the token's next reading
/// is apart from the row and a later stored-login reading records nothing over it. NEGATIVE
/// CONTROL: had the token's own windows been taken as the basis, its next reading would leave the
/// write due.
#[test]
fn a_tokens_reading_is_never_its_basis_and_a_stored_logins_is() {
    let scratch = Scratch::new("basis");
    let store = scratch.store();
    let token = unverified(true);
    let found = Lookup::Found(token.clone());
    let (stored, theirs) = (stored_at(NOW), others_at(NOW));
    minted_row(&store, &token);
    assert_eq!(
        TokenState::of(&found, scratch.table().rows.get(KEY), NOW).words(),
        "unverified (awaiting a stored-login reading)"
    );
    let _ = notes();
    assert_eq!(
        note_served(&store, &token, &served(theirs, vec![]), NOW).unwrap(),
        Due::Nothing,
        "nothing to compare the first reading with"
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        row.verification,
        Verification {
            basis: None,
            reading: Some(theirs),
            verdict: Some(Verdict::NoBasis),
            shared_with: BTreeSet::new(),
        }
    );
    assert_eq!(
        note_served(&store, &token, &served(theirs, vec![]), NOW + 60).unwrap(),
        Due::Nothing,
        "the token's own reading is still compared with nothing"
    );
    let table = scratch.table();
    assert_eq!(table.rows[KEY].verification.basis, None);
    assert_eq!(
        TokenState::of(&found, table.rows.get(KEY), NOW + 60).words(),
        "unverified (awaiting a stored-login reading)"
    );

    let control = Scratch::new("basis-control");
    let control_store = control.store();
    minted_row(&control_store, &token);
    assert!(stored_reading(&control_store, &token, theirs, NOW));
    assert!(
        matches!(
            note_served(&control_store, &token, &served(theirs, vec![]), NOW + 60).unwrap(),
            Due::Verify(_)
        ),
        "NEGATIVE CONTROL: a token's own windows as its basis verify it"
    );

    assert!(stored_reading(&store, &token, stored, NOW + 90));
    let table = scratch.table();
    assert_eq!(table.rows[KEY].verification.basis, Some((NOW + 90, stored)));
    assert_eq!(
        table.rows[KEY].verification.verdict,
        Some(Verdict::Differ),
        "the kept reading is compared with the basis"
    );
    assert_eq!(
        TokenState::of(&found, table.rows.get(KEY), NOW + 90).words(),
        "unverified (windows differ)"
    );
    assert_eq!(
        note_served(&store, &token, &served(theirs, vec![]), NOW + 120).unwrap(),
        Due::Apart
    );
    assert!(
        !stored_reading(&store, &token, stored_at(NOW + 150), NOW + 150),
        "a final verdict takes no basis"
    );
    assert_eq!(
        scratch.table().rows[KEY].verification.basis,
        Some((NOW + 90, stored))
    );
    let noted = notes();
    assert_eq!(
        noted.iter().filter(|n| n.kind == "token-served").count(),
        2,
        "one first-served note for each table: {noted:?}"
    );
}

/// A BASIS IS A STORED-LOGIN READING OF A WEEK STILL RUNNING, FOR THIS MAC'S OWN UNVERIFIED TOKEN,
/// ON THIS MAC'S ROW OF IT: with no row of the account [`basis_wanted`] looks for nothing, and a
/// reading [`note_basis`] is handed makes the token's row and is its basis; a row of an older
/// token follows the vault's newer one and takes it; where the account's row is of a newer token
/// (its write on its way), the vault's token's kept row takes it and the row keeps its own. It
/// records nothing for a token made on another Mac, verified or refused here (its standing's
/// `refused_at`, which the cheap look does not read), for a reading that names no week or one that
/// ended, and on a row removed or with a final verdict, whatever the row says of `verified` (the
/// vault's meta says that); a newer reading replaces an older basis.
/// CONTROL: the minted row and a reading of a running week.
#[test]
fn a_basis_is_a_running_week_of_this_macs_own_unverified_token() {
    let token = unverified(true);
    let minted_at = token.meta.as_ref().expect("a meta").minted_at;
    let stored = stored_at(NOW);
    let scratch = Scratch::new("basis-rules");
    let store = scratch.store();
    assert!(!basis_wanted(&store, &key()), "no row");
    assert!(
        stored_reading(&store, &token, stored, NOW),
        "no row: one made"
    );
    assert_eq!(scratch.table().rows[KEY].minted_at, Some(minted_at));
    assert_eq!(
        scratch.table().rows[KEY].verification.basis,
        Some((NOW, stored))
    );

    let mut older = token.clone();
    older.meta.as_mut().expect("a meta").minted_at -= 600;
    let followed = Scratch::new("basis-rules-follow");
    minted_row(&followed.store(), &older);
    assert!(
        stored_reading(&followed.store(), &token, stored, NOW),
        "a row of an older token follows the vault's"
    );
    assert_eq!(followed.table().rows[KEY].minted_at, Some(minted_at));
    let kept = Scratch::new("basis-rules-kept");
    let mut newer = token.clone();
    newer.meta.as_mut().expect("a meta").minted_at += 600;
    minted_row(&kept.store(), &newer);
    update_table(&kept.store(), |table| {
        table.rows.get_mut(KEY).expect("the row").vault = InVault::Waiting("waits".to_owned());
    })
    .unwrap();
    assert!(basis_wanted(&kept.store(), &key()));
    assert!(stored_reading(&kept.store(), &token, stored, NOW));
    let table = kept.table();
    assert_eq!(
        (
            table.rows[KEY].minted_at,
            table.rows[KEY].verification.basis
        ),
        (Some(minted_at + 600), None),
        "the row of the token on its way keeps its own"
    );
    assert_eq!(
        (
            table.replaced[KEY].minted_at,
            table.replaced[KEY].verification.basis
        ),
        (Some(minted_at), Some((NOW, stored))),
        "the vault's token's kept row takes it"
    );

    let scratch = Scratch::new("basis-rules-row");
    let store = scratch.store();
    minted_row(&store, &token);
    assert!(basis_wanted(&store, &key()));
    for (why, standing) in [
        ("made on m7, naming no route", unverified(false)),
        (
            "made on m7 at a terminal",
            unverified_by(false, MintedBy::Terminal),
        ),
        (
            "verified",
            match standing(Check::Readable, NOW + A_YEAR, true, true, &[]) {
                Answer::Found(standing) => *standing,
                Answer::Absent | Answer::Unknown => unreachable!(),
            },
        ),
        (
            "refused here",
            Standing {
                refused_at: Some(NOW - 60),
                ..token.clone()
            },
        ),
    ] {
        assert!(!stored_reading(&store, &standing, stored, NOW), "{why}");
    }
    for (why, reading) in [
        ("no week", Windows::default()),
        ("an ended week", stored_at(NOW - 4 * 86_400)),
    ] {
        assert!(!stored_reading(&store, &token, reading, NOW), "{why}");
    }
    assert_eq!(scratch.table().rows[KEY].verification.basis, None);

    assert!(stored_reading(&store, &token, stored, NOW), "CONTROL");
    let newer = stored_at(NOW + 600);
    assert!(stored_reading(&store, &token, newer, NOW + 600));
    assert_eq!(
        scratch.table().rows[KEY].verification.basis,
        Some((NOW + 600, newer)),
        "the newer reading replaces the basis"
    );

    let reopen = |row: &mut TokenRow| {
        row.verified = None;
        row.removed_at = None;
        row.verification.verdict = None;
    };
    for (why, close) in [
        (
            "a final verdict",
            (|row: &mut TokenRow| row.verification.verdict = Some(Verdict::Equal))
                as fn(&mut TokenRow),
        ),
        ("removed", |row| row.removed_at = Some(NOW)),
    ] {
        update_table(&store, |table| {
            let row = table.rows.get_mut(KEY).expect("the row");
            reopen(row);
            close(row);
        })
        .unwrap();
        assert!(!basis_wanted(&store, &key()), "{why}");
        assert!(
            !stored_reading(&store, &token, stored_at(NOW + 900), NOW + 900),
            "{why}"
        );
    }
    update_table(&store, |table| {
        let row = table.rows.get_mut(KEY).expect("the row");
        reopen(row);
        row.verified = Some(NOW);
    })
    .unwrap();
    assert!(
        basis_wanted(&store, &key()),
        "a row an earlier build marked verified, its meta's stamp counting for none here"
    );
    assert!(stored_reading(
        &store,
        &token,
        stored_at(NOW + 900),
        NOW + 900
    ));
}

/// NO BASIS IS NOT A FINAL VERDICT, AND THE READING COMPARED IS THE ONE THAT MEETS THE BASIS: a
/// token's first served reading finds no basis and is kept with `no-basis`; a stored-login
/// reading then records a basis, and the token's next reading, showing the same windows, leaves
/// the write due for the token compared (its `minted_at`), on that reading's resets. Windows
/// found different are never compared again. NEGATIVE CONTROL: the kept first reading of a week
/// that has since ended, compared with the new basis, reads `differ`, which would end
/// verification for a token of the right account.
#[test]
fn no_basis_is_not_final_and_the_reading_compared_meets_the_basis() {
    let token = unverified(true);
    let old = stored_at(NOW);
    let later = NOW + 4 * 86_400;
    let new = stored_at(later);
    let at = i64::try_from(later).unwrap();
    assert!(old.week_reset.is_some_and(|reset| reset < at));

    let scratch = Scratch::new("ended-week");
    let store = scratch.store();
    minted_row(&store, &token);
    assert_eq!(
        note_served(&store, &token, &served(old, vec![]), NOW).unwrap(),
        Due::Nothing
    );
    assert!(stored_reading(&store, &token, new, later - 30));
    assert_eq!(
        Verdict::of(Some(&new), &old, &[], at),
        Verdict::Differ,
        "NEGATIVE CONTROL: the kept first reading is of the week that ended"
    );
    let Due::Verify(verify) = note_served(&store, &token, &served(new, vec![]), later).unwrap()
    else {
        panic!("the reading that meets the basis is the one compared");
    };
    assert_eq!(
        verify.minted_at,
        token.meta.as_ref().unwrap().minted_at,
        "the evidence names the token compared"
    );
    assert_eq!(verify.evidence.by, aterm_secrets::claude_token::BY_WINDOWS);
    assert_eq!(verify.evidence.at, later);
    assert_eq!(
        verify.evidence.seven_day_resets_at,
        new.week_reset.and_then(|r| u64::try_from(r).ok())
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.verification.reading, Some(new));
    assert_eq!(row.verification.verdict, Some(Verdict::Equal));

    let differ = Scratch::new("differ-final");
    let store = differ.store();
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, others_at(NOW), NOW));
    assert_eq!(
        note_served(&store, &token, &served(old, vec![]), NOW + 60).unwrap(),
        Due::Apart
    );
    assert!(!stored_reading(&store, &token, old, NOW + 90));
    assert_eq!(
        note_served(&store, &token, &served(old, vec![]), NOW + 120).unwrap(),
        Due::Apart,
        "windows found different are never compared again"
    );
    let table = differ.table();
    assert_eq!(table.rows[KEY].verification.verdict, Some(Verdict::Differ));
    assert_eq!(table.rows[KEY].verified, None, "nothing wrote verified");
}

/// A STORED-LOGIN READING THAT COMES AFTER THE TOKEN'S OWN READING COMPARES THAT READING AT ONCE
/// (§5.6): on the Mac that minted the token, a token session's reading of a week still running
/// finds no basis and is kept (the token served while its row's stored login was not usable
/// here). The stored login's reading, when it comes, records the basis and compares the kept
/// reading with it there and then, as a token reading after it would be: another account's
/// windows are `differ` before any later token session, so the token is in use on no Mac and no
/// session moves onto it; the account's own windows are `equal`, and the write of `verified` is
/// due at once, for the token compared, on the kept reading's resets; windows another account
/// here shows too are `shared`. CONTROL: a kept reading whose week has ended since is compared
/// with nothing, and `no-basis` stays for the next token reading.
#[test]
fn a_stored_login_reading_after_the_tokens_compares_the_kept_reading_at_once() {
    let token = unverified(true);
    let found = Lookup::Found(token.clone());
    let (stored, theirs) = (stored_at(NOW), others_at(NOW));
    let kept_then_based =
        |name: &str, kept: (Windows, u64), basis: (Windows, u64), others: &[Windows]| {
            let scratch = Scratch::new(name);
            let store = scratch.store();
            minted_row(&store, &token);
            assert_eq!(
                note_served(&store, &token, &served(kept.0, vec![]), kept.1).unwrap(),
                Due::Nothing,
                "{name}: nothing to compare the token's reading with yet"
            );
            let based =
                note_basis(&store, &token, "work", &basis.0, &labelled(others), basis.1).unwrap();
            (based, scratch.table().rows.remove(KEY).expect("the row"))
        };

    let (based, row) = kept_then_based("after-differ", (theirs, NOW), (stored, NOW + 60), &[]);
    assert_eq!(based, Based::Recorded);
    assert_eq!(
        row.verification.verdict,
        Some(Verdict::Differ),
        "compared at once"
    );
    let state = TokenState::of(&found, Some(&row), NOW + 60);
    assert_eq!(state.words(), "unverified (windows differ)");
    assert!(!state.in_use());

    let (based, row) = kept_then_based("after-equal", (stored, NOW), (stored, NOW + 60), &[]);
    assert_eq!(
        row.verification.verdict,
        Some(Verdict::Equal),
        "compared at once"
    );
    let Based::Verify(verify) = based else {
        panic!("the write is due at once: {based:?}");
    };
    assert_eq!(verify.minted_at, token.meta.as_ref().unwrap().minted_at);
    assert_eq!(verify.evidence.by, aterm_secrets::claude_token::BY_WINDOWS);
    assert_eq!(verify.evidence.at, NOW + 60);
    assert_eq!(
        verify.evidence.seven_day_resets_at,
        stored.week_reset.and_then(|r| u64::try_from(r).ok())
    );

    let (based, row) =
        kept_then_based("after-shared", (stored, NOW), (stored, NOW + 60), &[stored]);
    assert_eq!(based, Based::Recorded, "nothing to write");
    assert_eq!(row.verification.verdict, Some(Verdict::Shared));

    let later = NOW + 4 * 86_400;
    let (based, row) =
        kept_then_based("after-ended", (stored, NOW), (stored_at(later), later), &[]);
    assert_eq!(based, Based::Recorded, "CONTROL: the basis is recorded");
    assert_eq!(
        row.verification.verdict,
        Some(Verdict::NoBasis),
        "CONTROL: a kept reading of a week that ended compares nothing"
    );
}

/// A BASIS WHOSE WEEK HAS ENDED IS NO BASIS: the token awaits a stored-login reading again, and
/// its reading then, of the week now running, is compared with nothing. CONTROL: before that week
/// ends, the same basis is current, the token reads `no reading yet`, and its reading showing
/// those windows leaves the write due.
#[test]
fn a_basis_whose_week_ended_is_none() {
    let token = unverified(true);
    let found = Lookup::Found(token.clone());
    let old = stored_at(NOW);
    let later = NOW + 4 * 86_400;
    let scratch = Scratch::new("ended-basis");
    let store = scratch.store();
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, old, NOW));
    let table = scratch.table();
    assert_eq!(
        TokenState::of(&found, table.rows.get(KEY), NOW + 60).words(),
        "unverified (no reading yet)",
        "CONTROL: the basis is current"
    );
    assert_eq!(
        TokenState::of(&found, table.rows.get(KEY), later).words(),
        "unverified (awaiting a stored-login reading)"
    );
    assert_eq!(
        note_served(&store, &token, &served(stored_at(later), vec![]), later).unwrap(),
        Due::Nothing,
        "a basis whose week has ended compares nothing"
    );
    assert_eq!(
        scratch.table().rows[KEY].verification.verdict,
        Some(Verdict::NoBasis)
    );

    let control = Scratch::new("current-basis");
    let store = control.store();
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, old, NOW));
    assert!(matches!(
        note_served(&store, &token, &served(old, vec![]), NOW + 60).unwrap(),
        Due::Verify(_)
    ));
}

/// EQUAL WINDOWS ON THE MINTING MAC WRITE `verified` FOR THE TOKEN COMPARED, AND IT THEN SERVES A
/// MAC THAT DID NOT MINT IT: on a scratch seated vault (m22) the token m22 minted reads `unverified
/// (awaiting a stored-login reading)` on the row its mint left; a stored-login reading records the
/// basis, and a token reading showing the same windows leaves the write due, which writes the
/// meta's `verified` (by `windows`, the reading's resets) in one draft and journals
/// `token-verified`. The vault then reads it verified, so it is `active` here and on a Mac that
/// did not mint it. NEGATIVE CONTROL: the same evidence naming another `minted_at` (a token the
/// vault no longer holds) writes nothing, here or in the vault.
#[test]
fn equal_windows_on_the_minting_mac_write_verified_and_the_token_turns_active() {
    let scratch = Scratch::new("verify");
    let lab = Lab::new();
    let now = aterm_time::unix_secs().unwrap();
    let meta = Meta {
        email: EMAIL.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: now - 60,
        expires_at: now + A_YEAR,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let written = claude_token::write(
        &lab.paths,
        &key(),
        &SecretBytes::from(sentinel().as_bytes()),
        &meta,
        OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the write");
    assert_eq!(written, MemberWritten::Posted);
    let found = claude_token::standing(&lab.paths, &key()).expect("a standing");
    let Lookup::Found(minted) = &found else {
        panic!("the vault holds the token");
    };
    assert!(minted.minted_here && !minted.verified());
    let store = scratch.store();
    minted_row(&store, minted);
    assert_eq!(
        TokenState::of(&found, scratch.table().rows.get(KEY), now).words(),
        "unverified (awaiting a stored-login reading)"
    );
    let elsewhere = Standing {
        minted_here: false,
        ..minted.clone()
    };
    assert!(
        matches!(
            elsewhere.state(now),
            aterm_secrets::claude_token::State::Unverified {
                minted_here: false,
                minted_by: None,
                ..
            }
        ) && !elsewhere.serves_here(),
        "PRECONDITION: unverified, its meta naming no route, it serves no other Mac"
    );

    let _ = notes();
    let stored = stored_at(now);
    assert!(stored_reading(&store, minted, stored, now));
    let Due::Verify(verify) =
        note_served(&store, minted, &served(stored, vec![others_at(now)]), now).unwrap()
    else {
        panic!("equal windows leave the write due");
    };
    assert_eq!(verify.minted_at, meta.minted_at);
    assert_eq!(verify.evidence.by, aterm_secrets::claude_token::BY_WINDOWS);
    assert!(
        verify.evidence.on.is_empty(),
        "the evidence names no Mac: the vault names the seat that writes it"
    );
    assert_eq!(
        verify.evidence.seven_day_resets_at,
        stored.week_reset.and_then(|r| u64::try_from(r).ok())
    );
    let stale = Verify {
        minted_at: meta.minted_at - 600,
        ..verify.clone()
    };
    assert!(
        !verify_served(&store, &Vault(lab.paths.clone()), &key(), stale, now).unwrap(),
        "NEGATIVE CONTROL: evidence of a token the vault no longer holds"
    );
    assert_eq!(scratch.table().rows[KEY].verified, None);
    assert!(matches!(
        claude_token::standing(&lab.paths, &key()),
        Ok(Lookup::Found(standing)) if !standing.verified()
    ));
    assert!(
        verify_served(&store, &Vault(lab.paths.clone()), &key(), verify, now).unwrap(),
        "the minting Mac writes it"
    );

    let found = claude_token::standing(&lab.paths, &key()).expect("a standing");
    let Lookup::Found(verified) = &found else {
        panic!("the vault holds the token");
    };
    assert!(verified.verified(), "the vault's meta says verified");
    assert_eq!(
        verified
            .meta
            .as_ref()
            .and_then(Meta::verification)
            .map(|v| v.on.as_str()),
        Some("m22"),
        "by the seat that wrote it"
    );
    assert_eq!(verified.check, Check::Readable, "one draft");
    let table = scratch.table();
    let row = table.rows.get(KEY).expect("the row");
    assert_eq!(row.verified, Some(now));
    assert_eq!(TokenState::of(&found, Some(row), now), TokenState::Active);
    let elsewhere = Standing {
        minted_here: false,
        ..verified.clone()
    };
    assert_eq!(
        TokenState::of(&Lookup::Found(elsewhere), None, now),
        TokenState::Active,
        "it now serves a Mac that did not mint it"
    );
    assert!(lab.holds_the_token(), "the same token");
    let kinds: Vec<&str> = notes().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["token-served", "token-verified"]);
    assert_eq!(
        note_served(&store, verified, &served(stored, vec![]), now + 60).unwrap(),
        Due::Nothing,
        "a verified token compares nothing again"
    );
}

/// A VERIFICATION THAT WAITS FOR THE VAULT PASS COUNTS AS WRITTEN (§5.6): it stands on the pair
/// it read, so the pass signs it or drops it whole, never putting back a token or meta that
/// changed since; a waiting verification is recorded on the token's row and journalled
/// `token-verified`, as a signed one is. CONTROL: a refused one records and journals nothing.
#[test]
fn a_verification_that_waits_for_the_vault_pass_counts_as_written() {
    let scratch = Scratch::new("verify-waits");
    let store = scratch.store();
    let token = unverified(true);
    let minted_at = token.meta.as_ref().expect("a meta").minted_at;
    minted_row(&store, &token);
    let verify = Verify {
        minted_at,
        evidence: aterm_secrets::claude_token::Verified {
            on: "m22".to_owned(),
            at: NOW,
            by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
            seven_day_resets_at: None,
            five_hour_resets_at: None,
        },
    };
    let refusing = FakeVault::new(Answer::Found(Box::new(token.clone())));
    let waiting = FakeVault {
        restamped: Some(MemberWritten::Waiting(WaitFor::RotationDue)),
        ..refusing.clone()
    };
    let _ = notes();
    assert!(
        !verify_served(&store, &refusing, &key(), verify.clone(), NOW).unwrap(),
        "CONTROL: refused"
    );
    assert_eq!(scratch.table().rows[KEY].verified, None, "CONTROL");
    assert!(notes().is_empty(), "CONTROL: nothing journalled");
    assert!(
        verify_served(&store, &waiting, &key(), verify, NOW).unwrap(),
        "a waiting verification is written"
    );
    assert_eq!(scratch.table().rows[KEY].verified, Some(NOW));
    let kinds: Vec<&str> = notes().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["token-verified"]);
}

/// UNEQUAL WINDOWS LEAVE `unverified (windows differ)`, NOT IN USE ON THIS MAC, ITS READINGS APART
/// FROM THE ROW, AND A REPLACEMENT OFFERED: the reading that differs from the stored login's basis,
/// and every later one (with or without rate limits, and one that does show the basis's windows:
/// the first reading compared decided it), is left apart, so its caller folds none into the row;
/// no session here starts on the token, a bare run of the verb says why and names `--replace`, and
/// a surface offers `--replace`. CONTROL: the same token with no verdict is in use on its minting
/// Mac, and so is a newer token made here whose row could not be written: the verdict is the
/// older token's.
#[test]
fn unequal_windows_leave_unverified_windows_differ() {
    let token = unverified(true);
    let stored = stored_at(NOW);
    let mut session_off = stored;
    session_off.session_reset = session_off.session_reset.map(|r| r + 60);
    let based = |tag: &str| {
        let scratch = Scratch::new(tag);
        minted_row(&scratch.store(), &token);
        assert!(stored_reading(&scratch.store(), &token, stored, NOW));
        scratch
    };
    for (why, reading) in [
        ("another week", others_at(NOW)),
        ("the week, but another current session", session_off),
    ] {
        let scratch = based("differ-case");
        assert_eq!(
            note_served(&scratch.store(), &token, &served(reading, vec![]), NOW).unwrap(),
            Due::Apart,
            "{why}"
        );
        assert_eq!(
            scratch.table().rows[KEY].verification.verdict,
            Some(Verdict::Differ),
            "{why}"
        );
    }
    let mut no_session = stored;
    no_session.session_reset = None;
    assert!(
        matches!(
            note_served(
                &based("no-session").store(),
                &token,
                &served(no_session, vec![]),
                NOW
            )
            .unwrap(),
            Due::Verify(_)
        ),
        "CONTROL: the week alone compares where the reading names no session"
    );

    let scratch = based("differ-state");
    let store = scratch.store();
    note_served(&store, &token, &served(others_at(NOW), vec![]), NOW).unwrap();
    assert_eq!(
        note_served(&store, &token, &served(stored, vec![]), NOW + 60).unwrap(),
        Due::Apart,
        "the first reading decided it"
    );
    assert_eq!(
        note_served(
            &store,
            &token,
            &served(Windows::default(), vec![]),
            NOW + 90
        )
        .unwrap(),
        Due::Apart,
        "a later reading with no rate limits is the same token's"
    );
    let table = scratch.table();
    let row = table.rows.get(KEY);
    assert_eq!(
        row.expect("the row").verified,
        None,
        "nothing wrote verified"
    );
    let here = Lookup::Found(token.clone());
    let unread = TokenState::of(&here, None, NOW);
    assert_eq!(
        unread.words(),
        "unverified (awaiting a stored-login reading)"
    );
    assert!(
        unread.in_use(),
        "CONTROL: before a verdict it serves the minting Mac (where no stored login holds it)"
    );
    let state = TokenState::of(&here, row, NOW);
    assert_eq!(state.words(), "unverified (windows differ)");
    assert!(!state.in_use(), "it does not serve the minting Mac");
    assert_eq!(
        crate::harness::token_route::fact_of(&state),
        TokenFact::Differ
    );
    let refusal = state
        .bare_refusal(EMAIL, None, false)
        .expect("a bare run is refused");
    assert!(
        refusal.contains("this Mac starts no session on it") && refusal.contains("`--replace`"),
        "{refusal}"
    );
    assert_eq!(
        state.offer("work", false),
        Some(Step::Token {
            label: "work".to_owned(),
            replace: true,
        }),
        "a surface offers its replacement"
    );
    assert_eq!(
        state.offer("work", true),
        None,
        "not while one is on its way"
    );
    let mut newer = token.clone();
    let meta = newer.meta.as_mut().expect("a meta");
    meta.minted_at += 60;
    let newer = TokenState::of(&Lookup::Found(newer), row, NOW);
    assert_eq!(
        newer.words(),
        "unverified (awaiting a stored-login reading)"
    );
    assert!(
        newer.in_use(),
        "CONTROL: a verdict of the token before it says nothing of a newer one"
    );
    let elsewhere = Lookup::Found(Standing {
        minted_here: false,
        ..token
    });
    assert!(
        !TokenState::of(&elsewhere, None, NOW).in_use(),
        "and no other Mac, its meta naming no route"
    );
}

/// EQUAL WINDOWS ANOTHER ACCOUNT HERE SHOWS TOO LEAVE `unverified (windows shared)`: equality
/// cannot tell the two accounts apart, so nothing writes `verified`. The token, whose meta names
/// no route, keeps serving its minting Mac alone, so the surfaces there offer its replacement and
/// a bare run says why. CONTROL: a token aterm's window made, whose mint proved the account, is
/// offered nothing for the same reading; another account's different windows leave the write due.
#[test]
fn windows_another_account_shares_leave_unverified_windows_shared() {
    let token = unverified(true);
    let stored = stored_at(NOW);
    let scratch = Scratch::new("shared");
    let store = scratch.store();
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, stored, NOW));
    assert_eq!(
        note_served(
            &store,
            &token,
            &served(stored, vec![others_at(NOW), stored]),
            NOW
        )
        .unwrap(),
        Due::Nothing
    );
    let table = scratch.table();
    assert_eq!(table.rows[KEY].verification.verdict, Some(Verdict::Shared));
    assert_eq!(table.rows[KEY].verified, None, "nothing wrote verified");
    let state = TokenState::of(&Lookup::Found(token.clone()), table.rows.get(KEY), NOW);
    assert_eq!(state.words(), "unverified (windows shared)");
    assert!(state.in_use(), "it serves the minting Mac");
    assert_eq!(
        state.offer("work", false),
        Some(Step::Token {
            label: "work".to_owned(),
            replace: true,
        }),
        "a token no route proved, never verified, is offered its replacement"
    );
    let refusal = state
        .bare_refusal(EMAIL, None, false)
        .expect("a bare run is refused");
    assert!(
        refusal.contains("could not tell the accounts apart") && refusal.contains("`--replace`"),
        "{refusal}"
    );
    let window = unverified_by(true, MintedBy::Window);
    let made = Scratch::new("shared-window");
    minted_row(&made.store(), &window);
    assert!(stored_reading(&made.store(), &window, stored, NOW));
    note_served(
        &made.store(),
        &window,
        &served(stored, vec![others_at(NOW), stored]),
        NOW,
    )
    .unwrap();
    let state = TokenState::of(
        &Lookup::Found(window.clone()),
        made.table().rows.get(KEY),
        NOW,
    );
    assert_eq!(state.words(), "unverified (windows shared)");
    assert!(state.in_use(), "CONTROL: a window token serves every Mac");
    assert_eq!(
        state.offer("work", false),
        None,
        "CONTROL: its mint proved the account, so nothing is offered"
    );
    let control = Scratch::new("shared-control");
    minted_row(&control.store(), &token);
    assert!(stored_reading(&control.store(), &token, stored, NOW));
    assert!(matches!(
        note_served(
            &control.store(),
            &token,
            &served(stored, vec![others_at(NOW)]),
            NOW
        )
        .unwrap(),
        Due::Verify(_)
    ));
}

/// A ROW-LESS ACCOUNT'S TOKEN STAYS `unverified (no roster row)`: its table row names no label
/// (the verb ran on an email no row names), so no stored-login reading under a row compares, and
/// it serves only its minting Mac. CONTROL: with a label it awaits a stored-login reading.
#[test]
fn a_row_less_accounts_token_stays_unverified_no_roster_row() {
    let scratch = Scratch::new("rowless");
    let store = scratch.store();
    let token = unverified(true);
    update_table(&store, |table| {
        let (row, _) = row_of_token(table, &token, token.meta.as_ref().unwrap());
        row.label = None;
    })
    .unwrap();
    let table = scratch.table();
    let found = Lookup::Found(token.clone());
    let state = TokenState::of(&found, table.rows.get(KEY), NOW);
    assert_eq!(state.words(), "unverified (no roster row)");
    assert!(state.in_use());
    let mut labelled = table.rows[KEY].clone();
    labelled.label = Some("work".to_owned());
    assert_eq!(
        TokenState::of(&found, Some(&labelled), NOW).words(),
        "unverified (awaiting a stored-login reading)"
    );
}

/// The token route's judgment of `found` for `work` on this scratch Mac at `now`: its table and
/// accounts state, `roster`, and the scratch home's login records.
fn judged(scratch: &Scratch, found: &Lookup<Standing>, roster: &Roster, now: u64) -> TokenFact {
    standing_judged(
        found,
        &Paths::locate_for_home(&scratch.path().join("home")),
        &scratch.store(),
        Some(roster),
        &key(),
        &Account {
            label: Some("work".to_owned()),
            email: EMAIL.to_owned(),
        },
        now,
    )
}

/// The default login's record under the scratch home, naming `email`.
fn login_record(scratch: &Scratch, email: &str) {
    std::fs::write(
        scratch.path().join("home/.claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}}}}"#),
    )
    .expect("the login record");
}

/// AN UNREADABLE CHECK FILE HOLDS ONLY THE TOKENS THIS MAC COMPARES (§5.1): this Mac's check file
/// is of a later schema, so it does not read. A token another Mac minted and verified is judged by
/// `tokens.json` alone: it is in use, a reading of a session on it records its served facts and a
/// refusal records `rejected`, both in `tokens.json`, and the check file's bytes stay as they
/// were. A token minted here, whose verdict (`differ`) lives in the check file, is `busy`: its
/// reading and its refusal are errors, and neither file is written. The failure is said once,
/// with the file and the reason, however often the table is read, and again once the file has
/// read and then failed again. CONTROL: with the check file readable again, the token minted
/// here is judged by its verdict there. A token another Mac minted in aterm's window and nobody
/// verified is compared here as one minted here is, so it is `busy` too and its reading is not
/// folded. NEGATIVE CONTROL (applied to [`TableRead::for_token`], then reverted): holding every
/// token to the whole table, as before, turns the token minted elsewhere `busy`, `left: Busy`.
#[test]
fn an_unreadable_check_file_holds_only_the_tokens_this_mac_compares() {
    let roster_of =
        |scratch: &Scratch| Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml"));
    let later = br#"{"schema":2,"windows_check":{}}"#;
    let unreadable = |store: &Store| {
        store.ensure().expect("the store");
        aterm_uds::private_file::write_private(&store.shared_dir(), CHECK_FILE, later)
            .expect("a later build's check file");
    };
    let bytes = |store: &Store, file: &str| std::fs::read(store.shared_dir().join(file)).ok();
    UNCHECKED_SAID.with(|said| said.borrow_mut().clear());

    let scratch = Scratch::new("unchecked-elsewhere");
    let store = scratch.store();
    let roster = roster_of(&scratch).unwrap();
    let elsewhere = match standing(Check::Readable, NOW + A_YEAR, true, false, &[]) {
        Answer::Found(standing) => *standing,
        Answer::Absent | Answer::Unknown => unreachable!(),
    };
    let found = Lookup::Found(elsewhere.clone());
    unreadable(&store);
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::InUse,
        "a token minted elsewhere is judged by tokens.json alone"
    );
    assert_eq!(
        note_served(&store, &elsewhere, &served(others_at(NOW), vec![]), NOW).unwrap(),
        Due::Nothing,
        "its reading is recorded"
    );
    let row = read_parts(&store)
        .unwrap()
        .table
        .rows
        .remove(KEY)
        .expect("the row");
    assert_eq!(row.used_on.last_served_at, Some(NOW));
    let paths = Paths::locate_for_home(&scratch.path().join("home"));
    assert!(
        note_rejected(&paths, &store, &elsewhere, 401, NOW + 60).unwrap(),
        "its refusal is recorded"
    );
    let refused = Lookup::Found(Standing {
        refused_at: Some(NOW + 60),
        ..elsewhere.clone()
    });
    assert_eq!(
        judged(&scratch, &refused, &roster, NOW + 90),
        TokenFact::Rejected
    );
    assert_eq!(
        bytes(&store, CHECK_FILE).as_deref(),
        Some(&later[..]),
        "the check file is never written over"
    );
    let said = UNCHECKED_SAID.with(|said| said.borrow().clone());
    assert_eq!(said.len(), 1, "said once for every read: {said:?}");
    assert!(
        said[0].contains(CHECK_FILE) && said[0].contains("schema 1"),
        "{said:?}"
    );

    let scratch = Scratch::new("unchecked-here");
    let store = scratch.store();
    let roster = roster_of(&scratch).unwrap();
    let here = unverified(true);
    let found = Lookup::Found(here.clone());
    minted_row(&store, &here);
    assert!(stored_reading(&store, &here, stored_at(NOW), NOW));
    assert_eq!(
        note_served(&store, &here, &served(others_at(NOW), vec![]), NOW).unwrap(),
        Due::Apart
    );
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::Differ,
        "PRECONDITION"
    );
    let checked = bytes(&store, CHECK_FILE).expect("the check file");
    unreadable(&store);
    let table = bytes(&store, TABLE_FILE);
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 60),
        TokenFact::Busy,
        "its verdict is not known"
    );
    assert!(
        note_served(&store, &here, &served(stored_at(NOW), vec![]), NOW + 60).is_err(),
        "its reading is not folded"
    );
    let paths = Paths::locate_for_home(&scratch.path().join("home"));
    assert!(note_rejected(&paths, &store, &here, 401, NOW + 60).is_err());
    assert_eq!(bytes(&store, CHECK_FILE).as_deref(), Some(&later[..]));
    assert_eq!(bytes(&store, TABLE_FILE), table, "nothing is written");
    assert_eq!(
        UNCHECKED_SAID.with(|said| said.borrow().len()),
        2,
        "this store's check file is said once too"
    );
    aterm_uds::private_file::write_private(&store.shared_dir(), CHECK_FILE, &checked)
        .expect("the check file restored");
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 90),
        TokenFact::Differ,
        "CONTROL: readable again, its verdict stands"
    );
    unreadable(&store);
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 120),
        TokenFact::Busy
    );
    assert_eq!(
        UNCHECKED_SAID.with(|said| said.borrow().len()),
        3,
        "a failure after a read is said again"
    );

    let scratch = Scratch::new("unchecked-window");
    let store = scratch.store();
    let roster = roster_of(&scratch).unwrap();
    let window = unverified_by(false, MintedBy::Window);
    unreadable(&store);
    assert_eq!(
        judged(&scratch, &Lookup::Found(window.clone()), &roster, NOW),
        TokenFact::Busy,
        "a window token another Mac minted is compared here: its verdict here is not known"
    );
    assert!(
        note_served(&store, &window, &served(stored_at(NOW), vec![]), NOW).is_err(),
        "its reading is not folded"
    );
}

/// THE HOLD (§5.6): a token made here and not verified, with no current basis, is not in use
/// while the row's stored login works here (`awaiting-basis`), so a launch stays on that login
/// and its reading gives the basis; once one did, the token is in use; once that basis's week
/// has ended, it is held again. A row whose stored login is not usable here is never held: no
/// login record, one naming another account, a row held by another Mac, a login signed out
/// here. NEGATIVE CONTROL: the token's state alone puts it in use (`TokenState::in_use`), so
/// without the hold the launch would start on a token whose readings compare nothing.
#[test]
fn a_token_awaiting_its_basis_is_held_while_the_stored_login_works_here() {
    let scratch = Scratch::new("hold");
    let store = scratch.store();
    let token = unverified(true);
    let found = Lookup::Found(token.clone());
    let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
    minted_row(&store, &token);
    let state = TokenState::of(&found, scratch.table().rows.get(KEY), NOW);
    assert!(
        state.in_use(),
        "NEGATIVE CONTROL: by its state alone it is in use"
    );
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::InUse,
        "no login record: no stored login here to hold it for"
    );
    login_record(&scratch, "someone-else@example.com");
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::InUse,
        "a record of another account"
    );
    login_record(&scratch, EMAIL);
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::AwaitingBasis
    );
    let mut lent = roster.clone();
    lent.rows
        .iter_mut()
        .filter(|row| row.label == "work")
        .for_each(|row| row.held_by = Some("m7".to_owned()));
    assert_eq!(
        judged(&scratch, &found, &lent, NOW),
        TokenFact::InUse,
        "a row another Mac holds"
    );

    assert!(stored_reading(&store, &token, stored_at(NOW), NOW));
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 60),
        TokenFact::InUse,
        "the stored login's reading lifts the hold"
    );
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 4 * 86_400),
        TokenFact::AwaitingBasis,
        "a basis whose week ended is none: held again"
    );

    let row = roster.row("work").expect("the row").clone();
    let mut note = None;
    store
        .transact_logins(i64::try_from(NOW).unwrap(), &mut note, |state, logins| {
            Ok(accounts::note_login_end(
                state,
                logins,
                &row.label,
                i64::try_from(NOW).unwrap(),
            ))
        })
        .expect("the login's end");
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 4 * 86_400),
        TokenFact::InUse,
        "a stored login signed out here holds nothing"
    );
}

/// The account `work` as the verb makes its token.
fn work() -> super::Account {
    super::Account {
        email: EMAIL.to_owned(),
        label: Some("work".to_owned()),
        account_id: Some("c-0123456789ab".to_owned()),
        key: key(),
        holder: None,
    }
}

/// `token` made `later` seconds after it, not verified: a `--replace` of it.
fn replacement(token: &Standing, later: u64) -> Standing {
    let mut new = token.clone();
    let meta = new.meta.as_mut().expect("a meta");
    meta.minted_at += later;
    meta.verified = None;
    new
}

/// A WAITING REPLACEMENT LEAVES THE TOKEN IT REPLACES JUDGED BY ITS OWN FACTS (§4.6, §5.6): the
/// vault holds the account's token, verified or found `differ` here; a `--replace` mint's write
/// waits in the vault's writes, and its row ([`record`]) is the new token's, `waiting`. The vault's
/// token is judged as before the mint: a verified one still serves (on a Mac whose stored login
/// is not usable, the account's only route), one whose windows differ still serves nowhere. A
/// session still on it records nothing of it, on the waiting token's row or its own: no served
/// fact, no verification (the waiting write changes the pair a verification would stand on, so
/// its vault write would only be refused), and its reading stays apart where its own row found its
/// windows different. Its refusal is its own: recorded of its `minted_at` beside this Mac's vault
/// state, never on the waiting token's row, and read back with its standing. A second
/// `--replace` that waits too keeps the first one's kept row. Once the vault holds the new token,
/// that is the row judged: held for its basis where the stored login works here, verified by
/// equal windows, and the replaced token's row goes. NEGATIVE CONTROL: judged by the waiting
/// token's row alone (its verdict none), the token whose windows differ would serve.
#[test]
fn a_waiting_replacement_leaves_the_token_it_replaces_judged_by_its_own_facts() {
    let waits = InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned());
    for case in ["verified", "differ"] {
        let scratch = Scratch::new("waiting-replacement");
        let store = scratch.store();
        let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
        let old = match case {
            "verified" => match standing(Check::Readable, NOW + A_YEAR, true, true, &[]) {
                Answer::Found(standing) => *standing,
                Answer::Absent | Answer::Unknown => unreachable!(),
            },
            _ => unverified(true),
        };
        let found = Lookup::Found(old.clone());
        minted_row(&store, &old);
        let before = if case == "verified" {
            TokenFact::InUse
        } else {
            assert!(stored_reading(&store, &old, stored_at(NOW), NOW));
            assert_eq!(
                note_served(&store, &old, &served(others_at(NOW), vec![]), NOW).unwrap(),
                Due::Apart
            );
            TokenFact::Differ
        };
        assert_eq!(
            judged(&scratch, &found, &roster, NOW),
            before,
            "PRECONDITION: {case}"
        );

        let new = replacement(&old, 600);
        let new_meta = new.meta.clone().expect("a meta");
        record(
            &store,
            &work(),
            &new_meta,
            waits.clone(),
            old.meta.as_ref().map(|meta| meta.minted_at),
        )
        .unwrap();
        assert_eq!(
            judged(&scratch, &found, &roster, NOW + 60),
            before,
            "{case}: the token the waiting write replaces is judged by its own facts"
        );
        if case == "differ" {
            assert!(
                TokenState::of(&found, scratch.table().rows.get(KEY), NOW + 60).in_use(),
                "NEGATIVE CONTROL: by the waiting token's row it would serve"
            );
        }
        let reading = if case == "verified" {
            stored_at(NOW)
        } else {
            others_at(NOW)
        };
        let due = note_served(&store, &old, &served(reading, vec![]), NOW + 90).unwrap();
        assert_eq!(
            due,
            if case == "verified" {
                Due::Nothing
            } else {
                Due::Apart
            },
            "{case}"
        );
        let row = scratch
            .table()
            .rows
            .remove(KEY)
            .expect("the waiting token's row");
        assert_eq!(
            (row.minted_at, &row.vault),
            (Some(new_meta.minted_at), &waits),
            "{case}: the row stays the waiting token's"
        );
        assert_eq!(
            row.used_on,
            UsedOn::default(),
            "{case}: nothing served of it"
        );
        assert_eq!(row.verification, Verification::default(), "{case}");
        assert_eq!(
            judged(&scratch, &found, &roster, NOW + 90),
            before,
            "{case}: and the replaced token is judged as before"
        );

        let newer = replacement(&old, 900);
        record(
            &store,
            &work(),
            newer.meta.as_ref().unwrap(),
            waits.clone(),
            old.meta.as_ref().map(|meta| meta.minted_at),
        )
        .unwrap();
        assert_eq!(
            scratch.table().replaced[KEY].minted_at,
            old.meta.as_ref().map(|meta| meta.minted_at),
            "{case}: a second waiting write keeps the token the vault holds"
        );
        let paths = Paths::locate_for_home(&scratch.path().join("home"));
        assert!(note_rejected(&paths, &store, &old, 401, NOW + 120).unwrap());
        assert!(
            !note_rejected(&paths, &store, &old, 401, NOW + 130).unwrap(),
            "{case}: recorded once, of the vault's token"
        );
        let refused = Lookup::Found(Standing {
            refused_at: Some(NOW + 120),
            ..old.clone()
        });
        assert_eq!(
            judged(&scratch, &refused, &roster, NOW + 120),
            TokenFact::Rejected,
            "{case}: its refusal is its own"
        );
        let table = scratch.table();
        assert_eq!(
            table.rows[KEY].minted_at,
            newer.meta.as_ref().map(|meta| meta.minted_at)
        );

        login_record(&scratch, EMAIL);
        let landed = Lookup::Found(newer.clone());
        assert_eq!(
            judged(&scratch, &landed, &roster, NOW + 150),
            TokenFact::AwaitingBasis,
            "{case}: the new token, once the vault holds it, is held for its basis"
        );
        assert!(stored_reading(&store, &newer, stored_at(NOW), NOW + 150));
        assert!(
            matches!(
                note_served(&store, &newer, &served(stored_at(NOW), vec![]), NOW + 180).unwrap(),
                Due::Verify(_)
            ),
            "{case}"
        );
        assert!(
            scratch.table().replaced.is_empty(),
            "{case}: the replaced token's row goes"
        );
    }
}

/// A VERIFIED TOKEN KEEPS SERVING WHILE ITS REPLACEMENT WAITS IN THE VAULT'S WRITES: m22's vault
/// holds the account's verified token, and no stored login of the account is usable here, so the
/// token is the account's only route; a `--replace` mint's write lands in the waiting writes (a
/// vault pass holds the claim), and its row says so ([`record`]). The vault's token still serves
/// this Mac: a session on it names it, so nothing of it counts for the token on its way. A table
/// that does not read is `busy`, never taken for an empty one.
#[test]
fn a_verified_token_keeps_serving_while_its_replacement_waits() {
    let scratch = Scratch::new("waiting-replacement-vault");
    let store = scratch.store();
    let lab = Lab::new();
    let now = aterm_time::unix_secs().unwrap();
    let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
    let meta = |minted_at: u64| Meta {
        email: EMAIL.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at,
        expires_at: now + A_YEAR,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let write = |meta: &Meta, on_busy: OnBusy| {
        claude_token::write(
            &lab.paths,
            &key(),
            &SecretBytes::from(sentinel().as_bytes()),
            meta,
            on_busy,
        )
        .expect("the write")
    };
    let old = meta(now - 86_400);
    let refuse = || OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(write(&old, refuse()), MemberWritten::Posted);
    let evidence = Verified {
        on: "m22".to_owned(),
        at: now - 3_600,
        by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
        seven_day_resets_at: None,
        five_hour_resets_at: None,
    };
    assert_eq!(
        claude_token::verify(&lab.paths, &key(), old.minted_at, evidence, refuse()).unwrap(),
        MemberWritten::Posted
    );
    let judge = |now: u64| {
        let found = claude_token::standing(&lab.paths, &key()).expect("a standing");
        standing_judged(
            &found,
            &lab.paths,
            &store,
            Some(&roster),
            &key(),
            &Account {
                label: Some("work".to_owned()),
                email: EMAIL.to_owned(),
            },
            now,
        )
    };
    let Lookup::Found(old_standing) = claude_token::standing(&lab.paths, &key()).unwrap() else {
        panic!("the vault holds the old token");
    };
    minted_row(&store, &old_standing);
    assert_eq!(judge(now), TokenFact::InUse, "PRECONDITION: the old token");

    let claim = aterm_secrets::client::test_hold_pass_claim(&lab.paths)
        .expect("the claim")
        .expect("no pass holds it");
    let new = meta(now - 60);
    let waits = write(&new, OnBusy::Wait(Instant::now()));
    assert!(matches!(waits, MemberWritten::Waiting(_)), "{waits:?}");
    assert!(claude_token::waiting_here(&lab.paths, &key()).unwrap());
    record(
        &store,
        &work(),
        &new,
        InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned()),
        Some(old.minted_at),
    )
    .unwrap();
    assert_eq!(
        judge(now),
        TokenFact::InUse,
        "the verified token still serves the account here"
    );

    let table_file = store.shared_dir().join(TABLE_FILE);
    let kept = std::fs::read(&table_file).expect("the table");
    aterm_uds::private_file::write_private(&store.shared_dir(), TABLE_FILE, b"not a table")
        .expect("a broken table");
    assert_eq!(
        judge(now),
        TokenFact::Busy,
        "an unreadable table is no empty one"
    );
    aterm_uds::private_file::write_private(&store.shared_dir(), TABLE_FILE, &kept)
        .expect("the table again");
    drop(claim);

    // A verification of the token the vault still holds stands on the pair it read, which the
    // waiting replacement changes: it is refused, the replacement still waits, and the next pass
    // lands it.
    let evidence = Verified {
        on: "m22".to_owned(),
        at: now,
        by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
        seven_day_resets_at: None,
        five_hour_resets_at: None,
    };
    assert!(
        !verify_served(
            &store,
            &Vault(lab.paths.clone()),
            &key(),
            Verify {
                minted_at: old.minted_at,
                evidence,
            },
            now,
        )
        .unwrap(),
        "a verification never replaces the replacement on its way"
    );
    assert!(claude_token::waiting_here(&lab.paths, &key()).unwrap());
    aterm_secrets::client::sync(&lab.paths, None).expect("a vault pass");
    let Lookup::Found(landed) = claude_token::standing(&lab.paths, &key()).unwrap() else {
        panic!("the vault holds a token");
    };
    assert_eq!(
        landed.meta.as_ref().map(|meta| meta.minted_at),
        Some(new.minted_at),
        "the replacement landed, not the token it replaced"
    );
}

/// A WAITING WRITE TAKEN BACK GIVES THE TOKEN IT REPLACED ITS OWN ROW AGAIN: the vault still holds
/// the token whose windows differ, and the row (`--remove` took the waiting write back) is the
/// removed one's; that token's next reading brings its kept row back, so it stays apart and its
/// verdict stands. CONTROL: the row as the removed token's alone judges nothing of it.
#[test]
fn a_waiting_write_taken_back_gives_the_token_it_replaced_its_own_row_again() {
    let scratch = Scratch::new("taken-back");
    let store = scratch.store();
    let old = unverified(true);
    let found = Lookup::Found(old.clone());
    minted_row(&store, &old);
    assert!(stored_reading(&store, &old, stored_at(NOW), NOW));
    note_served(&store, &old, &served(others_at(NOW), vec![]), NOW).unwrap();
    let new = replacement(&old, 600);
    let waits = InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned());
    record(
        &store,
        &work(),
        new.meta.as_ref().unwrap(),
        waits,
        old.meta.as_ref().map(|meta| meta.minted_at),
    )
    .unwrap();
    update_table(&store, |table| {
        let row = table.rows.get_mut(KEY).expect("the row");
        row.vault = InVault::Removed;
        row.removed_at = Some(NOW + 30);
    })
    .unwrap();
    assert_eq!(
        TokenState::of(&found, scratch.table().row_for(KEY, &found), NOW + 30).words(),
        "unverified (windows differ)"
    );
    assert_eq!(
        TokenState::of(&found, scratch.table().rows.get(KEY), NOW + 30).words(),
        "unverified (awaiting a stored-login reading)",
        "CONTROL: the removed token's row says nothing of it"
    );
    assert_eq!(
        note_served(&store, &old, &served(stored_at(NOW), vec![]), NOW + 60).unwrap(),
        Due::Apart
    );
    let table = scratch.table();
    assert_eq!(
        table.rows[KEY].minted_at,
        old.meta.as_ref().map(|meta| meta.minted_at)
    );
    assert_eq!(table.rows[KEY].verification.verdict, Some(Verdict::Differ));
    assert!(table.replaced.is_empty());
}

/// A TOKEN FOUND `differ` STAYS APART WHILE ITS REPLACEMENT WAITS, WHATEVER ITS ROW'S COLUMN SAYS
/// (§5.6): the first mint's write waited (the vault held no token), so its row says `waiting`; a
/// vault pass landed it, and the column still says so. Its windows differ. A `--replace` waits
/// too, its preflight having read the first token in the vault: that token's row is kept by what
/// the vault holds, never by the column, so it still serves nowhere, its readings stay apart, and
/// its verdict stands. NEGATIVE CONTROL: kept only where the earlier row's column did not say
/// `waiting` (the rule this replaces), the first token would be judged by the replacement's row
/// and serve.
#[test]
fn a_token_found_differ_stays_apart_while_its_replacement_waits_whatever_its_column_says() {
    let scratch = Scratch::new("stale-column");
    let store = scratch.store();
    let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
    let first = unverified(true);
    let first_at = first.meta.as_ref().map(|meta| meta.minted_at);
    let waits = InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned());
    record(
        &store,
        &work(),
        first.meta.as_ref().unwrap(),
        waits.clone(),
        None,
    )
    .unwrap();
    assert!(stored_reading(&store, &first, stored_at(NOW), NOW));
    assert_eq!(
        note_served(&store, &first, &served(others_at(NOW), vec![]), NOW).unwrap(),
        Due::Apart
    );
    let found = Lookup::Found(first.clone());
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::Differ,
        "PRECONDITION"
    );
    assert_eq!(
        scratch.table().rows[KEY].vault,
        waits,
        "PRECONDITION: the stale column"
    );

    let second = replacement(&first, 600);
    record(
        &store,
        &work(),
        second.meta.as_ref().unwrap(),
        waits,
        first_at,
    )
    .unwrap();
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 60),
        TokenFact::Differ,
        "the first token still serves nowhere"
    );
    assert_eq!(
        note_served(&store, &first, &served(stored_at(NOW), vec![]), NOW + 90).unwrap(),
        Due::Apart,
        "its readings stay apart"
    );
    let table = scratch.table();
    assert_eq!(table.replaced[KEY].minted_at, first_at);
    assert_eq!(
        table.replaced[KEY].verification.verdict,
        Some(Verdict::Differ)
    );
    assert_eq!(
        table.rows[KEY].minted_at,
        second.meta.as_ref().map(|meta| meta.minted_at)
    );
}

/// A STALE STANDING NEVER MOVES THE ROW BACK TO THE TOKEN IT REPLACED: a `--replace` was posted,
/// so the account's row is the new token's; a reading judged against a standing read before the
/// write landed names the token it replaced. That reading goes to that token's kept row, and the
/// account's row stays the new token's, its label and facts as the mint left them; the new
/// token's next reading finds it, and the kept row goes. NEGATIVE CONTROL: a row that follows the
/// vault's token backwards would be reset to the replaced token, the new token's row lost.
#[test]
fn a_stale_standing_never_moves_the_row_back_to_the_token_it_replaced() {
    let scratch = Scratch::new("stale-standing");
    let store = scratch.store();
    let first = unverified(true);
    let first_at = first.meta.as_ref().map(|meta| meta.minted_at);
    minted_row(&store, &first);
    let second = replacement(&first, 600);
    let second_at = second.meta.as_ref().map(|meta| meta.minted_at);
    record(
        &store,
        &work(),
        second.meta.as_ref().unwrap(),
        InVault::InVault,
        first_at,
    )
    .unwrap();
    assert_eq!(
        note_served(&store, &first, &served(others_at(NOW), vec![]), NOW).unwrap(),
        Due::Nothing
    );
    let table = scratch.table();
    assert_eq!(
        (table.rows[KEY].minted_at, table.rows[KEY].label.as_deref()),
        (second_at, Some("work")),
        "the row stays the new token's"
    );
    assert_eq!(table.rows[KEY].used_on, UsedOn::default());
    assert_eq!(
        (
            table.replaced[KEY].minted_at,
            table.replaced[KEY].used_on.last_served_at
        ),
        (first_at, Some(NOW)),
        "the reading is the replaced token's"
    );
    note_served(&store, &second, &served(stored_at(NOW), vec![]), NOW + 60).unwrap();
    let table = scratch.table();
    assert_eq!(table.rows[KEY].minted_at, second_at);
    assert_eq!(table.rows[KEY].used_on.first_served_at, Some(NOW + 60));
    assert!(table.replaced.is_empty(), "the replaced token's row goes");
}

/// A TOKEN MINTED HERE WITH NO ROW OF ITS OWN IS HELD FOR ITS BASIS, AND ITS ROW IS MADE (§5.6):
/// the vault holds a token this Mac minted whose row was never written (a mint whose table write
/// failed, or a session served before it was), or whose account's row is of an older token found
/// `differ`. It awaits a stored-login reading, so where the row's stored login works here it is
/// held, and the judgment makes the row that reading's basis goes on ([`note_held`],
/// [`basis_wanted`]); the reading lifts the hold. NEGATIVE CONTROL: a token with no row of its own
/// read `no reading yet` (the rule this replaces) would serve unchecked.
#[test]
fn a_token_minted_here_with_no_row_of_its_own_is_held_for_its_basis() {
    let token = unverified(true);
    let minted_at = token.meta.as_ref().map(|meta| meta.minted_at);
    let found = Lookup::Found(token.clone());
    for case in ["no row", "a row of an older token"] {
        let scratch = Scratch::new("no-row");
        let store = scratch.store();
        let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
        if case != "no row" {
            let mut older = token.clone();
            older.meta.as_mut().expect("a meta").minted_at -= 600;
            minted_row(&store, &older);
            update_table(&store, |table| {
                table
                    .rows
                    .get_mut(KEY)
                    .expect("the row")
                    .verification
                    .verdict = Some(Verdict::Differ);
            })
            .unwrap();
        }
        let table = scratch.table();
        assert_eq!(
            TokenState::of(&found, table.row_for(KEY, &found), NOW).words(),
            "unverified (awaiting a stored-login reading)",
            "{case}"
        );
        login_record(&scratch, EMAIL);
        assert_eq!(
            judged(&scratch, &found, &roster, NOW),
            TokenFact::AwaitingBasis,
            "{case}: held"
        );
        let row = scratch
            .table()
            .rows
            .remove(KEY)
            .expect("the held token's row");
        assert_eq!(
            (
                row.minted_at,
                row.label.as_deref(),
                row.verification.verdict
            ),
            (minted_at, Some("work"), None),
            "{case}: its row, made"
        );
        assert!(basis_wanted(&store, &key()), "{case}");
        assert!(
            stored_reading(&store, &token, stored_at(NOW), NOW),
            "{case}"
        );
        assert_eq!(
            judged(&scratch, &found, &roster, NOW + 60),
            TokenFact::InUse,
            "{case}: the hold lifts"
        );
    }
}

/// A READING THAT NAMES NO SEVEN-DAY WINDOW COMPARES NOTHING (§5.6): with a basis recorded, a
/// token reading whose status line carried the five-hour window alone leaves no verdict and no
/// write, and the token stays in use; the next reading naming both windows compares. NEGATIVE
/// CONTROL: compared, it would read `differ` (a basis's week is never shown by a reading that
/// names none) and end verification of a token of the right account.
#[test]
fn a_token_reading_naming_no_week_compares_nothing() {
    let scratch = Scratch::new("no-week");
    let store = scratch.store();
    let token = unverified(true);
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, stored_at(NOW), NOW));
    let session_only = Windows {
        week_reset: None,
        session_reset: stored_at(NOW).session_reset,
    };
    assert_eq!(
        note_served(&store, &token, &served(session_only, vec![]), NOW + 30).unwrap(),
        Due::Nothing
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        (row.verification.verdict, row.verification.reading),
        (None, None)
    );
    assert!(TokenState::of(&Lookup::Found(token.clone()), Some(&row), NOW + 30).in_use());
    assert!(
        matches!(
            note_served(&store, &token, &served(stored_at(NOW), vec![]), NOW + 60).unwrap(),
            Due::Verify(_)
        ),
        "CONTROL: a reading naming both windows compares"
    );
}

/// A READING OF A SESSION AN EARLIER WRAPPER SERVED IS NEVER COMPARED (§5.6): its wrapper named no
/// token, so it is taken for one on the vault's token only because that token was made before it
/// began ([`Served::named`] false); its served facts are recorded, but it leaves no verdict and no
/// write, not even once a named reading found the windows equal and their write is still to be
/// made. A verdict a named reading reached stands for it: `differ` leaves it apart. CONTROL: the
/// same windows from a named session leave the write due.
#[test]
fn a_token_reading_of_a_session_an_earlier_wrapper_served_is_never_compared() {
    let scratch = Scratch::new("unnamed");
    let store = scratch.store();
    let token = unverified(true);
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, stored_at(NOW), NOW));
    let unnamed = |windows: Windows| Served {
        named: false,
        ..served(windows, vec![])
    };
    assert_eq!(
        note_served(&store, &token, &unnamed(stored_at(NOW)), NOW + 30).unwrap(),
        Due::Nothing
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.used_on.first_served_at, Some(NOW + 30));
    assert_eq!(row.verification.verdict, None, "nothing compared");
    assert!(
        matches!(
            note_served(&store, &token, &served(stored_at(NOW), vec![]), NOW + 60).unwrap(),
            Due::Verify(_)
        ),
        "CONTROL: a named session's reading compares"
    );
    assert_eq!(
        note_served(&store, &token, &unnamed(stored_at(NOW)), NOW + 90).unwrap(),
        Due::Nothing,
        "the windows found equal, their write not made yet: an unnamed reading leaves no write"
    );
    let apart = Scratch::new("unnamed-differ");
    minted_row(&apart.store(), &token);
    assert!(stored_reading(&apart.store(), &token, stored_at(NOW), NOW));
    note_served(&apart.store(), &token, &served(others_at(NOW), vec![]), NOW).unwrap();
    assert_eq!(
        note_served(&apart.store(), &token, &unnamed(stored_at(NOW)), NOW + 30).unwrap(),
        Due::Apart
    );
}

/// A READING TAKEN BEFORE ITS WINDOWS RESET COMPARES NOTHING OF THEM (§5.6): a token session's
/// reading handed on after its windows reset (its session idle across the reset, the reading taken
/// again after an update's handoff) says nothing of the windows the token is in now. One of a
/// week that has ended leaves no verdict, and the next reading of the running windows verifies the
/// token; one whose five-hour window has ended is compared by its week alone, and so is a reading
/// against a basis whose own five-hour window has ended since it was recorded (the stored login
/// last reported in an earlier five hours of the same week). None turns a token of the right
/// account `differ`, which would stop it serving on this Mac for good.
/// NEGATIVE CONTROL (applied to `Windows::shown_by`, then reverted): comparing the basis's five
/// hours unfiltered, `current(self.session_reset)` made `self.session_reset`, finds the windows
/// differ and fails `a basis whose five-hour window has ended is compared by its week alone`.
#[test]
fn a_reading_taken_before_its_windows_reset_compares_nothing_of_them() {
    let token = unverified(true);
    let later = NOW + 4 * 86_400;
    let at = i64::try_from(later).unwrap();
    let basis = stored_at(later);
    let ended_week = stored_at(NOW);
    assert!(
        ended_week.week_reset.is_some_and(|reset| reset < at),
        "PRECONDITION: its week has ended"
    );
    let ended_session = Windows {
        session_reset: Some(at - 60),
        ..basis
    };
    let based = |tag: &str| {
        let scratch = Scratch::new(tag);
        minted_row(&scratch.store(), &token);
        assert!(stored_reading(&scratch.store(), &token, basis, later - 30));
        scratch
    };

    let scratch = based("ended-week-reading");
    let store = scratch.store();
    assert_eq!(
        note_served(&store, &token, &served(ended_week, vec![]), later).unwrap(),
        Due::Nothing,
        "a reading of a week that has ended"
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        (row.verification.verdict, row.verification.reading),
        (None, None),
        "nothing compared"
    );
    assert!(TokenState::of(&Lookup::Found(token.clone()), Some(&row), later).in_use());
    assert!(
        matches!(
            note_served(&store, &token, &served(basis, vec![]), later + 30).unwrap(),
            Due::Verify(_)
        ),
        "the next reading of the running windows verifies it"
    );

    let scratch = based("ended-session-reading");
    assert!(
        matches!(
            note_served(
                &scratch.store(),
                &token,
                &served(ended_session, vec![]),
                later
            )
            .unwrap(),
            Due::Verify(_)
        ),
        "a reading whose five-hour window has ended is compared by its week alone"
    );
    assert_eq!(
        scratch.table().rows[KEY].verification.verdict,
        Some(Verdict::Equal)
    );

    // The stored login reported an hour before, its five-hour window resetting a minute before
    // the token's reading, which is of the running week and a new five hours.
    let ended_basis = Windows {
        session_reset: Some(at - 60),
        ..basis
    };
    let scratch = Scratch::new("ended-session-basis");
    let store = scratch.store();
    minted_row(&store, &token);
    assert!(stored_reading(&store, &token, ended_basis, later - 3_600));
    assert_eq!(
        scratch.table().rows[KEY].verification.basis,
        Some((later - 3_600, ended_basis)),
        "PRECONDITION: the basis names a five-hour window that has ended by the reading"
    );
    assert!(
        matches!(
            note_served(&store, &token, &served(basis, vec![]), later).unwrap(),
            Due::Verify(_)
        ),
        "a basis whose five-hour window has ended is compared by its week alone"
    );
    assert_eq!(
        scratch.table().rows[KEY].verification.verdict,
        Some(Verdict::Equal),
        "ended-session-basis"
    );
}

/// A ROW WHOSE WRITE WAS OVERTAKEN FOLLOWS THE VAULT'S TOKEN, HOWEVER OLD (§5.1): this Mac's mint
/// was overtaken by another Mac's write of the account's token, made before it, which the vault
/// holds; that token's reading moves the account's row onto it, its label kept, and no kept row
/// is made. CONTROL: a row whose own later write is still on its way keeps its token, and the
/// reading goes to the vault's token's kept row.
#[test]
fn a_row_whose_write_was_overtaken_follows_the_vaults_older_token() {
    let theirs = unverified(false);
    let theirs_at = theirs.meta.as_ref().map(|meta| meta.minted_at);
    let mine = replacement(&theirs, 600);
    let mine_at = mine.meta.as_ref().map(|meta| meta.minted_at);
    for (case, vault) in [
        ("overtaken", InVault::Overtaken("m7".to_owned())),
        (
            "waiting",
            InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned()),
        ),
    ] {
        let scratch = Scratch::new("overtaken");
        let store = scratch.store();
        record(&store, &work(), mine.meta.as_ref().unwrap(), vault, None).unwrap();
        note_served(&store, &theirs, &served(stored_at(NOW), vec![]), NOW).unwrap();
        let table = scratch.table();
        if case == "overtaken" {
            assert_eq!(
                (table.rows[KEY].minted_at, table.rows[KEY].label.as_deref()),
                (theirs_at, Some("work")),
                "{case}: the row follows the vault's token"
            );
            assert_eq!(table.rows[KEY].used_on.last_served_at, Some(NOW), "{case}");
            assert!(table.replaced.is_empty(), "{case}: no kept row");
        } else {
            assert_eq!(table.rows[KEY].minted_at, mine_at, "CONTROL: {case}");
            assert_eq!(table.replaced[KEY].minted_at, theirs_at, "CONTROL: {case}");
        }
    }
}

/// A KEPT ROW'S EQUAL WINDOWS LEAVE NO WRITE (§5.6): the vault holds the account's unverified
/// token, with a basis, and a `--replace` write of this Mac's waits, so that token's facts are
/// its kept row's; a named reading of it showing the basis's windows finds them equal there, and
/// leaves nothing to write: a verification of a token whose replacement is on its way would only
/// be refused. CONTROL: with nothing on its way, the account's row of the same token leaves the
/// write due.
#[test]
fn a_kept_rows_equal_windows_leave_no_write() {
    let old = unverified(true);
    for waits in [true, false] {
        let scratch = Scratch::new("kept-equal");
        let store = scratch.store();
        minted_row(&store, &old);
        assert!(stored_reading(&store, &old, stored_at(NOW), NOW));
        if waits {
            record(
                &store,
                &work(),
                replacement(&old, 600).meta.as_ref().unwrap(),
                InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned()),
                old.meta.as_ref().map(|meta| meta.minted_at),
            )
            .unwrap();
        }
        let due = note_served(&store, &old, &served(stored_at(NOW), vec![]), NOW + 60).unwrap();
        let table = scratch.table();
        if waits {
            assert_eq!(due, Due::Nothing, "no write while the replacement waits");
            assert_eq!(
                table.replaced[KEY].verification.verdict,
                Some(Verdict::Equal),
                "the kept row found them equal"
            );
        } else {
            assert!(matches!(due, Due::Verify(_)), "CONTROL: {due:?}");
            assert_eq!(table.rows[KEY].verification.verdict, Some(Verdict::Equal));
        }
    }
}

/// THE SHARED CHECK READS ANOTHER ACCOUNT'S WINDOWS ONLY BY THE RESETS ITS SAMPLE NAMES (§5.6):
/// [`Windows::current`] gives each window still current by the reset the sample names, and leaves
/// out one whose reset it does not name (the end `Sample::session` makes up is only the latest
/// its window can end) or whose window has ended. So another account whose sample shows the
/// token's week and a five-hour percentage with no reset makes the comparison `shared`, never
/// `equal`. NEGATIVE CONTROL: that made-up session end, compared with the reading's, turns it
/// `equal`, which would verify a token two accounts' windows cannot tell apart.
#[test]
fn the_shared_check_reads_another_accounts_windows_by_the_resets_its_sample_names() {
    let at = i64::try_from(NOW).unwrap();
    let stored = stored_at(NOW);
    let sample = |session_reset: Option<i64>, week_reset: Option<i64>| accounts::Sample {
        session_pct: Some(10),
        session_reset,
        week_pct: Some(20),
        week_reset,
        at: at - 60,
        ..accounts::Sample::default()
    };
    assert_eq!(
        Windows::current(&sample(stored.session_reset, stored.week_reset), at),
        stored,
        "both resets named"
    );
    assert_eq!(
        Windows::current(&sample(None, None), at),
        Windows::default(),
        "neither named"
    );
    assert_eq!(
        Windows::current(&sample(stored.session_reset, None), at),
        Windows {
            week_reset: None,
            ..stored
        },
        "a week not named"
    );
    let week_only = Windows {
        session_reset: None,
        ..stored
    };
    assert_eq!(
        Windows::current(&sample(Some(at - 1), stored.week_reset), at),
        week_only,
        "a session that has ended"
    );
    let other = sample(None, stored.week_reset);
    assert_eq!(
        Windows::current(&other, at),
        week_only,
        "a session not named"
    );
    assert_eq!(
        Verdict::of(Some(&stored), &stored, &[Windows::current(&other, at)], at),
        Verdict::Shared
    );
    let made_up = Windows {
        session_reset: other.session(at).map(|(_, end)| end),
        ..week_only
    };
    assert_eq!(
        Verdict::of(Some(&stored), &stored, &[made_up], at),
        Verdict::Equal,
        "NEGATIVE CONTROL: the made-up session end"
    );
}

/// FIRST AND LAST SERVED ARE RECORDED UNDER `tokens.lock`: while another holds it, nothing is
/// written (the call says the lock stayed held); once it is free, the first reading sets both,
/// and a later one moves only the last. A Mac that did not mint the token records its served
/// facts too, and no verification.
#[test]
fn first_and_last_served_are_recorded_under_tokens_lock() {
    let scratch = Scratch::new("served");
    let store = scratch.store();
    store.ensure().unwrap();
    let token = unverified(false);
    let held = accounts::lock_at_within(&store.shared_dir().join(TABLE_LOCK), &[])
        .unwrap()
        .expect("the test holds tokens.lock");
    let stored = stored_at(NOW);
    assert!(note_served(&store, &token, &served(stored, vec![]), NOW).is_err());
    assert!(scratch.table().rows.is_empty(), "nothing written meanwhile");
    drop(held);
    let _ = notes();
    note_served(&store, &token, &served(stored, vec![]), NOW).unwrap();
    note_served(&store, &token, &served(stored, vec![]), NOW + 90).unwrap();
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.used_on.first_served_at, Some(NOW));
    assert_eq!(row.used_on.last_served_at, Some(NOW + 90));
    assert_eq!(
        row.verification,
        Verification::default(),
        "no verification of a token not in use here unverified"
    );
    assert_eq!(notes().len(), 1, "one token-served note");
}

/// A TOKEN ANOTHER MAC MINTED IN ATERM'S WINDOW IS COMPARED AND VERIFIED HERE AS ONE MINTED HERE IS
/// (§5.6), AND ONE IT MINTED AT A TERMINAL IS APART UNTIL VERIFIED: m7's window-minted token is in
/// use here from its mint, so this Mac's stored-login reading is its basis and a token reading is
/// compared with it. Equal windows leave the write due here, as on the minting Mac (the evidence
/// names no Mac: the vault names the seat that writes it), and the token stays in use meanwhile,
/// `unverified (made on m7; windows equal)`; different windows leave the reading apart, journal
/// `token-differ` once and stop the token on this Mac alone, while m7's own state of it is
/// untouched. CONTROL: the same token made at m7's terminal, or by a meta that names no route,
/// takes no basis here and its readings are apart, as a token not in use here. NEGATIVE CONTROL
/// (2026-11-14, by hand, then restored): with `note_served`'s write gated on `minted_here` again,
/// the equal windows leave nothing.
#[test]
fn a_window_minted_token_another_mac_minted_is_compared_here_and_a_terminal_one_is_apart() {
    let stored = stored_at(NOW);
    let window = unverified_by(false, MintedBy::Window);
    let found = Lookup::Found(window.clone());
    let equal = Scratch::new("peer-equal");
    assert!(
        stored_reading(&equal.store(), &window, stored, NOW),
        "this Mac's stored login gives the basis"
    );
    let Due::Verify(verify) =
        note_served(&equal.store(), &window, &served(stored, vec![]), NOW + 30).unwrap()
    else {
        panic!("equal windows: the write is due here");
    };
    assert_eq!(verify.minted_at, window.meta.as_ref().unwrap().minted_at);
    assert!(verify.evidence.on.is_empty());
    let row = equal.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.verification.verdict, Some(Verdict::Equal));
    let state = TokenState::of(&found, Some(&row), NOW + 30);
    assert_eq!(state.words(), "unverified (made on m7; windows equal)");
    assert!(state.in_use());

    let differ = Scratch::new("peer-differ");
    assert!(stored_reading(&differ.store(), &window, stored, NOW));
    let _ = notes();
    assert_eq!(
        note_served(
            &differ.store(),
            &window,
            &served(others_at(NOW), vec![]),
            NOW + 30
        )
        .unwrap(),
        Due::Apart,
        "different windows: the reading is apart"
    );
    let row = differ.table().rows.remove(KEY).expect("the row");
    let state = TokenState::of(&found, Some(&row), NOW + 30);
    assert_eq!(state.words(), "unverified (made on m7; windows differ)");
    assert!(!state.in_use(), "not in use on the Mac that compared");
    assert_eq!(
        crate::harness::token_route::fact_of(&state),
        TokenFact::Differ
    );
    let kinds: Vec<&str> = notes().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["token-served", "token-differ"], "journaled once");
    note_served(
        &differ.store(),
        &window,
        &served(others_at(NOW), vec![]),
        NOW + 60,
    )
    .unwrap();
    assert!(notes().is_empty(), "a final differ journals nothing again");
    assert!(
        TokenState::of(
            &Lookup::Found(Standing {
                minted_here: true,
                ..window.clone()
            }),
            None,
            NOW + 30
        )
        .in_use(),
        "m7, which compared nothing, still serves it"
    );

    for (case, token) in [
        ("at a terminal", unverified_by(false, MintedBy::Terminal)),
        ("naming no route", unverified(false)),
    ] {
        let scratch = Scratch::new("peer-terminal");
        assert!(
            !stored_reading(&scratch.store(), &token, stored, NOW),
            "CONTROL {case}: no basis"
        );
        assert_eq!(
            note_served(&scratch.store(), &token, &served(stored, vec![]), NOW + 30).unwrap(),
            Due::Apart,
            "CONTROL {case}: apart"
        );
        let row = scratch.table().rows.remove(KEY).expect("the row");
        assert_eq!(row.verification, Verification::default(), "CONTROL {case}");
        let state = TokenState::of(&Lookup::Found(token), Some(&row), NOW + 30);
        assert_eq!(
            state.words(),
            "unverified (made on m7; awaits verification there)",
            "CONTROL {case}"
        );
        assert!(!state.in_use(), "CONTROL {case}");
    }
}

/// AN EARLIER BUILD'S SAMPLE STAMP IS NO VERIFICATION, ON EITHER MAC (§5.6): 0.197.0 to 0.200.0
/// stamped `verified` by `windows` from the row's usage sample, which can hold another account's
/// readings. On a token whose meta names no route, that stamp leaves it unverified: on a Mac that
/// did not mint it, the token is not in use, takes no basis and its readings are apart; on the
/// Mac that minted it, it awaits a stored-login reading, the reading becomes its basis, and a
/// token reading of equal windows is compared and leaves this build's own stamp due, by
/// `windows-login`, which replaces the sample's. NEGATIVE CONTROL: with
/// `claude_token::Meta::verification` counting any stamp, the peer's token is in use and the
/// minting Mac's comparison is skipped (`Due::Nothing`).
#[test]
fn an_earlier_builds_sample_stamp_leaves_the_token_unverified_and_compared_again() {
    let stored = stored_at(NOW);
    let stamped = |here: bool| {
        let mut token = unverified(here);
        token.meta.as_mut().expect("a meta").verified =
            Some(aterm_secrets::claude_token::Verified {
                on: "m7".to_owned(),
                at: NOW - 60,
                by: "windows".to_owned(),
                seven_day_resets_at: stored.week_reset.and_then(|r| u64::try_from(r).ok()),
                five_hour_resets_at: None,
            });
        token
    };

    let peer = stamped(false);
    assert!(!peer.verified() && !peer.serves_here());
    let scratch = Scratch::new("sample-peer");
    assert!(
        !stored_reading(&scratch.store(), &peer, stored, NOW),
        "a peer takes no basis for it"
    );
    assert_eq!(
        note_served(&scratch.store(), &peer, &served(stored, vec![]), NOW + 30).unwrap(),
        Due::Apart,
        "its readings are apart"
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.verified, None, "the row records no verification");
    let state = TokenState::of(&Lookup::Found(peer), Some(&row), NOW + 30);
    assert!(!state.in_use(), "not in use on a Mac that did not mint it");
    assert_eq!(
        state.words(),
        "unverified (made on m7; awaits verification there)"
    );

    let minting = stamped(true);
    let scratch = Scratch::new("sample-minting");
    let store = scratch.store();
    minted_row(&store, &minting);
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        TokenState::of(&Lookup::Found(minting.clone()), Some(&row), NOW),
        TokenState::Unverified {
            minted_here: true,
            minted_by: None,
            why: Unverified::AwaitingBasis,
            minted_on: "m22".to_owned(),
            exposed: Vec::new(),
        },
        "the minting Mac awaits a stored-login reading"
    );
    assert!(
        stored_reading(&store, &minting, stored, NOW),
        "its stored login gives the basis"
    );
    match note_served(&store, &minting, &served(stored, vec![]), NOW + 30).unwrap() {
        Due::Verify(verify) => assert_eq!(
            verify.evidence.by,
            aterm_secrets::claude_token::BY_WINDOWS,
            "this build's own stamp replaces the sample's"
        ),
        other => panic!("compared again: {other:?}"),
    }
}

/// A ROW WITH NO LOGIN MADE HERE RUNS ON ITS FLEET TOKEN HERE WHERE THIS MAC'S CLAIMS COVER IT
/// (FL-135): m7's window-minted token of `away` is in use on every Mac, and `away`'s login
/// directory holds nothing here. On a Mac with `ai-pool.toml`, whose claims cover every account
/// ([`super::super::claims::covers`]), the row is one of the rows on their token, which the upkeep
/// keeps for rotation ([`accounts::LoginHere::OnToken`]); the sign-in ladder's vault route then
/// holds its sign-in and renewal leaves it out, so nothing makes its login: the reading writes
/// nothing in its directory and it stays not enrolled. NEGATIVE CONTROLS: on a Mac whose claims
/// cover no such account (no pool file, no lab naming it the home), nothing would keep a second
/// Mac off it, so it is not on its token and reads [`accounts::LoginHere::NotMade`]; and a token
/// m7 minted at a person's terminal, not yet verified, serves only m7, so `away` is not on it
/// either.
#[test]
fn a_row_with_no_login_made_here_runs_on_its_fleet_token_here() {
    let scratch = Scratch::new("not-made");
    let dir = scratch.path().join("logins").join("away");
    let roster_file = scratch.path().join("away.toml");
    std::fs::write(
        &roster_file,
        format!(
            "[[account]]\nlabel = \"away\"\nclass = \"dedicated\"\ndir = \"{}\"\nemail = \
             \"{EMAIL}\"\n",
            dir.display()
        ),
    )
    .expect("the roster");
    let roster = Roster::load_for(Cli::Claude, &roster_file).expect("it loads");
    let paths = Paths::for_home(&scratch.path().join("home"));
    let store = scratch.store();
    let on_token = |token: &Standing| {
        journal::with_keyer(keyer, || {
            rows_on_token_of(
                &paths,
                Lookup::Found(vec![token.clone()]),
                &store,
                &roster,
                i64::try_from(NOW).unwrap(),
            )
        })
        .expect("the listing was read")
    };
    let row = roster.row("away").expect("the row");
    let now = i64::try_from(NOW).unwrap();
    let here = |read: &BTreeMap<String, u64>| {
        let state = accounts::State {
            tokens_here: Some(accounts::TokensHere {
                read_at: now,
                ends: read
                    .iter()
                    .map(|(label, end)| (label.clone(), i64::try_from(*end).unwrap()))
                    .collect(),
            }),
            ..accounts::State::default()
        };
        accounts::login_here(
            row,
            &state,
            accounts::enrolled(&roster).contains("away"),
            now,
        )
    };
    assert!(!accounts::enrolled(&roster).contains("away"));

    let terminal = unverified_by(false, MintedBy::Terminal);
    assert!(
        on_token(&terminal).is_empty(),
        "CONTROL: a terminal token m7 has not verified serves only m7"
    );
    assert_eq!(here(&on_token(&terminal)), accounts::LoginHere::NotMade);

    let window = unverified_by(false, MintedBy::Window);
    assert!(
        on_token(&window).is_empty(),
        "CONTROL: no claims cover the account here"
    );
    crate::vault_entry::put(
        &paths.home,
        crate::harness::ai_pool::AI_POOL_FILE,
        "version = 1\n",
    );
    assert_eq!(here(&on_token(&terminal)), accounts::LoginHere::NotMade);
    let read = on_token(&window);
    assert_eq!(
        read.get("away"),
        window.meta.as_ref().map(|meta| meta.expires_at).as_ref(),
        "no login made here: its fleet token runs it"
    );
    assert_eq!(
        here(&read),
        if crate::harness::token_route::LAUNCHES_ON_TOKEN {
            accounts::LoginHere::OnToken
        } else {
            accounts::LoginHere::NotMade
        },
        "rotation can pick it wherever a launch starts on a token"
    );
    assert!(!dir.exists(), "the reading makes no login directory");
    assert!(
        !accounts::enrolled(&roster).contains("away"),
        "nor enrols the row"
    );
}

/// RENEWAL IS OFF FOR A ROW ON ITS VAULT TOKEN, AND `renew` ON ONE SAYS SO (§5.8): with a token
/// in use here in a scratch seated vault, the row is one of the rows on their token, and
/// `accounts renew work` says "work runs on its vault token until <date>; renewal is off for
/// it; nothing was run", running no sign-in and never taking `signin.lock`.
/// CONTROL: past the token's end no row is on it.
#[test]
fn renew_on_a_token_row_says_so_and_takes_no_sign_in_lock() {
    let lab = Lab::new();
    let now = aterm_time::unix_secs().unwrap();
    let meta = Meta {
        email: EMAIL.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: now - 60,
        expires_at: now + A_YEAR,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    claude_token::write(
        &lab.paths,
        &key(),
        &SecretBytes::from(sentinel().as_bytes()),
        &meta,
        OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the write");
    let scratch = Scratch::new("renew");
    let env = Env {
        home: Some(lab.paths.home.clone()),
        now: i64::try_from(now).unwrap(),
        ..scratch.env()
    };
    let store = env.accounts_store_for(Cli::Claude).expect("a store");
    let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
    let on_token = journal::with_keyer(keyer, || {
        rows_on_token(&lab.paths, &store, &roster, env.now)
    });
    assert_eq!(on_token.get("work"), Some(&meta.expires_at), "{on_token:?}");
    let ended = i64::try_from(meta.expires_at + 1).unwrap();
    assert!(
        journal::with_keyer(keyer, || rows_on_token(&lab.paths, &store, &roster, ended)).is_empty(),
        "CONTROL: past its end the token is in use nowhere"
    );

    let host = Host::new(VaultOf::Real(lab.paths.clone()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let form = AccountsCmd::Renew {
        labels: vec!["work".to_owned()],
        due: false,
        on: None,
    };
    let code = journal::with_keyer(keyer, || {
        run_accounts(&form, &env, &host, &mut out, &mut err)
    });
    let (out, err) = (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::SUCCESS),
        "{out} {err}"
    );
    assert_eq!(
        out.trim(),
        format!(
            "work: work runs on its vault token until {}; renewal is off for it; nothing was run",
            date(meta.expires_at)
        ),
        "{err}"
    );
    assert!(
        !store.shared_dir().join(accounts::SIGNIN_LOCK_FILE).exists(),
        "the sign-in lock was never taken"
    );

    // THE HOLD (§5.6): the token awaits a reading of the row's stored login, so where that login
    // works here (its record names the row's email, the roster saying which login is the row's)
    // the row is not on its token, and renewal goes on for it.
    std::fs::write(
        lab.paths.home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}"}}}}"#),
    )
    .expect("the login record");
    assert_eq!(
        journal::with_keyer(keyer, || {
            rows_on_token_read(&lab.paths, &store, &roster, env.now)
        }),
        Some(BTreeMap::new()),
        "held while the stored login works here"
    );
}

/// WINDOWS SHARED WITH ANOTHER ACCOUNT HERE WRITE NOTHING, AND ARE COMPARED AGAIN THE NEXT WEEK
/// ONLY AGAINST THE ACCOUNTS THAT SHARED THEM (§5.6): m7's window-made token, whose reading shows
/// the basis's windows while another account's current windows here (`other-0`'s) show them too,
/// is `shared`, nothing is due, and the row keeps `other-0` as the account that shared them. Later
/// that week a token reading compares nothing, though `other-0` is no longer read here, and a
/// stored-login reading of the same week is recorded as the basis and compares nothing either. A
/// basis of the next week reopens it: the kept reading and its verdict go, `other-0` stays. The
/// next token reading, the next week's windows, verifies it only where this Mac reads `other-0`'s
/// current windows apart from them: with `other-0` idle here that week (no current windows: a
/// token minted in its browser session would read its windows still), it is `shared` again and
/// nothing is due; with `other-0` still showing them, `shared`; with `other-0`'s windows apart,
/// `equal`, and the write is due. NEGATIVE CONTROL (2026-11-14, by hand, then restored): with
/// `Verification::compare`'s rule for the accounts that shared the windows cut, the idle case
/// leaves the write due.
#[test]
fn windows_shared_with_another_account_are_compared_again_the_next_week() {
    let next = NOW + 4 * 86_400;
    let week = stored_at(next);
    let stored = stored_at(NOW);
    assert_ne!(
        week.week_reset, stored.week_reset,
        "PRECONDITION: a later week"
    );
    for (case, others_next) in [
        ("idle", vec![]),
        ("sharing", vec![week]),
        ("apart", vec![others_at(next)]),
    ] {
        let scratch = Scratch::new(&format!("shared-week-{case}"));
        let store = scratch.store();
        let token = unverified_by(false, MintedBy::Window);
        let verdict = || scratch.table().rows[KEY].verification.verdict;
        assert!(stored_reading(&store, &token, stored, NOW));
        assert_eq!(
            note_served(&store, &token, &served(stored, vec![stored]), NOW + 30).unwrap(),
            Due::Nothing,
            "{case}: shared windows write nothing"
        );
        assert_eq!(verdict(), Some(Verdict::Shared));
        assert_eq!(
            scratch.table().rows[KEY].verification.shared_with,
            BTreeSet::from(["other-0".to_owned()]),
            "{case}: the account that shared them is kept"
        );
        assert_eq!(
            note_served(&store, &token, &served(stored, vec![]), NOW + 60).unwrap(),
            Due::Nothing,
            "{case}: the same week compares nothing again"
        );
        assert_eq!(verdict(), Some(Verdict::Shared));
        assert!(
            stored_reading(&store, &token, stored, NOW + 90),
            "{case}: a basis of the same week is recorded"
        );
        assert_eq!(
            verdict(),
            Some(Verdict::Shared),
            "{case}: and compares nothing"
        );

        assert!(stored_reading(&store, &token, week, next));
        let row = scratch.table().rows.remove(KEY).expect("the row");
        assert_eq!(
            (row.verification.verdict, row.verification.reading),
            (None, None),
            "{case}: a basis of a later week reopens it"
        );
        assert_eq!(
            row.verification.shared_with,
            BTreeSet::from(["other-0".to_owned()]),
            "{case}: and keeps who shared the windows"
        );
        let due = note_served(&store, &token, &served(week, others_next), next + 30).unwrap();
        if case == "apart" {
            assert!(matches!(due, Due::Verify(_)), "{case}: {due:?}");
            assert_eq!(verdict(), Some(Verdict::Equal));
        } else {
            assert_eq!(due, Due::Nothing, "{case}: nothing verified");
            assert_eq!(verdict(), Some(Verdict::Shared), "{case}");
        }
    }
}

/// A `shared` VERDICT THAT NAMES NO ACCOUNT IS FINAL, AS THE BUILD THAT WROTE IT MADE IT (§5.6):
/// an earlier build's check file holds `shared` with no accounts beside it (or an earlier build
/// wrote back a later one's, dropping them). No later comparison could tell the token's account
/// apart from accounts nobody named, so a basis of a later week records nothing and reopens
/// nothing, and once the basis's week has ended the token is not held for a stored-login reading:
/// it stays `unverified (windows shared)`. CONTROL: the same verdict naming `other-0` awaits a
/// stored-login reading once its week has ended.
#[test]
fn a_shared_verdict_naming_no_account_is_final() {
    let token = unverified_by(false, MintedBy::Window);
    let found = Lookup::Found(token.clone());
    let next = NOW + 4 * 86_400;
    for named in [false, true] {
        let scratch = Scratch::new(&format!("shared-unnamed-{named}"));
        let store = scratch.store();
        assert!(stored_reading(&store, &token, stored_at(NOW), NOW));
        update_table(&store, |table| {
            let check = &mut table.rows.get_mut(KEY).expect("the row").verification;
            check.verdict = Some(Verdict::Shared);
            check.reading = Some(stored_at(NOW));
            if named {
                check.shared_with.insert("other-0".to_owned());
            }
        })
        .unwrap();
        let why = match TokenState::of(&found, scratch.table().rows.get(KEY), next) {
            TokenState::Unverified { why, .. } => why,
            state => panic!("named: {named}: {state:?}"),
        };
        assert_eq!(
            why,
            if named {
                Unverified::AwaitingBasis
            } else {
                Unverified::WindowsShared
            },
            "named: {named}"
        );
        assert_eq!(
            stored_reading(&store, &token, stored_at(next), next),
            named,
            "named: {named}: a later week's basis reopens only a verdict that names its accounts"
        );
        let check = scratch
            .table()
            .rows
            .remove(KEY)
            .expect("the row")
            .verification;
        assert_eq!(
            check.verdict,
            (!named).then_some(Verdict::Shared),
            "named: {named}"
        );
    }
}

/// WINDOWS FOUND SHARED HAND THE TOKEN BACK TO THE HOLD ONCE THEIR WEEK ENDS, AS THE TOKEN ROUTE
/// JUDGES IT (§5.6, [`standing_judged`]): m7's window-made token, in use on every Mac, has no row
/// here, so while this Mac's stored login of the account works the route holds it for a basis.
/// The stored login's reading lifts the hold; the token's reading then shows the basis's windows
/// and `other-0`'s, so it is `shared`, and through that week the route puts launches on the token
/// (it serves, and nothing compares it). Once that basis's week has ended, the route holds the
/// token again, so a launch runs on the stored login and its reading gives the later week's basis,
/// which reopens the comparison and lifts the hold; the token's next reading, `other-0`'s windows
/// apart from it, leaves the write due. NEGATIVE CONTROL (2026-11-14, by hand, then restored):
/// with `Unverified::of` judging `shared` before an ended basis, the route puts every launch of
/// that week on the token, no stored-login reading comes, and the token stays `shared`.
#[test]
fn windows_shared_in_a_week_that_ended_hold_the_token_for_a_stored_login_reading() {
    let scratch = Scratch::new("shared-route");
    let store = scratch.store();
    let token = unverified_by(false, MintedBy::Window);
    let found = Lookup::Found(token.clone());
    let roster = Roster::load_for(Cli::Claude, &scratch.path().join("accounts.toml")).unwrap();
    login_record(&scratch, EMAIL);
    let stored = stored_at(NOW);
    assert_eq!(
        judged(&scratch, &found, &roster, NOW),
        TokenFact::AwaitingBasis,
        "held for its basis"
    );
    assert!(stored_reading(&store, &token, stored, NOW + 30));
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 60),
        TokenFact::InUse
    );
    assert_eq!(
        note_served(&store, &token, &served(stored, vec![stored]), NOW + 90).unwrap(),
        Due::Nothing
    );
    assert_eq!(
        scratch.table().rows[KEY].verification.verdict,
        Some(Verdict::Shared)
    );
    assert_eq!(
        judged(&scratch, &found, &roster, NOW + 120),
        TokenFact::InUse,
        "through its week the token serves"
    );

    let next = NOW + 4 * 86_400;
    assert_eq!(
        judged(&scratch, &found, &roster, next),
        TokenFact::AwaitingBasis,
        "its week ended: held for a stored-login reading"
    );
    let week = stored_at(next);
    assert!(stored_reading(&store, &token, week, next + 30));
    assert_eq!(
        judged(&scratch, &found, &roster, next + 60),
        TokenFact::InUse,
        "the later week's basis lifts the hold"
    );
    assert!(matches!(
        note_served(
            &store,
            &token,
            &served(week, vec![others_at(next)]),
            next + 90
        )
        .unwrap(),
        Due::Verify(_)
    ));
}

/// A PAIR THAT DOES NOT READ AS ONE TOKEN RECORDS NOTHING AND CREDITS NO READING (FL-134): two
/// Macs' concurrent writes split the pair (`mismatched (draft)`) until a settle, and its meta is
/// the meta register's winner, which may be of a token the settle drops (another Mac's later
/// replacement, one the token register no longer holds). A stored-login reading then records no
/// basis and moves no row to that meta's token, and a token reading leaves `Due::Apart`, a
/// verified meta beside it or not: a verification beside two tokens may be of the token a
/// replacement replaced, so it says nothing of whose this session's reading is. CONTROL: the same
/// readings against the pair read as one token record the basis and credit the reading.
/// NEGATIVE CONTROLS (2026-11-15, by hand, then restored): with `note_basis`'s check of the pair
/// cut, "gives no basis" fails; with `note_served`'s cut, "a verification beside two tokens
/// credits no reading" fails.
#[test]
fn a_split_pair_records_nothing_and_credits_no_reading() {
    let scratch = Scratch::new("split-pair");
    let store = scratch.store();
    let token = unverified_by(true, MintedBy::Window);
    minted_row(&store, &token);
    let before = scratch.table();
    let mut split = token.clone();
    split.check = Check::Mismatched(Mismatch::Draft);
    split.meta.as_mut().expect("a meta").minted_at += 600;
    let stored = stored_at(NOW);
    assert!(
        !stored_reading(&store, &split, stored, NOW),
        "a pair that does not read as one token gives no basis"
    );
    let mut stamped = split.clone();
    stamped.meta.as_mut().expect("a meta").verified = Some(Verified {
        on: "m7".to_owned(),
        at: NOW - 3_600,
        by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
        seven_day_resets_at: None,
        five_hour_resets_at: None,
    });
    assert!(stamped.verified(), "CONTROL: the meta reads verified");
    assert_eq!(
        note_served(&store, &stamped, &served(stored, vec![]), NOW + 30).unwrap(),
        Due::Apart,
        "a verification beside two tokens credits no reading"
    );
    assert_eq!(
        note_served(&store, &split, &served(stored, vec![]), NOW + 60).unwrap(),
        Due::Apart,
        "and its reading, of a token not verified, is no row's"
    );
    assert_eq!(scratch.table(), before, "no row made, moved or compared");
    assert!(
        stored_reading(&store, &token, stored, NOW + 90),
        "CONTROL: the pair read as one token takes the basis"
    );
    assert_eq!(
        note_served(&store, &token, &served(stored, vec![]), NOW + 120).unwrap(),
        Due::Verify(verify_of(
            token.meta.as_ref().expect("a meta"),
            Some(stored),
            NOW + 120
        )),
        "CONTROL: and its reading is compared"
    );
}

/// A READING THAT RECORDS NOTHING REWRITES NEITHER TABLE FILE (FL-134): a row whose token this
/// Mac found `equal` (its write still due) or `differ` is final, so the cheap look finds none
/// ([`basis_wanted`]), yet the tap still looks at the vault once per reading heartbeat for the
/// account's stored login, and its reading reaches [`note_basis`], which records nothing on a
/// final row. Neither `tokens.json` nor the check file is written again then: each write replaces
/// its file, so an unchanged inode is a file nothing rewrote. CONTROL: a reading that records a
/// basis replaces both. NEGATIVE CONTROL (2026-11-15, by hand, then restored): with
/// `update_parts` writing an unchanged table, "nothing rewritten" fails.
#[test]
fn a_reading_that_records_nothing_rewrites_no_table_file() {
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new("no-rewrite");
    let store = scratch.store();
    let token = unverified_by(true, MintedBy::Window);
    minted_row(&store, &token);
    let files = || {
        [TABLE_FILE, CHECK_FILE].map(|file| {
            std::fs::metadata(store.shared_dir().join(file))
                .expect("a table file")
                .ino()
        })
    };
    for verdict in [Verdict::Equal, Verdict::Differ] {
        update_table(&store, |table| {
            table
                .rows
                .get_mut(KEY)
                .expect("the row")
                .verification
                .verdict = Some(verdict);
        })
        .unwrap();
        assert!(!basis_wanted(&store, &key()), "{verdict:?}: final");
        let before = files();
        assert!(
            !stored_reading(&store, &token, stored_at(NOW), NOW),
            "{verdict:?}: records nothing"
        );
        assert_eq!(files(), before, "{verdict:?}: nothing rewritten");
    }
    update_table(&store, |table| {
        table
            .rows
            .get_mut(KEY)
            .expect("the row")
            .verification
            .verdict = None;
    })
    .unwrap();
    let before = files();
    assert!(stored_reading(&store, &token, stored_at(NOW), NOW));
    let after = files();
    assert!(
        before.iter().zip(&after).all(|(was, now)| was != now),
        "CONTROL: a basis rewrites both"
    );
}

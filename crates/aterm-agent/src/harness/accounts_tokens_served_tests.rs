// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A token session's served facts and verification by windows
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6, §5.7, §5.8, slice S11): the basis is the
//! stored login's windows from before any token reading was folded; equal windows on the minting
//! Mac write `verified` into the scratch vault, and the token then serves a Mac that did not mint
//! it; unequal or shared windows, and a row-less account, leave it `unverified (<why>)`; first
//! and last served are written under `tokens.lock`; and renewal is off for a row on its token.

use super::*;

/// Seconds in a day.
const DAY: i64 = 86_400;

/// The stored login's windows as the row's sample holds them: a week ending in three days, a
/// session in two hours, both after [`NOW`].
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

/// The account's token as this Mac's vault reads it: made here (`here`) or on m7, not verified.
fn unverified(here: bool) -> Standing {
    match standing(Check::Readable, NOW + A_YEAR, false, here, &[]) {
        Answer::Found(standing) => *standing,
        Answer::Absent | Answer::Unknown => unreachable!(),
    }
}

fn served(basis: Windows, reading: Windows, others: Vec<Windows>) -> Served {
    Served {
        label: "work".to_owned(),
        basis,
        reading,
        others,
    }
}

/// THE BASIS IS THE STORED LOGIN'S, RECORDED BEFORE ANY TOKEN READING IS FOLDED: the first served
/// reading records the row's stored-login windows its caller read before the fold, even with no
/// rate limits on it; the next reading, whose caller reads the token's own figures back from the
/// row, is compared with that recorded basis, never with itself, so another account's windows read
/// `differ`. NEGATIVE CONTROL: a basis taken from the folded token reading would verify it.
#[test]
fn the_basis_is_recorded_before_the_first_token_reading_is_folded() {
    let scratch = Scratch::new("basis");
    let store = scratch.store();
    let token = unverified(true);
    let (stored, theirs) = (stored_at(NOW), others_at(NOW));
    let _ = notes();
    assert_eq!(
        note_served(
            &store,
            &token,
            &served(stored, Windows::default(), vec![]),
            NOW
        )
        .unwrap(),
        Due::Nothing,
        "a reading with no rate limits compares nothing"
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.verification.basis, Some((NOW, stored)));
    assert_eq!(row.verification.verdict, None);
    assert_eq!(row.label.as_deref(), Some("work"));

    // The row's sample now holds the token's own reading.
    assert_eq!(
        note_served(&store, &token, &served(theirs, theirs, vec![]), NOW + 60).unwrap(),
        Due::Nothing
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        row.verification.basis,
        Some((NOW, stored)),
        "the basis stays"
    );
    assert_eq!(row.verification.first_reading, Some(theirs));
    assert_eq!(row.verification.verdict, Some(Verdict::Differ));

    let control = Scratch::new("basis-control");
    assert!(
        matches!(
            note_served(
                &control.store(),
                &token,
                &served(theirs, theirs, vec![]),
                NOW
            )
            .unwrap(),
            Due::Verify(_)
        ),
        "NEGATIVE CONTROL: a basis read after the fold compares the token with itself"
    );
    let noted = notes();
    assert_eq!(
        noted.iter().filter(|n| n.kind == "token-served").count(),
        2,
        "one first-served note for each table: {noted:?}"
    );
}

/// EQUAL WINDOWS ON THE MINTING MAC WRITE `verified`, AND THE TOKEN SERVES A MAC THAT DID NOT
/// MINT IT: on a scratch seated vault (m22) the token m22 minted is `unverified (no reading yet)`
/// and serves m22 alone; a served reading showing the row's stored-login windows leaves the write
/// due, which writes the meta's `verified` (by `windows`, the reading's resets) in one draft and
/// journals `token-verified`. The vault then reads it verified, so it is `active` here, and the
/// standing a Mac that did not mint it reads (the same records, `minted_here` false) turns from
/// served-nowhere to in use. `claude_token`'s own test reads it from a second real member.
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
    assert_eq!(
        TokenState::of(&found, None, now).words(),
        "unverified (no reading yet)"
    );
    let elsewhere = Standing {
        minted_here: false,
        ..minted.clone()
    };
    assert!(
        !elsewhere.in_use(now),
        "PRECONDITION: unverified, it serves no other Mac"
    );

    let _ = notes();
    let store = scratch.store();
    let stored = stored_at(now);
    let Due::Verify(evidence) = note_served(
        &store,
        minted,
        &served(stored, stored, vec![others_at(now)]),
        now,
    )
    .unwrap() else {
        panic!("equal windows leave the write due");
    };
    assert_eq!(evidence.by, "windows");
    assert_eq!(evidence.on, "m22");
    assert_eq!(
        evidence.seven_day_resets_at,
        stored.week_reset.and_then(|r| u64::try_from(r).ok())
    );
    assert!(
        verify_served(&store, &Vault(lab.paths.clone()), &key(), evidence, now).unwrap(),
        "the minting Mac writes it"
    );

    let found = claude_token::standing(&lab.paths, &key()).expect("a standing");
    let Lookup::Found(verified) = &found else {
        panic!("the vault holds the token");
    };
    assert!(verified.verified(), "the vault's meta says verified");
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
        elsewhere.state(now),
        aterm_secrets::claude_token::State::Active
    );
    assert!(
        elsewhere.in_use(now),
        "it now serves a Mac that did not mint it"
    );
    assert!(lab.holds_the_token(), "the same token");
    let kinds: Vec<&str> = notes().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["token-served", "token-verified"]);
    assert_eq!(
        note_served(&store, verified, &served(stored, stored, vec![]), now + 60).unwrap(),
        Due::Nothing,
        "a verified token compares nothing again"
    );
}

/// UNEQUAL WINDOWS LEAVE `unverified (windows differ)`, SERVED ON THE MINTING MAC ONLY: nothing
/// is due, a later reading that does show the stored login's windows is not compared again (the
/// first reading with rate limits decided it), and only the minting Mac's standing is in use.
#[test]
fn unequal_windows_leave_unverified_windows_differ() {
    let scratch = Scratch::new("differ");
    let store = scratch.store();
    let token = unverified(true);
    let stored = stored_at(NOW);
    let mut session_off = stored;
    session_off.session_reset = session_off.session_reset.map(|r| r + 60);
    for (why, reading) in [
        ("another week", others_at(NOW)),
        ("the week, but another current session", session_off),
    ] {
        let scratch = Scratch::new("differ-case");
        assert_eq!(
            note_served(
                &scratch.store(),
                &token,
                &served(stored, reading, vec![]),
                NOW
            )
            .unwrap(),
            Due::Nothing,
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
            note_served(&store, &token, &served(stored, no_session, vec![]), NOW).unwrap(),
            Due::Verify(_)
        ),
        "CONTROL: the week alone compares where the reading names no session"
    );

    let scratch = Scratch::new("differ-state");
    let store = scratch.store();
    note_served(&store, &token, &served(stored, others_at(NOW), vec![]), NOW).unwrap();
    assert_eq!(
        note_served(&store, &token, &served(stored, stored, vec![]), NOW + 60).unwrap(),
        Due::Nothing,
        "the first reading decided it"
    );
    let table = scratch.table();
    let row = table.rows.get(KEY);
    let here = Lookup::Found(token.clone());
    let state = TokenState::of(&here, row, NOW);
    assert_eq!(state.words(), "unverified (windows differ)");
    assert!(state.in_use(), "it serves the minting Mac");
    let elsewhere = Lookup::Found(Standing {
        minted_here: false,
        ..token
    });
    assert!(
        !TokenState::of(&elsewhere, None, NOW).in_use(),
        "and no other"
    );
}

/// EQUAL WINDOWS ANOTHER ACCOUNT HERE SHOWS TOO LEAVE `unverified (windows shared)`: equality
/// cannot tell the two accounts apart, so nothing writes `verified`. CONTROL: another account's
/// different windows leave the write due.
#[test]
fn windows_another_account_shares_leave_unverified_windows_shared() {
    let token = unverified(true);
    let stored = stored_at(NOW);
    let scratch = Scratch::new("shared");
    let store = scratch.store();
    assert_eq!(
        note_served(
            &store,
            &token,
            &served(stored, stored, vec![others_at(NOW), stored]),
            NOW
        )
        .unwrap(),
        Due::Nothing
    );
    let table = scratch.table();
    assert_eq!(table.rows[KEY].verification.verdict, Some(Verdict::Shared));
    assert_eq!(table.rows[KEY].verified, None, "nothing wrote verified");
    assert_eq!(
        TokenState::of(&Lookup::Found(token.clone()), table.rows.get(KEY), NOW).words(),
        "unverified (windows shared)"
    );
    let control = Scratch::new("shared-control");
    assert!(matches!(
        note_served(
            &control.store(),
            &token,
            &served(stored, stored, vec![others_at(NOW)]),
            NOW
        )
        .unwrap(),
        Due::Verify(_)
    ));
}

/// A ROW-LESS ACCOUNT'S TOKEN STAYS `unverified (no roster row)`: its table row names no label
/// (the verb ran on an email no row names), so no stored-login reading under a row compares, and
/// it serves only its minting Mac. CONTROL: with a label it waits for a reading.
#[test]
fn a_row_less_accounts_token_stays_unverified_no_roster_row() {
    let scratch = Scratch::new("rowless");
    let store = scratch.store();
    let token = unverified(true);
    update_table(&store, |table| {
        let row = row_following(table, &token, token.meta.as_ref().unwrap());
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
        "unverified (no reading yet)"
    );
}

/// FIRST AND LAST SERVED ARE RECORDED UNDER `tokens.lock`: while another holds it, nothing is
/// written (the call says the lock stayed held); once it is free, the first reading sets both,
/// and a later one moves only the last. A Mac that did not mint the token records its served
/// facts too, and no basis.
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
    assert!(note_served(&store, &token, &served(stored, stored, vec![]), NOW).is_err());
    assert!(scratch.table().rows.is_empty(), "nothing written meanwhile");
    drop(held);
    let _ = notes();
    note_served(&store, &token, &served(stored, stored, vec![]), NOW).unwrap();
    note_served(&store, &token, &served(stored, stored, vec![]), NOW + 90).unwrap();
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.used_on.first_served_at, Some(NOW));
    assert_eq!(row.used_on.last_served_at, Some(NOW + 90));
    assert_eq!(
        row.verification,
        Verification::default(),
        "no basis off the minting Mac"
    );
    assert_eq!(notes().len(), 1, "one token-served note");
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
}

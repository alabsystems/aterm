// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every word a harness note can carry is a word of the journal's vocabulary and the widest
/// note of each shape the harness makes fits one event, so no note is dropped at the glue;
/// and the two realm tables agree: the harness's claims key and the lab's account key name
/// one account (design §5).
/// NEGATIVE CONTROL: with `event` dropping the other account (`to: None`), the switch names
/// no account: `` called `Option::unwrap()` on a `None` value ``.
#[test]
fn every_harness_word_is_a_journal_word() {
    let account = journal::Key::new("ac-0123456789abcdef").unwrap();
    for kind in journal::KINDS {
        assert!(
            Kind::parse(kind).is_some(),
            "a harness kind the journal does not take: {kind:?}"
        );
    }
    for window in journal::WINDOWS {
        assert!(Window::parse(window).is_some(), "{window}");
    }
    for src in journal::SOURCES {
        assert!(Source::parse(src).is_some(), "{src}");
    }
    for res in journal::RESES {
        assert!(Res::parse(res).is_some(), "{res}");
    }
    for why in journal::WHYS {
        assert!(Word::<16>::new(why).is_some(), "{why}");
    }
    for cli in Cli::ALL {
        assert!(Word::<10>::new(cli.id()).is_some(), "{}", cli.id());
    }
    // The widest note of each shape the harness makes (`observe`): a reading, a wall, a
    // switch, an error with its reason. Each fits one event (192 B).
    let base = Note {
        t: 9_999_999_999,
        kind: "quota",
        account: Some(account),
        cli: "claude",
        sub: None,
        window: Some("7d-model"),
        pct_x10: Some(1000),
        reset: Some(9_999_999_999),
        res: Some("exact"),
        until: None,
        src: "session-usage",
        why: None,
        to: None,
        tab: None,
        harness: None,
    };
    let shapes = [
        base,
        Note {
            kind: "wall",
            pct_x10: None,
            reset: None,
            res: None,
            until: Some(9_999_999_999),
            src: "cli-output",
            ..base
        },
        Note {
            kind: "switch",
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            src: "cli-output",
            to: journal::Key::new("ac-fedcba9876543210"),
            ..base
        },
        Note {
            kind: "sign-in-failed",
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            src: "signin-flow",
            why: Some("reset-failed"),
            ..base
        },
        // A tab's sighting, with its tab and harness and the believed account of a
        // wrong-account beside it.
        journal::seen_note(
            Cli::Codex,
            account,
            journal::Tab::of("machine", "s-41a2"),
            "codex-alab-agent",
            journal::ObservedSrc::VaultToken,
            journal::SeenWhy::Changed,
            9_999_999_999,
        ),
        journal::wrong_account_note(
            Cli::Claude,
            account,
            journal::Key::new("ac-fedcba9876543210"),
            journal::Tab::of("machine", "s-41a2"),
            journal::ObservedSrc::Profile,
            9_999_999_999,
        ),
        // The machine sweep's sighting of an idle login, which names no tab.
        journal::idle_seen_note(
            Cli::Kimi,
            account,
            journal::ObservedSrc::AuthFile,
            journal::SeenWhy::First,
            9_999_999_999,
        ),
    ];
    let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
    for note in &shapes {
        let event = event(note).unwrap();
        assert_eq!(event.kind.word(), note.kind);
        assert!(event.encode(&mut buf).is_some(), "{note:?} fits an event");
    }
    let switch = event(&shapes[2]).unwrap();
    assert_eq!(switch.kind, Kind::Switch);
    assert_eq!(switch.to.unwrap().as_str(), "ac-fedcba9876543210");
    for realm in [
        "claude",
        "anthropic",
        "codex",
        "openai",
        "grok",
        "xai",
        "kimi",
        "kimi:mainland-cn",
        "agy",
        "antigravity",
    ] {
        let lease =
            aterm_agent::harness::claims::LeaseKey::new(realm, "Ann@Example.invalid").unwrap();
        assert_eq!(
            aterm_db::typed::keys::account_pk(&lease.pool, &lease.email).unwrap(),
            aterm_db::typed::keys::account_pk(realm, "ann@example.invalid").unwrap(),
            "{realm}"
        );
    }
    // An agy row's ledger events are keyed as the Google account a read of agy is attributed
    // to (`journal::realm_key("google", …)`): one account, one key, whichever path names it.
    let agy = aterm_agent::harness::claims::LeaseKey::for_cli(
        aterm_agent::harness::accounts::Cli::Agy,
        None,
        "Ann@Example.invalid",
    )
    .unwrap();
    assert_eq!(
        aterm_db::typed::keys::account_pk(&agy.pool, &agy.email).unwrap(),
        aterm_db::typed::keys::account_pk("google", "ann@example.invalid").unwrap(),
    );
    // CONTROL: another realm's key for the same address differs, so the equality above is
    // the mapping's and not a keyer that ignores its realm.
    assert_ne!(
        aterm_db::typed::keys::account_pk(&agy.pool, &agy.email).unwrap(),
        aterm_db::typed::keys::account_pk("meta", "ann@example.invalid").unwrap(),
    );
}

/// The backfill is marked done only once its notes are in the spool: a spool that cannot
/// be written leaves it unmarked, so the next start backfills again and no note is lost.
/// NEGATIVE CONTROL: with `record` marking first (`journal::mark_backfilled(state)?` before
/// the appends), the failed backfill is marked done: `` the failed backfill is not marked ``.
#[test]
fn a_backfill_is_marked_only_once_its_notes_are_spooled() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("harness");
    std::fs::create_dir_all(&state).unwrap();
    let blocked = scratch.path().join("alab");
    std::fs::write(&blocked, b"").unwrap();
    let spool = Spool::at(blocked.clone());
    let note = Note {
        t: 1_800_000_000,
        kind: "signed-in",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "claude",
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "cli-output",
        why: None,
        to: None,
        tab: None,
        harness: None,
    };
    assert!(record(&spool, &state, &[note]).is_err());
    assert!(
        !journal::backfilled(&state),
        "the failed backfill is not marked"
    );
    std::fs::remove_file(&blocked).unwrap();
    record(&spool, &state, &[note]).unwrap();
    assert!(journal::backfilled(&state));
    let spooled =
        std::fs::read_to_string(blocked.join(aterm_db::alab::journal::spool::SPOOL)).unwrap();
    assert_eq!(spooled.lines().count(), 1);
}

/// THE FLEET SAYS WHAT THE STORE HOLDS, end to end: each step is a real account transaction
/// (the recorders the harness calls, through `Store::transact_logins`, whose commit runs
/// `journal::observe` on the ledger rows it writes), its notes go through the glue's `event`
/// into a spool, and after every one the three folds a Mac's sign-in word is read from agree
/// with the login word the store then holds: this Mac's book (its `use` row and metrics
/// row), a peer that holds its events, and a peer that holds its `use` row too. The steps are
/// the audit's cases: a login wall, a failed sign-in, a renewal whose attempt row follows it
/// in the same second, a failed and a wrong-account sign-in of a login that still works, a
/// served turn that proves a login with no auth row, a displacement, a struck canary, a wall
/// older than the word the store holds, a forgotten row, and two changes in one second.
/// NEGATIVE CONTROLS: with `of_ledger` mapping every `Kind::SignIn` row to `sign-in-failed`
/// and `settle` telling every row's word (as before), the begun attempt is published as a
/// failure and the word list differs (`assertion `left == right` failed`; the state's own
/// word, told after, still heals the renewal); with `settle`'s closing comparison skipped,
/// `` a served turn proves it: the store holds Ok, the folds FoldedSignIn { book:
/// Some("ended"), … } ``.
#[test]
fn the_fleet_fold_ends_on_the_sign_in_state_the_store_holds() {
    use aterm_agent::harness::accounts::{
        self, AttemptOutcome, Auth, Event as Row, Logins, State, Store, Strike,
    };
    use aterm_db::alab::journal::fold_signin_for_test;
    const ANN: &str = "ac-0123456789abcdef";
    const BOB: &str = "ac-fedcba9876543210";
    type Step = fn(&mut State, &mut Logins, i64) -> Vec<Row>;
    fn key(state: &mut State) {
        for (label, pk) in [("ann", ANN), ("bob", BOB)] {
            state.accounts.entry(label.into()).or_default().pk = Some(pk.into());
        }
    }
    // (what happened, seconds since the step before, the transaction's edit)
    let steps: [(&str, i64, Step); 17] = [
        ("first seen signed in", 60, |state, _, at| {
            key(state);
            accounts::note_auth(state, "ann", Auth::Ok, at)
        }),
        ("a login wall", 60, |state, logins, at| {
            accounts::note_login_end(state, logins, "ann", at)
        }),
        ("a sign-in begun", 60, |_, _, at| {
            vec![
                Row::sign_in(at, "ann", accounts::SignInStep::Begun)
                    .because("shared preparation begun"),
            ]
        }),
        ("a sign-in that failed", 60, |_, logins, at| {
            accounts::note_sign_in_attempt(logins, "ann", AttemptOutcome::Failed, "device", at)
        }),
        (
            "renewed, its attempt row after it",
            60,
            |state, logins, at| {
                let mut rows =
                    accounts::note_renewed(state, logins, "ann", None, "/logins/ann-2", None, at);
                rows.extend(accounts::note_sign_in_attempt(
                    logins,
                    "ann",
                    AttemptOutcome::SignedIn,
                    "device",
                    at,
                ));
                rows
            },
        ),
        (
            "a failed sign-in of a working login",
            60,
            |_, logins, at| {
                accounts::note_sign_in_attempt(logins, "ann", AttemptOutcome::Blocked, "page", at)
            },
        ),
        (
            "a wrong account, the login still works",
            60,
            |_, logins, at| accounts::note_wrong_account(logins, "ann", at),
        ),
        ("a browser window ended", 60, |_, _, at| {
            vec![
                Row::sign_in(at, "ann", accounts::SignInStep::Window)
                    .because("x window closed no-session"),
            ]
        }),
        ("a login wall again", 60, |state, logins, at| {
            accounts::note_login_end(state, logins, "ann", at)
        }),
        ("a served turn proves it", 60, |state, _, at| {
            state.active = Some("ann".into());
            state.proven = false;
            accounts::prove(state, "ann", at).into_iter().collect()
        }),
        (
            "displaced from the default login",
            60,
            |state, logins, at| accounts::note_displaced(state, logins, "ann", "bob", at),
        ),
        (
            "served again after being signed out",
            60,
            |state, logins, at| accounts::note_signed_back(state, logins, "ann", at),
        ),
        ("a canary struck signed out", 60, |state, _, at| {
            accounts::strike(state, "ann", "s-1", &Strike::SignedOut, at)
        }),
        ("signed in, in the same second", 0, |state, _, at| {
            accounts::note_auth(state, "ann", Auth::Ok, at)
        }),
        (
            "a wall older than the word held",
            60,
            |state, logins, at| accounts::note_login_end(state, logins, "ann", at - 3600),
        ),
        ("the row forgotten", 60, |state, logins, _| {
            accounts::forget_account(state, logins, "ann");
            Vec::new()
        }),
        ("the row back, signed out", 60, |state, _, at| {
            key(state);
            accounts::note_auth(state, "ann", Auth::SignedOut, at)
        }),
    ];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at_for(
        &scratch.path().join("harness"),
        Cli::Claude,
        &scratch.path().join("home"),
    );
    let spool = Spool::at(scratch.path().join("alab"));
    let ann = Pk::new(ANN).unwrap();
    let mut now = 1_800_000_000;
    let mut words = Vec::new();
    for (what, later, step) in steps {
        now += later;
        let (done, notes) = journal::capture(|| {
            store.transact_logins(now, &mut None, |state, logins| Ok(step(state, logins, now)))
        });
        done.unwrap_or_else(|why| panic!("{what}: {why}"));
        for note in &notes {
            let event = event(note).unwrap_or_else(|| panic!("{what}: {note:?} is no event"));
            assert!(
                matches!(spool.append(&event), Ok(Appended::Written)),
                "{what}"
            );
        }
        let (state, _) = store.load(now).unwrap();
        let held = state
            .accounts
            .get("ann")
            .map_or(Auth::Unseen, accounts::Account::auth_on_record);
        let folds = fold_signin_for_test(&spool, &ann, now.unsigned_abs()).unwrap();
        for fold in [folds.book, folds.peer_events, folds.peer] {
            let word = fold.map_or(Some(journal::SignWord::Unknown), journal::SignWord::parse);
            assert!(
                word.is_some_and(|word| word.agrees(held)),
                "{what}: the store holds {held:?}, the folds {folds:?}"
            );
        }
        assert_eq!(folds.book, folds.peer, "{what}");
        assert_eq!(folds.book, folds.peer_events, "{what}");
        words.push(folds.book);
    }
    // Which word, not only that it agrees: a success is `in`, a begun attempt changes
    // nothing, a heal is heard, and a forgotten row leaves no word.
    assert_eq!(
        words,
        [
            Some("in"),
            Some("ended"),
            Some("ended"),
            Some("failed"),
            Some("in"),
            Some("in"),
            Some("in"),
            Some("in"),
            Some("ended"),
            Some("in"),
            Some("out"),
            Some("in"),
            Some("out"),
            Some("in"),
            Some("in"),
            None,
            Some("out"),
        ]
    );
}

/// Before any harness has created its state directory, or when the clock is not yet past the
/// Unix epoch, `backfill_at` leaves the backfill unmarked instead of creating `<state>/alab`
/// and sealing an empty pass; and `event` rejects zero-timestamp or over-100% quota notes
/// rather than stripping `pct_x10`.
/// NEGATIVE CONTROL: with `backfill` calling `record` unconditionally on `!state.is_dir()`,
/// `journal::mark_backfilled` creates `state` and marks it done before any harness runs.
#[test]
fn missing_harness_state_or_pre_epoch_clock_leaves_backfill_unmarked_and_bad_notes_drop() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let missing_state = scratch.path().join("no-harness-yet");
    backfill_at(&spool, &missing_state, scratch.path(), None, 1_800_000_000).unwrap();
    assert!(!missing_state.exists());
    assert!(!journal::backfilled(&missing_state));

    let present_state = scratch.path().join("harness");
    std::fs::create_dir_all(&present_state).unwrap();
    backfill_at(&spool, &present_state, scratch.path(), None, 0).unwrap();
    assert!(!journal::backfilled(&present_state));

    backfill_at(&spool, &present_state, scratch.path(), None, 1_800_000_000).unwrap();
    assert!(journal::backfilled(&present_state));

    let account = journal::Key::new("ac-0123456789abcdef").unwrap();
    let zero_time = Note {
        t: 0,
        kind: "signed-in",
        account: Some(account),
        cli: "claude",
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "cli-output",
        why: None,
        to: None,
        tab: None,
        harness: None,
    };
    assert!(event(&zero_time).is_none());
    let over_pct = Note {
        t: 1_800_000_000,
        kind: "quota",
        window: Some("5h"),
        pct_x10: Some(1001),
        ..zero_time
    };
    assert!(event(&over_pct).is_none());
}

/// A note's workspace and a realm-only probe reach the journal as they are: the workspace as
/// the event's `s`, an agy read with no login as a `probe` naming no account; a note of any
/// other kind without an account is dropped at the glue, as every reader would drop it.
/// NEGATIVE CONTROL: with `event` mapping no workspace (`sub: None`), the reading's `s` is
/// lost: `` left: None right: Some(0123456789ab) ``.
#[test]
fn a_workspace_and_a_realm_only_probe_reach_the_journal() {
    let reading = Note {
        t: 1_800_000_000,
        kind: "quota",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "codex",
        sub: journal::SubKey::new("0123456789ab"),
        window: Some("7d"),
        pct_x10: Some(250),
        reset: Some(1_800_003_600),
        res: Some("exact"),
        until: None,
        src: "rollout",
        why: None,
        to: None,
        tab: None,
        harness: None,
    };
    assert_eq!(event(&reading).unwrap().sub, Word::new("0123456789ab"));
    let probe = Note {
        kind: "probe",
        account: None,
        cli: "agy",
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        src: "probe",
        why: Some("launching"),
        ..reading
    };
    let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
    let noted = event(&probe).unwrap();
    let len = noted.encode(&mut buf).unwrap();
    let line = &buf[..len];
    assert_eq!(Event::from_line(line), Some(noted), "every reader takes it");
    assert_eq!(noted.account, None);
    let signin = Note {
        kind: "signed-in",
        src: "signin-flow",
        why: None,
        ..probe
    };
    assert_eq!(event(&signin), None);
}

/// A note made while `[alab] journal = false` is never spooled, so no pass can publish it
/// once the journal is on again; one made after it is turned on is.
/// NEGATIVE CONTROL: with `Glue::note` not asking `publishing` (its check removed), the
/// sign-in noted while off is spooled: `` nothing is spooled while off ``.
#[test]
fn a_note_made_while_the_journal_is_off_is_never_spooled() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    let glue = Glue {
        spool: Some(Spool::at(scratch.path().join("alab"))),
        publishing: Publishing::new(Some(config.clone())),
    };
    let note = Note {
        t: 1_800_000_000,
        kind: "signed-in",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "claude",
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "signin-flow",
        why: None,
        to: None,
        tab: None,
        harness: None,
    };
    let spooled = scratch
        .path()
        .join("alab")
        .join(aterm_db::alab::journal::spool::SPOOL);
    assert!(!glue.note(&note), "a note made while off is not taken");
    assert!(!spooled.exists(), "nothing is spooled while off");
    std::fs::write(&config, "[alab]\njournal = true\n").unwrap();
    assert!(glue.note(&note), "a spooled note is taken");
    assert!(spooled.exists(), "on again, the note is spooled");
}

/// With `[alab] journal = false` the backfill is marked done and notes nothing, so turning
/// the journal on again publishes nothing from before.
/// NEGATIVE CONTROL: with `backfill_at` not reading `publishing` (its check removed), the
/// ledger's sign-in is spooled: `` nothing is spooled while off ``.
#[test]
fn a_backfill_with_the_journal_off_is_marked_and_notes_nothing() {
    use aterm_agent::harness::accounts::{Event as Row, Kind as RowKind, Store};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let now = 1_800_000_000;
    let store = Store::at_for(&state, Cli::Claude, &home);
    store
        .append(&[Row::new(now - 600, RowKind::SignedIn, "ann")])
        .unwrap();
    let mut keyed = accounts::State::default();
    keyed.accounts.insert(
        "ann".into(),
        accounts::Account {
            pk: Some("ac-0123456789abcdef".into()),
            ..accounts::Account::default()
        },
    );
    aterm_uds::private_file::write_private(
        store.state_file().parent().unwrap(),
        "state.json",
        (keyed.to_json() + "\n").as_bytes(),
    )
    .unwrap();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    backfill_at(&spool, &state, &home, Some(&config), now).unwrap();
    assert!(journal::backfilled(&state));
    assert!(
        !spool
            .dir()
            .join(aterm_db::alab::journal::spool::SPOOL)
            .exists(),
        "nothing is spooled while off"
    );
    // On, the same backfill notes the sign-in: the check above is what kept it out.
    let on = scratch.path().join("on");
    std::fs::create_dir_all(&on).unwrap();
    std::fs::rename(state.join("alab"), on.join("mark")).unwrap();
    backfill_at(&spool, &state, &home, None, now).unwrap();
    assert!(
        spool
            .dir()
            .join(aterm_db::alab::journal::spool::SPOOL)
            .exists()
    );
}

/// A Mac that lists no account holds the login its default home holds, and a default row
/// whose place holds another account's login reports that account, never its own.
/// NEGATIVE CONTROL: with `holdings_in` passing no home (`observe::sweep::places_of(cli,
/// &roster, None, on_token)`), the Mac with no roster reports nothing: `left: Some([]) right: Some([Holding {
/// .. Fill }])`.
#[test]
fn a_default_home_login_no_row_lists_is_held() {
    use aterm_agent::harness::{
        accounts::{Row, Vendor},
        alab_roster::{LabAccount, MachineRef},
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let account = |email: &str, preferred: Option<&str>| LabAccount {
        pk: aterm_db::typed::keys::account_pk("claude", email).unwrap(),
        cli: Cli::Claude,
        vendor: Vendor::default(),
        label: "claude-aaaaaa".into(),
        legacy_label: None,
        email: email.into(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .map(|machine| {
                (
                    MachineRef::from_registry(format!("fixture-{machine}"), Some(machine.into())),
                    Some(1),
                )
            })
            .into_iter()
            .collect(),
    };
    let (uni, ann) = ("uni@example.test", "ann@example.test");
    let lab = Lab {
        me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
        accounts: vec![account(uni, None), account(ann, Some("m3"))],
        ..Lab::default()
    };
    let held = |why| {
        Some(vec![Holding {
            account: Pk::new(&lab.accounts[0].pk).unwrap(),
            why,
        }])
    };
    std::fs::write(
        home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{uni}"}}}}"#),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        held(Why::Fill)
    );
    let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
    let row = Row {
        label: "m22".into(),
        email: Some(ann.into()),
        ..Row::default()
    };
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        held(Why::Fill),
        "the default row's place holds another account"
    );
    std::fs::remove_file(home.join(".claude.json")).unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        Some(vec![Holding {
            account: Pk::new(&lab.accounts[1].pk).unwrap(),
            why: Why::Borrow,
        }]),
        "nothing read: the row, the belief"
    );
}

/// The flusher's holdings come from every pooled CLI's roster beside `aterm.toml`, keyed by
/// the lab's account key with the harness's reason; a lab that names no machine for this Mac,
/// or a roster that is there and does not read, reports nothing known (`None`), so no place
/// row ends on a passing failure; a CLI with no roster and no login in its home holds nothing.
/// NEGATIVE CONTROL: with `holdings_in` skipping a roster that does not read (`continue`
/// instead of `?`), the broken roster reports `Some([])` and every place row would end
/// (`left: Some([]) right: None`).
#[test]
fn holdings_come_from_the_rosters_beside_the_config() {
    use aterm_agent::harness::{
        accounts::{Row, Vendor},
        alab_roster::{LabAccount, MachineRef},
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let email = "ann@example.test";
    let key = aterm_db::typed::keys::account_pk("claude", email).unwrap();
    let lab = Lab {
        me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
        accounts: vec![LabAccount {
            pk: key.clone(),
            cli: Cli::Claude,
            vendor: Vendor::default(),
            label: "claude-aaaaaa".into(),
            legacy_label: None,
            email: email.into(),
            identity: None,
            browser: None,
            joinable: true,
            preferred: vec![(
                MachineRef::from_registry("fixture-m3", Some("m3".into())),
                Some(1),
            )],
        }],
        ..Lab::default()
    };
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        Some(Vec::new()),
        "no roster"
    );
    let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
    let row = Row {
        label: "claude-aaaaaa".into(),
        dir: Some("/scratch/ann".into()),
        email: Some(email.into()),
        ..Row::default()
    };
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        Some(vec![Holding {
            account: Pk::new(&key).unwrap(),
            why: Why::Borrow,
        }])
    );
    let unnamed = Lab {
        me: None,
        ..lab.clone()
    };
    assert_eq!(holdings_in(&unnamed, &config, None, &OnToken::none()), None);
    std::fs::write(&roster, "[[account]]\nlabel = 7\n").unwrap();
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        None,
        "a broken roster"
    );
}

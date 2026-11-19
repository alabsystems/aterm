// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::store::{Ended, Outcome};

const NOW: i64 = 1_800_000_000;

pub(super) fn write_codex_login(home: &Path, email: &str) {
    fs::create_dir_all(home).unwrap();
    fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='file'\n",
    )
    .unwrap();
    let token = super::super::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": email,
        "https://api.openai.com/auth": {"chatgpt_user_id": email, "chatgpt_account_id": "fixture-workspace"}
    }));
    fs::write(
        home.join("auth.json"),
        aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        })
        .to_string(),
    )
    .unwrap();
}

fn review_case(provider: i64, location: i64, outcome: i64, case: i64) -> [bool; 5] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let cli = if provider == 0 {
        Cli::Codex
    } else {
        Cli::Claude
    };
    let home = root.path().join(cli.accounts().default_home);
    let store = accounts::Store::at_for(root.path(), cli, root.path());
    let path = root.path().join(cli.accounts().roster_file);
    let mut row = Row {
        label: "account".into(),
        dir: (location == 1).then(|| root.path().join("login").display().to_string()),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(cli, "", &[row.clone()]).unwrap(),
    )
    .unwrap();
    let context = if cli == Cli::Codex {
        write_codex_login(
            row.dir.as_deref().map_or(home.as_path(), Path::new),
            "fixture@example.test",
        );
        Some(store.capture_auth_context(&row).unwrap())
    } else {
        None
    };
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            accounts::note_served_login(logins, "account", NOW - 20);
            accounts::note_auth(state, "foreign", accounts::Auth::Ok, NOW - 20);
            let word = if outcome == 0 {
                accounts::Auth::SignedOut
            } else {
                accounts::Auth::Ok
            };
            if let Some(context) = context.clone() {
                accounts::stage_auth(state, &row, logins, context, word, NOW - 10);
                Ok(Vec::new())
            } else {
                Ok(accounts::note_auth(state, "account", word, NOW - 10))
            }
        })
        .unwrap();
    if matches!(case, 12 | 13) {
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.remove("account");
                Ok(Vec::new())
            })
            .unwrap();
    }
    let mut login = accounts::login_env_for(cli, &row);
    if case == 8 {
        login.1 = Some(
            root.path()
                .join("different/auth.json")
                .display()
                .to_string(),
        );
    }
    let witness = capture_review_login(&store, cli, &path, "account", &login, NOW);
    match case {
        1 => {
            store
                .transact_logins(NOW, &mut None, |_, logins| {
                    accounts::note_served_login(logins, "account", NOW - 5);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        2 => {
            store
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.get_mut("account").unwrap().renewal_generation += 1;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        3 | 4 => {
            if case == 3 {
                row.email = Some("replacement@example.test".into());
            } else {
                row.held_by = Some("m2".into());
            }
            fs::write(&path, accounts::append_rows_for(cli, "", &[row]).unwrap()).unwrap();
        }
        5 | 9 | 12 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::move_account(state, logins, "account", "away")?;
                    Ok(Vec::new())
                })
                .unwrap();
            if matches!(case, 9 | 12) {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::move_account(state, logins, "away", "account")?;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
        }
        10 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "foreign",
                        None,
                        root.path().join("foreign").to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        11 | 13 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::forget_account(state, logins, "account");
                    Ok(accounts::note_auth(
                        state,
                        "account",
                        if outcome == 0 {
                            accounts::Auth::SignedOut
                        } else {
                            accounts::Auth::Ok
                        },
                        NOW - 10,
                    ))
                })
                .unwrap();
        }
        14 | 15 if outcome == 2 => {
            let cleared_at = if case == 15 { NOW } else { NOW + 1 };
            store
                .transact(cleared_at, &mut None, |state| {
                    Ok(vec![accounts::pin(
                        state,
                        &accounts::Roster::load(&path)?,
                        &["account".to_owned()].into_iter().collect(),
                        "account",
                        cleared_at,
                    )?])
                })
                .unwrap();
        }
        16 => {
            if location == 0 {
                record_default_sign_in(&store, cli, &path, witness.as_ref().unwrap(), &home, NOW)
                    .unwrap();
            } else {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        if let Some(context) = context.clone() {
                            accounts::stage_auth(
                                state,
                                &row,
                                logins,
                                context.clone(),
                                accounts::Auth::SignedOut,
                                NOW,
                            );
                            accounts::stage_signed_back(state, &row, logins, context, NOW);
                            return Ok(Vec::new());
                        }
                        let mut events =
                            accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
                        events.extend(accounts::note_checked_sign_in(
                            state, logins, "account", NOW,
                        ));
                        Ok(events)
                    })
                    .unwrap();
            }
        }
        _ => {}
    }
    let finish = if case > 13 { NOW + 2 } else { NOW };
    let before = store.load(finish).unwrap().0;
    let before_logins = store.load_logins(finish).unwrap().0;
    let other_store = accounts::Store::at_for(&root.path().join("other"), cli, root.path());
    let other_roster = root.path().join("other-roster.toml");
    let result = witness
        .as_ref()
        .ok_or_else(|| "no matching login witness".to_owned())
        .and_then(|witness| {
            record_review_login(
                if case == 6 { &other_store } else { &store },
                cli,
                if case == 7 { &other_roster } else { &path },
                witness,
                &home,
                match outcome {
                    0 => Outcome::Worked,
                    1 => Outcome::Ended(Ended::ReviewAuth),
                    2 => Outcome::Limited(Some(NOW + 3600)),
                    _ => unreachable!(),
                },
                finish,
            )
        });
    let after = store.load(finish).unwrap().0;
    let after_logins = store.load_logins(finish).unwrap().0;
    let applied = result.is_ok()
        && after
            .accounts
            .get("account")
            .is_some_and(|account| match outcome {
                0 => {
                    account.auth == accounts::Auth::Ok
                        && after_logins.sign_in_receipts.contains_key("account")
                }
                1 => account.auth == accounts::Auth::SignedOut,
                2 => account
                    .walls
                    .get("five_hour")
                    .is_some_and(|wall| !wall.cleared),
                _ => unreachable!(),
            });
    [
        result.is_ok(),
        before == after && before_logins.to_json() == after_logins.to_json(),
        applied,
        before.accounts.get("foreign") == after.accounts.get("foreign")
            && before_logins.get("foreign") == after_logins.get("foreign"),
        before_logins.generation_clock != after_logins.generation_clock,
    ]
}

pub(crate) fn review_observation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Provider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "Location" => {
            next.insert("location", state["location"] + 1);
        }
        "Outcome" => {
            next.insert("outcome", state["outcome"] + 1);
        }
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Record" => {
            next.insert("phase", 1);
            for (field, value) in ["accepted", "preserved", "applied", "foreign", "fenced"]
                .into_iter()
                .zip(review_case(
                    state["provider"],
                    state["location"],
                    state["outcome"],
                    state["case"],
                ))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn review_outcomes_conform_to_the_captured_account_and_login() {
    let model = aterm_models_harness::account_review_observation_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 408);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = review_observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let current = review_observation_transition(&model.init_state(), "Record");
    let mut stale = model.init_state();
    stale.insert("case", 2);
    let stale = review_observation_transition(&stale, "Record");
    for (state, field, law) in [
        (&current, "accepted", "CurrentAccepted"),
        (&current, "applied", "CurrentApplied"),
        (&stale, "accepted", "StaleRefused"),
        (&stale, "preserved", "RefusalPreserves"),
        (&current, "foreign", "ForeignPreserved"),
        (&current, "fenced", "NoCredentialFence"),
    ] {
        let mut bad = state.clone();
        bad.insert(field, 1 - state[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

/// A REVIEW THAT ENDS ON A LOGIN ERROR ENDS NOTHING ON A ROW THAT NEVER HELD A LOGIN: no login is
/// stored, none was ever renewed, served or ended and it was never seen signed out, so the row stays
/// never signed in (the report's `not-enrolled`, not `signed-out`). CONTROL: with a login stored, the
/// same outcome records its end.
#[test]
fn a_reviews_login_error_ends_nothing_on_a_row_never_signed_in() {
    for stored in [false, true] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let cli = Cli::Codex;
        let home = root.path().join(cli.accounts().default_home);
        fs::create_dir_all(&home).unwrap();
        if stored {
            write_codex_login(&home, "fixture@example.test");
        }
        let store = accounts::Store::at_for(root.path(), cli, root.path());
        let path = root.path().join(cli.accounts().roster_file);
        let row = Row {
            label: "account".into(),
            ..Row::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        let login = accounts::login_env_for(cli, &row);
        let witness =
            capture_review_login(&store, cli, &path, "account", &login, NOW).expect("a witness");
        record_review_login(
            &store,
            cli,
            &path,
            &witness,
            &home,
            Outcome::Ended(Ended::ReviewAuth),
            NOW,
        )
        .expect("the outcome");
        let state = store.load(NOW).unwrap().0;
        let signed_out = state
            .accounts
            .get("account")
            .is_some_and(|account| account.auth == accounts::Auth::SignedOut);
        assert_eq!(signed_out, stored, "a login stored: {stored}");
    }
}

/// A CLAUDE CODE LOGIN A MAC KEEPS IN ITS KEYCHAIN IS A LOGIN: no `.credentials.json` is stored, and
/// the profile of the row's home names an account, so the first review's login error ends it, on
/// the default login and on a row of its own. CONTROL: with no profile naming an account, the row
/// never held a login and nothing ends.
#[test]
fn a_reviews_login_error_ends_a_claude_login_its_profile_names() {
    for (own_home, named) in [(false, true), (true, true), (false, false), (true, false)] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let cli = Cli::Claude;
        let home = root.path().join(cli.accounts().default_home);
        let own = root.path().join("login");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&own).unwrap();
        if named {
            let profile = if own_home {
                own.join(".claude.json")
            } else {
                root.path().join(".claude.json")
            };
            fs::write(
                profile,
                r#"{"oauthAccount":{"accountUuid":"00000000-0000-4000-8000-000000000001"}}"#,
            )
            .unwrap();
        }
        let store = accounts::Store::at_for(root.path(), cli, root.path());
        let path = root.path().join(cli.accounts().roster_file);
        let row = Row {
            label: "account".into(),
            dir: own_home.then(|| own.display().to_string()),
            ..Row::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        let login = accounts::login_env_for(cli, &row);
        let witness =
            capture_review_login(&store, cli, &path, "account", &login, NOW).expect("a witness");
        record_review_login(
            &store,
            cli,
            &path,
            &witness,
            &home,
            Outcome::Ended(Ended::ReviewAuth),
            NOW,
        )
        .expect("the outcome");
        let signed_out = store
            .load(NOW)
            .unwrap()
            .0
            .accounts
            .get("account")
            .is_some_and(|account| account.auth == accounts::Auth::SignedOut);
        assert_eq!(signed_out, named, "own home {own_home}, named {named}");
    }
}

/// AN UNLISTED LOGIN'S CHECK REACHES THE JOURNAL UNDER ITS OWN ACCOUNT: no roster row lists the
/// default login, so the store has no record to carry what a review or a status read learned;
/// the note names the account its files name, `signed-out` for a login that ended and `auth ok`
/// for one that served.
/// CONTROLS: a usage limit says nothing of the sign-in; a home whose files name no account, a
/// CLI whose files aterm reads no account from, and a Mac with no key for the account, say
/// nothing at all.
#[test]
fn an_unlisted_logins_check_is_noted_under_the_account_its_files_name() {
    // One account per realm here: its key names the realm.
    fn keyer(realm: &str, email: &str) -> Option<String> {
        (email == "unlisted@example.test").then(|| match realm {
            "claude" => "ac-00000000000000c1".to_owned(),
            _ => "ac-00000000000000c2".to_owned(),
        })
    }
    let root = aterm_tempfile::TempDir::new().unwrap();
    let claude = root.path().join(Cli::Claude.accounts().default_home);
    fs::create_dir_all(&claude).unwrap();
    fs::write(
        root.path().join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"unlisted@example.test"}}"#,
    )
    .unwrap();
    let codex = root.path().join("codex-home");
    write_codex_login(&codex, "unlisted@example.test");
    let bare = root.path().join("bare").join(".claude");
    fs::create_dir_all(&bare).unwrap();
    let said = |cli, home: &Path, outcome| {
        super::super::journal::with_keyer(keyer, || unlisted_login_notes(cli, home, outcome, NOW))
            .into_iter()
            .map(|note| {
                (
                    note.kind,
                    note.why,
                    note.account.map(|key| key.as_str().to_owned()),
                    note.cli,
                    note.t,
                )
            })
            .collect::<Vec<_>>()
    };
    let ended = Outcome::Ended(Ended::ReviewAuth);
    for (cli, home) in [(Cli::Claude, &claude), (Cli::Codex, &codex)] {
        let (realm, email) =
            super::super::journal::claims_of(cli, "unlisted@example.test").unwrap();
        let key = keyer(&realm, &email);
        let t = u64::try_from(NOW).unwrap();
        assert_eq!(
            said(cli, home, ended),
            vec![("signed-out", None, key.clone(), cli.id(), t)],
            "{cli:?}"
        );
        assert_eq!(
            said(cli, home, Outcome::Worked),
            vec![("auth", Some("ok"), key, cli.id(), t)],
            "{cli:?}"
        );
        assert!(
            said(cli, home, Outcome::Limited(None)).is_empty(),
            "CONTROL: {cli:?}"
        );
        assert!(
            unlisted_login_notes(cli, home, ended, NOW).is_empty(),
            "CONTROL: no key for the account, {cli:?}"
        );
    }
    assert!(
        said(Cli::Claude, &bare, ended).is_empty(),
        "CONTROL: no account named"
    );
    assert!(
        said(Cli::Grok, &codex, ended).is_empty(),
        "CONTROL: no reader"
    );
}

/// A BLOCKED SIGN-IN RECORDS ITS PATH AND NEVER STRIKES THE LOGIN (plan package UB-05): a sign-in
/// or a review whose run met a blocked path (Cloudflare's bot check, a 403 named by its status
/// alone under Claude Code's remedy, no answer) ends `Ended::Unreached`. The AI store keeps it as
/// a path observation ([`crate::ai::store::Record::unreached_at`]), never the login's end; the
/// account's record keeps the path ([`accounts::Account::unreached`]) while its login ledger
/// stays as its evidence reads it: no strike, the verdict still the turn it served, no event,
/// and nothing staged on a Codex login.
/// CONTROL: the same runs ended on words that name a refused credential (`Ended::ReviewAuth`, a
/// status that names nobody signed in) strike the login and read it ended.
/// NEGATIVE CONTROLS (2026-11-18, each restored with cp from a backup plus touch, confirmed with
/// cmp): with `record_review_login`'s blocked-path return disabled, the ledger holds a
/// `login-end` strike; with `Record::apply` taking `Unreached` as an end, the AI store reads it
/// ended.
#[test]
fn a_blocked_sign_in_records_its_path_and_never_strikes_the_login() {
    use crate::ai::store::{How, Record, Records};
    for said in [
        "Please run /login · API Error: 403 Forbidden",
        "Please run /login · API Error: 403 Attention Required! | Cloudflare",
    ] {
        assert!(!aterm_phase::wall::strike_cause(said).strikes(), "{said}");
    }
    assert!(
        aterm_phase::wall::strike_cause("Login expired · Please run /login").strikes(),
        "CONTROL: a refused credential's words"
    );

    let mut record = Record::default();
    record.apply(How::Login, Outcome::Worked, NOW - 20);
    record.apply(How::Login, Outcome::Ended(Ended::Unreached), NOW);
    assert_eq!(
        (record.ended_at, record.ended_why, record.unreached_at),
        (None, None, Some(NOW)),
        "the AI store keeps the path"
    );
    assert!(!record.ended(), "a blocked path is no login's end");
    let kept = Records {
        by_id: [("claude".to_owned(), record.clone())]
            .into_iter()
            .collect(),
        ..Records::default()
    };
    assert_eq!(
        Records::parse(&kept.render()).unwrap().of("claude"),
        record,
        "it is written and read back"
    );
    record.apply(How::Login, Outcome::Ended(Ended::StatusSignedOut), NOW + 1);
    assert!(
        record.ended(),
        "CONTROL: a status that names nobody signed in"
    );

    for (why, strikes) in [(Ended::Unreached, false), (Ended::ReviewAuth, true)] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let cli = Cli::Claude;
        let home = root.path().join(cli.accounts().default_home);
        fs::create_dir_all(&home).unwrap();
        fs::write(
            root.path().join(".claude.json"),
            r#"{"oauthAccount":{"accountUuid":"00000000-0000-4000-8000-000000000001","emailAddress":"blocked@example.com"}}"#,
        )
        .unwrap();
        let store = accounts::Store::at_for(root.path(), cli, root.path());
        let path = root.path().join(cli.accounts().roster_file);
        let row = Row {
            label: "account".into(),
            ..Row::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::observe_served(state, logins, "account", NOW - 20))
            })
            .unwrap();
        let login = accounts::login_env_for(cli, &row);
        let witness =
            capture_review_login(&store, cli, &path, "account", &login, NOW).expect("a witness");
        let events = record_review_login(
            &store,
            cli,
            &path,
            &witness,
            &home,
            Outcome::Ended(why),
            NOW,
        )
        .expect("the outcome");
        let state = store.load(NOW).unwrap().0;
        let account = state.accounts.get("account").expect("the account's record");
        let verdict = account.evidence_verdict().expect("a verdict");
        assert_eq!(
            (
                verdict.login == accounts::Login::Struck,
                account.login.struck.is_some()
            ),
            (strikes, strikes),
            "{why:?}: {:?}",
            account.login
        );
        assert_eq!(
            account.auth == accounts::Auth::SignedOut,
            strikes,
            "{why:?}"
        );
        if strikes {
            assert!(account.unreached.is_none(), "CONTROL: {why:?} is no path");
        } else {
            assert_eq!(verdict.said, accounts::Said::Served, "the turn it served");
            assert!(events.is_empty(), "{events:?}");
            assert_eq!(
                account.unreached.as_ref().map(|obs| obs.at),
                Some(NOW),
                "its path is recorded"
            );
        }
    }

    // A Codex review's outcome stages its login's end where it is one; a blocked path stages none.
    for (why, ends) in [(Ended::Unreached, false), (Ended::ReviewAuth, true)] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let cli = Cli::Codex;
        let home = root.path().join(cli.accounts().default_home);
        write_codex_login(&home, "fixture@example.test");
        let store = accounts::Store::at_for(root.path(), cli, root.path());
        let path = root.path().join(cli.accounts().roster_file);
        let row = Row {
            label: "account".into(),
            ..Row::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        let login = accounts::login_env_for(cli, &row);
        let witness =
            capture_review_login(&store, cli, &path, "account", &login, NOW).expect("a witness");
        record_review_login(
            &store,
            cli,
            &path,
            &witness,
            &home,
            Outcome::Ended(why),
            NOW,
        )
        .expect("the outcome");
        let account = store.load(NOW).unwrap().0.accounts.remove("account");
        assert_eq!(
            account
                .as_ref()
                .is_some_and(|account| account.auth == accounts::Auth::SignedOut),
            ends,
            "Codex, {why:?}"
        );
        assert_eq!(
            account
                .and_then(|account| account.unreached)
                .map(|obs| obs.at),
            (!ends).then_some(NOW),
            "Codex, {why:?}: its path"
        );
    }
}

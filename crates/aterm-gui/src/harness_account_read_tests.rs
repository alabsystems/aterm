// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn pending_local_login(a: &Accts) -> String {
    let store = a.store();
    if !store.state_witness_file().exists() {
        renew_to(a, &a.login_dir("old"));
    }
    let login = a.login_dir("renewed");
    pause_publication(a, || {
        store.transact_logins(a.clock(), &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "main",
                None,
                &login,
                None,
                a.clock(),
            ))
        })
    });
    login
}

fn renew_to(a: &Accts, login: &str) {
    a.store()
        .transact_logins(a.clock(), &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "main",
                None,
                login,
                None,
                a.clock(),
            ))
        })
        .unwrap();
}

fn pause_publication(a: &Accts, publish: impl FnOnce() -> Result<Vec<accounts::Event>, String>) {
    let store = a.store();
    let witness = store.state_witness_file();
    let retained = witness.with_extension("retained");
    std::fs::rename(&witness, &retained).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(publish().is_err());
    assert!(store.load(a.clock()).is_err(), "the journal was admitted");
    assert!(store.load_logins(a.clock()).is_err());
    std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
    std::fs::rename(&retained, &witness).unwrap();
}

#[test]
fn usage_read_recovers_an_admitted_login_before_binding_the_roster() {
    let a = Accts::new();
    let login = pending_local_login(&a);
    let roster = usage_roster(Some(&a.roster_path()), &a.store(), (true, true), a.clock())
        .expect("the read recovers the admitted local login");
    assert_eq!(
        roster.row("main").unwrap().dir.as_deref(),
        Some(login.as_str())
    );
    assert_eq!(
        a.ledger().iter().filter(|&&kind| kind == "renewed").count(),
        2
    );
}

#[test]
fn absent_roster_does_not_hide_an_unreadable_account_receipt() {
    let a = Accts::new();
    let store = a.store();
    std::fs::create_dir_all(&store.dir).unwrap();
    std::fs::write(store.dir.join("sync-commit.json"), b"{").unwrap();
    assert_eq!(
        usage_roster(None, &store, (true, true), a.clock()).err(),
        Some("account-state-refused")
    );
    let mut seat = (a.hooks().acts.account_seat)().unwrap();
    seat.roster = None;
    assert!(account_roster_or_own(&seat).is_none());
}

fn project(roster: accounts::Roster) -> Vec<(String, Option<String>)> {
    roster
        .rows
        .into_iter()
        .map(|row| (row.label, row.dir))
        .collect()
}

fn pending_roster(a: &Accts) {
    let store = a.store();
    let roster = a.roster();
    let row = roster.row("main").unwrap();
    let who = accounts::ProcessLogin {
        login: row.dir.clone(),
        default_home: true,
        stored_login: true,
    };
    pause_publication(a, || {
        store.observe_login_within(
            &[],
            a.clock(),
            accounts::LoginObservation {
                roster_path: &a.roster_path(),
                row,
                who: &who,
            },
            |state, logins| Ok(accounts::note_login_end(state, logins, "main", a.clock())),
        )
    });
}

fn exercise(reader: i64, shape: i64, fault: i64) -> [bool; 4] {
    let a = Accts::new();
    *a.ladder.lock().unwrap() = &[];
    let path = a.roster_path();
    std::fs::write(&path, OWN_ROW).unwrap();
    let store = a.store();
    let old = a.login_dir("old");
    let new = a.login_dir("renewed");
    let credentials = [&old, &new].map(|login| PathBuf::from(login).join(".claude.json"));
    for credential in &credentials {
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(credential, "opaque retained credential").unwrap();
    }
    renew_to(&a, &old);
    if fault == 7 {
        pending_roster(&a);
    } else if fault > 1 {
        assert_eq!(pending_local_login(&a), new);
    }
    if shape == 1 {
        std::fs::write(&path, "").unwrap();
    }
    match fault {
        4 => std::fs::write(store.logins_file(), "{\"schema\":99}").unwrap(),
        5 => std::fs::write(store.dir.join("sync-commit.json"), "{").unwrap(),
        6 => std::fs::rename(
            store.dir.join("sync-commit.json"),
            store.dir.join("retained-receipt"),
        )
        .unwrap(),
        _ => {}
    }
    let _held = (fault == 3).then(|| store.acquire().unwrap());
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.dir.join("sync-commit.json"),
            store.state_witness_file(),
            store.logins_witness_file(),
            path.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let configured = (shape != 0).then_some(path.as_path());
    let seat = AccountSeat {
        roster: configured.map(Path::to_path_buf),
        state: a.state_root(),
        home: a.tmp.path().join("home"),
        now: a.clock(),
        ladder: &[],
    };
    let read = || match reader {
        1 => read_account_roster(configured, &store, a.clock())
            .map(project)
            .map_err(RosterUnread::recording_word),
        2 => {
            let snap = Snapshot {
                pid: u32::MAX,
                ..snap("s-read")
            };
            let agent = WalledAgent {
                snap: &snap,
                session: "unused",
                own: true,
            };
            let (word, _) = account_wall_at(
                (&seat.home, &store, configured),
                "s-read",
                &agent,
                &|| true,
                a.clock(),
            );
            if word == "no-env" {
                Ok(Vec::new())
            } else {
                Err(word)
            }
        }
        3 => usage_roster(configured, &store, (true, true), a.clock())
            .map(|read| project(read.roster().clone())),
        4 => match account_roster_or_own(&seat) {
            Some(roster) => Ok(project(roster)),
            None => Err(roster_read(&seat).unwrap_err().recording_word()),
        },
        _ => unreachable!(),
    };
    let available = fault <= 2 || fault == 7 && shape == 2;
    let expected_login = if fault == 2 { &new } else { &old };
    let expected = if !available {
        Err("account-state-refused")
    } else if reader == 2 || shape < 2 && reader == 1 {
        Ok(Vec::new())
    } else if shape < 2 {
        Ok(project(accounts::Roster::own()))
    } else {
        Ok(vec![("main".to_string(), Some(expected_login.clone()))])
    };
    let before = images();
    let got = read();
    let after = images();
    let coherent = got == expected
        && if available {
            let logins = store.load_logins(a.clock()).unwrap().0;
            let state = store.load(a.clock()).unwrap().0;
            let binding = logins.get("main").unwrap();
            binding.dir.as_ref() == Some(expected_login)
                && binding.ended_at == (fault == 7).then_some(a.clock())
                && logins.generation_clock == if fault == 2 { 2 } else { 1 }
                && state.account("main").auth
                    == if fault == 7 {
                        accounts::Auth::SignedOut
                    } else {
                        accounts::Auth::Ok
                    }
                && store
                    .tail(20)
                    .unwrap()
                    .iter()
                    .filter(|event| event.kind == accounts::Kind::Renewed)
                    .count()
                    == if fault == 2 { 2 } else { 1 }
        } else {
            before == after
        };
    let stable = read() == got && images() == after;
    let preserved = credentials
        .iter()
        .all(|path| std::fs::read_to_string(path).unwrap() == "opaque retained credential")
        && std::fs::read(&path).ok() == before[7];
    [got.is_ok(), coherent, stable, preserved]
}

pub(super) fn read_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherReader" => {
            next.insert("reader", state["reader"] + 1);
        }
        "AnotherShape" => {
            next.insert("shape", state["shape"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Read" => {
            let flags = exercise(state["reader"], state["shape"], state["fault"]);
            next.insert("phase", 1);
            for (field, flag) in ["available", "coherent", "stable", "preserved"]
                .into_iter()
                .zip(flags)
            {
                next.insert(field, i64::from(flag));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn gui_account_readers_conform_to_recovery_and_refusal() {
    let model = aterm_models_harness::account_gui_read_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = read_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} => {next:?}"
                );
            }
        }
    }
    let complete = read_transition(&model.init_state(), "Read");
    for (field, law) in [
        ("available", "ReplayableReadAvailable"),
        ("coherent", "ExpectedReadAndRecords"),
        ("stable", "StableRepeat"),
        ("preserved", "CredentialsAndRosterPreserved"),
    ] {
        let mut bad = complete.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = complete;
    bad.insert("fault", 3);
    assert!(!model.check_invariant("UnresolvedReadRefused", &bad));
}

#[test]
fn an_unknown_or_different_roster_path_cannot_recover_a_roster_bound_plan() {
    let a = Accts::new();
    *a.ladder.lock().unwrap() = &[];
    std::fs::write(a.roster_path(), OWN_ROW).unwrap();
    renew_to(&a, &a.login_dir("old"));
    pending_roster(&a);
    let other = a.tmp.path().join("other.toml");
    std::fs::write(&other, OWN_ROW).unwrap();
    let store = a.store();
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.dir.join("sync-commit.json"),
            a.roster_path(),
            other.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = images();
    for path in [None, Some(other.as_path())] {
        assert!(matches!(
            read_account_roster(path, &store, a.clock()),
            Err(RosterUnread::LoginsUnread)
        ));
        assert_eq!(images(), before);
    }
    assert!(read_account_roster(Some(&a.roster_path()), &store, a.clock()).is_ok());
}

#[test]
fn clean_missing_rosters_are_inert_but_malformed_rosters_never_become_defaults() {
    let a = Accts::new();
    let store = a.store();
    let missing = a.tmp.path().join("missing.toml");
    for path in [None, Some(missing.as_path())] {
        assert!(
            read_account_roster(path, &store, a.clock())
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            usage_roster(path, &store, (true, false), a.clock())
                .unwrap()
                .roster(),
            &accounts::Roster::own()
        );
        assert!(!a.state_root().exists());
    }
    std::fs::write(&missing, "[[broken").unwrap();
    assert!(matches!(
        read_account_roster(Some(&missing), &store, a.clock()),
        Err(RosterUnread::Refused)
    ));
    assert_eq!(
        usage_roster(Some(&missing), &store, (true, false), a.clock()).err(),
        Some("roster-refused")
    );
    assert!(!a.state_root().exists());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn replace_login(a: &Accts, prior: &str) -> String {
    let next = a.login_dir("main+2");
    std::fs::create_dir_all(&next).unwrap();
    a.store()
        .transact_logins(a.clock(), &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "main",
                Some(prior),
                &next,
                None,
                a.clock(),
            ))
        })
        .unwrap();
    next
}

fn unproven(a: &Accts) {
    edited(a, |state| {
        state.active = Some("main".into());
        state.proven = false;
    });
}

#[test]
fn a_superseded_observation_never_proves_the_replacement_login() {
    let a = Accts::new();
    let sid = "s-old-observation";
    a.spawn(sid, ACCT_TALK_A, "main");
    let idle = a.worker(sid, &on());
    let current = renew_own(&a);
    unproven(&a);
    let before = a.state();
    let bindings = a.store().load_logins(a.clock()).unwrap().0;
    let ledger = a.ledger();
    assert!(idle.login_superseded());
    idle.account_served();
    assert_eq!(a.state(), before);
    assert_eq!(a.store().load_logins(a.clock()).unwrap().0, bindings);
    assert_eq!(a.ledger(), ledger);

    a.exec(sid, ACCT_TALK_A, Some(&current), a.clock());
    assert!(refresh_snapshot(sid, &idle.kept, &idle.hooks));
    idle.account_served();
    assert!(
        a.state().proven,
        "a response on the current login still proves it"
    );
}

#[test]
fn a_renewal_between_observation_and_recording_preserves_the_new_login() {
    for action in ["turn", "notice"] {
        let a = Accts::new();
        let sid = "s-racing-observation";
        a.spawn(sid, ACCT_TALK_A, "main");
        let old = renew_own(&a);
        a.exec(sid, ACCT_TALK_A, Some(&old), a.clock());
        unproven(&a);
        let mut idle = a.worker(sid, &on());
        let read = Arc::clone(&idle.hooks.acts.account_seat);
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let world = Arc::clone(&a);
        let prior = old.clone();
        idle.hooks.acts.account_seat = Arc::new(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                replace_login(&world, &prior);
            }
            read()
        });
        if action == "turn" {
            idle.account_served();
        } else {
            idle.login_notice(5);
        }
        assert!(seen.load(Ordering::SeqCst) >= 2, "the recording read ran");
        assert!(
            !a.state().proven,
            "the old observation cannot release the canary"
        );
        let bindings = a.store().load_logins(a.clock()).unwrap().0;
        assert_eq!(
            bindings.get("main").unwrap().dir,
            Some(a.login_dir("main+2"))
        );
        assert_eq!(bindings.get("main").unwrap().served_at, None);
        assert_eq!(bindings.get("main").unwrap().notice_end, None);
        assert!(!a.ledger().contains(&"proven"));
        assert!(!a.ledger().contains(&"session"));
    }
}

pub(crate) fn answer_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "CurrentAuth" => 1,
        "SupersededAuth" => 2,
        "RenewedDuringAuth" => 3,
        "ChangedPolicyAuth" => 4,
        "SupersededQuota" => 5,
        "MissingRowAuth" => 6,
        "UnreadableRowAuth" => 7,
        _ => unreachable!(),
    };
    let a = Accts::new();
    let sid = "s-auth-answer";
    a.spawn(sid, ACCT_TALK_A, "main");
    let old = renew_own(&a);
    a.exec(sid, ACCT_TALK_A, Some(&old), a.clock());
    if matches!(case, 2 | 5) {
        replace_login(&a, &old);
    }
    if case == 5 {
        a.now.store(ACCT_NOW + 30, Ordering::SeqCst);
        a.wall(ACCT_TALK_A, a.clock() - 1);
    }
    let mut idle = a.worker(sid, &on());
    with_kept_of(&idle.kept, |kept| {
        let snap = kept.snapshot.as_ref().unwrap();
        kept.account.moved = Some(Moved {
            to: "main".into(),
            login: Some(old.clone()),
            pid: snap.pid,
            start: snap.start.clone(),
            answered: false,
            untold: false,
        });
    });
    let snapshot = |a: &Accts| {
        (
            a.state(),
            a.store().load_logins(a.clock()).unwrap().0,
            a.ledger(),
        )
    };
    let baseline = Arc::new(Mutex::new(snapshot(&a)));
    let saved = Arc::clone(&baseline);
    let read = Arc::clone(&idle.hooks.acts.account_seat);
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let world = Arc::clone(&a);
    idle.hooks.acts.account_seat = Arc::new(move || {
        if count.fetch_add(1, Ordering::SeqCst) == 1 && matches!(case, 3 | 4 | 6 | 7) {
            match case {
                3 => {
                    replace_login(&world, &old);
                }
                4 => std::fs::write(
                    world.roster_path(),
                    OWN_ROW.replace("owner@example.com", "changed@example.test"),
                )
                .unwrap(),
                6 => std::fs::rename(
                    world.roster_path(),
                    world.roster_path().with_extension("retained"),
                )
                .unwrap(),
                7 => std::fs::write(world.roster_path(), "[[account]\n").unwrap(),
                _ => unreachable!(),
            }
            *saved.lock().unwrap() = snapshot(&world);
        }
        read()
    });
    let notifications = Arc::new(AtomicUsize::new(0));
    let sent = Arc::clone(&notifications);
    idle.hooks.acts.accounts_upkeep = AccountsUpkeep::Made(Arc::new(move |_| {
        sent.fetch_add(1, Ordering::SeqCst);
    }));
    idle.account_answered(if case == 5 {
        AccountBad::Wall
    } else {
        AccountBad::SignedOut
    });
    if matches!(case, 3 | 4 | 6 | 7) {
        assert!(seen.load(Ordering::SeqCst) >= 2, "the recording read ran");
    }
    let after = snapshot(&a);
    let before = baseline.lock().unwrap();
    let accepted = matches!(case, 1 | 5);
    assert_eq!(
        notifications.load(Ordering::SeqCst),
        usize::from(accepted),
        "{action}"
    );
    let answered = with_kept_of(&idle.kept, |kept| {
        kept.account.moved.as_ref().unwrap().answered
    });
    assert_eq!(
        answered, accepted,
        "{action}: only a recorded strike answers the move"
    );
    let mut next = state.clone();
    next.insert("case", case);
    next.insert(
        "signed_out",
        i64::from(after.0.account("main").auth == accounts::Auth::SignedOut),
    );
    next.insert(
        "limited",
        i64::from(after.0.account("main").walls.contains_key("five_hour")),
    );
    next.insert("bindings", i64::from(after.1 == before.1));
    next.insert("unchanged", i64::from(accepted || after == *before));
    next
}

#[test]
fn auth_answers_only_strike_the_observed_login_but_quota_survives_renewal() {
    let model = aterm_models_harness::account_answer_observation_model();
    assert_eq!(model.mutant_dials(), (1..=10).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 8);
    let start = model.init_state();
    for action in &model.actions {
        let after = answer_transition(&start, action.name);
        assert!(
            model.successors(action.name, &start).contains(&after),
            "{}: {after:?}",
            action.name
        );
    }
    for (action, field, value, law) in [
        ("CurrentAuth", "signed_out", 0, "CurrentAuthRecorded"),
        ("RenewedDuringAuth", "signed_out", 1, "OnlyCurrentAuth"),
        ("SupersededQuota", "limited", 0, "QuotaAcrossRenewal"),
        ("CurrentAuth", "limited", 1, "OnlyQuotaLimits"),
        ("CurrentAuth", "bindings", 0, "RetainsBindings"),
        ("SupersededAuth", "unchanged", 0, "RefusalsUnchanged"),
    ] {
        let mut bad = answer_transition(&start, action);
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}

#[test]
fn a_refused_auth_answer_flushes_only_its_owned_reset_without_changing_the_new_login() {
    for own in [false, true] {
        let a = Accts::new();
        let sid = "s-reset-and-auth";
        a.spawn(sid, ACCT_TALK_A, "main");
        let old = renew_own(&a);
        let process = a.exec(sid, ACCT_TALK_A, Some(&old), a.clock());
        let mut idle = a.worker(sid, &on());
        let until = a.clock() + 3600;
        edited(&a, |state| {
            state.reset = Some(accounts::ResetClaim {
                account: "main".into(),
                until: Some(until),
                window: Some("five_hour".into()),
                sid: if own { sid } else { "s-other" }.into(),
                at: a.clock(),
                pid: process.pid,
                start: process.start.clone(),
                done: None,
            });
        });
        with_kept_of(&idle.kept, |kept| {
            kept.account.reset_owed = Some(ResetOwed {
                label: "main".into(),
                until,
                read: ResetRead::Kept("kept".into()),
                offer: None,
                at: a.clock(),
            });
        });
        let read = Arc::clone(&idle.hooks.acts.account_seat);
        let count = AtomicUsize::new(0);
        let world = Arc::clone(&a);
        let bindings = Arc::new(Mutex::new(None));
        let saved = Arc::clone(&bindings);
        idle.hooks.acts.account_seat = Arc::new(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                replace_login(&world, &old);
                *saved.lock().unwrap() = Some(world.store().load_logins(world.clock()).unwrap().0);
            }
            read()
        });
        idle.account_answered(AccountBad::SignedOut);
        assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
        assert_eq!(
            a.store().load_logins(a.clock()).unwrap().0,
            bindings.lock().unwrap().clone().unwrap()
        );
        assert_eq!(
            a.state().reset.unwrap().done.as_deref(),
            own.then_some("kept")
        );
        assert_eq!(a.ledger().contains(&"reset-kept"), own);
        assert!(!a.ledger().contains(&"struck"));
        assert!(with_kept_of(&idle.kept, |kept| kept
            .account
            .reset_owed
            .is_none()));
    }
}

#[test]
fn captured_login_lifecycle_observations_do_not_edit_a_new_binding() {
    for action in ["ended", "served", "signed-back"] {
        let a = Accts::new();
        let sid = "s-lifecycle-observation";
        a.spawn(sid, ACCT_TALK_A, "main");
        let old = renew_own(&a);
        std::fs::write(
            a.tmp.path().join("home/.claude.json"),
            r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
        )
        .unwrap();
        a.exec(sid, ACCT_TALK_A, Some(&old), a.clock());
        let mut idle = a.worker(sid, &on());
        let (seat, roster, look) = idle.account_ctx().unwrap();
        replace_login(&a, &old);
        if action == "signed-back" {
            edited(&a, |state| {
                accounts::note_auth(state, "main", accounts::Auth::SignedOut, a.clock());
            });
        }
        let state = a.state();
        let bindings = a.store().load_logins(a.clock()).unwrap().0;
        let ledger = a.ledger();
        let reads = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&reads);
        let read = Arc::clone(&idle.hooks.acts.account_seat);
        idle.hooks.acts.account_seat = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            read()
        });
        match action {
            "ended" => idle.login_ended(&seat, &roster, &look),
            "served" => idle.login_served(&seat, &roster, &look),
            "signed-back" => idle.signed_back(&seat, &roster, &look, "main"),
            _ => unreachable!(),
        }
        assert!(
            reads.load(Ordering::SeqCst) > 0,
            "{action}: the recording guard ran"
        );
        assert_eq!(a.state(), state, "{action}");
        assert_eq!(
            a.store().load_logins(a.clock()).unwrap().0,
            bindings,
            "{action}"
        );
        assert_eq!(a.ledger(), ledger, "{action}");
    }
}

#[test]
fn a_signed_out_observation_with_wrong_identity_cannot_prove_the_canary() {
    for identity in ["direct", "alias", "identity-only"] {
        let a = Accts::new();
        let sid = "s-unconfirmed-observation";
        let row = match identity {
            "alias" => {
                "[[account]]\nlabel='main'\nclass='dedicated'\nemail='login-alias@example.com'\nidentity_email='owner@example.com'\n"
            }
            "identity-only" => {
                "[[account]]\nlabel='main'\nclass='dedicated'\nidentity_email='owner@example.com'\n"
            }
            _ => OWN_ROW,
        };
        std::fs::write(a.roster_path(), row).unwrap();
        a.spawn(sid, ACCT_TALK_A, "main");
        let idle = a.worker(sid, &on());
        unproven(&a);
        edited(&a, |state| {
            accounts::note_auth(state, "main", accounts::Auth::SignedOut, a.clock());
        });
        let profile = a.tmp.path().join("home/.claude.json");
        for data in [
            r#"{"oauthAccount":{"emailAddress":"someone-else@example.test"}}"#,
            r#"{"oauthAccount":{"accountUuid":"no-email"}}"#,
        ] {
            std::fs::write(&profile, data).unwrap();
            let before = a.state();
            let ledger = a.ledger();
            idle.account_served();
            assert_eq!(a.state(), before);
            assert_eq!(a.ledger(), ledger);
        }
        std::fs::write(
            &profile,
            r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
        )
        .unwrap();
        idle.account_served();
        assert!(
            a.state().proven,
            "{identity}: the confirmed current login proves the canary"
        );
        assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    }
}

#[test]
fn auth_changes_before_the_observation_lock_use_independent_identity_evidence() {
    for named in [false, true] {
        let a = Accts::new();
        let sid = "s-auth-observation";
        if named {
            std::fs::write(a.roster_path(), OWN_ROW).unwrap();
            std::fs::write(
                a.tmp.path().join("home/.claude.json"),
                r#"{"oauthAccount":{"emailAddress":"owner@example.com"}}"#,
            )
            .unwrap();
        }
        a.spawn(sid, ACCT_TALK_A, "main");
        unproven(&a);
        edited(&a, |state| {
            accounts::note_auth(state, "main", accounts::Auth::Ok, a.clock());
        });
        let mut idle = a.worker(sid, &on());
        let count = Arc::new(AtomicUsize::new(0));
        let changed = Arc::clone(&count);
        let read = Arc::clone(&idle.hooks.acts.account_seat);
        let world = Arc::clone(&a);
        idle.hooks.acts.account_seat = Arc::new(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                edited(&world, |state| {
                    accounts::note_auth(state, "main", accounts::Auth::SignedOut, world.clock());
                });
            }
            read()
        });
        idle.account_served();
        assert!(changed.load(Ordering::SeqCst) >= 2);
        assert!(a.state().proven, "named={named}");
        assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    }
}

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

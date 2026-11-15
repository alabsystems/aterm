// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Served evidence after a strike lifts a stale signed-out word through the one writer
//! ([`WorkerIdle::served_back`]): a served point ([`WorkerIdle::claude_login_seen`]), a break's
//! served turn dated by its reply ([`IdleHost::login_served_at`]) and a usage-tap reading
//! ([`WorkerIdle::tap_lifts`]) on the struck row's own stored login, dated after the strike. A
//! lift is no sign-in ([`accounts::note_served_back`]): the ledger records the word's return
//! (`auth`), never `signed-in`, and the login's renewal and notice stand. Claude Code's `Please
//! run /login` after a Cloudflare 403 struck an account on `m16` on 2026-11-13 while its login
//! served all day, and nothing lifted the word: the tap fold leaves the word alone, and a cruise
//! break's turn under background work was told no served point. A turn end
//! ([`WorkerIdle::login_served`]) is no such evidence: the TurnEnd point is asked at idle points
//! no turn worked to, so it lifts nothing.

use super::*;

const OWN_PROFILE: &str = r#"{"oauthAccount":{"emailAddress":"Owner@Example.com"}}"#;
const OTHER_PROFILE: &str = r#"{"oauthAccount":{"emailAddress":"other@example.com"}}"#;
const UNNAMED_PROFILE: &str = r#"{"oauthAccount":{"accountUuid":"u-1"}}"#;

/// `main` on the default home naming its email, and `work-2` in a directory of its own naming
/// another: the roster both worlds below list.
fn roster_text(work_2: &str) -> String {
    format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"owner@example.com\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = '{work_2}'\nemail = \
         \"other@example.com\"\n"
    )
}

/// A tab on `main`, its default-home profile `profile`, whose login wall struck `main` signed
/// out at [`ACCT_NOW`].
fn struck(profile: &str) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    std::fs::write(a.tmp.path().join("home").join(".claude.json"), profile).unwrap();
    a.spawn("s-served-back", ACCT_TALK_A, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let idle = a.worker("s-served-back", &cfg);
    assert_eq!(idle.account(AccountPoint::LoginEnded), None);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "PRECONDITION: the wall struck it"
    );
    assert_eq!(a.ledger(), ["auth", "login-ended"]);
    (a, idle)
}

fn said(idle: &WorkerIdle) -> Option<String> {
    idle.kept.lock().unwrap().account.login_said.clone()
}

/// The rows aterm's sign-in ladder would sign in now ([`accounts::renew_due`]).
fn due(a: &Accts) -> Vec<String> {
    let logins = a.store().load_logins(a.clock()).expect("reads").0;
    accounts::renew_due(
        &a.roster(),
        &logins,
        &a.state(),
        &accounts::OtherMacs::NONE,
        a.clock(),
    )
}

/// A TURN END IS NO EVIDENCE A TURN SERVED: the TurnEnd point ([`WorkerIdle::login_served`]) is
/// asked at any idle point (a screen decided again on account news, the first point after a
/// launch) with no turn worked to it, so a minute after the strike, under a profile that proves
/// `main`'s address, it leaves `main` signed out, records no sign-in, and keeps its sign-in due
/// for aterm's sign-in ladder. Negative control: a turn end that lifts records `main` signed in.
#[test]
fn a_turn_end_after_a_strike_lifts_nothing() {
    let (a, idle) = struck(OWN_PROFILE);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    let _ = idle.account(AccountPoint::TurnEnd);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert_eq!(a.ledger(), ["auth", "login-ended"]);
    assert_eq!(due(&a), ["main"]);
}

/// AN IDLE TAB'S TURN END NEVER LIFTS ANOTHER TAB'S STRIKE: two tabs share `main`'s default
/// login, and `s-tab-b`'s turn ended served before `s-tab-a`'s own login wall struck `main`
/// (the login really ended). `s-tab-b`, idle with no wall since, is asked its TurnEnd point
/// again on that account news, a minute after the strike: `main` stays signed out with its
/// sign-in due, and the ledger records no sign-in that `s-tab-a`'s next wall would strike again.
/// Negative control: a turn end that lifts, dated when it is asked, records `main` signed in
/// and drops it from the sign-in ladder.
#[test]
fn an_idle_tabs_turn_end_never_lifts_another_tabs_strike() {
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    std::fs::write(a.tmp.path().join("home").join(".claude.json"), OWN_PROFILE).unwrap();
    a.spawn("s-tab-a", ACCT_TALK_A, "main");
    a.spawn("s-tab-b", ACCT_TALK_B, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let tab_a = a.worker("s-tab-a", &cfg);
    let tab_b = a.worker("s-tab-b", &cfg);
    let _ = tab_b.account(AccountPoint::TurnEnd);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    assert_eq!(tab_a.account(AccountPoint::LoginEnded), None);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "PRECONDITION: s-tab-a's wall struck it"
    );
    assert_eq!(due(&a), ["main"], "PRECONDITION");
    a.now.store(ACCT_NOW + 120, Ordering::SeqCst);
    let _ = tab_b.account(AccountPoint::TurnEnd);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert_eq!(a.ledger().last(), Some(&"login-ended"), "{:?}", a.ledger());
    assert_eq!(due(&a), ["main"]);
}

/// A SERVED POINT ([`LoginSeen::Served`]) ON THE STRUCK ROW'S OWN STORED LOGIN LIFTS IT THROUGH
/// THE SAME WRITER, recording the word's return and no sign-in, and does nothing at the strike's
/// own second.
#[test]
fn a_served_point_after_a_strike_signs_its_own_login_back_in() {
    let (a, idle) = struck(OWN_PROFILE);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert!(
        said(&idle).is_some_and(|s| s.contains("not after its strike")),
        "{:?}",
        said(&idle)
    );
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    assert_eq!(a.ledger(), ["auth", "login-ended", "auth"]);
}

/// A BREAK'S SERVED TURN IS EVIDENCE OF ITS REPLY'S TIME, NEVER OF WHEN IT IS TOLD
/// ([`IdleHost::login_served_at`]): a break is told once settled, a while after the reply that
/// ended its turn, and a strike recorded between the two (another tab's real lost login) is none
/// that reply speaks for. Told a minute after the strike, a reply written five seconds before it
/// lifts nothing, and the log says why. CONTROL: the same tell undated, as a point is (its time
/// now, what a break told before this), lifts it. A reply dated past now counts as now, so it
/// lifts no strike of this second; a reply written after the strike lifts it.
#[test]
fn a_breaks_reply_written_before_the_strike_lifts_nothing() {
    let (a, idle) = struck(OWN_PROFILE);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    idle.login_served_at(ACCT_NOW - 5);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert!(
        said(&idle).is_some_and(|s| s.contains("not after its strike")),
        "{:?}",
        said(&idle)
    );
    assert_eq!(a.ledger(), ["auth", "login-ended"]);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::Ok,
        "CONTROL: dated by the tell, it lifts"
    );
    a.now.store(ACCT_NOW + 120, Ordering::SeqCst);
    idle.login_seen(LoginSeen::Lost);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "PRECONDITION: struck again"
    );
    idle.login_served_at(ACCT_NOW + 3_600);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "a reply dated past now is the strike's own second"
    );
    a.now.store(ACCT_NOW + 180, Ordering::SeqCst);
    idle.login_served_at(ACCT_NOW + 150);
    assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    assert_eq!(
        a.ledger(),
        ["auth", "login-ended", "auth", "auth", "login-ended", "auth"]
    );
}

/// A PROFILE THAT NAMES NO ADDRESS PROVES NOBODY'S LOGIN: a served point a minute after the
/// strike leaves `main` signed out, and the log says its identity is not confirmed.
#[test]
fn a_served_point_whose_identity_is_unproved_lifts_nothing() {
    let (a, idle) = struck(UNNAMED_PROFILE);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert!(
        said(&idle).is_some_and(|s| s.contains("identity is not confirmed")),
        "{:?}",
        said(&idle)
    );
}

/// A DEFAULT HOME A `/login` MOVED ONTO `work-2`'S ACCOUNT CREDITS NO WRONG ROW: a served point
/// there lifts neither `main` nor a struck `work-2` (whose own login is its directory's, not the
/// default home's), and records no sign-in.
#[test]
fn a_served_point_on_a_default_home_moved_to_another_account_lifts_no_row() {
    let (a, idle) = struck(OWN_PROFILE);
    a.store()
        .transact_logins(a.clock(), &mut None, |state, logins| {
            Ok(accounts::note_login_end(state, logins, "work-2", a.clock()))
        })
        .expect("the strike");
    assert_eq!(
        a.state().account("work-2").auth,
        accounts::Auth::SignedOut,
        "PRECONDITION"
    );
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    std::fs::write(
        a.tmp.path().join("home").join(".claude.json"),
        OTHER_PROFILE,
    )
    .unwrap();
    idle.login_seen(LoginSeen::Served);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert_eq!(a.state().account("work-2").auth, accounts::Auth::SignedOut);
    assert_eq!(a.ledger().last(), Some(&"login-ended"), "{:?}", a.ledger());
}

/// A LOOK WHOSE LOGIN FILE NAMES ANOTHER ACCOUNT ([`accounts::ReadingOwner::Another`]) IS NO
/// EVIDENCE FOR `main`: the writer is not reached, and the reason is said.
#[test]
fn served_evidence_whose_login_file_names_another_account_lifts_nothing() {
    let (a, idle) = struck(OWN_PROFILE);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    std::fs::write(
        a.tmp.path().join("home").join(".claude.json"),
        OTHER_PROFILE,
    )
    .unwrap();
    let (seat, roster, look) = idle.account_ctx().expect("the accounts");
    assert_eq!(
        accounts::ReadingOwner::of(&accounts::reconcile(&roster, &look.who, &look.seen), "main"),
        accounts::ReadingOwner::Another,
        "PRECONDITION"
    );
    let before = a.ledger_len();
    assert!(!idle.served_back(&seat, &roster, &look, "main", a.clock(), "test"));
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert_eq!(a.ledger_len(), before);
    assert!(
        said(&idle).is_some_and(|s| s.contains("names another account")),
        "{:?}",
        said(&idle)
    );
}

/// THE PERSISTED m2 SHAPE HEALS BY ITS OWN SERVED POINT (2026-11-13: Claude Code wrote
/// Cloudflare's block page as `Please run /login`, the upkeep's headless sign-in met Cloudflare's
/// bot check, and the login served for hours while its row stayed signed out): a lost login the
/// host is told ends `main`'s login and makes its sign-in due; the sign-in is blocked, a later
/// look stamps the word again, and the sign-in is blocked again. A point its own stored-login
/// process then serves signs it in with no hand edit: the word is ok, the blocked sign-in is
/// dropped, its end is no longer the login's and aterm's sign-in ladder has nothing due. No
/// credential changed, so the heal is no sign-in: the login's renewal (an in-place sign-in three
/// days before) and its unset notice stand, and the ledger records the word's return, never
/// `signed-in`. Negative controls: a served point under a profile naming another account changes
/// nothing, and a lost login after the heal ends the login again.
#[test]
fn a_login_ended_with_its_sign_in_blocked_heals_by_its_own_served_point() {
    const BOT_CHECK: &str = "mailbox: the sign-in did not finish; it met Cloudflare's bot \
                             check on claude.ai, which aterm never tries to pass";
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    let profile = |text: &str| {
        std::fs::write(a.tmp.path().join("home").join(".claude.json"), text).unwrap();
    };
    profile(OWN_PROFILE);
    a.spawn("s-m2-shape", ACCT_TALK_A, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let idle = a.worker("s-m2-shape", &cfg);
    // The in-place sign-in that last made `main`'s login, three days before.
    let signed_in = a.clock() - 3 * 86_400;
    a.store()
        .transact_logins(a.clock(), &mut None, |_, logins| {
            logins
                .labels
                .entry("main".to_owned())
                .or_default()
                .renewed_at = Some(signed_in);
            Ok(Vec::new())
        })
        .expect("the earlier sign-in");
    // When `main`'s login was made and the notice it was given.
    let made = || {
        let logins = a.store().load_logins(a.clock()).expect("the logins").0;
        logins
            .get("main")
            .map(|binding| (binding.renewed_at, binding.notice_end))
    };
    // `main`'s word, whether a sign-in attempt is on its binding, and when its login ended.
    let facts = || {
        let roster = a.roster();
        let row = roster.row("main").expect("the row");
        let logins = a.store().load_logins(a.clock()).expect("the logins").0;
        let state = a.state();
        (
            state.account("main").auth,
            logins
                .get("main")
                .is_some_and(|binding| binding.sign_in_attempt.is_some()),
            accounts::login_ended_at(row, &logins, &state),
        )
    };
    let blocked = || {
        a.store()
            .transact_logins(a.clock(), &mut None, |_, logins| {
                Ok(accounts::note_sign_in_attempt(
                    logins,
                    "main",
                    accounts::AttemptOutcome::Blocked,
                    BOT_CHECK,
                    a.clock(),
                ))
            })
            .expect("recorded");
    };
    // The session runs on the row, so its login is used here and its end makes a sign-in due.
    a.store()
        .transact(a.clock(), &mut None, |state| {
            Ok(accounts::note_session(
                state,
                "s-m2-shape",
                "main",
                None,
                a.clock(),
            ))
        })
        .expect("the session runs on the row");
    let ended = a.clock();
    idle.login_seen(LoginSeen::Lost);
    assert_eq!(
        facts(),
        (accounts::Auth::SignedOut, false, Some(ended)),
        "PRECONDITION: a lost login ends the row's login"
    );
    assert_eq!(due(&a), ["main"], "PRECONDITION");
    a.now.fetch_add(92, Ordering::SeqCst);
    blocked();
    a.now.fetch_add(4_711, Ordering::SeqCst);
    auth_at(&a, "main", accounts::Auth::SignedOut, a.clock());
    a.now.fetch_add(17_000, Ordering::SeqCst);
    blocked();
    assert_eq!(
        facts(),
        (accounts::Auth::SignedOut, true, Some(ended)),
        "PRECONDITION: the persisted shape, its blocked sign-in standing"
    );
    a.now.fetch_add(200, Ordering::SeqCst);
    profile(OTHER_PROFILE);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(
        facts(),
        (accounts::Auth::SignedOut, true, Some(ended)),
        "CONTROL: a profile that names another account heals nothing"
    );
    profile(OWN_PROFILE);
    idle.login_seen(LoginSeen::Served);
    assert_eq!(facts(), (accounts::Auth::Ok, false, None), "healed");
    assert_eq!(due(&a), Vec::<String>::new());
    assert_eq!(a.ledger().last(), Some(&"auth"), "{:?}", a.ledger());
    assert!(!a.ledger().contains(&"signed-in"), "{:?}", a.ledger());
    assert_eq!(
        made(),
        Some((Some(signed_in), None)),
        "no renewal stamped by the heal"
    );
    a.now.fetch_add(60, Ordering::SeqCst);
    idle.login_seen(LoginSeen::Lost);
    assert_eq!(
        facts(),
        (accounts::Auth::SignedOut, false, Some(a.clock())),
        "CONTROL: a real lost login after the heal ends the login again"
    );
    assert_eq!(due(&a), ["main"]);
}

#[cfg(unix)]
mod tap {
    use super::*;
    use aterm_agent::harness::token_route::{DESCRIPTOR_ENV, MARKER_ENV, MINTED_ENV};

    const KEY: &str = "ac-fedcba9876543210";

    fn keyer(_realm: &str, email: &str) -> Option<String> {
        email.starts_with("owner@").then(|| KEY.to_owned())
    }

    /// A tapped tab on `main` whose default-home profile (under its process's `HOME`) is
    /// `profile`, with no reading taken yet.
    fn tapped(profile: &str) -> (Arc<TapWorld>, WorkerIdle) {
        let w = TapWorld::new();
        std::fs::write(w.roster_path(), roster_text("/logins/work-2")).unwrap();
        let home = w.tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".claude.json"), profile).unwrap();
        *w.env.lock().unwrap() = vec![format!("HOME={}", home.display())];
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        (w, idle)
    }

    /// [`tapped`] with its first reading taken, then its login struck signed out a minute
    /// later.
    fn struck_tap(profile: &str) -> (Arc<TapWorld>, WorkerIdle) {
        let (w, idle) = tapped(profile);
        w.tap(10, 42, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        w.later(60);
        strike(&w, "main");
        (w, idle)
    }

    fn step(idle: &WorkerIdle) -> Option<&'static str> {
        journal::with_keyer(keyer, || idle.tap_step(TapAt::Point))
    }

    fn strike(w: &TapWorld, label: &str) {
        accounts::Store::at(&w.state())
            .transact_logins(w.clock(), &mut None, |state, logins| {
                Ok(accounts::note_login_end(state, logins, label, w.clock()))
            })
            .expect("the strike");
    }

    fn auth(w: &TapWorld, label: &str) -> accounts::Auth {
        let (state, _) = accounts::Store::at(&w.state())
            .load(w.clock())
            .expect("the store");
        state.account(label).auth
    }

    /// `label`'s word as the gates read it ([`accounts::Account::current_auth`]).
    fn current(w: &TapWorld, label: &str) -> accounts::Auth {
        let store = accounts::Store::at(&w.state());
        let file = accounts::Roster::load(&w.roster_path()).expect("the roster");
        let roster = store.bound(&file, w.clock()).expect("bound");
        let (state, _) = store.load(w.clock()).expect("the store");
        state
            .account(label)
            .current_auth(roster.row(label).expect("a row"))
    }

    fn kinds(w: &TapWorld) -> Vec<&'static str> {
        accounts::Store::at(&w.state())
            .tail(100)
            .unwrap_or_default()
            .iter()
            .filter(|e| e.kind != accounts::Kind::Sample)
            .map(|e| e.kind.as_str())
            .collect()
    }

    /// A TAP READING SERVED AFTER THE STRIKE ON THE ROW'S OWN STORED LOGIN LIFTS IT: the same
    /// process's next reading, its API time grown, records `main` signed in again, the word's
    /// return on the ledger and no sign-in. Negative control: the fold alone (as before) leaves
    /// the word.
    #[test]
    fn a_tap_reading_after_a_strike_signs_its_own_login_back_in() {
        let (w, idle) = struck_tap(OWN_PROFILE);
        assert_eq!(
            current(&w, "main"),
            accounts::Auth::SignedOut,
            "PRECONDITION"
        );
        w.later(60);
        w.tap(20, 43, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(auth(&w, "main"), accounts::Auth::Ok);
        assert_eq!(kinds(&w), ["auth", "login-ended", "auth"]);
        assert_eq!(said(&idle), None);
    }

    /// A READING WHOSE WINDOWS MOVED WHILE ITS PROCESS MADE NO CALLS IS NO SERVICE: its API time
    /// the same as at its last reading, it is recorded, and `main` stays signed out.
    #[test]
    fn a_tap_reading_with_no_calls_since_lifts_nothing() {
        let (w, idle) = struck_tap(OWN_PROFILE);
        w.later(60);
        w.tap(10, 44, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(auth(&w, "main"), accounts::Auth::SignedOut);
        assert!(
            said(&idle).is_some_and(|s| s.contains("made no calls")),
            "{:?}",
            said(&idle)
        );
    }

    /// A READING DATED BEFORE THE STRIKE SAYS NOTHING OF THE LOGIN AFTER IT: reported before
    /// the strike and read after it, it leaves `main` signed out.
    #[test]
    fn a_tap_reading_dated_before_the_strike_lifts_nothing() {
        let (w, idle) = tapped(OWN_PROFILE);
        w.tap(10, 42, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        w.later(30);
        w.tap(20, 43, None);
        w.later(10);
        strike(&w, "main");
        w.later(20);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(auth(&w, "main"), accounts::Auth::SignedOut);
        assert!(
            said(&idle).is_some_and(|s| s.contains("not after its strike")),
            "{:?}",
            said(&idle)
        );
    }

    /// A PROFILE THAT NAMES NO ADDRESS PROVES NO ONE'S LOGIN: the reading is folded under
    /// `main`, its word stays signed out, and the unconfirmed identity is said.
    #[test]
    fn a_tap_reading_whose_identity_is_unproved_lifts_nothing() {
        let (w, idle) = struck_tap(UNNAMED_PROFILE);
        w.later(60);
        w.tap(20, 43, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(auth(&w, "main"), accounts::Auth::SignedOut);
        assert!(
            said(&idle).is_some_and(|s| s.contains("identity is not confirmed")),
            "{:?}",
            said(&idle)
        );
    }

    /// A DEFAULT HOME A `/login` MOVED ONTO ANOTHER ACCOUNT CREDITS NO WRONG ROW: its reading is
    /// `work-2`'s, and neither `main` nor a struck `work-2` (whose own login is its directory's,
    /// not the default home's) lifts.
    #[test]
    fn a_tap_reading_of_another_account_in_the_default_home_lifts_no_row() {
        let (w, idle) = struck_tap(OWN_PROFILE);
        strike(&w, "work-2");
        assert_eq!(
            current(&w, "work-2"),
            accounts::Auth::SignedOut,
            "PRECONDITION"
        );
        std::fs::write(w.tmp.path().join("home/.claude.json"), OTHER_PROFILE).unwrap();
        w.later(60);
        w.tap(20, 43, None);
        let _ = step(&idle);
        assert_eq!(auth(&w, "main"), accounts::Auth::SignedOut);
        assert_eq!(auth(&w, "work-2"), accounts::Auth::SignedOut);
        assert_eq!(kinds(&w).last(), Some(&"login-ended"), "{:?}", kinds(&w));
    }

    /// A READING OF THE ROW'S VAULT TOKEN IS THE TOKEN'S, NEVER ITS STORED LOGIN'S: credited to
    /// `main`, it leaves the stored login's word signed out and says so. The vault holds the row's
    /// token, made here before the session began, and the wrapper named it: a session on a token
    /// the vault does not hold is recorded nowhere.
    #[test]
    fn a_token_route_tap_reading_lifts_no_stored_login() {
        let (w, idle) = struck_tap(OWN_PROFILE);
        let home = w.tmp.path().join("home");
        std::fs::create_dir_all(home.join(".secrets")).unwrap();
        let minted_at = u64::try_from(w.born).unwrap() - 600;
        let _vault =
            super::super::token_tests::Vault::minted_for(&home, minted_at, "owner@example.com");
        let entry = accounts::TokenEntry::parse(&format!("aterm-claude-token-{KEY}")).unwrap();
        w.env.lock().unwrap().extend([
            format!("{DESCRIPTOR_ENV}=5"),
            format!("{MARKER_ENV}={}", entry.name()),
            format!("{MINTED_ENV}={minted_at}"),
        ]);
        if let Some(snap) = idle.kept.lock().unwrap().snapshot.as_mut() {
            snap.route = accounts::AuthRoute::VaultToken { entry };
        }
        w.later(60);
        w.tap(20, 43, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(auth(&w, "main"), accounts::Auth::SignedOut);
        assert!(
            said(&idle).is_some_and(|s| s.contains("token route")),
            "{:?}",
            said(&idle)
        );
    }
}

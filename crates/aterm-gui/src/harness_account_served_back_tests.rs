// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Served evidence after a strike is a served reading of the struck row's own stored login
//! ([`WorkerIdle::served_seen`], [`accounts::observe_served`]), dated by its own evidence: a
//! served point ([`WorkerIdle::claude_login_seen`]), a break's served turn dated by its reply
//! ([`IdleHost::login_served_at`]) and a usage-tap reading ([`WorkerIdle::tap_served`]). The
//! login verdict folds it against the strike by date (plan package UB-04): a reading newer than
//! the strike reads the row signed in, and nothing lifts the strike, which stays on record. It is
//! no sign-in: the ledger records the word's return (`auth`), never `signed-in`, and the login's
//! renewal and notice stand. Claude Code's `Please run /login` after a Cloudflare 403 struck an
//! account on `m16` on 2026-11-13 while its login served all day, and the stored word held: the
//! tap fold left the word alone, and a cruise break's turn under background work was told no
//! served point. A turn end's own time ([`WorkerIdle::login_served`]) is no such evidence: the
//! TurnEnd point is asked at idle points no turn worked to. The replies its process's transcript
//! already holds are, each dated by its row ([`WorkerIdle::transcript_served`]), so the first look
//! of a build that keeps the fold reads the service an older build's strike stood against. A wall
//! is dated by its first sighting on its process's screen ([`WorkerIdle::wall_sighted`]), and a
//! wall that process draws after it ran a turn or served is a new one.

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

/// AN IDLE TAB'S OLD WALL, ASKED AGAIN, NEVER STRIKES A LOGIN THAT SERVED SINCE (the m16 shape):
/// `s-tab-a`'s wall struck `main` and its screen still shows it; `s-tab-b` on the same login then
/// served a point, so `main` reads signed in. Hours later `s-tab-a`, idle, is asked its LoginEnded
/// point again on account news: the wall is dated by its first sighting on that process's screen
/// ([`WorkerIdle::wall_sighted`]), so it renews nothing, `main` stays signed in and no login end
/// is recorded again. Negative control: a wall dated when it is asked strikes `main` again.
#[test]
fn an_idle_tabs_old_wall_asked_again_never_strikes_a_login_that_served_since() {
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    std::fs::write(a.tmp.path().join("home").join(".claude.json"), OWN_PROFILE).unwrap();
    a.spawn("s-tab-a", ACCT_TALK_A, "main");
    a.spawn("s-tab-b", ACCT_TALK_B, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let tab_a = a.worker("s-tab-a", &cfg);
    let tab_b = a.worker("s-tab-b", &cfg);
    assert_eq!(tab_a.account(AccountPoint::LoginEnded), None);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "PRECONDITION: s-tab-a's wall struck it"
    );
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    tab_b.login_seen(LoginSeen::Served);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::Ok,
        "PRECONDITION: s-tab-b served since"
    );
    let ledger = a.ledger();
    for later in [3_600, 5 * 3_600, 8 * 3_600] {
        a.now.store(ACCT_NOW + later, Ordering::SeqCst);
        let _ = tab_a.account(AccountPoint::LoginEnded);
        assert_eq!(
            a.state().account("main").auth,
            accounts::Auth::Ok,
            "asked again {later} s after the wall"
        );
    }
    assert_eq!(a.ledger(), ledger, "nothing recorded");
}

/// A WALL ITS PROCESS DRAWS AFTER IT RAN A TURN IS A NEW ONE, DATED WHEN IT IS SEEN: `s-tab-a`'s
/// wall struck `main` and `s-tab-b` served since, so `s-tab-a`'s old wall, asked again with no
/// turn run, strikes nothing. Then the login really ends: `s-tab-a`'s process runs a turn
/// ([`IdleHost::turn_ran`]) and draws its wall again. That wall is first sighted now, newer than
/// `s-tab-b`'s service, and strikes `main` and ends its login, told at its point
/// ([`LoginSeen::Lost`]) or asked at its account point ([`AccountPoint::LoginEnded`]) alike.
/// NEGATIVE CONTROL (run): a sighting kept across the turn dates the new wall by the old one,
/// and `main` stays signed in.
#[test]
fn a_wall_drawn_after_its_process_ran_a_turn_strikes_a_login_that_served_since() {
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    std::fs::write(a.tmp.path().join("home").join(".claude.json"), OWN_PROFILE).unwrap();
    a.spawn("s-tab-a", ACCT_TALK_A, "main");
    a.spawn("s-tab-b", ACCT_TALK_B, "main");
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let tab_a = a.worker("s-tab-a", &cfg);
    let tab_b = a.worker("s-tab-b", &cfg);
    assert_eq!(tab_a.account(AccountPoint::LoginEnded), None);
    a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
    tab_b.login_seen(LoginSeen::Served);
    a.now.store(ACCT_NOW + 3_600, Ordering::SeqCst);
    let _ = tab_a.account(AccountPoint::LoginEnded);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::Ok,
        "PRECONDITION: the old wall, asked again with no turn run, strikes nothing"
    );
    assert_eq!(a.ledger(), ["auth", "login-ended", "auth"], "PRECONDITION");
    tab_a.turn_ran();
    a.now.store(ACCT_NOW + 3_700, Ordering::SeqCst);
    tab_a.login_seen(LoginSeen::Lost);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "the wall drawn after the turn strikes"
    );
    assert_eq!(
        a.ledger(),
        ["auth", "login-ended", "auth", "auth", "login-ended"]
    );
    a.now.store(ACCT_NOW + 3_760, Ordering::SeqCst);
    tab_b.login_seen(LoginSeen::Served);
    assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    tab_a.turn_ran();
    a.now.store(ACCT_NOW + 7_200, Ordering::SeqCst);
    let _ = tab_a.account(AccountPoint::LoginEnded);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "asked at its account point, the new wall strikes too"
    );
    assert_eq!(a.ledger().last(), Some(&"login-ended"), "{:?}", a.ledger());
    assert_eq!(due(&a), ["main"]);
}

/// A TURN END ON A STRUCK ROW READS THE REPLY ITS PROCESS ALREADY WROTE
/// ([`WorkerIdle::transcript_served`]): an hour after `main`'s strike, its process's transcript
/// holds a reply from before the strike, which says nothing after it, so `main` stays signed out
/// and the log says why. Once it holds one written half an hour after the strike, the next turn
/// end records that reply as a served reading dated by its row: `main` reads signed in, the
/// ledger records the word's return and no sign-in, and its sign-in is no longer due.
/// NEGATIVE CONTROL (run): a turn end that reads no transcript leaves `main` signed out.
#[test]
fn a_turn_end_on_a_struck_row_folds_the_reply_its_process_wrote_since() {
    let (a, idle) = struck(OWN_PROFILE);
    a.now.store(ACCT_NOW + 3_600, Ordering::SeqCst);
    a.replied(ACCT_TALK_A, ACCT_NOW - 50);
    let _ = idle.account(AccountPoint::TurnEnd);
    assert_eq!(a.state().account("main").auth, accounts::Auth::SignedOut);
    assert!(
        said(&idle).is_some_and(|s| s.contains("not after its strike")),
        "{:?}",
        said(&idle)
    );
    a.replied(ACCT_TALK_A, ACCT_NOW + 1_800);
    let _ = idle.account(AccountPoint::TurnEnd);
    assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    assert_eq!(a.state().account("main").auth_at, Some(ACCT_NOW + 1_800));
    assert_eq!(a.ledger(), ["auth", "login-ended", "auth"]);
    assert!(due(&a).is_empty(), "{:?}", due(&a));
}

/// THE FIRST LOOK ON A STRIKE AN OLDER BUILD RECORDED READS THE SERVICE ITS PROCESS WROTE SINCE:
/// `state.json` holds `main`'s word signed out at the wall and no ledger (the build that struck
/// it kept none), and `logins.json` a served clock that build's idle points re-stamped since,
/// with no turn worked to them. On this build's first turn-end point, with no reply in its
/// process's transcript since the wall, the re-stamped clock reads nothing and `main` stays
/// signed out; once the transcript holds a reply written after the wall, the next turn end reads
/// `main` signed in, with no new turn served. NEGATIVE CONTROL (run): a turn end that reads no
/// transcript leaves `main` signed out on the older build's records.
#[test]
fn the_first_look_on_an_older_builds_strike_reads_the_service_its_process_wrote() {
    let a = Accts::new();
    std::fs::write(a.roster_path(), roster_text(&a.login_dir("work-2"))).unwrap();
    std::fs::write(a.tmp.path().join("home").join(".claude.json"), OWN_PROFILE).unwrap();
    a.spawn("s-old", ACCT_TALK_A, "main");
    a.store()
        .transact_logins(ACCT_NOW, &mut None, |state, logins| {
            let main = state.accounts.entry("main".into()).or_default();
            main.auth = accounts::Auth::SignedOut;
            main.auth_at = Some(ACCT_NOW);
            let binding = logins.labels.entry("main".into()).or_default();
            binding.ended_at = Some(ACCT_NOW);
            binding.served_at = Some(ACCT_NOW + 3_000);
            Ok(Vec::new())
        })
        .expect("the older build's records");
    assert!(
        a.state().account("main").login.kept_nothing(),
        "PRECONDITION: no ledger"
    );
    let mut cfg = on();
    cfg.set("accounts", "off").unwrap();
    let idle = a.worker("s-old", &cfg);
    a.now.store(ACCT_NOW + 3_600, Ordering::SeqCst);
    let _ = idle.account(AccountPoint::TurnEnd);
    assert_eq!(
        a.state().account("main").auth,
        accounts::Auth::SignedOut,
        "CONTROL: a re-stamped served clock reads nothing"
    );
    a.replied(ACCT_TALK_A, ACCT_NOW + 1_800);
    let _ = idle.account(AccountPoint::TurnEnd);
    assert_eq!(a.state().account("main").auth, accounts::Auth::Ok);
    assert_eq!(a.ledger(), ["auth"]);
}

/// A SERVED POINT ([`LoginSeen::Served`]) ON THE STRUCK ROW'S OWN STORED LOGIN IS NEWER EVIDENCE
/// THAN THE STRIKE, so the row reads signed in, recording the word's return and no sign-in; a
/// point in the strike's own second decides nothing.
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
    assert!(!idle.served_seen(&seat, &roster, &look, "main", a.clock(), "test"));
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
/// process then serves signs it in with no hand edit: the served reading is the newest evidence,
/// so the word is ok, the blocked sign-in is dropped and aterm's sign-in ladder has nothing due,
/// while the login's end stays on record as the evidence it was. No credential changed, so the
/// heal is no sign-in: the login's renewal (an in-place sign-in three days before) and its unset
/// notice stand, and the ledger records the word's return, never `signed-in`. Negative
/// controls: a served point under a profile naming another account changes nothing, and a lost
/// login after the heal ends the login again.
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
    assert_eq!(facts(), (accounts::Auth::Ok, false, Some(ended)), "healed");
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

    /// A TAP READING SERVED AFTER THE STRIKE ON THE ROW'S OWN STORED LOGIN IS NEWER EVIDENCE: the
    /// same process's next reading, its API time grown, reads `main` signed in again, the word's
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

    /// A READING WHOSE API TIME GREW ENDS ITS PROCESS'S WALL SIGHTING: the process served, so a
    /// wall it shows after that is a new one, dated when it is seen, though its row's login reads
    /// signed in and the reading tells it nothing new ([`WorkerIdle::tap_step`]). CONTROL: a
    /// reading whose windows moved with no calls since keeps the sighting. NEGATIVE CONTROL
    /// (run): a tap step that never ends the sighting dates the later wall by the first.
    #[test]
    fn a_tap_reading_whose_api_time_grew_ends_its_processs_wall_sighting() {
        let (w, idle) = tapped(OWN_PROFILE);
        w.tap(10, 42, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        let snap = idle
            .kept
            .lock()
            .unwrap()
            .snapshot
            .clone()
            .expect("its process");
        let sighted = |at: i64| {
            idle.kept.lock().unwrap().account.wall_sighting.sighted(
                &snap,
                at,
                account_process_matches,
            )
        };
        let first = w.clock();
        assert_eq!(sighted(first), first);
        w.later(60);
        w.tap(10, 44, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(sighted(w.clock()), first, "CONTROL: no calls since");
        w.later(60);
        w.tap(20, 45, None);
        assert!(step(&idle).is_some_and(|s| s.starts_with("reading")));
        assert_eq!(
            sighted(w.clock()),
            w.clock(),
            "a wall after its calls is new"
        );
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's account judgments read the observation, not the belief
//! (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1), from the live bug on m65.

use super::*;
use aterm_agent::harness::journal::ObservedSrc;
use aterm_agent::harness::observe::Observed;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";

fn look(seen: Observation) -> AccountLook {
    AccountLook {
        screen_wall: None,
        profile_env: Vec::new(),
        who: accounts::ProcessLogin {
            login: None,
            default_home: true,
            route: accounts::AuthRoute::Stored,
        },
        label: Some("m22".into()),
        snap: snap("s-observed"),
        generation: Arc::default(),
        seen,
    }
}

fn seen(email: &str) -> Observation {
    Observation::Seen(Observed {
        cli: accounts::Cli::Claude.into(),
        email: email.to_owned(),
        key: None,
        region: None,
        src: ObservedSrc::Profile,
        path: "/home/someone/.claude.json".into(),
        mtime: Some(1),
    })
}

fn roster(text: &str) -> accounts::Roster {
    accounts::Roster::parse_for(accounts::Cli::Claude, text).unwrap()
}

/// The execution hold, the probe and the tap judge the row the login file names: the believed
/// default row `m22` while the file agrees, the row that lists unicorn where one does, and none
/// for an account no row lists, so nothing is held, probed or filed under `m22` on unicorn's
/// behalf.
/// NEGATIVE CONTROL: with `AccountLook::judged` reading `accounts::judged_label`, the unlisted
/// tab is held under `m22`: `` left: Some("m22") right: None ``.
#[test]
fn hold_uses_observed_row() {
    let one = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"
    ));
    assert_eq!(look(seen(BRONTO)).judged(&one), Some("m22"));
    assert_eq!(look(Observation::Unread).judged(&one), Some("m22"));
    assert_eq!(look(seen(UNICORN)).judged(&one), None);
    let dir = if cfg!(windows) {
        "C:\\\\logins\\\\u"
    } else {
        "/logins/u"
    };
    let two = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n\n\
         [[account]]\nlabel = \"m15\"\nclass = \"dedicated\"\ndir = \"{dir}\"\nemail = \"{UNICORN}\"\n"
    ));
    assert_eq!(look(seen(UNICORN)).judged(&two), Some("m15"));
}

/// A tap reading of an account no row lists is no row's: the reading's owner for the believed
/// row is another account, so `note_reading_held` files nothing there, and the tap's word is
/// [`TAP_UNLISTED`], which sends it to the journal instead.
/// NEGATIVE CONTROL: with `ReadingOwner::of` giving `Unobserved` for `Unlisted`, the owner is
/// unobserved: `` left: Unobserved right: Another ``.
#[test]
fn unlisted_tap_reading_goes_to_journal_not_state() {
    let one = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"
    ));
    let tab = look(seen(UNICORN));
    let reconciled = accounts::reconcile(&one, &tab.who, &tab.seen);
    assert!(matches!(reconciled, accounts::Reconciled::Unlisted(_)));
    assert_eq!(
        accounts::ReadingOwner::of(&reconciled, "m22"),
        accounts::ReadingOwner::Another
    );
    let mut state = accounts::State::default();
    let read = accounts::TapRead {
        seven_day: Some(accounts::WindowRead {
            pct_x10: 620,
            reset: 1_800_000_000,
            refilled: None,
        }),
        at: 1_790_000_000,
        ..Default::default()
    };
    let (events, _) = accounts::note_reading_held(
        &mut state,
        "m22",
        &read,
        1_790_000_000,
        accounts::ReadingOwner::of(&reconciled, "m22"),
    );
    assert!(events.is_empty());
    assert!(
        state
            .accounts
            .get("m22")
            .is_none_or(|a| a.windows.is_empty())
    );
    // The journal's notes of it are keyed by the observed account.
    aterm_agent::harness::journal::with_keyer(
        |_, email| (email == UNICORN).then(|| "ac-00000000000000a1".to_owned()),
        || {
            let key = aterm_agent::harness::journal::claude_email_key(UNICORN).unwrap();
            let notes =
                aterm_agent::harness::journal::quota_notes(accounts::Cli::Claude, key, &read);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].kind, "quota");
            assert_eq!(notes[0].window, Some("7d"));
            assert_eq!(notes[0].account, Some(key));
        },
    );
}

/// THE WALL BUG, at the host: a default-home tab whose `~/.claude.json` names unicorn hits its
/// weekly limit while the roster's dir-less row is `m22`. The wall is `m15`'s, the row that
/// lists unicorn, and `m22` is not walled.
/// NEGATIVE CONTROL: with `account_wall_of` passing `Observation::Unread` to the recorder, the
/// wall is the believed row's: `` left: ("wall", Some("m22")) right: ("wall", Some("m15")) ``.
#[test]
fn a_wall_is_recorded_on_the_account_the_login_file_names() {
    const NOW: i64 = 1_790_000_000;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-hh-observed-wall-")
        .tempdir()
        .unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{UNICORN}"}}}}"#),
    )
    .unwrap();
    let dir = if cfg!(windows) {
        "C:\\\\logins\\\\u"
    } else {
        "/logins/u"
    };
    let two = roster(&format!(
        "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n\n\
         [[account]]\nlabel = \"m15\"\nclass = \"dedicated\"\ndir = \"{dir}\"\nemail = \"{UNICORN}\"\n"
    ));
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let store = accounts::Store::at(&state);
    let captured = usage_fixture(&store, &two, NOW);
    let env = vec!["PATH=/usr/bin".to_owned()];
    let read = WallRead {
        env: &env,
        settled: true,
        wall: Some(aterm_agent::harness::upgrade::StandingWall {
            at: (NOW - 10).unsigned_abs(),
            until: Some((NOW + 3600).unsigned_abs()),
            window: Some("seven_day".to_owned()),
            credits: None,
        }),
        screen_wall: None,
        started: Some(NOW - 70),
    };
    let (word, label) = super::super::account_wall_of(
        (&store, Some(home.as_path())),
        &captured,
        "s-observed",
        &read,
        NOW,
    );
    assert_eq!((word, label.as_deref()), ("wall", Some("m15")));
    let held = store.load(NOW).unwrap().0;
    assert_eq!(held.accounts["m15"].walls.len(), 1);
    assert!(held.accounts.get("m22").is_none_or(|a| a.walls.is_empty()));
}

/// THE FLOOD BUG: a tap reading of an account no row lists is sent to the journal once. The
/// same windows of the same account are news again only a heartbeat later; other windows or
/// another account are news at once.
/// NEGATIVE CONTROL: with `tap_unlisted_due` always true, the second step publishes again:
/// `` assertion failed: !tap_unlisted_due(..) ``.
#[test]
fn an_unlisted_tap_reading_is_published_once() {
    let key = |text: &str| journal::Key::new(text).unwrap();
    let (a, b) = (key("ac-00000000000000a1"), key("ac-00000000000000b2"));
    let window = |pct| {
        Some(tap::Window {
            pct,
            resets_at: 1_800_000_000,
            pct_x10: Some(pct * 10),
        })
    };
    let now = 1_790_000_000;
    let sent = (a, (window(62), None), now);
    assert!(tap_unlisted_due(None, a, (window(62), None), now));
    assert!(!tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + 10
    ));
    assert!(!tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + accounts::READING_HEARTBEAT_S - 1
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        a,
        (window(62), None),
        now + accounts::READING_HEARTBEAT_S
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        a,
        (window(63), None),
        now + 10
    ));
    assert!(tap_unlisted_due(
        Some(&sent),
        b,
        (window(62), None),
        now + 10
    ));
}

/// A session on a Mac that lists no account is sighted: the worker's sighting names the login
/// its home's profile holds and opens its session under that account, with no roster path, no
/// roster file, a roster that lists none, and the account rotation and login repair both off;
/// the same tab is named at every look (the store keeps the machine id its first look made),
/// and the account advice of that Mac stays inert.
/// NEGATIVE CONTROL: with `sight` taking its roster from `account_roster(&seat)?` (the gate
/// `account_note` sat behind), the Mac with no roster is not sighted: `no roster path: left:
/// None right: Some(["seen", "session-start"])`.
#[test]
fn a_session_on_a_mac_that_lists_no_account_is_sighted() {
    use aterm_agent::harness::journal::{self, LoginState};
    journal::with_keyer(
        |_, email| (email == UNICORN).then(|| "ac-00000000000000a1".to_owned()),
        || {
            let a = Accts::new();
            std::fs::write(
                a.tmp.path().join("home").join(".claude.json"),
                format!(r#"{{"oauthAccount":{{"emailAddress":"{UNICORN}"}}}}"#),
            )
            .unwrap();
            a.spawn("s-sighted", ACCT_TALK_A, "main");
            let mut quiet = on();
            quiet.set("accounts", "off").unwrap();
            quiet.set("logins", "off").unwrap();
            let kinds = |sighted: &Option<observe::Sighted>| {
                sighted
                    .as_ref()
                    .map(|sighted| sighted.notes.iter().map(|n| n.kind).collect::<Vec<_>>())
            };
            let mut tabs = Vec::new();
            let mut first = |why: &str, cfg: &SupervisorConfig| {
                let idle = a.worker("s-sighted", cfg);
                let sighted = idle.sight();
                assert_eq!(
                    kinds(&sighted),
                    Some(vec!["seen", "session-start"]),
                    "{why}"
                );
                let login = sighted.unwrap().login.expect("the registry's login");
                assert_eq!(
                    (login.email.as_str(), login.state),
                    (UNICORN, LoginState::Running),
                    "{why}"
                );
                tabs.push(login.tab.expect("its tab"));
                assert_eq!(kinds(&idle.sight()), Some(vec![]), "{why}: noted once");
                assert_eq!(idle.account(AccountPoint::TurnEnd), None, "{why}");
            };
            a.listed.store(false, Ordering::SeqCst);
            first("no roster path", &on());
            a.listed.store(true, Ordering::SeqCst);
            std::fs::remove_file(a.roster_path()).unwrap();
            first("no roster file", &on());
            std::fs::write(a.roster_path(), "").unwrap();
            first("a roster that lists none", &on());
            first("accounts and logins off", &quiet);
            assert!(
                tabs.windows(2).all(|pair| pair[0] == pair[1]),
                "one tab, by the kept machine id: {tabs:?}"
            );
        },
    );
}

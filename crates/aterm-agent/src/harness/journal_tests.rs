// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness journal's tests: keying by the claims email, the ledger kind mapping, the
//! state diff's sessions, readings and walls, and the backfill (design §8, step 8).

use super::*;
use crate::harness::accounts::{
    Account, Row, SESSION_LIMIT, SOURCE_TAP, SessionRow, SignInStep, WEEK_LIMIT, Wall,
};

const ANN: &str = "ac-0123456789abcdef";
const BOB: &str = "ac-fedcba9876543210";
/// This Mac's machine id in the states the tests build, and the readings it made.
const HERE: &str = "ab12";

/// A keyer that knows two accounts by (realm, claims email), as the account key does.
fn keyer(realm: &str, email: &str) -> Option<String> {
    match (realm, email) {
        ("claude", "ann@example.invalid") => Some(ANN.to_owned()),
        ("claude", "bob@example.invalid") => Some(BOB.to_owned()),
        _ => None,
    }
}

fn roster() -> Roster {
    Roster {
        rows: vec![
            Row {
                label: "ann".into(),
                email: Some("Ann@Example.invalid ".into()),
                ..Row::default()
            },
            // An alias login: the claims email is its identity.
            Row {
                label: "bob".into(),
                email: Some("alias@proton.example".into()),
                identity_email: Some("bob@example.invalid".into()),
                ..Row::default()
            },
        ],
    }
}

/// A driven renewal's ledger line, as it writes it ([`crate::harness::login_drive::Drove::why`]).
fn renewal(
    outcome: crate::harness::login_drive::Outcome,
    host: Option<&str>,
    flow: crate::ai::connector::Flow,
) -> String {
    crate::harness::login_drive::Drove {
        outcome,
        host: host.map(str::to_owned),
    }
    .why(flow)
}

/// Grok's sign-in's ledger line, as it writes it ([`crate::harness::grok_signin::ledger_line`]).
fn grok(host: Option<&str>, word: &str) -> String {
    crate::harness::grok_signin::ledger_line(host, word, "row(r) headless(h) person(p)")
}

fn keyed() -> State {
    let mut state = State::default();
    for (label, pk) in [("ann", ANN), ("bob", BOB)] {
        state.accounts.insert(
            label.into(),
            Account {
                pk: Some(pk.into()),
                ..Account::default()
            },
        );
    }
    state
}

/// `learn` keys each account a roster row names by the claims email's account key: an alias
/// login by its identity, an address in any case by its lower-case form; it adds no account
/// the state does not hold.
/// NEGATIVE CONTROL: with `row_key` keying by the login address (`row.email.as_deref()?` in
/// place of `row.expected_identity_email()?`), the alias row is not keyed: `` left: None
/// right: Some("ac-fedcba9876543210") ``.
#[test]
fn learn_keys_accounts_by_the_claims_email() {
    let mut state = State::default();
    state.accounts.insert("ann".into(), Account::default());
    state.accounts.insert("bob".into(), Account::default());
    learn_with(Cli::Claude, &roster(), &mut state, &keyer);
    assert_eq!(state.accounts["ann"].pk.as_deref(), Some(ANN));
    assert_eq!(state.accounts["bob"].pk.as_deref(), Some(BOB));
    assert_eq!(state.accounts.len(), 2, "no account is added");
    let mut none = State::default();
    learn(Cli::Claude, &roster(), &mut none);
    assert!(none.accounts.is_empty());
}

#[test]
fn current_account_keys_follow_provider_identity_and_survive_a_relabel() {
    let mut rows = roster();
    rows.rows[1].identity_email = Some(" BoB@Example.INVALID \t".into());
    let mut state = State::default();
    for row in &rows.rows {
        state.accounts.insert(row.label.clone(), Account::default());
    }
    learn_with(Cli::Claude, &rows, &mut state, &keyer);
    for (row, expected) in rows.rows.iter().zip([ANN, BOB]) {
        assert_eq!(
            account_key_with(Cli::Claude, row, &keyer),
            Key::new(expected)
        );
        assert_eq!(state.accounts[&row.label].pk.as_deref(), Some(expected));
        let mut renamed = row.clone();
        renamed.label = "renamed".into();
        renamed.dir = Some("/synthetic/another-login-route".into());
        assert_eq!(
            account_key_with(Cli::Claude, &renamed, &keyer),
            Key::new(expected),
            "a local label and login path do not change the provider account"
        );
    }
}

#[test]
fn current_account_keys_separate_kimi_regions_and_default_to_global() {
    use crate::ai::connector::Region;

    let mut row = Row {
        label: "kimi".into(),
        email: Some(" Ann@Example.INVALID \t".into()),
        ..Row::default()
    };
    let observed = std::cell::RefCell::new(Vec::new());
    let region_keyer = |realm: &str, email: &str| {
        observed
            .borrow_mut()
            .push((realm.to_owned(), email.to_owned()));
        match (realm, email) {
            ("kimi:global", "ann@example.invalid") => Some(ANN.to_owned()),
            ("kimi:mainland-cn", "ann@example.invalid") => Some(BOB.to_owned()),
            _ => None,
        }
    };
    for (region, expected) in [
        (None, ANN),
        (Some(Region::Global), ANN),
        (Some(Region::MainlandCn), BOB),
    ] {
        row.vendor.region = region;
        assert_eq!(
            account_key_with(Cli::Kimi, &row, &region_keyer),
            Key::new(expected)
        );
    }
    assert_eq!(
        observed.into_inner(),
        ["kimi:global", "kimi:global", "kimi:mainland-cn"]
            .map(|realm| (realm.to_owned(), "ann@example.invalid".to_owned()))
    );
}

#[test]
fn current_account_keys_require_identity_and_a_valid_keyer_result() {
    let mut row = Row::default();
    let must_not_key = |_: &str, _: &str| -> Option<String> {
        panic!("an absent or invalid provider identity must not reach the keyer")
    };
    assert_eq!(account_key_with(Cli::Claude, &row, &must_not_key), None);
    row.email = Some("ann@example.invalid".into());
    for invalid in ["", " ", "not-an-address"] {
        row.identity_email = Some(invalid.into());
        assert_eq!(account_key_with(Cli::Claude, &row, &must_not_key), None);
    }
    row.identity_email = None;
    assert_eq!(account_key_with(Cli::Claude, &row, &|_, _| None), None);
    for invalid in [
        "",
        "not-a-key",
        "ac-0123",
        "ac-0123456789abcdeg",
        "ac-0123456789ABCDEf",
    ] {
        assert_eq!(
            account_key_with(Cli::Claude, &row, &|_, _| Some(invalid.to_owned())),
            None,
            "a keyer response is validated: {invalid:?}"
        );
    }
}

#[test]
fn scoped_keyers_restore_the_previous_lookup_after_nesting_and_unwind() {
    fn bob_keyer(_: &str, _: &str) -> Option<String> {
        Some(BOB.to_owned())
    }
    fn no_keyer(_: &str, _: &str) -> Option<String> {
        None
    }

    let row = roster().rows.remove(0);
    let before = account_key(Cli::Claude, &row);
    with_keyer(keyer, || {
        assert_eq!(account_key(Cli::Claude, &row), Key::new(ANN));
        with_keyer(bob_keyer, || {
            assert_eq!(account_key(Cli::Claude, &row), Key::new(BOB));
        });
        assert_eq!(account_key(Cli::Claude, &row), Key::new(ANN));
        let failed = std::panic::catch_unwind(|| {
            with_keyer(no_keyer, || {
                assert_eq!(account_key(Cli::Claude, &row), None);
                panic!("unwind the inner keyer scope");
            });
        });
        assert!(failed.is_err());
        assert_eq!(account_key(Cli::Claude, &row), Key::new(ANN));
    });
    assert_eq!(account_key(Cli::Claude, &row), before);
    let failed = std::panic::catch_unwind(|| {
        with_keyer(bob_keyer, || panic!("unwind the outer keyer scope"));
    });
    assert!(failed.is_err());
    assert_eq!(account_key(Cli::Claude, &row), before);
}

/// `observe` keys each ledger row by its account's learned key and maps its kind (design §8):
/// a rotation is a switch naming the account rotated to, an auth word is `signed-out`, a sample
/// is nothing; a sign-in row is the step its writer states, never its reason line: only one that
/// failed (an automatic one's, a driven renewal's, Grok's, each `why` made by its writer's own
/// function) is `sign-in-failed`, with how it ended as one closed word where its writer's form
/// says; one that began, one that bound a login (its `renewed` row notes it), a person's browser
/// window ending and a row that states no step (as rows written before the step were) are no
/// note; no note carries a word that is not one of the module's constants.
/// NEGATIVE CONTROLS: with `of_ledger` leaving `to` unset, the switch names no account:
/// `` left: None right: Some(ac-fedcba9876543210) ``; with `Kind::SignIn` mapped to
/// `sign-in-failed` whatever its step (as before the audit of 2026-11-09), the begun, window and
/// signed-in rows are failures too and the kinds differ.
#[test]
fn observe_keys_ledger_rows_and_maps_their_kinds() {
    let state = keyed();
    let mut events = vec![
        Event::new(100, Kind::Rotate, "ann").with_to("bob"),
        Event::sign_in(101, "ann", SignInStep::Failed)
            .because("auto failed: ann@example.invalid https://x"),
        Event::sign_in(101, "ann", SignInStep::Begun).because("shared adoption begun"),
        Event::sign_in(101, "ann", SignInStep::Window).because("x window closed session-kept"),
        Event::sign_in(101, "ann", SignInStep::SignedIn).because("auto signed-in: device"),
        Event::new(101, Kind::SignIn, "ann").because("auto failed: device"),
        Event::new(102, Kind::Auth, "bob").because("signed-out"),
        Event::new(103, Kind::Sample, "ann"),
        Event::new(104, Kind::Rotate, "unkeyed"),
        Event::sign_in(105, "bob", SignInStep::SignedIn).because("auto signed-in: mailbox"),
        Event::sign_in(106, "bob", SignInStep::Failed).because("auto no-route: profile(none)"),
        Event::sign_in(107, "ann", SignInStep::Failed).because("a reason no ladder wrote"),
        Event::sign_in(108, "ann", SignInStep::Begun)
            .because(crate::harness::accounts::SHARED_ADOPTION_BEGUN),
        Event::sign_in(109, "ann", SignInStep::Begun)
            .because(crate::harness::accounts::SHARED_PREPARATION_BEGUN),
        Event::sign_in(110, "bob", SignInStep::Window).because(
            &crate::harness::browser_window::ledger_why(
                "claude.ai",
                crate::harness::browser_window::WindowEnd::Done,
                "session-kept",
            ),
        ),
        Event::sign_in(111, "bob", SignInStep::Failed).because(&renewal(
            crate::harness::login_drive::Outcome::BotCheck(String::new()),
            Some("claude.ai"),
            crate::ai::connector::Flow::Browser,
        )),
        Event::sign_in(112, "ann", SignInStep::Failed).because(&renewal(
            crate::harness::login_drive::Outcome::Hung,
            None,
            crate::ai::connector::Flow::Device,
        )),
        Event::sign_in(113, "bob", SignInStep::SignedIn)
            .because(&grok(Some("accounts.x.ai"), "signed-in")),
        Event::sign_in(114, "bob", SignInStep::SignedIn)
            .because(&grok(Some("accounts.x.ai"), "signed-in-elsewhere")),
        Event::sign_in(115, "ann", SignInStep::Failed).because(&grok(None, "stopped")),
        Event::sign_in(116, "ann", SignInStep::Failed)
            .because(&grok(Some("accounts.x.ai"), "bot-check")),
        // A page host that is the automatic form's word is still read as a renewal's.
        Event::sign_in(117, "ann", SignInStep::Failed).because(&renewal(
            crate::harness::login_drive::Outcome::OffPin,
            Some("auto"),
            crate::ai::connector::Flow::Browser,
        )),
    ];
    let notes = observe(Cli::Claude, &state, &mut state.clone(), &mut events, 200);
    assert_eq!(events[0].pk.as_deref(), Some(ANN));
    assert_eq!(events[6].pk.as_deref(), Some(BOB));
    assert_eq!(events[8].pk, None, "an unkeyed account is not noted");
    let kinds: Vec<&str> = notes.iter().map(|n| n.kind).collect();
    assert_eq!(
        kinds,
        [
            "switch",
            "sign-in-failed",
            "signed-out",
            "sign-in-failed",
            "sign-in-failed",
            "sign-in-failed",
            "sign-in-failed",
            "sign-in-failed",
            "sign-in-failed",
            "sign-in-failed"
        ],
        "a sign-in that signed its row in, or a step of one, is not a failure"
    );
    assert_eq!(notes[0].to, Key::new(BOB));
    let whys: Vec<_> = notes.iter().map(|n| n.why).collect();
    assert_eq!(
        whys,
        [
            None,
            Some("failed"),
            None,
            Some("no-route"),
            None,
            Some("blocked"),
            Some("failed"),
            Some("failed"),
            Some("blocked"),
            Some("failed")
        ],
        "how a sign-in ended, and no reason line, reaches the journal"
    );
    for note in &notes {
        assert!(KINDS.contains(&note.kind));
        assert!(SOURCES.contains(&note.src));
        assert!(note.why.is_none_or(|w| WHYS.contains(&w)));
        assert_eq!(note.cli, "claude");
    }
}

/// A sign-in row's step survives the ledger, and only on a sign-in row.
/// NEGATIVE CONTROL: with `to_json_line` not writing `step`, the row reads back with none:
/// `` left: None right: Some(Begun) ``.
#[test]
fn a_sign_in_rows_step_round_trips_the_ledger() {
    for step in [
        SignInStep::Begun,
        SignInStep::Window,
        SignInStep::SignedIn,
        SignInStep::Failed,
    ] {
        let row = Event::sign_in(7, "ann", step).because("shared adoption begun");
        let back = Event::of_json_line(&row.to_json_line()).unwrap();
        assert_eq!(back.step, Some(step));
        assert_eq!(back, row);
    }
    let mut other = Event::new(7, Kind::Rotate, "ann");
    other.step = Some(SignInStep::Failed);
    let back = Event::of_json_line(&other.to_json_line()).unwrap();
    assert_eq!(back.step, None, "no other kind states a step");
    let legacy = Event::of_json_line(r#"{"t":7,"kind":"sign-in","account":"ann","why":"x"}"#);
    assert_eq!(legacy.unwrap().step, None);
}

/// A told note's kind, reason and time.
type ToldNote = (&'static str, Option<&'static str>, u64);

/// The kinds and reasons of the notes `observe` tells a journal that keeps them, with the state
/// `after` holding `auth` for ann and the journal last told `said`.
fn told_of(
    auth: Auth,
    said: Option<Told>,
    mut events: Vec<Event>,
    now: i64,
) -> (Vec<ToldNote>, Option<Told>) {
    let mut after = keyed();
    after.accounts.get_mut("ann").unwrap().auth = auth;
    // Bob's word is settled, so only ann's notes are read.
    after.told.extend(said.map(|said| (ANN.to_owned(), said)));
    let before = after.clone();
    let (notes, published) = capture(|| {
        let notes = observe(Cli::Claude, &before, &mut after, &mut events, now);
        publish(&notes);
        notes
    });
    assert_eq!(notes, published);
    let ann = Key::new(ANN);
    let told = notes
        .iter()
        .filter(|note| note.account == ann)
        .map(|note| (note.kind, note.why, note.t))
        .collect();
    (told, after.told.get(ANN).copied())
}

const fn said(word: SignWord, t: u64) -> Option<Told> {
    Some(Told { word, t })
}

/// THE AUDIT'S FIRST FINDING: a sign-in that succeeded is told as `renewed`, and the attempt row
/// its transaction writes after it, in the same second, is no word at all, so no fold ends on
/// `failed`; neither is a sign-in that only began, nor a browser window's end.
/// NEGATIVE CONTROL: with `SignInStep::SignedIn` mapped to `sign-in-failed` (every sign-in row
/// a failure, as before), the working login is given an `error` for a sign-in that succeeded:
/// `` left: [("renewed", None, 100), ("error", Some("sign-in-failed"), 100)] ``.
#[test]
fn a_sign_in_that_succeeded_is_never_told_as_a_failure() {
    let rows = vec![
        Event::sign_in(90, "ann", SignInStep::Begun).because("shared preparation begun"),
        Event::new(100, Kind::Auth, "ann").because("ok"),
        Event::new(100, Kind::Renewed, "ann"),
        Event::sign_in(100, "ann", SignInStep::SignedIn).because("auto signed-in: device"),
        Event::sign_in(100, "ann", SignInStep::Window).because("x window closed no-session"),
    ];
    let (told, cursor) = told_of(Auth::Ok, said(SignWord::Out, 50), rows, 100);
    assert_eq!(told, [("renewed", None, 100)]);
    assert_eq!(cursor, said(SignWord::In, 100));
}

/// A row's word is told only where the store holds it. A sign-in that failed, or answered
/// another account, on a login that still works here leaves the fleet's word `in` and is an
/// `error` saying which; on a login that is signed out it is the account's word. A login wall
/// the store refused (it holds a later word) is not told.
/// NEGATIVE CONTROL: with `settle` telling every row's word (`holds` always true), the working
/// login is published as failed: `` left: [("sign-in-failed", None, 201)] right: [("error",
/// Some("sign-in-failed"), 200)] ``.
#[test]
fn a_rows_word_is_told_only_where_the_store_holds_it() {
    let failed = || vec![Event::sign_in(200, "ann", SignInStep::Failed)];
    let wrong = || vec![Event::new(200, Kind::WrongAccount, "ann")];
    let held = said(SignWord::In, 100);
    let (told, cursor) = told_of(Auth::Ok, held, failed(), 200);
    assert_eq!(told, [("error", Some("sign-in-failed"), 200)]);
    assert_eq!(cursor, held, "the fleet's word stands");
    let (told, _) = told_of(Auth::Ok, held, wrong(), 200);
    assert_eq!(told, [("error", Some("wrong-account"), 200)]);
    let (told, _) = told_of(
        Auth::Ok,
        held,
        vec![Event::new(90, Kind::LoginEnded, "ann")],
        200,
    );
    assert_eq!(told, [], "a wall older than the word the store holds");
    let out = said(SignWord::Out, 100);
    let (told, cursor) = told_of(Auth::SignedOut, out, failed(), 200);
    assert_eq!(told, [("sign-in-failed", None, 200)]);
    assert_eq!(cursor, said(SignWord::Failed, 200));
    let (told, cursor) = told_of(Auth::SignedOut, out, wrong(), 200);
    assert_eq!(told, [("wrong-account", None, 200)]);
    assert_eq!(cursor, said(SignWord::Wrong, 200));
    // A login wall: the auth word and the wall are one transaction, told once, as the wall.
    let wall = vec![
        Event::new(200, Kind::Auth, "ann").because("signed-out"),
        Event::new(200, Kind::LoginEnded, "ann"),
    ];
    let (told, cursor) = told_of(Auth::SignedOut, held, wall, 200);
    assert_eq!(told, [("login-ended", None, 200)]);
    assert_eq!(cursor, said(SignWord::Ended, 200));
}

/// THE AUDIT'S SECOND FINDING: a word no ledger row states is told from the state. A login that
/// reads working again (a served turn proves it, with no auth row) is `signed-in`; one found
/// signed out is `signed-out`; a word this Mac held before it kept a record of what it told is
/// stated once as `auth` saying `ok`; a word the store no longer holds, or an account it no
/// longer has a row of, is withdrawn with `auth` saying `unseen`; and a word the journal already
/// holds is not told again.
/// NEGATIVE CONTROL: with `settle`'s closing comparison skipped (`agreed` always true), the heal
/// is not heard: `` left: [] right: [("signed-in", None, 300)] ``.
#[test]
fn a_word_no_row_states_is_told_from_the_state() {
    let none = Vec::new;
    let (told, cursor) = told_of(Auth::Ok, said(SignWord::Failed, 100), none(), 300);
    assert_eq!(told, [("signed-in", None, 300)]);
    assert_eq!(cursor, said(SignWord::In, 300));
    let (told, _) = told_of(Auth::SignedOut, said(SignWord::In, 100), none(), 300);
    assert_eq!(told, [("signed-out", None, 300)]);
    let (told, cursor) = told_of(Auth::Ok, None, none(), 300);
    assert_eq!(told, [("auth", Some("ok"), 300)]);
    assert_eq!(cursor, said(SignWord::In, 300));
    let (told, cursor) = told_of(Auth::Unseen, said(SignWord::In, 100), none(), 300);
    assert_eq!(told, [("auth", Some("unseen"), 300)]);
    assert_eq!(cursor, said(SignWord::Unknown, 300));
    for (auth, held) in [
        (Auth::Ok, said(SignWord::In, 100)),
        (Auth::SignedOut, said(SignWord::Ended, 100)),
        (Auth::Unseen, said(SignWord::Failed, 100)),
        (Auth::Unseen, None),
    ] {
        let (told, cursor) = told_of(auth, held, none(), 300);
        assert_eq!(told, [], "{auth:?} is what {held:?} says");
        assert_eq!(cursor, held);
    }
    // No row of the account any more: the Mac's word is withdrawn, and that is kept as told.
    let mut after = keyed();
    after.told.insert(
        "ac-00000000000000aa".into(),
        Told {
            word: SignWord::In,
            t: 100,
        },
    );
    let before = after.clone();
    let (notes, _) = capture(|| observe(Cli::Claude, &before, &mut after, &mut [], 300));
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].kind, notes[0].why), ("auth", Some("unseen")));
    assert_eq!(notes[0].account, Key::new("ac-00000000000000aa"));
    assert_eq!(
        after.told["ac-00000000000000aa"].word,
        SignWord::Unknown,
        "kept, so a later word of it is told after this one"
    );
}

/// Each word of an account is told under a later second than the one before it, whatever the
/// clock or a row's own time says, so every fold takes the last one told as the newest and none
/// has to break a tie; and what was told is saved with the state.
/// NEGATIVE CONTROL: with `stamp` returning `natural`, the word told after a clock step back
/// carries the older time and loses every fold: `` left: [("signed-out", None, 400)] right:
/// [("signed-out", None, 501)] ``.
#[test]
fn a_later_word_is_told_under_a_later_time() {
    let held = said(SignWord::In, 500);
    let row = vec![Event::new(400, Kind::Auth, "ann").because("signed-out")];
    let (told, cursor) = told_of(Auth::SignedOut, held, row, 400);
    assert_eq!(told, [("signed-out", None, 501)]);
    assert_eq!(cursor, said(SignWord::Out, 501));
    let (told, _) = told_of(Auth::SignedOut, held, Vec::new(), 400);
    assert_eq!(told, [("signed-out", None, 501)]);
    let again = vec![Event::new(400, Kind::Renewed, "ann")];
    let (told, cursor) = told_of(Auth::Ok, held, again, 400);
    assert_eq!(told, [("renewed", None, 500)], "the same word, no earlier");
    assert_eq!(cursor, held);
    let mut state = keyed();
    state.told.insert(ANN.into(), held.unwrap());
    let back = State::from_json(&state.to_json(), 600).unwrap();
    assert_eq!(back.told, state.told);
    assert_eq!(back, state);
    assert_ne!(back, keyed(), "what was told is part of the state");
}

/// A process whose notes go nowhere records nothing as told, so the next one that keeps notes
/// tells the journal what it left; and a captured scope ends with the thread as it was.
/// NEGATIVE CONTROL: with `telling` always true, the sinkless transaction records the word as
/// told though nothing took it: `` left: {"ac-0123456789abcdef": Told { word: In, t: 300 }}
/// right: {} ``.
#[test]
fn a_process_with_no_journal_records_nothing_as_told() {
    let mut after = keyed();
    after.accounts.get_mut("ann").unwrap().auth = Auth::Ok;
    let before = after.clone();
    let notes = observe(Cli::Claude, &before, &mut after, &mut [], 300);
    assert_eq!(notes, []);
    assert_eq!(after.told, BTreeMap::new());
    assert_eq!(after, before);
    let ((), outer) = capture(|| {
        let ((), inner) = capture(|| {
            note(&Note::new(
                1,
                "switch",
                Key::new(ANN).unwrap(),
                Cli::Claude,
                "cli-output",
            ))
        });
        assert_eq!(inner.len(), 1);
        note(&Note::new(
            2,
            "switch",
            Key::new(BOB).unwrap(),
            Cli::Claude,
            "cli-output",
        ));
    });
    assert_eq!(outer.len(), 1, "a nested scope's notes are its own");
    assert_eq!(outer[0].t, 2);
    assert!(!telling());
}

fn fig(pct_x10: u32, reset: i64, at: i64) -> WindowFig {
    WindowFig {
        pct_x10,
        reset,
        at,
        by: HERE.into(),
        source: SOURCE_TAP.into(),
        epoch_at: None,
    }
}

/// The state's diff gives session starts and ends (a session gone, a new one, one moved to
/// another account), a reading that moved a point or more or changed its reset (not one that
/// moved less), and each wall seen or cleared.
/// NEGATIVE CONTROL: with `moved` noting any change (`>= 0`), the half-point move is noted:
/// `` left: [… ("quota", "ac-0123456789abcdef"), ("quota", "ac-0123456789abcdef"), ("wall",
/// "ac-0123456789abcdef")] ``.
#[test]
fn observe_notes_sessions_readings_and_walls_from_the_diff() {
    let mut before = keyed();
    let row = |account: &str| SessionRow {
        account: account.into(),
        at: 1,
        held: None,
    };
    before.sessions.insert("s1".into(), row("ann"));
    before.sessions.insert("s2".into(), row("ann"));
    let ann = before.accounts.get_mut("ann").unwrap();
    ann.windows
        .insert(SESSION_LIMIT.into(), fig(100, 5_000, 10));
    ann.windows.insert(WEEK_LIMIT.into(), fig(300, 90_000, 10));
    let mut after = before.clone();
    after.sessions.remove("s1");
    after.sessions.insert("s2".into(), row("bob"));
    after.sessions.insert("s3".into(), row("ann"));
    let ann = after.accounts.get_mut("ann").unwrap();
    ann.windows
        .insert(SESSION_LIMIT.into(), fig(105, 5_000, 20));
    ann.windows.insert(WEEK_LIMIT.into(), fig(310, 90_000, 20));
    ann.walls.insert(
        SESSION_LIMIT.into(),
        Wall {
            until: Some(5_000),
            at: 30,
            by: HERE.into(),
            attempts: 0,
            cleared: false,
        },
    );
    let notes = observe(Cli::Claude, &before, &mut after.clone(), &mut [], 40);
    let summary: Vec<(&str, &str)> = notes
        .iter()
        .map(|n| (n.kind, n.account.as_ref().map_or("", Key::as_str)))
        .collect();
    assert_eq!(
        summary,
        [
            ("session-end", ANN),
            ("session-start", BOB),
            ("session-start", ANN),
            ("session-end", ANN),
            ("quota", ANN),
            ("wall", ANN),
        ]
    );
    let quota = &notes[4];
    assert_eq!(
        (
            quota.window,
            quota.pct_x10,
            quota.reset,
            quota.src,
            quota.res
        ),
        (
            Some("7d"),
            Some(310),
            Some(90_000),
            "statusline",
            Some("exact")
        )
    );
    assert_eq!((notes[5].window, notes[5].until), (Some("5h"), Some(5_000)));
}

/// The backfill notes the local ledger's last 30 days and the state's standing facts once,
/// keyed by the roster where no key is learned yet; it leaves no marker of its own, which the
/// caller writes once the notes are durable.
/// NEGATIVE CONTROL: with the 30-day filter removed (`notes.retain(…)`), the 40-day-old
/// rotation is noted: `` left: ["switch", "signed-in", "quota", "auth", "session-end"] ``.
#[cfg(unix)]
#[test]
fn backfill_notes_the_last_thirty_days_once() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let now = 100 * 86_400;
    let store = Store::at_for(&harness, Cli::Claude, &home);
    let mut state = State {
        machine: HERE.into(),
        ..State::default()
    };
    let ann = state.accounts.entry("ann".into()).or_default();
    ann.windows
        .insert(SESSION_LIMIT.into(), fig(420, now + 3600, now - 60));
    ann.windows
        .insert(WEEK_LIMIT.into(), fig(100, now - 10, now - 7200));
    ann.auth = Auth::Ok;
    ann.auth_at = Some(now - 300);
    ann.ran_at = Some(now - 120);
    store
        .append(&[
            Event::new(now - 40 * 86_400, Kind::Rotate, "ann").with_to("bob"),
            Event::new(now - 600, Kind::SignedIn, "ann"),
        ])
        .unwrap();
    aterm_uds::private_file::write_private(
        store.state_file().parent().unwrap(),
        "state.json",
        (state.to_json() + "\n").as_bytes(),
    )
    .unwrap();
    assert!(!backfilled(&harness));
    let roster = roster();
    let notes = backfill(
        &harness,
        &home,
        &|cli| (cli == Cli::Claude).then(|| roster.clone()),
        &keyer,
        now,
    );
    let kinds: Vec<&str> = notes.iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["signed-in", "quota", "auth", "session-end"]);
    assert!(notes.iter().all(|n| n.account == Key::new(ANN)));
    assert_eq!(notes[1].window, Some("5h"), "only a current reading");
    assert!(
        !backfilled(&harness),
        "not done until the notes are durable"
    );
    let mark_dir = harness.join("alab");
    aterm_uds::private_file::ensure_private_dir(&mark_dir).unwrap();
    let target = scratch.path().join("elsewhere");
    std::fs::write(&target, b"1\n").unwrap();
    std::os::unix::fs::symlink(&target, mark_dir.join("backfilled")).unwrap();
    assert!(
        !backfilled(&harness),
        "a symlink is no private backfill marker"
    );
    std::fs::remove_file(mark_dir.join("backfilled")).unwrap();
    mark_backfilled(&harness).unwrap();
    assert!(backfilled(&harness));
}

/// A provider read the door reports is a `probe` saying why the account is in use: a pooled read
/// keyed by its roster row's claims email, an unpooled one (agy, Muse) by its realm, with the
/// login it saw where it saw one; a label no row keys and a CLI with no roster here are not
/// noted, and a ledger probe claim notes nothing, since the read it leads to is the door's.
/// NEGATIVE CONTROLS: with `probe_note` leaving `why` unset, the reason is lost:
/// `` left: None right: Some("live-session") ``; with agy and Muse returning `None` again (as
/// before step 8's reconciliation), `` called `Option::unwrap()` on a `None` value ``.
#[test]
fn a_provider_read_is_a_probe_of_its_account_saying_why() {
    use crate::harness::provider_read::{InUse, ProbeEvent, Read};
    let roster = roster();
    let rosters = |cli: Cli| (cli == Cli::Claude).then(|| roster.clone());
    let read = |account: Option<&str>, read: Read| ProbeEvent {
        at: 500,
        read,
        account: account.map(str::to_owned),
        login: None,
        why: InUse::LiveSession,
    };
    let note = |event: &ProbeEvent| probe_note(&rosters, &keyer, event);
    let ann = note(&read(Some("ann"), Read::ClaudePanel)).unwrap();
    assert_eq!(
        (ann.kind, ann.src, ann.cli, ann.t),
        ("probe", "probe", "claude", 500)
    );
    assert_eq!(ann.account, Key::new(ANN));
    assert_eq!(ann.why, Some("live-session"));
    assert!(ann.why.is_some_and(|why| WHYS.contains(&why)));
    let bob = note(&read(Some("bob"), Read::ClaudePanel)).unwrap();
    assert_eq!(bob.account, Key::new(BOB), "an alias by its identity");
    for unkeyed in [
        read(Some("nobody"), Read::ClaudePanel),
        read(Some("ann"), Read::CodexEndpoint),
    ] {
        assert_eq!(note(&unkeyed), None, "{unkeyed:?}");
    }
    // An unpooled read is attributed by realm: its CLI word, and the login it saw where it saw
    // one, so every agy and Muse read is on record.
    for (cli, kind) in [("agy", Read::AgyUsage), ("muse", Read::MuseUsage)] {
        let realm_only = note(&read(None, kind)).unwrap();
        assert_eq!(
            (
                realm_only.kind,
                realm_only.cli,
                realm_only.account,
                realm_only.why
            ),
            ("probe", cli, None, Some("live-session"))
        );
        let seen = note(&ProbeEvent {
            login: Key::new(BOB),
            ..read(None, kind)
        })
        .unwrap();
        assert_eq!((seen.cli, seen.account), (cli, Key::new(BOB)));
    }
    let keyed = keyed();
    let mut claims = vec![Event::new(300, Kind::Probe, "ann")];
    assert!(
        observe(Cli::Claude, &keyed, &mut keyed.clone(), &mut claims, 400).is_empty(),
        "a probe claim is no read"
    );
}

/// A reading or a wall of a provider workspace names it, so it folds into that sub-account's own
/// `use` row (design §4.13): a Codex account's observed key is a workspace, any other provider's
/// is its one account and names none. A reading or a wall another Mac made is not this Mac's to
/// publish.
/// NEGATIVE CONTROLS: with `SubKey::workspace` taking any provider's key (no `x-` prefix
/// check), the Claude reading names a workspace: `` left: Some(…) right: None ``; with `observe`
/// noting every reading (`mine(&fig.by)` removed), the other Mac's reading is noted:
/// `` left: 3 right: 2 ``.
#[test]
fn a_workspace_reading_names_its_sub_account_and_another_macs_is_left() {
    use crate::harness::profile::AccountKey;
    let codex = AccountKey::codex("acct-1", "user-1").unwrap();
    let claude = AccountKey::claude("uuid-1").unwrap();
    let hex = codex.to_string();
    assert_eq!(
        SubKey::workspace(&codex).map(|s| s.as_str().to_owned()),
        hex.strip_prefix("x-").map(str::to_owned)
    );
    assert_eq!(SubKey::workspace(&claude), None);
    let mut before = keyed();
    before.machine = HERE.into();
    let mut after = before.clone();
    after.accounts.get_mut("ann").unwrap().key = Some(codex);
    after.accounts.get_mut("bob").unwrap().key = Some(claude);
    for label in ["ann", "bob"] {
        let account = after.accounts.get_mut(label).unwrap();
        account
            .windows
            .insert(WEEK_LIMIT.into(), fig(300, 90_000, 20));
        account.walls.insert(
            SESSION_LIMIT.into(),
            Wall {
                until: Some(5_000),
                at: 30,
                by: HERE.into(),
                attempts: 0,
                cleared: false,
            },
        );
    }
    let elsewhere = WindowFig {
        by: "cd34".into(),
        ..fig(500, 7_000, 20)
    };
    after
        .accounts
        .get_mut("bob")
        .unwrap()
        .windows
        .insert(SESSION_LIMIT.into(), elsewhere);
    let notes = observe(Cli::Codex, &before, &mut after.clone(), &mut [], 40);
    let ann: Vec<&Note> = notes
        .iter()
        .filter(|n| n.account == Key::new(ANN))
        .collect();
    let bob: Vec<&Note> = notes
        .iter()
        .filter(|n| n.account == Key::new(BOB))
        .collect();
    assert_eq!(ann.len(), 2);
    assert!(
        ann.iter()
            .all(|n| n.sub.map(|s| s.as_str().to_owned())
                == hex.strip_prefix("x-").map(str::to_owned)),
        "{ann:?}"
    );
    assert_eq!(bob.iter().find(|n| n.kind == "quota").unwrap().sub, None);
    assert_eq!(bob.len(), 2, "{bob:?}");
}

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

/// A blocked renewal of a login that still works here keeps its word: the `error` it is kept as
/// says `blocked`, the form the journal sends out in its next pass
/// (`aterm_db::alab::journal::Event::sharp`), so its `ai/logins` row goes with it; one that
/// failed otherwise says `sign-in-failed`, which waits for the routine pass. A displacement of a
/// login by the vendor's own sign-in is told as `displaced`, the store's sign-out, which is sharp
/// too.
/// NEGATIVE CONTROL, run: with `settle` keeping every failed sign-in of a working login as
/// `sign-in-failed` (its `blocked` arm removed), the blocked renewal's `error` says
/// `sign-in-failed`: `` left: [("error", Some("sign-in-failed"), 200)] right: [("error",
/// Some("blocked"), 200)] ``.
#[test]
fn a_blocked_renewal_of_a_working_login_is_kept_as_blocked_and_a_displacement_is_told() {
    let held = said(SignWord::In, 100);
    let blocked = vec![
        Event::sign_in(200, "ann", SignInStep::Failed).because(&renewal(
            crate::harness::login_drive::Outcome::BotCheck(String::new()),
            Some("claude.ai"),
            crate::ai::connector::Flow::Browser,
        )),
    ];
    let (told, cursor) = told_of(Auth::Ok, held, blocked, 200);
    assert_eq!(told, [("error", Some("blocked"), 200)]);
    assert_eq!(cursor, held, "the fleet's word stands");
    let failed =
        vec![Event::sign_in(200, "ann", SignInStep::Failed).because("auto failed: device")];
    let (told, _) = told_of(Auth::Ok, held, failed, 200);
    assert_eq!(told, [("error", Some("sign-in-failed"), 200)]);
    let displaced = vec![Event::new(300, Kind::Displaced, "ann").with_to("bob")];
    let (told, cursor) = told_of(Auth::SignedOut, held, displaced, 300);
    assert_eq!(told, [("displaced", None, 300)]);
    assert_eq!(cursor, said(SignWord::Out, 300));
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
/// moved less), and each wall seen or cleared, once: a standing wall seen again, which moves only
/// its newest sighting ([`Wall::seen()`]), is no new note.
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
        owner: None,
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
            seen: None,
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

    // The same wall seen standing again moves only its newest sighting: no second note.
    let mut again = after.clone();
    let wall = again
        .accounts
        .get_mut("ann")
        .and_then(|ann| ann.walls.get_mut(SESSION_LIMIT))
        .unwrap();
    wall.seen = Some(35);
    let notes = observe(Cli::Claude, &after, &mut again, &mut [], 41);
    assert!(notes.is_empty(), "{notes:?}");
}

/// A wall of a model's own week names its model's bucket (FL-269), and so does its clearing, so
/// the fleet walls that model's sessions alone; a wall of the account's own window names none.
/// A wall of any other `seven_day_*` window (`seven_day_other`, a panel's `seven_day_haiku`),
/// which the harness takes as the account's, names `seven_day_other`, so the fleet takes it as
/// every session's as this Mac does.
/// NEGATIVE CONTROLS: with `observe` noting no model (`note.model = None`), Fable's wall and
/// Opus's clearing name none, and the assertion fails; run: with `model_word` naming only the
/// models' own weeks (`model_week(name)`), the other weeks' walls name none (`` left: [("wall",
/// …, Some("5h"), None), ("wall", …, Some("7d-model"), None), … ``).
#[test]
fn a_model_walls_note_names_its_models_bucket() {
    let before = keyed();
    let mut after = before.clone();
    let wall = |cleared: bool| Wall {
        until: Some(5_000),
        at: 30,
        by: HERE.into(),
        attempts: 0,
        cleared,
        seen: None,
    };
    let ann = after.accounts.get_mut("ann").unwrap();
    for window in [
        "seven_day_overage_included",
        "seven_day_other",
        "seven_day_haiku",
        SESSION_LIMIT,
    ] {
        ann.walls.insert(window.into(), wall(false));
    }
    let bob = after.accounts.get_mut("bob").unwrap();
    bob.walls.insert("seven_day_opus".into(), wall(true));
    let notes = observe(Cli::Claude, &before, &mut after.clone(), &mut [], 40);
    let walls: Vec<(&str, &str, Option<&str>, Option<&str>)> = notes
        .iter()
        .map(|n| {
            (
                n.kind,
                n.account.as_ref().map_or("", Key::as_str),
                n.window,
                n.model,
            )
        })
        .collect();
    assert_eq!(
        walls,
        [
            ("wall", ANN, Some("5h"), None),
            ("wall", ANN, Some("7d-model"), Some("seven_day_other")),
            ("wall", ANN, Some("7d-model"), Some("seven_day_other")),
            (
                "wall",
                ANN,
                Some("7d-model"),
                Some("seven_day_overage_included")
            ),
            (
                "wall-cleared",
                BOB,
                Some("7d-model"),
                Some("seven_day_opus")
            ),
        ]
    );
    for note in &notes {
        assert!(note.model.is_none_or(|m| MODELS.contains(&m)), "{note:?}");
    }
}

/// After an older build's backfill (its marker says `1`), the backfill notes again only each
/// standing wall of a model's week, now with its model (FL-269): that build's notes of them
/// named none, which no reader takes as any model's wall, and the harness would not note an
/// unchanged wall again. It replays no ledger row and notes no reading, no account-wide wall,
/// no lapsed or cleared wall and no login word; once marked, the backfill is done.
/// NEGATIVE CONTROL, run: with `backfilled` taking any marker as done (`.is_some()`), the older
/// build's marker reads as done: `` an older build's marker leaves the model walls due ``.
#[cfg(unix)]
#[test]
fn after_an_older_builds_backfill_each_standing_model_wall_is_noted_again_with_its_model() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let now = 100 * 86_400;
    let store = Store::at_for(&harness, Cli::Claude, &home);
    let mut state = State {
        machine: HERE.into(),
        ..keyed()
    };
    let wall = |until: i64, cleared: bool| Wall {
        until: Some(until),
        at: now - 600,
        by: HERE.into(),
        attempts: 0,
        cleared,
        seen: None,
    };
    let ann = state.accounts.get_mut("ann").unwrap();
    ann.windows
        .insert(SESSION_LIMIT.into(), fig(420, now + 3600, now - 60));
    ann.walls.insert(
        "seven_day_overage_included".into(),
        wall(now + 86_400, false),
    );
    ann.walls
        .insert(SESSION_LIMIT.into(), wall(now + 3600, false));
    ann.walls.insert(
        "seven_day_opus".into(),
        Wall {
            at: now - 3 * 86_400,
            ..wall(now - 2 * 86_400, false)
        },
    );
    ann.auth = Auth::Ok;
    ann.auth_at = Some(now - 300);
    let bob = state.accounts.get_mut("bob").unwrap();
    bob.walls
        .insert("seven_day_other".into(), wall(now + 7200, false));
    bob.walls
        .insert("seven_day_sonnet".into(), wall(now + 7200, true));
    store
        .append(&[Event::new(now - 600, Kind::SignedIn, "ann")])
        .unwrap();
    aterm_uds::private_file::write_private(
        store.state_file().parent().unwrap(),
        "state.json",
        (state.to_json() + "\n").as_bytes(),
    )
    .unwrap();
    let mark_dir = harness.join("alab");
    aterm_uds::private_file::ensure_private_dir(&mark_dir).unwrap();
    aterm_uds::private_file::write_private(&mark_dir, "backfilled", b"1\n").unwrap();
    assert!(
        !backfilled(&harness),
        "an older build's marker leaves the model walls due"
    );
    let notes = backfill(&harness, &home, &|_| None, &keyer, now);
    let noted: Vec<_> = notes
        .iter()
        .map(|n| {
            (
                n.kind,
                n.account.as_ref().map_or("", Key::as_str),
                n.window,
                n.model,
                n.until,
            )
        })
        .collect();
    let until = |t: i64| Some(u64::try_from(t).unwrap());
    assert_eq!(
        noted,
        [
            (
                "wall",
                ANN,
                Some("7d-model"),
                Some("seven_day_overage_included"),
                until(now + 86_400)
            ),
            (
                "wall",
                BOB,
                Some("7d-model"),
                Some("seven_day_other"),
                until(now + 7200)
            ),
        ]
    );
    mark_backfilled(&harness).unwrap();
    assert!(backfilled(&harness));
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

/// A RESET'S NOTE NAMES THE WINDOWS IT REFILLED: a `reset-used` of the week, of a workspace's
/// account under its `sub` as its readings are; one note for each window of a full reset, a
/// model's week under its journal word with its model (FL-271), so a refill of the Sonnet week
/// ends no Fable wall, and one per model; a refill inside the five-hour window is that window's
/// and names no model; a reset spent with the limit still standing (its row names no window) is
/// one note of no window, which no fold cuts by.
/// NEGATIVE CONTROLS: with `refills` returning `of_ledger`'s note as it is, no note names a
/// window or a workspace, and the `shown` assertion fails; run: with `refills` naming no model
/// on its notes (`model: None`), the models' weeks' refills name none (`` left: […, (120,
/// Some("7d-model"), None, false), (120, Some("7d-model"), None, false), … ``).
#[test]
fn a_resets_note_names_the_windows_it_refilled() {
    use crate::harness::profile::AccountKey;
    let codex = AccountKey::codex("acct-1", "user-1").unwrap();
    let workspace = SubKey::workspace(&codex);
    let mut state = keyed();
    state.accounts.get_mut("ann").unwrap().key = Some(codex);
    let mut events = vec![
        Event::new(100, Kind::ResetUsed, "ann").refilling([WEEK_LIMIT]),
        Event::new(110, Kind::ResetUsed, "bob").because("no Fable resets left, still walled"),
        Event::new(120, Kind::ResetUsed, "bob").refilling([
            "seven_day_opus",
            "seven_day_sonnet",
            SESSION_LIMIT,
        ]),
        Event::new(130, Kind::Reset, "bob").refilling([SESSION_LIMIT]),
    ];
    let notes = observe(Cli::Codex, &state, &mut state.clone(), &mut events, 200);
    assert!(notes.iter().all(|n| n.kind == "reset-used"), "{notes:?}");
    let shown: Vec<(u64, Option<&str>, Option<&str>, bool)> = notes
        .iter()
        .map(|n| (n.t, n.window, n.model, n.sub.is_some()))
        .collect();
    assert_eq!(
        shown,
        [
            (100, Some("7d"), None, true),
            (110, None, None, false),
            (120, Some("7d-model"), Some("seven_day_opus"), false),
            (120, Some("7d-model"), Some("seven_day_sonnet"), false),
            (120, Some("5h"), None, false),
            (130, Some("5h"), None, false),
        ]
    );
    assert_eq!(notes[0].sub, workspace, "the workspace's own");
    for note in &notes {
        assert!(note.window.is_none_or(|w| WINDOWS.contains(&w)));
        assert!(note.model.is_none_or(|m| MODELS.contains(&m)), "{note:?}");
    }
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
                seen: None,
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

/// What a login row must never carry, planted in every free-text field of the three stores: a
/// Claude token, a session cookie, an OAuth code, a refresh token, an address and a path. Each
/// is split so no source line reads as a credential.
const PLANTED: [&str; 6] = [
    concat!("sk-", "ant-oat01-", "SENTINELsentinel0123456789"),
    concat!("sessionKey=", "sk-", "ant-sid01-", "cookievalue"),
    "4/0AbCdEfGhIjKlMnOpQrStUvWxYz-oauth-code",
    concat!("refresh_token=", "rt-", "refreshvalue0123"),
    "ada.private@x.example",
    "/Users//someone/.claude-login-ada",
];

/// VALUE-FREE AND KEYED (docs/DESIGN-shared-files-2026-11-11.md §10 file 3): each login row is
/// keyed by its account (the claims email's key, else the state's learned key; a row with
/// neither is left out) and says its store's word, the one the journal is told: a row on its
/// vault token in the upkeep's reading is signed in on the token; a signed-in login stays
/// `signed-in` though a renewal since answered another account, which is only its attempt
/// (`failed`); a signed-out one is `ended` where an end was seen since its renewal; a row with no
/// word on record is left out. Its since is when the journal was last told that word, else its
/// renewal, else when the word was learned. Two rows of one account are one login, the signed-in
/// row's, with the sessions of both. Its proof, place and source are words, its tabs a count,
/// its blocked attempt and when, its route only for a sign-in that found the vault token, and its
/// plan only where the word is one. A token, a cookie, an OAuth code, a refresh token, an address
/// and a path planted in every free-text field of the stores (a login directory, its retained
/// ones, the proved address and directory, the routes line, a subscription, a bot check's
/// address, a session's hold, a label) reach no row.
/// NEGATIVE CONTROLS, each run: with `login_rows` taking the subscription unchecked (its
/// `.and_then(plan_word)` replaced by a `.map` that keeps the text), bob's row carries the
/// planted token: `` bob's subscription is no plan ``; with the first row of an account kept
/// whatever its word (the `rank > slot.0` arm removed), fay's login is her signed-out row: ``
/// fay `` (`left: ("signed-out", 1300, "managed", 3) right: ("signed-in", 1350, "default", 3)`).
#[test]
fn login_rows_are_value_free_keyed_by_account_and_say_the_stores_words() {
    use crate::harness::accounts::{
        AccountRecords, AttemptOutcome, Binding, CheckStop, Logins, SignInAttempt, TokensHere,
    };
    const CAL: &str = "ac-00112233445566aa";
    const DEE: &str = "ac-00112233445566bb";
    const FAY: &str = "ac-00112233445566dd";
    const NOW: i64 = 5_000;
    let [token, cookie, oauth, refresh, address, path] = PLANTED;
    let row = |label: &str, email: &str, dir: Option<&str>| Row {
        label: label.into(),
        email: Some(email.into()),
        dir: dir.map(str::to_owned),
        ..Row::default()
    };
    let roster = Roster {
        rows: vec![
            row("ann", "ann@example.invalid", Some(path)),
            row("bob", "bob@example.invalid", None),
            // Keyed by neither the keyer nor a learned key: never stated, under its label or
            // otherwise.
            row(token, address, Some(path)),
            row("cal", "cal@example.invalid", None),
            row("dee", "dee@example.invalid", None),
            // Keyed, with no word on record: left out.
            row("eve", "eve@example.invalid", None),
            // Two rows of one account: a stale managed login signed out, then the default home
            // signed in, each with sessions.
            row("fay", "fay@example.invalid", Some(path)),
            row("fay2", "fay@example.invalid", None),
        ],
    };
    let mut state = State::default();
    state.contextual_auth = true;
    let account = |auth, auth_at, pk: Option<&str>| Account {
        auth,
        auth_at,
        pk: pk.map(str::to_owned),
        ..Account::default()
    };
    state
        .accounts
        .insert("ann".into(), account(Auth::Ok, Some(1_000), None));
    state
        .accounts
        .insert("bob".into(), account(Auth::SignedOut, Some(1_400), None));
    state.accounts.insert(
        "cal".into(),
        account(Auth::SignedOut, Some(1_600), Some(CAL)),
    );
    state
        .accounts
        .insert("dee".into(), account(Auth::Ok, Some(1_900), Some(DEE)));
    state.accounts.insert(
        "eve".into(),
        account(Auth::Unseen, None, Some("ac-00112233445566cc")),
    );
    state.accounts.insert(
        "fay".into(),
        account(Auth::SignedOut, Some(1_300), Some(FAY)),
    );
    state
        .accounts
        .insert("fay2".into(), account(Auth::Ok, Some(1_350), Some(FAY)));
    // Ann's word was told when she signed in; the store has noted it again since.
    state.told.insert(
        ANN.into(),
        Told {
            word: SignWord::In,
            t: 950,
        },
    );
    for (sid, label) in [
        ("s1", "ann"),
        ("s2", token),
        ("s3", "fay"),
        ("s4", "fay2"),
        ("s5", "fay2"),
    ] {
        state.sessions.insert(
            sid.into(),
            SessionRow {
                account: label.into(),
                at: 1_050,
                held: Some(format!("{cookie} {refresh}")),
                owner: None,
            },
        );
    }
    // The upkeep's reading: cal runs on its vault token here.
    state.tokens_here = Some(TokensHere {
        read_at: NOW - 60,
        ends: [("cal".to_owned(), NOW + 3_600)].into(),
        vouched: false,
    });
    let attempt = |outcome, routes: String, since, tried_at| SignInAttempt {
        outcome,
        routes,
        since,
        tried_at,
        stop: None,
    };
    let mut logins = Logins::default();
    logins.labels.insert(
        "ann".into(),
        Binding {
            dir: Some(path.into()),
            renewed_at: Some(900),
            subscription: Some("max".into()),
            was: vec![Some(format!("{path}-old"))],
            proved_email: Some(address.into()),
            proved_dir: Some(path.into()),
            sign_in_attempt: Some(attempt(
                AttemptOutcome::Blocked,
                format!("browser({address}) https://claude.ai/x?code={oauth} {path} {token}"),
                1_100,
                1_200,
            )),
            check_stop: Some(CheckStop {
                account: Some(address.into()),
                region: None,
                at: 1_150,
            }),
            ..Binding::default()
        },
    );
    logins.labels.insert(
        "bob".into(),
        Binding {
            renewed_at: Some(1_500),
            ended_at: Some(1_800),
            // A subscription that is no plan word is left out.
            subscription: Some(token.into()),
            ..Binding::default()
        },
    );
    logins.labels.insert(
        "cal".into(),
        Binding {
            sign_in_attempt: Some(attempt(
                AttemptOutcome::OnToken,
                format!("vault {refresh}"),
                1_700,
                1_700,
            )),
            ..Binding::default()
        },
    );
    logins.labels.insert(
        "dee".into(),
        Binding {
            renewed_at: Some(2_000),
            wrong_at: Some(2_100),
            ..Binding::default()
        },
    );
    let records = AccountRecords {
        raw_roster: roster.clone(),
        roster,
        roster_present: true,
        state,
        logins,
        notes: Vec::new(),
    };
    let held = |key: &Key| {
        if key.as_str() == ANN {
            "linked"
        } else {
            "fill"
        }
    };
    let rows = login_rows(Cli::Claude, &records, &keyer, &held, &|_, _| None, NOW);
    let keys: Vec<&str> = rows.iter().map(|row| row.account.as_str()).collect();
    assert_eq!(
        keys,
        [ANN, BOB, CAL, DEE, FAY],
        "the unkeyed and the unseen are left out, and fay is one login"
    );

    let ann = &rows[0];
    assert_eq!(
        (
            ann.state, ann.since, ann.place, ann.proof, ann.src, ann.tabs, ann.held
        ),
        (
            "signed-in",
            950,
            "managed",
            "status",
            "profile",
            1,
            "linked"
        ),
        "since the word was told, not when the store last noted it"
    );
    assert_eq!(
        (ann.attempt, ann.attempt_at, ann.attempt_route, ann.plan),
        ("blocked", Some(1_200), None, Some("max"))
    );
    let bob = &rows[1];
    assert_eq!(
        (
            bob.state,
            bob.since,
            bob.ended_at,
            bob.place,
            bob.held,
            bob.plan
        ),
        ("ended", 1_800, Some(1_800), "default", "fill", None),
        "bob's subscription is no plan"
    );
    let cal = &rows[2];
    let on_token = crate::harness::token_route::LAUNCHES_ON_TOKEN;
    assert_eq!(
        (cal.state, cal.since, cal.src),
        if on_token {
            ("signed-in", 1_700, "vault-token")
        } else {
            ("signed-out", 1_600, "profile")
        },
        "on its token where a launch starts on one"
    );
    assert_eq!(
        (cal.attempt, cal.attempt_route, cal.proof),
        ("signed-in", Some("vault"), "unchecked")
    );
    let dee = &rows[3];
    assert_eq!(
        (dee.state, dee.since, dee.attempt, dee.attempt_at),
        ("signed-in", 2_000, "failed", Some(2_100)),
        "a wrong renewal leaves the working login signed in"
    );
    let fay = &rows[4];
    assert_eq!(
        (fay.state, fay.since, fay.place, fay.tabs),
        ("signed-in", 1_350, "default", 3),
        "fay"
    );

    let said = format!("{rows:?}");
    for planted in PLANTED {
        assert!(!said.contains(planted), "a row carries {planted:?}");
    }
    for word in ["@", "/", "=", "sk-"] {
        assert!(!said.contains(word), "a row carries {word:?}: {said}");
    }
    for row in &rows {
        assert_eq!(row.cli, "claude");
        assert!(["managed", "default"].contains(&row.place));
        assert!(["signed-in", "signed-out", "ended", "wrong-account"].contains(&row.state));
        assert!(["status", "page", "unchecked"].contains(&row.proof));
        assert!(["profile", "auth-file", "cli-log", "vault-token"].contains(&row.src));
        assert!(["none", "signed-in", "failed", "blocked", "no-route"].contains(&row.attempt));
        assert!(row.plan.is_none_or(|plan| PLANS.contains(&plan)));
    }
}

/// A signed-out login that a renewal since its last one answered another account is
/// `wrong-account`, and so is a row with no word on record that one did; with no word and no
/// wrong renewal a row is left out.
/// NEGATIVE CONTROL, run: with `login_rows` leaving out every row with no word on record (the
/// `Auth::Unseen` arm always `continue`), the unseen wrong renewal is not stated: `` keys ``
/// (one key on the left, both on the right).
#[test]
fn a_wrong_renewal_of_a_signed_out_login_is_wrong_account() {
    use crate::harness::accounts::{AccountRecords, Binding, Logins};
    let mut state = State::default();
    for (label, auth, pk) in [("ann", Auth::SignedOut, ANN), ("bob", Auth::Unseen, BOB)] {
        state.accounts.insert(
            label.into(),
            Account {
                auth,
                auth_at: Some(100),
                pk: Some(pk.into()),
                ..Account::default()
            },
        );
    }
    let mut logins = Logins::default();
    for label in ["ann", "bob"] {
        logins.labels.insert(
            label.into(),
            Binding {
                renewed_at: Some(150),
                wrong_at: Some(200),
                ..Binding::default()
            },
        );
    }
    let records = AccountRecords {
        raw_roster: roster(),
        roster: roster(),
        roster_present: true,
        state,
        logins,
        notes: Vec::new(),
    };
    let rows = login_rows(
        Cli::Claude,
        &records,
        &keyer,
        &|_| "fill",
        &|_, _| None,
        300,
    );
    let keys: Vec<&str> = rows.iter().map(|row| row.account.as_str()).collect();
    assert_eq!(keys, [ANN, BOB], "keys");
    for row in &rows {
        assert_eq!(
            (row.state, row.since, row.attempt, row.attempt_at),
            ("wrong-account", 200, "failed", Some(200))
        );
    }
}

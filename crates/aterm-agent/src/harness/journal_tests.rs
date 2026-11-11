// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness journal's tests: keying by the claims email, the ledger kind mapping, the
//! state diff's sessions, readings and walls, and the backfill (design §8, step 8).

use super::*;
use crate::harness::accounts::{
    Account, Row, SESSION_LIMIT, SOURCE_TAP, SessionRow, WEEK_LIMIT, Wall,
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
/// a rotation is a switch naming the account rotated to, a failed sign-in row (an automatic one's,
/// a driven renewal's or Grok's, each written by its writer's own function) is `sign-in-failed` with how
/// it ended as one word and never its reason line, one that signed its row in is nothing (its
/// `renewed` row notes it), and so is a step of one (a shared adoption or preparation begun, a
/// person's browser window ending), an auth word is `signed-out` or `auth`, a sample is nothing;
/// no note carries a word that is not one of the module's constants.
/// NEGATIVE CONTROL: with `of_ledger` leaving `to` unset, the switch names no account:
/// `` left: None right: Some(ac-fedcba9876543210) ``.
#[test]
fn observe_keys_ledger_rows_and_maps_their_kinds() {
    let state = keyed();
    let mut events = vec![
        Event::new(100, Kind::Rotate, "ann").with_to("bob"),
        Event::new(101, Kind::SignIn, "ann").because("auto failed: ann@example.invalid https://x"),
        Event::new(102, Kind::Auth, "bob").because("signed-out"),
        Event::new(103, Kind::Sample, "ann"),
        Event::new(104, Kind::Rotate, "unkeyed"),
        Event::new(105, Kind::SignIn, "bob").because("auto signed-in: mailbox"),
        Event::new(106, Kind::SignIn, "bob").because("auto no-route: profile(none)"),
        Event::new(107, Kind::SignIn, "ann").because("a reason no ladder wrote"),
        Event::new(108, Kind::SignIn, "ann")
            .because(crate::harness::accounts::SHARED_ADOPTION_BEGUN),
        Event::new(109, Kind::SignIn, "ann")
            .because(crate::harness::accounts::SHARED_PREPARATION_BEGUN),
        Event::new(110, Kind::SignIn, "bob").because(&crate::harness::browser_window::ledger_why(
            "claude.ai",
            crate::harness::browser_window::WindowEnd::Done,
            "session-kept",
        )),
        Event::new(111, Kind::SignIn, "bob").because(&renewal(
            crate::harness::login_drive::Outcome::BotCheck(String::new()),
            Some("claude.ai"),
            crate::ai::connector::Flow::Browser,
        )),
        Event::new(112, Kind::SignIn, "ann").because(&renewal(
            crate::harness::login_drive::Outcome::Hung,
            None,
            crate::ai::connector::Flow::Device,
        )),
        Event::new(113, Kind::SignIn, "bob").because(&grok(Some("accounts.x.ai"), "signed-in")),
        Event::new(114, Kind::SignIn, "bob")
            .because(&grok(Some("accounts.x.ai"), "signed-in-elsewhere")),
        Event::new(115, Kind::SignIn, "ann").because(&grok(None, "stopped")),
        Event::new(116, Kind::SignIn, "ann").because(&grok(Some("accounts.x.ai"), "bot-check")),
        // A page host that is the automatic form's word is still read as a renewal's.
        Event::new(117, Kind::SignIn, "ann").because(&renewal(
            crate::harness::login_drive::Outcome::OffPin,
            Some("auto"),
            crate::ai::connector::Flow::Browser,
        )),
    ];
    let notes = observe(Cli::Claude, &state, &state, &mut events, 200);
    assert_eq!(events[0].pk.as_deref(), Some(ANN));
    assert_eq!(events[2].pk.as_deref(), Some(BOB));
    assert_eq!(events[4].pk, None, "an unkeyed account is not noted");
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
    let notes = observe(Cli::Claude, &before, &after, &mut [], 40);
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
        observe(Cli::Claude, &keyed, &keyed, &mut claims, 400).is_empty(),
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
    let notes = observe(Cli::Codex, &before, &after, &mut [], 40);
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

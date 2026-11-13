// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's vault token facts (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9, §5.10):
//! `ctl status`'s fields, the Account card's line and its one step.

use super::*;
use crate::autopilot_menu::{MenuAct, MenuRow};
use crate::claude_lights::Fact;
use crate::fact_menu::FactAct;
use aterm_agent::harness::accounts::AuthRoute;
use aterm_agent::harness::footer::FooterFacts;

const KEY: &str = "ac-fedcba9876543210";

fn fact(kind: TokenState, offer: Option<bool>) -> TokenFact {
    let state = kind.word();
    TokenFact {
        key: KEY.into(),
        phrase: offer.map(|_| format!("token: {state}")),
        offer: offer.map(|replace| TokenOffer {
            line: "offer".into(),
            label: "work".into(),
            replace,
        }),
        ..TokenFact::for_test(Some("work"), "person@example.com", kind)
    }
}

fn field(record: &str, key: &str) -> Option<String> {
    record
        .split(' ')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_owned)
}

#[test]
fn status_fields_name_the_route_and_the_accounts_state() {
    let read = Tokens::read(vec![fact(TokenState::Soon, Some(true))]);
    assert_eq!(
        status_fields(None, (None, None), Some(&read)),
        "auth=- token=-"
    );
    let stored = AuthRoute::Stored;
    assert_eq!(
        status_fields(
            Some(&stored),
            (None, Some("person@example.com")),
            Some(&read)
        ),
        "auth=stored token=soon"
    );
    assert_eq!(
        status_fields(Some(&stored), (Some("other"), None), Some(&read)),
        "auth=stored token=-"
    );
    assert_eq!(
        status_fields(Some(&stored), (Some("work"), None), None),
        "auth=stored token=-",
        "before the first read"
    );
}

/// The real `status` record of a Claude Code tab carries `auth=` and `token=`.
#[test]
fn the_status_record_carries_auth_and_token() {
    let app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    let session = app.focused_session_id(wid).expect("a session");
    let before = app.session_status_record(session).expect("a record");
    assert_eq!(field(&before, "auth").as_deref(), Some("-"));
    assert_eq!(field(&before, "token").as_deref(), Some("-"));
    publish(&app, session);
    show(Some(Tokens::read(vec![fact(
        TokenState::None,
        Some(false),
    )])));
    let record = app.session_status_record(session).expect("a record");
    assert_eq!(
        field(&record, "auth").as_deref(),
        Some("stored"),
        "{record}"
    );
    assert_eq!(field(&record, "token").as_deref(), Some("none"), "{record}");
    show(Some(Tokens::read(Vec::new())));
    let record = app.session_status_record(session).expect("a record");
    assert_eq!(field(&record, "token").as_deref(), Some("-"), "{record}");
    show(None);
}

fn publish(app: &crate::App, session: u64) {
    let entry = app.pool.get(session).expect("pooled");
    let mut timeline = entry.ctx.timeline.lock().expect("timeline");
    timeline.note_foreground_group(4242);
    timeline.set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
    timeline.set_claude_footer(
        4242,
        Some(1),
        Some(FooterFacts {
            account: Some("work".into()),
            route: Some(AuthRoute::Stored),
            ..Default::default()
        }),
    );
}

fn token_rows(app: &crate::App) -> Vec<MenuRow> {
    app.autopilot_menu
        .as_ref()
        .expect("a menu is open")
        .rows
        .iter()
        .filter(|row| match row {
            MenuRow::Item { act, .. } => *act == MenuAct::Fact(Fact::Account, FactAct::Token),
            MenuRow::Fact { label, .. } => label == "Token",
            _ => false,
        })
        .cloned()
        .collect()
}

/// The Account card answers for the account's token first and, while an offer stands, has its
/// one step, whose press opens a tab that runs the verb; with no offer it says the state
/// alone; a read that holds no word of the account says it was not checked, never none.
#[test]
fn the_accounts_cards_press_opens_a_tab_that_runs_the_step() {
    STEPS_ASKED.with(|asked| asked.borrow_mut().clear());
    let mut app = crate::App::headless_for_test();
    app.hold_account_menu_reads_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    let session = app.focused_session_id(wid).expect("a session");
    publish(&app, session);

    show(Some(Tokens::read(vec![fact(TokenState::Soon, Some(true))])));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    let rows = token_rows(&app);
    assert_eq!(rows.len(), 2, "the state and the step: {rows:?}");
    let said = |rows: &[MenuRow]| match rows.first() {
        Some(MenuRow::Fact { value, .. }) => value.clone(),
        other => panic!("the Token answer leads: {other:?}"),
    };
    assert_eq!(said(&rows), "in the vault \u{b7} ends soon");
    assert!(
        matches!(&rows[1], MenuRow::Item { label, .. } if label == "Replace the vault token"),
        "{rows:?}"
    );
    let at = app
        .autopilot_menu
        .as_ref()
        .expect("a menu is open")
        .rows
        .iter()
        .position(|row| {
            matches!(row, MenuRow::Item { act, .. } if *act == MenuAct::Fact(Fact::Account, FactAct::Token))
        })
        .expect("the step's row");
    app.choose_autopilot_menu_row(at);
    assert_eq!(
        STEPS_ASKED.with(|asked| asked.borrow().clone()),
        [["harness", "accounts", "token", "work", "--replace"]]
    );

    app.autopilot_menu = None;
    show(Some(Tokens::read(vec![fact(TokenState::Active, None)])));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    let rows = token_rows(&app);
    assert_eq!(rows.len(), 1, "the state alone: {rows:?}");
    assert_eq!(said(&rows), "in the vault");

    app.autopilot_menu = None;
    show(Some(Tokens::read(Vec::new())));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    let rows = token_rows(&app);
    assert_eq!(
        rows.len(),
        1,
        "no step for an account the read does not cover"
    );
    assert_eq!(said(&rows), "not checked on this Mac");

    app.autopilot_menu = None;
    show(Some(Tokens::no_vault()));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    assert_eq!(said(&token_rows(&app)), "no vault on this Mac");

    app.autopilot_menu = None;
    show(None);
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    assert_eq!(said(&token_rows(&app)), "not read yet");
    show(None);
}

/// A view becomes the window's facts with its offer's step, and the read records the pair once.
#[cfg(unix)]
#[test]
fn a_view_becomes_facts_and_records_its_offer_once() {
    use aterm_agent::harness::accounts_step::Step;
    use aterm_agent::harness::accounts_token_view::{AccountToken, Offer};
    let dir = crate::test_scratch::short_dir("token-shown");
    let account = AccountToken {
        key: KEY.into(),
        email: "person@example.com".into(),
        label: Some("work".into()),
        state: TokenState::None,
        standing: None,
        row: None,
        pending: Some(false),
        logged_in: true,
        login: LoginHere::Half(LoginHalf::Ok),
        offer: Some(Offer {
            pair: "none:-/ok:-".into(),
            step: Step::Token {
                label: "work".into(),
                replace: false,
            },
            since: None,
        }),
    };
    assert_eq!(
        facts_of_view(dir.path(), None, 100),
        Tokens::no_vault(),
        "no view is no vault, never a read that found no token"
    );
    assert_eq!(
        facts_of_view(dir.path(), Some(Vec::new()), 100),
        Tokens::read(Vec::new()),
        "a vault that covers no account was still read"
    );
    let read = facts_of_view(dir.path(), Some(vec![account.clone()]), 100);
    assert_eq!(read.read, TokensRead::Read);
    let facts = read.facts;
    let [shown] = facts.as_slice() else {
        panic!("one account: {facts:?}");
    };
    assert_eq!(shown.state, "none");
    assert_eq!(shown.kind, TokenState::None);
    assert_eq!(shown.login, LoginHere::Half(LoginHalf::Ok));
    assert!(shown.logged_in && !shown.not_yet);
    assert_eq!(
        (shown.made_at, shown.ends_at, shown.made_on.as_deref()),
        (None, None, None),
        "no entry has no dates and no maker"
    );
    let offer = shown.offer.as_ref().expect("the offer stands");
    assert_eq!(offer.words(), ["harness", "accounts", "token", "work"]);
    assert!(
        shown.phrase.is_some(),
        "the state line's phrase stands with it"
    );
    let store = aterm_agent::harness::accounts::Store::at(dir.path());
    let table = aterm_agent::harness::accounts_tokens::read_table(&store).expect("recorded");
    let recorded = table.offers.get(KEY).expect("the pair is recorded");
    assert_eq!((recorded.pair.as_str(), recorded.at), ("none:-/ok:-", 100));
    let _ = facts_of_view(dir.path(), Some(vec![account]), 200);
    let again = aterm_agent::harness::accounts_tokens::read_table(&store).expect("kept");
    assert_eq!(
        again.offers.get(KEY).map(|o| o.at),
        Some(100),
        "a re-read records nothing"
    );
}

/// A fact keeps what the view typed of a token this Mac's table holds: its dates, the Mac that
/// made it, its verification, the peers of the last fleet read, the login half and the vault
/// column, so a surface words them itself.
#[cfg(unix)]
#[test]
fn a_fact_keeps_the_views_typed_fields() {
    use aterm_agent::harness::accounts_token_view::AccountToken;
    use aterm_agent::harness::accounts_tokens::{InVault, TokenRow, UsedOn, Verification};
    let dir = crate::test_scratch::short_dir("token-typed");
    let account = AccountToken {
        key: KEY.into(),
        email: "person@example.com".into(),
        label: Some("work".into()),
        state: TokenState::Soon,
        standing: None,
        row: Some(TokenRow {
            key: KEY.into(),
            email: "person@example.com".into(),
            account_id: None,
            label: Some("work".into()),
            entry: String::new(),
            minted_at: Some(1_000),
            expires_at: Some(9_000),
            expiry_from: None,
            minted_on: Some("m15".into()),
            signer: None,
            verified: Some(1_500),
            verification: Verification::default(),
            vault: InVault::SignedHere,
            used_on: UsedOn {
                peers: vec!["m22".into()],
                peers_read_at: Some(2_000),
                ..UsedOn::default()
            },
            rejected_at: None,
            removed_at: None,
        }),
        pending: Some(false),
        logged_in: false,
        login: LoginHere::Half(LoginHalf::Ending { end: 7_000 }),
        offer: None,
    };
    let read = facts_of_view(dir.path(), Some(vec![account.clone()]), 100);
    let [shown] = read.facts.as_slice() else {
        panic!("one account: {read:?}");
    };
    assert_eq!(shown.kind, TokenState::Soon);
    assert_eq!((shown.made_at, shown.ends_at), (Some(1_000), Some(9_000)));
    assert_eq!(shown.made_on.as_deref(), Some("m15"));
    assert_eq!(shown.verified_at, Some(1_500));
    assert!(shown.not_yet, "signed here, and no server took it yet");
    assert_eq!(
        shown.login,
        LoginHere::Half(LoginHalf::Ending { end: 7_000 })
    );
    assert!(!shown.logged_in);
    assert_eq!(shown.vault_words, account.vault_words());
    assert_eq!(shown.peers, ["m22"]);
    assert_eq!(shown.peers_read_at, Some(2_000));
}

#[test]
fn a_read_is_due_once_a_minute() {
    assert!(due(None, 100));
    assert!(!due(Some(100), 100 + READ_EVERY_S - 1));
    assert!(due(Some(100), 100 + READ_EVERY_S));
    assert!(due(Some(100), 99), "a clock that stepped back");
}

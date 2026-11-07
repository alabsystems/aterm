// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's vault token facts (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9, §5.10):
//! `ctl status`'s fields, the Accounts card's line and its one step.

use super::*;
use crate::autopilot_menu::{MenuAct, MenuRow};
use crate::claude_lights::Fact;
use crate::fact_menu::FactAct;
use aterm_agent::harness::accounts::AuthRoute;
use aterm_agent::harness::footer::FooterFacts;

const KEY: &str = "ac-fedcba9876543210";

fn fact(state: &str, offer: Option<bool>) -> TokenFact {
    TokenFact {
        key: KEY.into(),
        label: Some("work".into()),
        email: "person@example.com".into(),
        state: state.into(),
        words: state.into(),
        phrase: offer.map(|_| format!("token: {state}")),
        offer: offer.map(|replace| TokenOffer {
            line: "offer".into(),
            label: "work".into(),
            replace,
        }),
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
    let read = Tokens(vec![fact("soon", Some(true))]);
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
    show(Some(Tokens(vec![fact("none", Some(false))])));
    let record = app.session_status_record(session).expect("a record");
    assert_eq!(
        field(&record, "auth").as_deref(),
        Some("stored"),
        "{record}"
    );
    assert_eq!(field(&record, "token").as_deref(), Some("none"), "{record}");
    show(Some(Tokens::default()));
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
            MenuRow::Note { text, .. } => text.starts_with("vault token"),
            _ => false,
        })
        .cloned()
        .collect()
}

/// The Accounts card shows the account's token and, while an offer stands, its one step, whose
/// press opens a tab that runs the verb; with no offer it shows the state alone.
#[test]
fn the_accounts_cards_press_opens_a_tab_that_runs_the_step() {
    STEPS_ASKED.with(|asked| asked.borrow_mut().clear());
    let mut app = crate::App::headless_for_test();
    app.hold_account_menu_reads_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    let session = app.focused_session_id(wid).expect("a session");
    publish(&app, session);

    show(Some(Tokens(vec![fact("soon", Some(true))])));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    let rows = token_rows(&app);
    assert_eq!(rows.len(), 2, "the state and the step: {rows:?}");
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
    show(Some(Tokens(vec![fact("active", None)])));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    assert_eq!(token_rows(&app).len(), 1, "the state alone");

    app.autopilot_menu = None;
    show(Some(Tokens::default()));
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
    assert!(
        token_rows(&app).is_empty(),
        "no fact of this account, no row"
    );
    show(None);
}

/// A view becomes the window's facts with its offer's step, and the read records the pair once.
#[cfg(unix)]
#[test]
fn a_view_becomes_facts_and_records_its_offer_once() {
    use aterm_agent::harness::accounts_step::Step;
    use aterm_agent::harness::accounts_token_view::{AccountToken, LoginHalf, Offer};
    use aterm_agent::harness::accounts_tokens::TokenState;
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
        login: LoginHalf::Ok,
        offer: Some(Offer {
            pair: "none:-/ok:-".into(),
            step: Step::Token {
                label: "work".into(),
                replace: false,
            },
            since: None,
        }),
    };
    assert_eq!(facts_of_view(dir.path(), None, 100), Tokens::default());
    let Tokens(facts) = facts_of_view(dir.path(), Some(vec![account.clone()]), 100);
    let [shown] = facts.as_slice() else {
        panic!("one account: {facts:?}");
    };
    assert_eq!(shown.state, "none");
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

#[test]
fn a_read_is_due_once_a_minute() {
    assert!(due(None, 100));
    assert!(!due(Some(100), 100 + READ_EVERY_S - 1));
    assert!(due(Some(100), 100 + READ_EVERY_S));
    assert!(due(Some(100), 99), "a clock that stepped back");
}

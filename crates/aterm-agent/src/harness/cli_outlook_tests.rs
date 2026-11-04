// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The report's outlook (`aterm harness accounts`, `docs/DESIGN-accounts-outlook-2026-10-25.md`
//! §9): the text and the JSON say the card's numbers for the same seed, and the report writes
//! nothing.

use super::*;
use crate::harness::account_series::{self, Fig};
use crate::harness::outlook::{self, CardRow};

const H: i64 = 3600;
const D: i64 = 86_400;

/// A synthetic pool on `env`: `me` (in use, 58% of its week, read now, a day of series behind
/// it), `alt1` (31%, read three hours ago) and `pool1` (never read).
fn seeded(tmp: &Tmp, env: &Env) {
    let mut text = String::new();
    for (label, class) in [
        ("me", Class::Preferred),
        ("alt1", Class::Preferred),
        ("pool1", Class::Pool),
    ] {
        let dir = tmp.path().join("logins").join(label);
        login_made_in(&dir);
        text = accounts::append_row(&text, label, class, dir.to_str()).expect("the row admits");
    }
    std::fs::write(env.accounts_roster().expect("a config dir"), text).expect("write");
    let now = env.now;
    let week = |label: &str| match label {
        "me" => now + 3 * D + 13 * H,
        _ => now + 5 * D,
    };
    let read = |label: &str, pct_x10: u32, at: i64| accounts::TapRead {
        five_hour: (label == "me").then_some(accounts::WindowRead {
            pct_x10: 380,
            reset: now + 2 * H,
            refilled: None,
        }),
        seven_day: Some(accounts::WindowRead {
            pct_x10,
            reset: week(label),
            refilled: None,
        }),
        at,
        seen_before: true,
        born: None,
        key: None,
    };
    let mut note = None;
    env.accounts_store()
        .transact(now, &mut note, |state| {
            state.active = Some("me".into());
            let (mut events, _) =
                accounts::note_reading_held(state, "me", &read("me", 580, now), now);
            events.extend(
                accounts::note_reading_held(state, "alt1", &read("alt1", 310, now - 3 * H), now).0,
            );
            Ok(events)
        })
        .expect("seeded");
    let machine = env.accounts_store().load(now).expect("load").0.machine;
    for (i, pct_x10) in [(24, 420), (16, 470), (8, 520), (0, 580)] {
        let at = now - i * H;
        account_series::note_within(
            &env.state,
            "me",
            &machine,
            (
                None,
                Some(Fig {
                    pct_x10,
                    reset: week("me"),
                    at,
                    src: accounts::SOURCE_TAP.into(),
                    epoch: None,
                }),
            ),
            at,
            std::time::Duration::from_secs(60),
        )
        .expect("series");
    }
}

/// The card's outlook for `env`'s store, as the window's memo builds it
/// (`aterm-gui/src/accounts_outlook.rs`): the bound roster, the store's state, the series and
/// the renewals, the policy's switches.
fn card_outlook(env: &Env) -> outlook::Outlook {
    let store = env.accounts_store();
    let roster =
        accounts::Roster::load(&env.accounts_roster().expect("a roster path")).expect("the roster");
    let roster = store.bound(&roster, env.now).expect("bound");
    let (state, _) = store.load(env.now).expect("load");
    let (logins, _) = store.load_logins(env.now).expect("logins");
    let enrolled = accounts::enrolled(&roster);
    let series = outlook::series_of(&env.state, &roster, &state.machine);
    let policy = SupervisorConfig::from_path(env.config.as_deref()).0;
    outlook::outlook(&outlook::Inputs {
        roster: &roster,
        state: &state,
        enrolled: &enrolled,
        series: &series,
        renewals: &outlook::renewals_of(&roster, &logins),
        mode: outlook::replay_mode(&policy),
        credits: policy.usage_credits,
        now: env.now,
        offset: |_| 0,
    })
}

/// ACCEPTANCE 3 (and T14's report half): THE REPORT SAYS THE CARD'S NUMBERS. For one seed, the
/// `--json` report's `outlook` is the card's model rendered, its text carries the card's
/// outlook block and, under each row, the card's figure for that row; neither report writes a
/// file. Negative control: a report whose outlook is built without the series refuses the pace
/// the card projects, and its JSON is not the card's.
#[test]
fn the_report_says_the_cards_numbers_and_writes_nothing() {
    let tmp = tmp_dir("accounts-outlook");
    let env = env_at(&tmp);
    seeded(&tmp, &env);
    let before = every_file(tmp.path());
    let card = card_outlook(&env);
    assert!(
        card.demand.week.is_ok(),
        "the seed projects: {:?}",
        card.demand.week
    );

    let (_, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    let json: Value = aterm_json::from_str(json.trim()).expect("the report is JSON");
    assert_eq!(
        json.get("outlook"),
        Some(&card.json()),
        "the report's outlook is the card's model"
    );

    let (_, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert!(
        text.contains(card.text().trim_end()),
        "the text's outlook block is the card's:\n{text}"
    );
    let mut figures = 0;
    for row in card.card_rows(Some("me"), 30, None) {
        if let CardRow::Account {
            label, text: item, ..
        } = row
            && let Some(pct) = item
                .split(" \u{b7} ")
                .find(|part| part.ends_with("% left"))
                .map(|part| part.trim_end_matches(" left"))
        {
            // The row's own line, then the lines indented under it.
            let lines: Vec<&str> = text.lines().collect();
            let at = lines
                .iter()
                .position(|l| l.split_whitespace().next() == Some(label.as_str()))
                .unwrap_or_else(|| panic!("{label}: its row in\n{text}"));
            let block = std::iter::once(lines[at])
                .chain(
                    lines[at + 1..]
                        .iter()
                        .copied()
                        .take_while(|l| l.starts_with(' ')),
                )
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                block.contains(pct),
                "{label}: the card's {pct} in the report's lines:\n{block}\n---\n{text}"
            );
            figures += 1;
        }
    }
    assert!(figures >= 2, "the card shows figures for me and alt1");
    assert_eq!(before, every_file(tmp.path()), "the reports wrote nothing");
}

#[test]
fn the_report_does_not_forecast_service_after_a_signed_out_accounts_reset() {
    let tmp = tmp_dir("accounts-outlook-signed-out");
    let env = env_at(&tmp);
    seeded(&tmp, &env);
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            let mut events = accounts::note_wall(
                state,
                "me",
                accounts::WEEK_LIMIT,
                Some(env.now + 3 * D),
                env.now,
                env.now,
            );
            for label in ["me", "alt1", "pool1"] {
                events.extend(accounts::note_auth(
                    state,
                    label,
                    accounts::Auth::SignedOut,
                    env.now,
                ));
            }
            state.proven = false;
            Ok(events)
        })
        .expect("signed-out state");
    let before = every_file(tmp.path());
    let (code, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
    let json: Value = aterm_json::from_str(json.trim()).unwrap();
    assert_eq!(
        json["outlook"]["verdict"]["why"].as_str(),
        Some("account-unavailable")
    );
    assert!(json["outlook"]["passes"].as_array().unwrap().is_empty());
    let model = card_outlook(&env);
    assert_eq!(json.get("outlook"), Some(&model.json()));
    let (code, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
    assert!(text.contains(model.text().trim_end()));
    assert!(text.contains(&outlook::Refusal::AccountUnavailable.words()));
    assert_eq!(every_file(tmp.path()), before);
}

/// THE REPORT REPLAYS UNDER THE MODE THE WINDOW DOES: with the harness off (`[harness] enabled =
/// false`), or off for Claude Code (`[harness.claude] enabled = false`), nothing rotates Claude
/// Code's accounts, so the report's outlook says `rotation off` whatever `[harness] accounts`
/// says, as the window's card does. Control: the harness on, the same seed says `rotation
/// early`. Negative control: the report reading `[harness] accounts` alone says `early` with the
/// harness off.
#[test]
fn the_report_replays_under_the_windows_mode() {
    for (toml, mode) in [
        ("[harness]\naccounts = \"early\"\n", "early"),
        ("[harness]\nenabled = false\naccounts = \"early\"\n", "off"),
        (
            "[harness]\naccounts = \"early\"\n[harness.claude]\nenabled = false\n",
            "off",
        ),
    ] {
        let tmp = tmp_dir("accounts-outlook-mode");
        let env = env_at(&tmp);
        std::fs::write(env.config.as_ref().expect("a config path"), toml).expect("write");
        seeded(&tmp, &env);
        let (_, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
        assert!(
            text.contains(&format!("\u{b7} rotation {mode} \u{b7}")),
            "{toml}: rotation {mode}:\n{text}"
        );
        assert_eq!(card_outlook(&env).mode.as_str(), mode, "{toml}: the card's");
    }
}

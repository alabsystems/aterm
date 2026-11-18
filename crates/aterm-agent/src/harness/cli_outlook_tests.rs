// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The report's outlook (`aterm harness accounts`, `docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §9): the text and the JSON say the card's numbers for the same seed, and the report writes
//! nothing.

use super::*;
use crate::harness::account_series::{self, Fig};
use crate::harness::outlook;

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
            let (mut events, _) = accounts::note_reading_held(
                state,
                "me",
                &read("me", 580, now),
                now,
                accounts::ReadingOwner::Unobserved,
            );
            events.extend(
                accounts::note_reading_held(
                    state,
                    "alt1",
                    &read("alt1", 310, now - 3 * H),
                    now,
                    accounts::ReadingOwner::Unobserved,
                )
                .0,
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
            (None, None),
            at,
            std::time::Duration::from_secs(60),
        )
        .expect("series");
    }
}

/// Every stop's end in an outlook's JSON, its lean branches' too, as `(kind, at)`: the report
/// writes `"end": {"kind": "reset"|"about"|"open", "at": t|null}`, `at` null only for an open
/// stop, and no `to` or `at_reset`.
fn stop_ends(outlook: &Value) -> Vec<(String, Option<i64>)> {
    let mut out = Vec::new();
    let mut passes: Vec<&Value> = outlook["passes"].as_array().into_iter().flatten().collect();
    while let Some(pass) = passes.pop() {
        for stop in pass["stops"].as_array().into_iter().flatten() {
            assert!(
                stop.get("to").is_none() && stop.get("at_reset").is_none(),
                "{stop:?}"
            );
            let end = &stop["end"];
            let kind = end["kind"].as_str().expect("an end's kind").to_owned();
            assert!(
                ["reset", "about", "open"].contains(&kind.as_str()),
                "{kind}"
            );
            let at = end["at"].as_i64();
            assert_eq!(at.is_none(), kind == "open", "{end:?}");
            out.push((kind, at));
        }
        if let Some(lean) = pass.get("lean").filter(|l| !l.is_null()) {
            passes.push(&lean["worst"]);
            passes.push(&lean["best"]);
        }
    }
    out
}

/// The card's outlook for `env`'s store, as the window's memo builds it
/// (`aterm-gui/src/accounts_outlook.rs`): the bound roster, the store's state, the series and
/// the renewals, the policy's switches; its rows named as the report names them.
fn card_outlook(env: &Env) -> outlook::Outlook {
    let store = env.accounts_store();
    let roster =
        accounts::Roster::load(&env.accounts_roster().expect("a roster path")).expect("the roster");
    let roster = store.bound(&roster, env.now).expect("bound");
    let (state, _) = store.load(env.now).expect("load");
    let (logins, _) = store.load_logins(env.now).expect("logins");
    let enrolled = accounts::enrolled(&roster);
    let series = outlook::series_of(&env.state, &roster, &state.machine, env.now);
    let policy = SupervisorConfig::from_path(env.config.as_deref()).0;
    outlook::outlook(&outlook::Inputs {
        roster: &roster,
        state: &state,
        enrolled: &enrolled,
        series: &series,
        renewals: &outlook::renewals_of(&roster, &logins),
        lab: &BTreeMap::new(),
        mode: outlook::replay_mode(&policy),
        credits: policy.usage_credits,
        now: env.now,
        offset: |_| 0,
        signing_in: None,
    })
    .named(crate::harness::account_names::Listing::here(
        Cli::Claude,
        &roster.rows,
    ))
}

/// The row `label` of `env`'s roster as the report's JSON names it: its account's key, else its
/// label.
fn id_of(env: &Env, label: &str) -> String {
    let roster =
        accounts::Roster::load(&env.accounts_roster().expect("a roster path")).expect("the roster");
    crate::harness::account_names::Listing::of(Cli::Claude, &roster.rows, None)
        .id(label)
        .to_owned()
}

/// ACCEPTANCE 3 (and T14's report half): THE REPORT SAYS THE CARD'S NUMBERS. For one seed, the
/// `--json` report's `outlook` is the card's model rendered (each stop's `end` a kind and an
/// instant, the lean's worst case stopping to `me`'s reset), its text carries the card's
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
    assert!(
        stop_ends(&json["outlook"]).contains(&("reset".into(), Some(env.now + 3 * D + 13 * H))),
        "a stop to me's reset: {:?}",
        json["outlook"]
    );
    assert!(json["outlook"]["verdict"].get("end").is_some());

    let (_, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert!(
        text.contains(card.text().trim_end()),
        "the text's outlook block is the card's:\n{text}"
    );
    let mut figures = 0;
    for row in &card.rows {
        // The row's figure as the glass says it: `42% left`.
        let label = &row.label;
        let pct = row.figure(card.as_of).words();
        if pct.ends_with("% left") {
            // The row's own line, by the account the report names it by, then the lines
            // indented under it.
            let shown = format!("{} ", card.names.shown(label));
            let lines: Vec<&str> = text.lines().collect();
            let at = lines
                .iter()
                .position(|l| l.starts_with(&shown))
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
                block.contains(&pct),
                "{label}: the card's {pct} in the report's lines:\n{block}\n---\n{text}"
            );
            figures += 1;
        }
    }
    assert!(figures >= 2, "the card shows figures for me and alt1");
    assert_eq!(before, every_file(tmp.path()), "the reports wrote nothing");
}

/// THE OUTLOOK NAMES EACH ACCOUNT BY ITS NAME: over the seeded pool with each row given an
/// address and the carrier (`me`) a plan that names its multiple (Max 20x), so the rows whose plan
/// was not seen are counted as the carrier's (`Approx::PlanAssumed`), the whole report (the
/// outlook block, its verdict, chain, lean and approximations, and the outlook's lines under each
/// row, `counted as Max 20x like <carrier>`) names each row by its account's name (`claude
/// me@x.example`) and never by its label, and its JSON (the outlook's version, 2; the carrier,
/// the verdict's row, each link of a chain, the lean, each approximation's account and carrier,
/// and `not_needed`) by its account's key. CONTROL: the outlook is projected, its chain naming
/// `me` and the next row, and it counts a row's plan as the carrier's, in its words and its JSON.
/// NEGATIVE CONTROL: with `row_lines` naming the carrier by its label, the plan-assumed words
/// naming both rows by label and the approximation's JSON `account` a label, it fails at its
/// control: `CONTROL: a row's plan is counted as the carrier's, named by its account: …`, the
/// report reading `plan  ? (not seen; counted as Max 20x like me)` and the approximation `alt1:
/// plan not seen, counted as Max 20x like me` (2026-11-16, by hand on SF-07H S7a's review
/// settle, then restored).
#[test]
fn the_outlook_names_each_account_by_its_name() {
    let tmp = tmp_dir("accounts-outlook-names");
    let env = env_at(&tmp);
    seeded(&tmp, &env);
    let labels = ["me", "alt1", "pool1"];
    let addressed = labels.map(|label| (label, format!("{label}@x.example")));
    address_rows(
        &env,
        &addressed
            .iter()
            .map(|(label, email)| (*label, email.as_str()))
            .collect::<Vec<_>>(),
    );
    let machine = env.accounts_store().load(env.now).expect("load").0.machine;
    account_series::note_plan(
        &env.state,
        "me",
        &machine,
        &account_series::PlanSaid {
            word: "max".into(),
            tier: Some("20x".into()),
            src: "profile".into(),
        },
        env.now - H,
    )
    .expect("the carrier's plan");
    let (_, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("outlook (as of "))
        .unwrap_or_else(|| panic!("an outlook block in\n{text}"));
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
        block.contains("claude me@x.example ") && block.contains("\u{2192}"),
        "CONTROL: a chain names me: {block}"
    );
    assert!(
        text.contains("like claude me@x.example"),
        "CONTROL: a row's plan is counted as the carrier's, named by its account: {text}"
    );
    // With every account's name taken out, no word of the report is a label.
    let unnamed = addressed.iter().fold(text.clone(), |text, (_, email)| {
        text.replace(&format!("claude {email}"), "")
    });
    let printed: Vec<&str> = unnamed
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| labels.contains(word))
        .collect();
    assert!(
        printed.is_empty(),
        "labels {printed:?} in the report:\n{text}"
    );

    let (_, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    let json: Value = aterm_json::from_str(json.trim()).expect("the report is JSON");
    let outlook = &json["outlook"];
    assert_eq!(outlook["v"], Value::from(2u64), "{outlook}");
    let keys: Vec<Value> = labels
        .iter()
        .map(|label| Value::from(id_of(&env, label)))
        .collect();
    assert!(
        keys.iter()
            .all(|key| key.as_str().is_some_and(|key| key.starts_with("ac-"))),
        "{keys:?}"
    );
    assert_eq!(outlook["carrier"], keys[0], "{outlook}");
    let assumed: Vec<&Value> = outlook["approx"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|approx| approx["kind"] == Value::from("plan-assumed"))
        .collect();
    assert!(
        assumed
            .iter()
            .any(|approx| approx["carrier"] == keys[0] && keys[1..].contains(&approx["account"])),
        "CONTROL: a plan-assumed approximation of another row like the carrier: {outlook}"
    );
    let mut named = vec![&outlook["verdict"]["row"]];
    let mut passes: Vec<&Value> = outlook["passes"].as_array().into_iter().flatten().collect();
    while let Some(pass) = passes.pop() {
        named.extend(
            pass["chain"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|link| &link["account"]),
        );
        if let Some(lean) = pass.get("lean").filter(|l| !l.is_null()) {
            named.push(&lean["row"]);
            passes.push(&lean["worst"]);
            passes.push(&lean["best"]);
        }
    }
    for approx in outlook["approx"].as_array().into_iter().flatten() {
        named.extend([&approx["carrier"], &approx["account"]]);
    }
    named.extend(outlook["not_needed"].as_array().into_iter().flatten());
    let named: Vec<&Value> = named.into_iter().filter(|v| !v.is_null()).collect();
    assert!(named.len() >= 2, "CONTROL: a chain names rows: {outlook}");
    for v in named {
        assert!(
            keys.contains(v),
            "{v} names no account by its key: {outlook}"
        );
    }
    for key in outlook["lab"]
        .as_object()
        .into_iter()
        .flat_map(|lab| lab.keys())
    {
        assert!(
            keys.contains(&Value::from(key.clone())),
            "the lab's bounds of {key}, no account's key: {outlook}"
        );
    }
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
    assert!(
        json["outlook"]["verdict"]["end"].is_null(),
        "a refusal has no end"
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
/// says, as the window's card does, and its JSON's stop (`me` alone walls) ends at `me`'s reset.
/// Control: the harness on, the same seed says `rotation early`. Negative control: the report
/// reading `[harness] accounts` alone says `early` with the harness off.
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
        aterm_uds::atomic_file::write(
            env.config.as_ref().expect("a config path"),
            (toml).as_ref(),
            0o600,
        )
        .expect("write");
        seeded(&tmp, &env);
        let (_, text, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
        assert!(
            text.contains(&format!("\u{b7} rotation {mode} \u{b7}")),
            "{toml}: rotation {mode}:\n{text}"
        );
        if mode == "off" {
            let (_, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
            let json: Value = aterm_json::from_str(json.trim()).expect("the report is JSON");
            assert!(
                stop_ends(&json["outlook"])
                    .contains(&("reset".into(), Some(env.now + 3 * D + 13 * H))),
                "{toml}: me waits to its reset: {:?}",
                json["outlook"]
            );
        }
        assert_eq!(card_outlook(&env).mode.as_str(), mode, "{toml}: the card's");
    }
}

/// The `since` of the window `id` in a `windows` array of `--json`.
fn since_in(windows: &Value, id: &str) -> Value {
    windows
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["id"] == Value::from(id))
        .map_or(Value::Null, |w| w["since"].clone())
}

/// STATS AND THE ACCOUNTS REPORT GIVE ONE SINCE. On the seed, `me`'s week read 42% a day ago and
/// 58% now: `accounts stats --json` run on `me`'s home says it rose 16 points over 24h, and the
/// `--json` accounts report says exactly that `since` under `me`'s row, the windows with no
/// series saying none in both. Negative control: the report's `since` left null (its
/// `with_since` call in `accounts_json` removed) differs from the stats one, and the second
/// assertion fails.
#[test]
fn stats_and_the_accounts_report_give_one_since() {
    let tmp = tmp_dir("accounts-since");
    let mut env = env_at(&tmp);
    seeded(&tmp, &env);
    let home = tmp.path().join("logins").join("me");
    env.claude_profile = Some(home.join(".claude.json"));
    env.claude_dir = Some(home);
    let (code, stats, err) = acc_quiet(
        AccountsCmd::Stats {
            json: true,
            since_s: D,
        },
        &env,
    );
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS), "{err}");
    let stats: Value = aterm_json::from_str(stats.trim()).expect("the stats are JSON");
    let account = &stats["accounts"][0];
    assert_eq!(account["label"], Value::from("me"));
    let week = since_in(&account["windows"], "seven_day");
    assert_eq!(
        week,
        aterm_json::json!({"span_s": D, "used_pct": 16, "new_window": null,
                           "new_window_rfc3339": null, "reset_used": null,
                           "reset_used_rfc3339": null}),
        "{stats}"
    );
    let (_, report, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    let report: Value = aterm_json::from_str(report.trim()).expect("the report is JSON");
    let row = report["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["label"] == Value::from("me"))
        .expect("me's row");
    assert_eq!(since_in(&row["windows"], "seven_day"), week, "{report}");
    for id in ["five_hour", profile::Bucket::Fable.id()] {
        assert_eq!(since_in(&account["windows"], id), Value::Null, "{id}");
        assert_eq!(since_in(&row["windows"], id), Value::Null, "{id}");
    }
}

/// T9's report half: THE REPORT WRITES NOTHING WITH THE JOURNAL ON OR OFF. Over the seeded store,
/// with `pool1` (never read here) given an address the journal knows, the report run with no
/// journal and the report run with one in which a peer read `pool1`'s week leave every file
/// byte-identical; only the one with the journal says the peer's bound (`lab.pool1` in the JSON,
/// `others may use it` in the text), and the rows' own figures are this Mac's either way.
/// NEGATIVE CONTROL: with `write_accounts_report_snapshot` giving the outlook no bounds (its
/// `lab` read removed), `the journal's bound is shown` fails.
#[test]
fn the_report_writes_nothing_with_the_journal_on_or_off() {
    use crate::harness::alab_seen::{self, AccountSeen, Figure, Seen, WindowSeen};
    const KEY: &str = "ac-00000000000000b1";
    const PEER: &str = "13000000000000000000000000000013";
    const OWN: &str = "02000000000000000000000000000002";
    let tmp = tmp_dir("accounts-outlook-journal");
    let env = env_at(&tmp);
    seeded(&tmp, &env);
    let dir = |label: &str| {
        let dir = tmp.path().join("logins").join(label);
        Some(dir.to_str().expect("utf-8").to_owned())
    };
    let roster = accounts::append_rows_for(
        Cli::Claude,
        "",
        &[
            accounts::Row {
                label: "me".into(),
                class: Class::Preferred,
                dir: dir("me"),
                ..Default::default()
            },
            accounts::Row {
                label: "alt1".into(),
                class: Class::Preferred,
                dir: dir("alt1"),
                ..Default::default()
            },
            accounts::Row {
                label: "pool1".into(),
                class: Class::Pool,
                dir: dir("pool1"),
                email: Some("pool1@example.test".into()),
                ..Default::default()
            },
        ],
    )
    .expect("the rows admit");
    std::fs::write(env.accounts_roster().expect("a config dir"), roster).expect("write");
    let seen = Seen {
        me: [OWN.to_owned()].into(),
        at: env.now,
        accounts: [(
            KEY.to_owned(),
            AccountSeen {
                windows: vec![WindowSeen {
                    window: alab_seen::WEEK.into(),
                    sub: None,
                    figure: Some(Figure::Used {
                        pct_x10: 400,
                        reset: Some(env.now + 5 * D),
                    }),
                    read: Some((env.now - 3 * H, "m13".into(), "statusline".into())),
                    reader: Some(PEER.into()),
                    wall_until: None,
                    wall_by: None,
                }],
                ..AccountSeen::default()
            },
        )]
        .into(),
    };
    let report = |json: bool| {
        crate::harness::journal::with_keyer(
            |realm, email| (realm == "claude" && email == "pool1@example.test").then(|| KEY.into()),
            || acc_quiet(AccountsCmd::Report { json }, &env).1,
        )
    };
    let before = every_file(tmp.path());
    let (off_json, off_text) = (report(true), report(false));
    let (on_json, on_text) = alab_seen::with_seen(seen, || (report(true), report(false)));
    assert_eq!(before, every_file(tmp.path()), "the reports wrote nothing");
    let off_json: Value = aterm_json::from_str(off_json.trim()).expect("the report is JSON");
    let on_json: Value = aterm_json::from_str(on_json.trim()).expect("the report is JSON");
    assert!(
        off_json["outlook"]["lab"]
            .as_object()
            .is_some_and(|lab| lab.is_empty()),
        "CONTROL: no journal, no bound: {}",
        off_json["outlook"]
    );
    assert!(
        !off_text.contains("others may use it"),
        "CONTROL: {off_text}"
    );
    assert_eq!(
        on_json["outlook"]["lab"][id_of(&env, "pool1").as_str()]["seven_day"]["used_x10"].as_u64(),
        Some(400),
        "the journal's bound is shown: {}",
        on_json["outlook"]
    );
    assert!(on_text.contains("others may use it"), "{on_text}");
    let figures = |doc: &Value| {
        doc["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|row| row["windows"].as_array().into_iter().flatten())
            .map(figures_of)
            .collect::<Vec<_>>()
    };
    assert!(
        !figures(&off_json).is_empty(),
        "CONTROL: the rows have figures"
    );
    assert_eq!(
        figures(&on_json),
        figures(&off_json),
        "the rows' own figures"
    );
}

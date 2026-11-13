// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn seen_account() -> AccountSeen {
    AccountSeen {
        last_use: Some(LastUse {
            by: "m17".into(),
            t: 1_000 - 3 * 3_600,
        }),
        signed_in: vec![
            SignedIn {
                on: "m3".into(),
                state: "in".into(),
                t: 900,
            },
            SignedIn {
                on: "m17".into(),
                state: "out".into(),
                t: 800,
            },
        ],
        windows: vec![
            WindowSeen {
                window: "5h".into(),
                sub: None,
                figure: Some(Figure::Used {
                    pct_x10: 425,
                    reset: None,
                }),
                read: Some((1_000 - 1_200, "m3".into(), "statusline".into())),
                wall_until: None,
            },
            WindowSeen {
                window: "7d".into(),
                sub: None,
                figure: Some(Figure::ResetUnread { reset: 0 }),
                read: None,
                wall_until: Some(None),
            },
        ],
        history: Vec::new(),
    }
}

/// A report says who last used the account, by its clock, the retained sign-in records, and
/// each window with its age, Mac and source, as what is left (`57.5% left`); a reset nobody
/// read since is never shown as a fresh window. NEGATIVE CONTROL: with `window_line`
/// printing a `ResetUnread` figure as `100% left`, the 7d line reads `7d 100% left…`:
/// `left: "7d 100% left, at its limit"`.
#[test]
fn a_report_names_the_mac_the_age_and_never_a_guessed_zero() {
    let held = seen_account();
    let original = held.clone();
    let lines = super::lines(&held, 1_000);
    assert_eq!(lines[0], "last used by m17, about 3 h ago (its clock)");
    for part in [
        "m3 (in), about 1 min ago (its clock)",
        "m17 (out), about 3 min ago (its clock)",
    ] {
        assert!(lines[1].contains(part), "{}", lines[1]);
    }
    assert_eq!(
        lines[2],
        "5h 57.5% left, reset unknown, read on m3, about 20 min ago (its clock) (statusline)"
    );
    assert_eq!(
        lines[3],
        "7d reset at 1970-01-01T00:00:00Z, not read since, at its limit"
    );
    assert!(lines.iter().all(|l| !l.contains('@')));
    let later = super::lines(&held, 1_000 + 3 * 86_400);
    assert!(later[1].contains("m3 (in), about 3 d ago (its clock)"));
    assert!(later[1].contains("m17 (out), about 3 d ago (its clock)"));
    let ahead = super::lines(&held, 700);
    assert!(ahead[1].contains("m3 (in), 3 min ahead (its clock)"));
    assert!(ahead[1].contains("m17 (out), 1 min ahead (its clock)"));
    assert_eq!(held, original);
    assert_eq!(signed_in_words(&[], 1_000), None);
}

/// A cached reading crossing its known reset is unknown in both report forms, while the
/// retained reading and its provenance stay intact.
/// NEGATIVE CONTROL: normalizing only `window_line` leaves JSON's state current and its
/// positive quota and wall fields present after the reset.
#[test]
fn cached_quota_reports_expire_together_without_changing_the_reading() {
    let rows = vec![(
        "one".into(),
        "ac-0123456789abcdef".into(),
        AccountSeen {
            windows: vec![
                WindowSeen {
                    window: "5h".into(),
                    sub: None,
                    figure: Some(Figure::Used {
                        pct_x10: 1000,
                        reset: Some(300),
                    }),
                    read: Some((200, "m17".into(), "statusline".into())),
                    wall_until: Some(Some(300)),
                },
                WindowSeen {
                    window: "7d".into(),
                    sub: None,
                    figure: None,
                    read: None,
                    wall_until: Some(Some(300)),
                },
            ],
            ..Default::default()
        },
    )];
    let original = rows.clone();
    let before = rows_json(&rows, 299);
    let window = &before["one"]["windows"][0];
    assert_eq!(window["state"].as_str(), Some("current"));
    assert_eq!(window["used_pct_x10"].as_u64(), Some(1000));
    assert_eq!(window["wall_until"].as_i64(), Some(300));
    assert_eq!(
        before["one"]["windows"][1]["state"].as_str(),
        Some("wall-only")
    );
    for now in [300, 301] {
        let json = rows_json(&rows, now);
        let window = &json["one"]["windows"][0];
        assert_eq!(window["state"].as_str(), Some("reset-unread"));
        assert!(window["used_pct_x10"].is_null());
        assert!(window["wall_until"].is_null());
        assert_eq!(window["reset"].as_i64(), Some(300));
        assert_eq!(window["read"]["t"].as_i64(), Some(200));
        assert_eq!(window["read"]["by"].as_str(), Some("m17"));
        assert_eq!(window["read"]["src"].as_str(), Some("statusline"));
        let line = window["line"].as_str().unwrap();
        assert!(line.contains("not read since"), "{line}");
        assert!(!line.contains("100%") && !line.contains("at its limit"));
        assert!(rows_text(&rows, now).contains(line));
        let wall_only = &json["one"]["windows"][1];
        assert_eq!(wall_only["state"].as_str(), Some("unknown"));
        assert!(wall_only["wall_until"].is_null());
        assert!(!wall_only["line"].as_str().unwrap().contains("at its limit"));
    }
    assert_eq!(rows, original);
}

#[test]
fn quota_read_provenance_tracks_the_source_clock_without_changing_the_window() {
    let reading = 1_120;
    let held = vec![(
        "one".into(),
        "ac-0123456789abcdef".into(),
        AccountSeen {
            last_use: Some(LastUse {
                by: "m3".into(),
                t: reading,
            }),
            windows: vec![WindowSeen {
                window: "5h".into(),
                sub: None,
                figure: Some(Figure::Used {
                    pct_x10: 420,
                    reset: Some(1_300),
                }),
                read: Some((reading, "m3".into(), "statusline".into())),
                wall_until: Some(Some(1_300)),
            }],
            ..Default::default()
        },
    )];
    let original = held.clone();
    for (now, words) in [
        (1_000, "2 min ahead"),
        (reading, "0 s ago"),
        (1_240, "2 min ago"),
        (1_300, "3 min ago"),
    ] {
        let json = rows_json(&held, now);
        let shown = &json["one"]["windows"][0];
        let line = shown["line"].as_str().unwrap();
        for part in [words, "m3", "its clock", "statusline"] {
            assert!(line.contains(part), "{line}");
        }
        assert_eq!(shown["read"]["t"].as_i64(), Some(reading));
        assert_eq!(shown["read"]["age_s"].as_i64(), Some(now - reading));
        assert_eq!(
            json["one"]["last_use"]["age_s"].as_i64(),
            Some(now - reading)
        );
        if now < 1_300 {
            assert_eq!(shown["state"].as_str(), Some("current"));
            assert_eq!(shown["used_pct_x10"].as_u64(), Some(420));
            assert_eq!(shown["wall_until"].as_i64(), Some(1_300));
        } else {
            assert_eq!(shown["state"].as_str(), Some("reset-unread"));
            assert!(shown["used_pct_x10"].is_null());
            assert!(shown["wall_until"].is_null());
        }
        assert!(rows_text(&held, now).contains(line));
    }
    let mut no_read = held[0].2.windows[0].clone();
    no_read.read = None;
    assert!(!window_line(&no_read, 1_000).contains("read on"));
    assert_eq!(held, original);
}

#[test]
fn relative_provenance_keeps_raw_times_when_the_signed_age_cannot_fit() {
    for (now, t, direction) in [(i64::MIN, i64::MAX, "ahead"), (i64::MAX, i64::MIN, "ago")] {
        let window = WindowSeen {
            window: "5h".into(),
            sub: None,
            figure: Some(Figure::Used {
                pct_x10: 420,
                reset: None,
            }),
            read: Some((t, "m3".into(), "statusline".into())),
            wall_until: None,
        };
        let shown = rows_json(
            &[(
                "one".into(),
                "ac-0123456789abcdef".into(),
                AccountSeen {
                    last_use: Some(LastUse { by: "m3".into(), t }),
                    windows: vec![window.clone()],
                    ..Default::default()
                },
            )],
            now,
        );
        let read = &shown["one"]["windows"][0]["read"];
        assert_eq!(read["t"].as_i64(), Some(t));
        assert!(read["age_s"].is_null());
        assert_eq!(shown["one"]["last_use"]["t"].as_i64(), Some(t));
        assert!(shown["one"]["last_use"]["age_s"].is_null());
        let line = window_line(&window, now);
        assert!(line.contains(direction), "{line}");
        assert!(line.contains(&format!("{} d", u64::MAX / 86_400)), "{line}");
        assert_eq!(
            shown["one"]["windows"][0]["used_pct_x10"].as_u64(),
            Some(420)
        );
        assert_eq!(window.read.as_ref().unwrap().0, t);
    }
}

fn roster(label: &str, email: &str) -> super::super::accounts::Roster {
    super::super::accounts::Roster {
        rows: vec![super::super::accounts::Row {
            label: label.into(),
            email: Some(email.into()),
            ..Default::default()
        }],
    }
}

#[test]
fn readonly_journal_attribution_follows_an_externally_replaced_identity() {
    use super::super::accounts::{self, Cli, Store};
    const ANN: &str = "ac-0123456789abcdef";
    const BOB: &str = "ac-fedcba9876543210";
    let keyer = |_: &str, email: &str| match email {
        "ann@example.test" => Some(ANN.to_owned()),
        "bob@example.test" => Some(BOB.to_owned()),
        _ => None,
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let store = Store::at_for(&scratch.path().join("harness"), Cli::Claude, &home).bounded_by(&[]);
    let path = scratch.path().join("accounts.toml");
    let old = roster("work", "ann@example.test");
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &old.rows).unwrap(),
    )
    .unwrap();
    store
        .transact(1_000, &mut None, |state| {
            state.accounts.entry("work".into()).or_default();
            super::super::journal::learn_with(Cli::Claude, &old, state, &keyer);
            Ok(Vec::new())
        })
        .unwrap();
    let replacement = roster("work", "bob@example.test");
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &replacement.rows).unwrap(),
    )
    .unwrap();
    let images = || {
        [
            path.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.state_witness_file(),
            store.logins_witness_file(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = images();
    let records = store.read_account_records(Some(&path), 1_000).unwrap();
    assert_eq!(records.state.accounts["work"].pk.as_deref(), Some(ANN));
    assert_eq!(
        records
            .roster
            .row("work")
            .unwrap()
            .expected_identity_email(),
        Some("bob@example.test")
    );
    assert_eq!(images(), before, "the read must not rekey persisted state");
    let ann = seen_account();
    let mut bob = ann.clone();
    bob.last_use.as_mut().unwrap().by = "m22".into();
    bob.windows[0].figure = Some(Figure::Used {
        pct_x10: 950,
        reset: Some(2_000),
    });
    let seen = Seen {
        accounts: [(ANN.into(), ann), (BOB.into(), bob.clone())].into(),
        at: 1_000,
    };
    let reported =
        super::super::journal::with_keyer(keyer, || rows(Cli::Claude, &records.roster, &seen));
    assert_eq!(reported.len(), 1);
    assert_eq!(
        reported[0].1, BOB,
        "a reused label is not current identity evidence"
    );
    assert_eq!(reported[0].2, bob);
    let json = rows_json(&reported, 1_000);
    assert_eq!(
        json["work"]["windows"][0]["used_pct_x10"].as_u64(),
        Some(950)
    );
    assert_eq!(json["work"]["last_use"]["by"].as_str(), Some("m22"));
    let old_history = super::super::journal::with_keyer(keyer, || {
        resolve(Cli::Claude, ANN, &records.roster, None).unwrap()
    });
    assert_eq!(old_history.pk.as_deref(), Some(ANN));
    assert_eq!(
        old_history.label, None,
        "the replaced row no longer names Ann"
    );
    assert_eq!(images(), before);
}

#[test]
fn explicit_history_keys_survive_a_colliding_replacement_label() {
    use super::super::{
        accounts::{Cli, Event, Kind},
        journal,
    };
    const ANN: &str = "ac-0123456789abcdef";
    const BOB: &str = "ac-fedcba9876543210";
    let mut current = roster(ANN, "bob@example.test");
    current.rows.push(super::super::accounts::Row {
        label: "sa-0123456789abcdef".into(),
        email: Some("bob@example.test".into()),
        ..Default::default()
    });
    journal::with_keyer(
        |realm, email| (realm == "claude" && email == "bob@example.test").then(|| BOB.to_owned()),
        || {
            let old = resolve(Cli::Claude, ANN, &current, None).unwrap();
            assert_eq!(
                old,
                Asked {
                    label: None,
                    pk: Some(ANN.into())
                }
            );
            let replacement = resolve(Cli::Claude, BOB, &current, None).unwrap();
            assert_eq!(replacement.label.as_deref(), Some(ANN));
            assert_eq!(replacement.pk.as_deref(), Some(BOB));
            let sub_like_label =
                resolve(Cli::Claude, "sa-0123456789abcdef", &current, None).unwrap();
            assert_eq!(sub_like_label.pk.as_deref(), Some(BOB));
            let mut old_event = Event::new(10, Kind::Auth, "before");
            old_event.pk = Some(ANN.into());
            let mut new_event = Event::new(20, Kind::Auth, ANN);
            new_event.pk = Some(BOB.into());
            let lines = history(
                &old,
                &[old_event, new_event, Event::new(30, Kind::Rotate, ANN)],
                None,
            );
            assert_eq!(
                lines.len(),
                1,
                "old key must not borrow the replacement label's events"
            );
            assert_eq!(lines[0].0, 10, "the line keeps its row's instant");
            assert!(lines[0].1.contains(" here auth before"));
        },
    );
}

#[test]
fn unresolved_current_identity_keeps_local_history_without_borrowing_lab_aliases() {
    use super::super::{
        accounts::{Cli, Event, Kind},
        alab_roster::{Lab, LabAccount},
        journal,
    };
    const ANN: &str = "ac-0123456789abcdef";
    let mut current = roster("work", "ann@example.test");
    current.rows[0].identity_email = Some("bob@example.test".into());
    let lab = Lab {
        accounts: vec![LabAccount {
            pk: ANN.into(),
            cli: Cli::Claude,
            vendor: Default::default(),
            label: "ann".into(),
            legacy_label: Some("before".into()),
            email: "ann@example.test".into(),
            identity: None,
            browser: None,
            joinable: true,
            preferred: Vec::new(),
        }],
        ..Lab::default()
    };
    let seen = Seen {
        accounts: [(ANN.into(), seen_account())].into(),
        at: 1_000,
    };
    journal::with_keyer(
        |_, _| None,
        || {
            let asked = resolve(Cli::Claude, "work", &current, Some(&lab)).unwrap();
            assert_eq!(
                asked,
                Asked {
                    label: Some("work".into()),
                    pk: None
                }
            );
            assert!(rows(Cli::Claude, &current, &seen).is_empty());
            assert_eq!(
                history(&asked, &[Event::new(10, Kind::Auth, "work")], None).len(),
                1
            );
            for name in [ANN, "ann", "before"] {
                let old = resolve(Cli::Claude, name, &current, Some(&lab)).unwrap();
                assert_eq!(
                    old,
                    Asked {
                        label: None,
                        pk: Some(ANN.into())
                    }
                );
            }
        },
    );
}

/// A forgotten row's observations stay under its account key (through `state.json`), and the
/// row that comes back for that account under another label takes them again; a row of
/// another account takes nothing.
/// NEGATIVE CONTROL: with `forget_account` not keeping the removed row (`remember_forgotten`
/// call removed), the new row starts blind: `assertion failed: back.walls.contains_key("5h")`.
#[test]
fn a_forgotten_row_keeps_its_history_under_its_account_key() {
    use super::super::accounts::{self, Account, Logins, State, Wall};
    const KEY: &str = "ac-0123456789abcdef";
    let keyer = |_: &str, email: &str| (email == "ann@example.test").then(|| KEY.to_owned());
    let mut state = State::default();
    let mut walls = BTreeMap::new();
    walls.insert(
        "5h".to_owned(),
        Wall {
            until: Some(2_000),
            at: 1_000,
            by: "aa".into(),
            attempts: 0,
            cleared: false,
        },
    );
    state.accounts.insert(
        "old".into(),
        Account {
            pk: Some(KEY.into()),
            walls,
            ran_at: Some(900),
            auth: accounts::Auth::Ok,
            ..Default::default()
        },
    );
    accounts::forget_account(&mut state, &mut Logins::default(), "old");
    assert!(!state.accounts.contains_key("old"));
    let mut state = State::from_json(&state.to_json(), 1_500).unwrap();
    assert!(state.forgotten.contains_key(KEY), "{:?}", state.forgotten);
    super::super::journal::learn_with(
        accounts::Cli::Claude,
        &roster("other", "bob@example.test"),
        &mut state,
        &keyer,
    );
    assert!(
        state.forgotten.contains_key(KEY),
        "another account takes nothing"
    );
    super::super::journal::learn_with(
        accounts::Cli::Claude,
        &roster("new", "ann@example.test"),
        &mut state,
        &keyer,
    );
    let back = &state.accounts["new"];
    assert!(back.walls.contains_key("5h"));
    assert_eq!(back.ran_at, Some(900));
    assert_eq!(back.pk.as_deref(), Some(KEY));
    assert_eq!(
        back.auth,
        accounts::Auth::Unseen,
        "the old login's word stays behind"
    );
    assert!(state.forgotten.is_empty());
}

/// A row whose email is edited to another account is keyed as that account, and what this
/// Mac observed under it stays with the account it was keyed as: filed under the old key, so a
/// later forget of the label files nothing under the new one and the new account's row is not
/// walled by the old login's wall. Its login's word stays with the label.
/// NEGATIVE CONTROL: with `learn_with` re-keying in place (`account.pk = Some(pk)`, no
/// `take_observations`), the old wall is carried to the new account:
/// `assertion failed: !state.accounts["x"].walls.contains_key("5h")`.
#[test]
fn a_row_edited_to_another_account_leaves_its_observations_with_the_old_one() {
    use super::super::accounts::{self, Account, Logins, State, Wall};
    const ANN: &str = "ac-0123456789abcdef";
    const BOB: &str = "ac-fedcba9876543210";
    let keyer = |_: &str, email: &str| match email {
        "ann@example.test" => Some(ANN.to_owned()),
        "bob@example.test" => Some(BOB.to_owned()),
        _ => None,
    };
    let mut state = State::default();
    let mut walls = BTreeMap::new();
    walls.insert(
        "5h".to_owned(),
        Wall {
            until: Some(2_000),
            at: 1_000,
            by: "aa".into(),
            attempts: 0,
            cleared: false,
        },
    );
    state.accounts.insert(
        "x".into(),
        Account {
            pk: Some(ANN.into()),
            walls,
            auth: accounts::Auth::Ok,
            ..Default::default()
        },
    );
    super::super::journal::learn_with(
        accounts::Cli::Claude,
        &roster("x", "bob@example.test"),
        &mut state,
        &keyer,
    );
    assert_eq!(state.accounts["x"].pk.as_deref(), Some(BOB));
    assert!(!state.accounts["x"].walls.contains_key("5h"));
    assert_eq!(state.accounts["x"].auth, accounts::Auth::Ok);
    assert!(state.forgotten[ANN].walls.contains_key("5h"));
    accounts::forget_account(&mut state, &mut Logins::default(), "x");
    super::super::journal::learn_with(
        accounts::Cli::Claude,
        &roster("x", "bob@example.test"),
        &mut state,
        &keyer,
    );
    assert!(
        !state.accounts["x"].walls.contains_key("5h"),
        "bob's row is not walled by ann's wall"
    );
    assert!(state.forgotten[ANN].walls.contains_key("5h"));
}

/// `accounts history` finds an account by its row label or its key, never by an address, and
/// lists this Mac's ledger rows of it (by key, or by label before a key was learned) beside
/// every Mac's journal lines, oldest first, never a ledger row's free-text reason.
/// NEGATIVE CONTROL: with `history` matching ledger rows by label alone, the row keyed to the
/// account under its old label is missed: `left: 2 right: 3`.
#[test]
fn history_merges_this_macs_ledger_and_the_journal_by_account_key() {
    use super::super::accounts::{self, Event, Kind};
    const KEY: &str = "ac-0123456789abcdef";
    let roster = roster("now", "ann@example.test");
    let (asked, by_key) = super::super::journal::with_keyer(
        |realm, email| (realm == "claude" && email == "ann@example.test").then(|| KEY.to_owned()),
        || {
            assert!(resolve(accounts::Cli::Claude, "ann@example.test", &roster, None).is_none());
            (
                resolve(accounts::Cli::Claude, "now", &roster, None).unwrap(),
                resolve(accounts::Cli::Claude, KEY, &roster, None).unwrap(),
            )
        },
    );
    assert_eq!(asked.pk.as_deref(), Some(KEY));
    assert_eq!(by_key.label.as_deref(), Some("now"));
    let mut renamed = Event::new(10, Kind::Auth, "before");
    renamed.pk = Some(KEY.into());
    renamed.why = "secret text".into();
    let ledger = vec![
        renamed,
        Event::new(30, Kind::Rotate, "now"),
        Event::new(40, Kind::Rotate, "elsewhere"),
    ];
    let journal = AccountSeen {
        history: vec![Line {
            t: 20,
            by: "m17".into(),
            kind: "signed-in".into(),
            cli: "claude".into(),
            detail: String::new(),
            heard: true,
            window: None,
            sub: false,
            pct_x10: None,
            reset: None,
        }],
        ..Default::default()
    };
    let lines = history(&asked, &ledger, Some(&journal));
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(
        lines.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [10, 20, 30],
        "each line carries its instant, oldest first"
    );
    for (t, line) in &lines {
        assert!(
            line.starts_with(&super::super::usage::rfc3339_utc(*t)),
            "{line}"
        );
    }
    assert!(lines[0].1.contains(" here auth before"));
    assert!(
        lines[1]
            .1
            .contains(" m17 signed-in claude (heard by this Mac)")
    );
    assert!(lines[2].1.contains(" here rotate now"));
    assert!(lines.iter().all(|(_, l)| !l.contains("secret")));
}

/// The report's journal section names each row the journal knows by its label and key, and a
/// row the journal knows nothing of is left out.
/// NEGATIVE CONTROL: with `rows` keeping rows the journal has no entry for, `left: 2 right: 1`.
#[test]
fn the_report_lists_only_rows_the_journal_knows() {
    use super::super::accounts;
    const KEY: &str = "ac-0123456789abcdef";
    let mut both = roster("one", "ann@example.test");
    both.rows.push(accounts::Row {
        label: "two".into(),
        ..Default::default()
    });
    let mut seen = Seen::default();
    seen.accounts.insert(KEY.into(), seen_account());
    let rows = super::super::journal::with_keyer(
        |realm, email| (realm == "claude" && email == "ann@example.test").then(|| KEY.to_owned()),
        || rows(accounts::Cli::Claude, &both, &seen),
    );
    assert_eq!(rows.len(), 1);
    let json = rows_json(&rows, 1_000);
    assert_eq!(json["one"]["ac"].as_str(), Some(KEY));
    assert_eq!(json["one"]["last_use"]["by"].as_str(), Some("m17"));
    assert!(rows_text(&rows, 1_000).contains("  one: last used by m17"));
}

/// With no source installed nothing is held; a thread's override is what `current` answers.
/// NEGATIVE CONTROL: with `current` ignoring the override, the second assertion fails.
#[test]
fn current_reads_the_installed_source_only() {
    assert!(current(false).is_none());
    let held = with_seen(Seen::default(), || current(true));
    assert_eq!(held.as_deref(), Some(&Seen::default()));
}

fn line(t: i64, by: &str, kind: &str) -> Line {
    Line {
        t,
        by: by.into(),
        kind: kind.into(),
        cli: "claude".into(),
        detail: String::new(),
        heard: false,
        window: None,
        sub: false,
        pct_x10: None,
        reset: None,
    }
}

/// THE JOURNAL'S WEEK IS THE ACCOUNT'S OWN `7d` READINGS THAT NAME THEIR WEEK. A quota line of
/// the week with a figure and a reset is a reading; the five-hour window's, a workspace's, one
/// with no figure, one with no reset and a line of another kind are not.
#[test]
fn the_journals_week_is_the_accounts_own_dated_readings() {
    let quota = |t: i64, window: &str, pct_x10: Option<u16>, reset: Option<i64>| Line {
        window: Some(window.into()),
        pct_x10,
        reset,
        ..line(t, "m15", QUOTA)
    };
    let account = AccountSeen {
        history: vec![
            quota(10, WEEK, Some(200), Some(900)),
            quota(20, "5h", Some(300), Some(900)),
            Line {
                sub: true,
                ..quota(30, WEEK, Some(990), Some(900))
            },
            quota(40, WEEK, None, Some(900)),
            quota(50, WEEK, Some(400), None),
            Line {
                kind: SESSION_START.into(),
                ..quota(60, WEEK, Some(500), Some(900))
            },
            quota(70, WEEK, Some(450), Some(900)),
        ],
        ..Default::default()
    };
    assert_eq!(week_reads(&account), [(10, 200, 900), (70, 450, 900)]);
    assert_eq!(week_reads(&AccountSeen::default()), []);
}

/// STARTS ARE COUNTED BY MAC OVER THE LINES HELD, AND SAY SINCE WHEN. Only `session-start` lines
/// count; the Macs come most starts first; `since` is the oldest held line of any kind, since
/// the history is the newest lines and no fixed span. A history with no start says nothing.
#[test]
fn starts_by_mac_counts_starts_and_says_the_oldest_line_held() {
    let account = AccountSeen {
        history: vec![
            line(100, "m3", "quota"),
            line(200, "m15", SESSION_START),
            line(300, "m20", SESSION_START),
            line(400, "m15", "signed-in"),
            line(500, "m15", SESSION_START),
            line(600, "uuid:0a1b2c3d", SESSION_START),
        ],
        ..AccountSeen::default()
    };
    assert_eq!(
        starts_by_mac(&account),
        Some((
            vec![
                ("m15".to_owned(), 2),
                ("m20".to_owned(), 1),
                ("uuid:0a1b2c3d".to_owned(), 1),
            ],
            100
        ))
    );
    let none = AccountSeen {
        history: vec![line(100, "m3", "quota"), line(400, "m15", "signed-in")],
        ..AccountSeen::default()
    };
    assert_eq!(starts_by_mac(&none), None, "no start, no count");
    assert_eq!(starts_by_mac(&AccountSeen::default()), None);
}

/// THE JOURNAL SAYS WHY IT HOLDS NOTHING. A test process installs no source, so it reads no
/// journal: `Off`, never `NotReadYet`. With a journal held it is `Read`, and asking never waits.
#[test]
fn journal_tells_off_from_read() {
    assert_eq!(journal(), Journal::Off);
    let seen = Seen {
        at: 7,
        ..Seen::default()
    };
    let held = with_seen(seen.clone(), journal);
    assert_eq!(held, Journal::Read(Arc::new(seen)));
    assert_eq!(journal(), Journal::Off, "the override ended with its call");
}

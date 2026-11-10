// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reconciling a tab's believed row with what its CLI really runs on
//! (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1), from the live bug on m65: a default
//! row `m22` that expects brontosaurus while `~/.claude.json` names unicorn.

use super::*;
use crate::harness::journal::ObservedSrc;
use crate::harness::observe::{Observation, Observed};
use std::path::PathBuf;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";

fn roster(rows: &[(&str, Option<&str>, Option<&str>)]) -> Roster {
    let mut text = String::new();
    for (label, dir, email) in rows {
        text.push_str(&format!(
            "[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\n"
        ));
        if let Some(dir) = dir {
            text.push_str(&format!("dir = \"{dir}\"\n"));
        }
        if let Some(email) = email {
            text.push_str(&format!("email = \"{email}\"\n"));
        }
    }
    Roster::parse_for(Cli::Claude, &text).unwrap()
}

fn stored() -> ProcessLogin {
    ProcessLogin {
        login: None,
        default_home: true,
        route: AuthRoute::Stored,
    }
}

fn seen(email: &str) -> Observation {
    Observation::Seen(Observed {
        cli: Cli::Claude.into(),
        email: email.to_owned(),
        key: None,
        region: None,
        src: ObservedSrc::Profile,
        path: PathBuf::from("/home/someone/.claude.json"),
        mtime: Some(1),
    })
}

fn dir() -> String {
    if cfg!(windows) {
        "C:\\logins\\u".to_owned()
    } else {
        "/logins/u".to_owned()
    }
}

/// THE LIVE BUG: the default row `m22` expects brontosaurus, the profile names unicorn, and no
/// row lists unicorn. The tab runs on unicorn, unlisted: no surface names `m22`, and the footer
/// names the observed address with `m22` as the belief.
/// NEGATIVE CONTROL: with `reconcile` returning the belief for every observation, the footer
/// names `m22`: `` left: Some("m22") right: None ``.
#[test]
fn m22_default_row_with_unicorn_profile_is_unlisted() {
    let r = roster(&[("m22", None, Some(BRONTO))]);
    let got = reconcile(&r, &stored(), &seen(UNICORN));
    assert!(
        matches!(&got, Reconciled::Unlisted(o) if o.email == UNICORN),
        "{got:?}"
    );
    assert!(got.disagrees());
    assert_eq!(observed_label(&r, &stored(), &seen(UNICORN)), None);
    assert_eq!(footer_label(&r, &stored(), &seen(UNICORN)), None);
    let footer = footer_account(&r, &stored(), &seen(UNICORN)).unwrap();
    assert_eq!(footer.label, None);
    assert_eq!(footer.email.as_deref(), Some(UNICORN));
    assert_eq!(footer.believed.as_deref(), Some("m22"));
}

/// Where another row lists the observed address, the tab runs on that row.
/// NEGATIVE CONTROL: with `Other` mapped to the believed row in `Reconciled::row`, the label is
/// `m22`: `` left: Some("m22") right: Some("m15") ``.
#[test]
fn other_row_answering_is_credited() {
    let d = dir();
    let r = roster(&[
        ("m22", None, Some(BRONTO)),
        ("m15", Some(&d), Some(UNICORN)),
    ]);
    let got = reconcile(&r, &stored(), &seen(UNICORN));
    assert!(
        matches!(got, Reconciled::Other(row) if row.label == "m15"),
        "{got:?}"
    );
    assert_eq!(observed_label(&r, &stored(), &seen(UNICORN)), Some("m15"));
    let footer = footer_account(&r, &stored(), &seen(UNICORN)).unwrap();
    assert_eq!(footer.label.as_deref(), Some("m15"));
    assert_eq!(footer.believed.as_deref(), Some("m22"));
}

/// The observed address answers to the believed row: it agrees, whatever the case.
/// NEGATIVE CONTROL: with `email_matches` replaced by `==`, the upper-case address is unlisted:
/// `` Unlisted(Observed { … }) ``.
#[test]
fn agrees_when_identity_matches() {
    let r = roster(&[("m22", None, Some(BRONTO))]);
    let got = reconcile(&r, &stored(), &seen(&BRONTO.to_ascii_uppercase()));
    assert!(
        matches!(got, Reconciled::Agrees(row) if row.label == "m22"),
        "{got:?}"
    );
    assert!(!got.disagrees());
    let footer = footer_account(&r, &stored(), &seen(BRONTO)).unwrap();
    assert_eq!(footer.believed, None);
    // One row that agrees is not named in the footer, as before.
    assert_eq!(footer.label, None);
}

/// No observation leaves the belief standing, unverified; so does a believed row that names no
/// address, which no address can contradict.
/// NEGATIVE CONTROL: with `Unread` given `None`, the label is lost: `` left: None right:
/// Some("m22") ``.
#[test]
fn unread_keeps_belief_unverified() {
    let r = roster(&[("m22", None, Some(BRONTO))]);
    assert_eq!(
        reconcile(&r, &stored(), &Observation::Unread),
        Reconciled::Unread(r.row("m22"))
    );
    assert_eq!(
        observed_label(&r, &stored(), &Observation::Unread),
        Some("m22")
    );
    let signed_out = Observation::SignedOut {
        path: PathBuf::from("/home/someone/.claude.json"),
    };
    assert_eq!(observed_label(&r, &stored(), &signed_out), Some("m22"));
    let bare = roster(&[("main", None, None)]);
    assert_eq!(
        reconcile(&bare, &stored(), &seen(UNICORN)),
        Reconciled::Unread(bare.row("main"))
    );
}

/// Two rows that answer to one address credit neither: the belief stands, unverified, and no
/// account is born for it.
/// NEGATIVE CONTROL: with `row_answering` taking the first match, the second row is credited:
/// `` Other(Row { label: "a", … }) ``.
#[test]
fn two_rows_answering_refuse() {
    let d = dir();
    // The roster file refuses two rows of one email; a roster built another way (a projection,
    // a stale copy) is still held to it here.
    let mut r = roster(&[("m22", None, Some(BRONTO)), ("a", Some(&d), Some(UNICORN))]);
    let mut twin = r.row("a").unwrap().clone();
    twin.label = "b".to_owned();
    twin.dir = Some(format!("{d}2"));
    r.rows.push(twin);
    assert_eq!(r.row_answering(UNICORN), None);
    assert_eq!(
        reconcile(&r, &stored(), &seen(UNICORN)),
        Reconciled::Unread(r.row("m22"))
    );
}

/// A one-row roster does not hide a disagreement: the footer names the row the tab really runs
/// on, though it names nothing where the one row agrees.
/// NEGATIVE CONTROL: with the `several()` gate kept for `Other`, the footer names nothing:
/// `` left: None right: Some("m15") ``.
#[test]
fn footer_names_observed_even_with_one_row() {
    let d = dir();
    // The one row the environment names is m15's login directory, yet the profile says bronto
    // and only the default-home row lists bronto: that row is credited.
    let r = roster(&[
        ("m15", Some(&d), Some(UNICORN)),
        ("m22", None, Some(BRONTO)),
    ]);
    let on_m15 = ProcessLogin {
        login: Some(d.clone()),
        ..stored()
    };
    assert_eq!(footer_label(&r, &on_m15, &seen(BRONTO)), Some("m22"));
    let one = roster(&[("m15", Some(&d), Some(UNICORN))]);
    assert_eq!(footer_label(&one, &on_m15, &seen(UNICORN)), None, "agrees");
    let one_bronto = roster(&[
        ("m22", None, Some(BRONTO)),
        ("m15", Some(&d), Some(UNICORN)),
    ]);
    assert_eq!(
        footer_label(&one_bronto, &stored(), &seen(UNICORN)),
        Some("m15")
    );
    let single = roster(&[("m22", None, Some(BRONTO))]);
    let footer = footer_account(&single, &stored(), &seen(UNICORN)).unwrap();
    assert_eq!(
        footer.email.as_deref(),
        Some(UNICORN),
        "the address is named"
    );
}

const NOW: i64 = 1_790_000_000;

fn weekly_wall() -> crate::harness::upgrade::StandingWall {
    crate::harness::upgrade::StandingWall {
        at: (NOW - 10).unsigned_abs(),
        until: Some((NOW + 3600).unsigned_abs()),
        window: Some("seven_day".to_owned()),
        credits: None,
    }
}

fn credits_said() -> UsageSaid {
    UsageSaid {
        at: NOW,
        day: NOW.div_euclid(86_400),
        credits: Some(CreditsRead { until: None }),
        warning: None,
        week: None,
    }
}

/// THE WALL BUG: a default-home tab signed in as unicorn hits its weekly limit while the
/// roster's dir-less row is `m22`. The wall and the `/usage` page are unicorn's row's (`m15`),
/// and `m22` keeps a clean record; with no row listing unicorn, neither is any row's.
/// NEGATIVE CONTROL: with `record_wall` reading `who.me(roster)`, the wall is `m22`'s:
/// `` left: "m22" right: "m15" ``.
#[test]
fn a_wall_and_a_usage_page_are_the_observed_accounts() {
    let d = dir();
    let two = roster(&[
        ("m22", None, Some(BRONTO)),
        ("m15", Some(&d), Some(UNICORN)),
    ]);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path());
    let (_held, captured) = usage_fixture(&store, &two);
    let who = stored();
    let looked = (&who, &seen(UNICORN));
    let wall = record_wall(
        &store,
        &captured,
        "s-tab",
        looked,
        &weekly_wall(),
        NOW - 70,
        NOW,
    );
    assert!(
        matches!(&wall, Recorded::Wall { account, .. } if account == "m15"),
        "{wall:?}"
    );
    let usage = record_usage(&store, &captured, "s-tab", looked, &credits_said(), NOW);
    assert!(
        matches!(&usage, Recorded::Usage { account, .. } if account == "m15"),
        "{usage:?}"
    );
    let state = store.load(NOW).unwrap().0;
    assert_eq!(state.sessions_on("m15"), ["s-tab"]);
    assert_eq!(state.accounts["m15"].walls.len(), 1);
    assert!(state.accounts.get("m22").is_none_or(|a| a.walls.is_empty()));
    assert!(state.sessions_on("m22").is_empty());

    let one = roster(&[("m22", None, Some(BRONTO))]);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path());
    let (_held, captured) = usage_fixture(&store, &one);
    assert_eq!(
        record_wall(
            &store,
            &captured,
            "s-tab",
            looked,
            &weekly_wall(),
            NOW - 70,
            NOW
        ),
        Recorded::NotListed
    );
    assert_eq!(
        record_usage(&store, &captured, "s-tab", looked, &credits_said(), NOW),
        Recorded::NotListed
    );
}

/// THE REFUSAL BUG: an account-wide record of the row the login file names (`m15`, while the
/// environment's belief is `m22`) is written, checked under the lock against the same
/// observation that chose the row; without the observation the row is not the process's.
/// NEGATIVE CONTROL: with `matches_account` checking the belief alone, the record is refused:
/// `` account `m15` changed since observation; nothing recorded ``.
#[test]
fn an_account_record_of_the_observed_row_is_written() {
    let d = dir();
    let two = roster(&[
        ("m22", None, Some(BRONTO)),
        ("m15", Some(&d), Some(UNICORN)),
    ]);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path());
    let (held, _captured) = usage_fixture(&store, &two);
    let path = held.path().join("accounts.toml");
    let who = stored();
    let record = |seen: &Observation| {
        store.observe_account_within(
            &[],
            NOW,
            LoginObservation {
                roster_path: &path,
                row: two.row("m15").unwrap(),
                who: &who,
                seen,
            },
            |state, _| Ok(note_session(state, "s-tab", "m15", None, NOW)),
        )
    };
    assert!(record(&Observation::Unread).is_err());
    record(&seen(UNICORN)).unwrap();
    assert_eq!(store.load(NOW).unwrap().0.sessions_on("m15"), ["s-tab"]);
}

/// A Mac with no roster file reads an empty roster: no row is believed, the observed login is
/// unlisted, and the footer still names its address, with nothing to contradict.
/// NEGATIVE CONTROL: with `footer_account` requiring a row, it is `None` and the unwrap panics.
#[test]
fn an_empty_roster_still_names_the_observed_address() {
    let empty = Roster::default();
    let got = reconcile(&empty, &stored(), &seen(UNICORN));
    assert!(matches!(&got, Reconciled::Unlisted(_)), "{got:?}");
    let footer = footer_account(&empty, &stored(), &seen(UNICORN)).unwrap();
    assert_eq!(footer.label, None);
    assert_eq!(footer.email.as_deref(), Some(UNICORN));
    assert_eq!(footer.believed, None);
}

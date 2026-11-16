// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const MAC_A: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
const MAC_B: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const NOW: u64 = 1_800_000_000_000;
const HOUR: u64 = 3_600_000;

const DAY: u64 = 24 * HOUR;

fn obs(account: &str, uuid: &str, at_ms: u64, state: ObsState, tabs: u64) -> Obs {
    Obs {
        account: account.to_owned(),
        uuid: uuid.to_owned(),
        at_ms,
        state,
        tabs,
    }
}

fn heard(uuid: &str, account: &str, tab: Option<&str>, t_ms: u64, ended: bool) -> Heard {
    Heard {
        uuid: uuid.to_owned(),
        account: account.to_owned(),
        tab: tab.map(str::to_owned),
        t_ms,
        ended,
    }
}

/// EVERY MAC THAT SIGHTED AN ACCOUNT IS LISTED WITH ITS OWN NEWEST SIGHTING, WHATEVER ITS AGE
/// AND WHETHER OR NOT A TAB RUNS THERE: a row that says `idle`, a `seen` the machine sweep
/// logged with no tab and a tab's `seen` each date their Mac's sighting; the newest on any Mac
/// is the account's; a `session-end` dates none.
/// NEGATIVE CONTROL: with `fold` dating a sighting only from an event that names a tab, the
/// sweep's Mac reads `left: None right: Some(OnMac { at_ms: 1799999400000, tabs: 0,
/// lapses_ms: None })`.
#[test]
fn every_mac_that_sighted_an_account_is_dated_by_its_own_newest_sighting() {
    let rows = [obs("ac-1", MAC_A, NOW - 90 * DAY, ObsState::Idle, 0)];
    let events = [
        // The sweep found a login place on B and names no tab; it ended a tab there long ago.
        heard(MAC_B, "ac-1", None, NOW - 600_000, false),
        heard(MAC_B, "ac-1", Some("t1"), NOW - 60_000, true),
        // A's row is older than a `seen` A logged.
        heard(MAC_A, "ac-1", None, NOW - 30 * DAY, false),
        // Another account's Mac only ever ended a tab: it was never sighted.
        heard(MAC_A, "ac-2", Some("t9"), NOW - 5_000, true),
    ];
    let folded = fold(&rows, &events, NOW, NOW);
    let one = &folded["ac-1"];
    assert_eq!(
        one.macs.get(MAC_B).copied(),
        Some(OnMac {
            at_ms: NOW - 600_000,
            tabs: 0,
            lapses_ms: None,
        })
    );
    assert_eq!(
        one.macs.get(MAC_A).copied(),
        Some(OnMac {
            at_ms: NOW - 30 * DAY,
            tabs: 0,
            lapses_ms: None,
        })
    );
    assert_eq!(one.last(), Some((NOW - 600_000, MAC_B)));
    assert_eq!(folded.get("ac-2"), None);
}

/// A MAC'S RUNNING TABS ARE THE MORE OF ITS ROW'S COUNT AND ITS DISTINCT LIVE TABS, AND THE
/// FOLD NAMES THE INSTANT THAT NUMBER NEXT CHANGES BY ITSELF: a row or a tab not refreshed for
/// two hours runs nothing, and folding again at the named instant, and not a millisecond
/// before, gives the next number.
/// NEGATIVE CONTROL: with `Running::lapses` naming the first lapse of any sighting instead of
/// the first at which the count changes, `the row's lapse changes nothing` fails.
#[test]
fn a_macs_running_tabs_lapse_at_the_instant_the_fold_names() {
    let rows = [obs("ac-1", MAC_A, NOW - HOUR, ObsState::Running, 2)];
    let events = [
        heard(MAC_A, "ac-1", Some("t1"), NOW - 30 * 60_000, false),
        heard(MAC_A, "ac-1", Some("t2"), NOW - 20 * 60_000, false),
        heard(MAC_A, "ac-1", Some("t3"), NOW - 10 * 60_000, false),
        // t3 ended since: it runs no more, and its end dates nothing.
        heard(MAC_A, "ac-1", Some("t3"), NOW - 60_000, true),
    ];
    let on = |now: u64| fold(&rows, &events, NOW, now)["ac-1"].macs[MAC_A];
    let now = on(NOW);
    assert_eq!(now.at_ms, NOW - 10 * 60_000);
    assert_eq!(now.tabs, 2);
    // The row lapses first (written an hour ago) and the two heard tabs still stand at 2.
    let first = NOW - 30 * 60_000 + SEEN_LIVE_MS + 1;
    assert_eq!(
        now.lapses_ms,
        Some(first),
        "the row's lapse changes nothing"
    );
    assert_eq!(on(first - 1).tabs, 2);
    let then = on(first);
    assert_eq!(then.tabs, 1);
    let second = NOW - 20 * 60_000 + SEEN_LIVE_MS + 1;
    assert_eq!(then.lapses_ms, Some(second));
    let last = on(second);
    assert_eq!((last.tabs, last.lapses_ms), (0, None));
    // The sighting itself stays, however old.
    assert_eq!(last.at_ms, NOW - 10 * 60_000);
}

/// A ROW THAT SAYS `signed-out` IS NO SIGHTING, AND ENDS WHAT ITS MAC SIGHTED UP TO ITS WRITE:
/// it dates nothing, a tab seen before it runs the account no more, and the fold names the Mac
/// as signed out for as long as that Mac sights nothing later. A `seen` it logs after its row
/// said so is a login that is back: the tab runs, and the Mac is not signed out.
/// NEGATIVE CONTROLS: with `fold` dating a sighting from every row whatever it says (as
/// before), `the earlier sighting still dates it` fails `left: 1799999900000 right:
/// 1799999400000`; with a tab's sighting outliving the row, `the row ended the tab` fails
/// `left: 1 right: 0`.
#[test]
fn a_row_that_says_signed_out_is_no_sighting_and_ends_what_its_mac_sighted() {
    let rows = [
        obs("ac-1", MAC_A, NOW - 100_000, ObsState::SignedOut, 0),
        obs("ac-1", MAC_B, NOW - 600_000, ObsState::SignedOut, 0),
        obs("ac-2", MAC_A, NOW - 100_000, ObsState::SignedOut, 0),
    ];
    let events = [
        // A's tab was seen before its row said so; B's after.
        heard(MAC_A, "ac-1", Some("t1"), NOW - 600_000, false),
        heard(MAC_B, "ac-1", Some("t2"), NOW - 60_000, false),
    ];
    let folded = fold(&rows, &events, NOW, NOW);
    let one = &folded["ac-1"];
    let a = one.macs[MAC_A];
    assert_eq!(
        a.at_ms,
        NOW - 600_000,
        "the earlier sighting still dates it"
    );
    assert_eq!(a.tabs, 0, "the row ended the tab");
    assert_eq!(a.lapses_ms, None);
    let b = one.macs[MAC_B];
    assert_eq!((b.at_ms, b.tabs), (NOW - 60_000, 1));
    assert_eq!(b.lapses_ms, Some(NOW - 60_000 + SEEN_LIVE_MS + 1));
    assert_eq!(
        one.out,
        BTreeMap::from([(MAC_A.to_owned(), NOW - 100_000)]),
        "B sighted it since its row said so"
    );
    // The account only a `signed-out` row names was sighted nowhere.
    let two = &folded["ac-2"];
    assert_eq!(two.last(), None);
    assert!(two.macs.is_empty());
    assert_eq!(two.out.get(MAC_A), Some(&(NOW - 100_000)));
}

/// A RECORD DATED PAST THE CLOCK-SKEW BOUND AHEAD OF ITS READ IS NO SIGHTING, AND ONE INSIDE THE
/// BOUND IS A SIGHTING AT THE READ: a Mac whose clock runs a day ahead dates nothing, runs no
/// tab and wins no "newest", at that read or at any later fold of it, and no sighting is dated
/// at an instant after the read, when nothing was observed.
/// NEGATIVE CONTROLS: with `fold` believing every record, the account only clocks a day ahead
/// name reads `left: Some(Sighted { macs: {"0a0a…": OnMac { at_ms: 1800000000000, tabs: 0,
/// lapses_ms: None }, "0b0b…": OnMac { at_ms: 1800000000000, tabs: 0, lapses_ms: None }},
/// out: {"0a0a…": 1800000000000} }) right: None`, both Macs sighting it at the read; with a
/// sighting inside the bound not held to the read, `at the read` fails `left: 1800000900000
/// right: 1800000000000`.
#[test]
fn a_record_dated_past_the_skew_bound_ahead_of_its_read_is_no_sighting() {
    let rows = [
        obs("ac-1", MAC_A, NOW + DAY, ObsState::Running, 2),
        obs("ac-1", MAC_A, NOW + DAY, ObsState::SignedOut, 0),
        // Exactly at the bound: believed.
        obs("ac-2", MAC_B, NOW + SKEW_MS, ObsState::Idle, 0),
    ];
    let events = [
        heard(MAC_A, "ac-1", Some("t1"), NOW + SKEW_MS + 1, false),
        heard(MAC_B, "ac-1", None, NOW + DAY, false),
        heard(MAC_A, "ac-2", Some("t2"), NOW + 60_000, false),
    ];
    let folded = fold(&rows, &events, NOW, NOW);
    assert_eq!(folded.get("ac-1"), None);
    let two = &folded["ac-2"];
    assert_eq!(two.macs[MAC_B].at_ms, NOW, "at the read");
    assert_eq!((two.macs[MAC_A].at_ms, two.macs[MAC_A].tabs), (NOW, 1));
    assert_eq!(two.last(), Some((NOW, MAC_B)));
    // The read is what a record is ahead of: folding it later believes no more of it.
    assert_eq!(fold(&rows, &events, NOW, NOW + 2 * DAY).get("ac-1"), None);
}

/// AN `account-obs` ROW IS A MAC'S WORD ONLY UNDER THAT MAC'S OWN KEY: `<account>.<uuid>`, the
/// prefix the account its body names. Any other key is skipped. SO IS A ROW THAT DOES NOT SAY
/// WHAT A SIGHTING READS OF IT: with no time, with a state this build does not know, or saying
/// a tab runs the account and not how many, it is skipped, and no default (a write at time 0,
/// no tab) stands in for what it lacks.
/// NEGATIVE CONTROLS: with a missing `at` read as 0 (as before), the list holds both `ac-5`
/// rows as `Obs { account: "ac-5", uuid: "0a0a…", at_ms: 0, state: Idle, tabs: 0 }`; with any
/// state but `running` read as idle (as before), it holds `ac-7` as `Idle` and `ac-2`'s
/// `signed-out` row as `Idle`.
#[test]
fn an_observation_row_is_read_only_under_its_writers_key_of_its_account() {
    use crate::typed::owned::{OwnedRead, OwnedRow};
    use aterm_json::json;
    let owned = |value: Value| OwnedRow {
        machine: [0; 16],
        actor: [0; 48],
        value,
        rank: (0, 1, 1),
        operation: [0; 32],
    };
    let row = |account: &str, state: &str, tabs: u64| {
        owned(json!({"account": account, "state": state, "tabs": tabs, "at": NOW}))
    };
    let mut read = OwnedRead::default();
    read.rows
        .insert(format!("ac-1.{MAC_A}"), row("ac-1", "running", 2));
    read.rows
        .insert(format!("ac-1.{MAC_B}"), row("ac-1", "idle", 0));
    read.rows
        .insert(format!("ac-2.{MAC_A}"), row("ac-2", "signed-out", 0));
    // Another account's body under this account's key, and a key that ends in no uuid.
    read.rows
        .insert(format!("ac-3.{MAC_A}"), row("ac-9", "running", 1));
    read.rows
        .insert("ac-4.nothex".to_owned(), row("ac-4", "running", 1));
    // No time; a time that is no number; a tab said to run and no count; an unknown state.
    read.rows.insert(
        format!("ac-5.{MAC_A}"),
        owned(json!({"account": "ac-5", "state": "idle", "tabs": 0})),
    );
    read.rows.insert(
        format!("ac-5.{MAC_B}"),
        owned(json!({"account": "ac-5", "state": "idle", "tabs": 0, "at": "now"})),
    );
    read.rows.insert(
        format!("ac-6.{MAC_A}"),
        owned(json!({"account": "ac-6", "state": "running", "at": NOW})),
    );
    read.rows
        .insert(format!("ac-7.{MAC_A}"), row("ac-7", "parked", 0));
    // An idle row says no tab runs it: it needs no count.
    read.rows.insert(
        format!("ac-8.{MAC_A}"),
        owned(json!({"account": "ac-8", "state": "idle", "at": NOW})),
    );
    assert_eq!(
        obs_rows(&read),
        vec![
            obs("ac-1", MAC_A, NOW, ObsState::Running, 2),
            obs("ac-1", MAC_B, NOW, ObsState::Idle, 0),
            obs("ac-2", MAC_A, NOW, ObsState::SignedOut, 0),
            obs("ac-8", MAC_A, NOW, ObsState::Idle, 0),
        ]
    );
}

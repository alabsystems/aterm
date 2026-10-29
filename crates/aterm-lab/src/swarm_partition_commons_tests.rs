// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn after(mac: &str, state: &str, heads: &[&str], rows: &[&str]) -> After {
    After {
        mac: mac.to_owned(),
        state: state.to_owned(),
        heads: heads.iter().map(|h| (*h).to_owned()).collect(),
        rows: rows.iter().map(|r| (*r).to_owned()).collect(),
        unreadable: None,
    }
}

const ROWS: [&str; 3] = ["a-in-cut", "b-in-cut", "before-cut"];

/// A run where Commons came back whole: both newcomers in, both rows on every Mac, one head.
fn whole() -> Evidence {
    let wrote = |row: &str, by: &str, island| Wrote {
        row: row.to_owned(),
        by: by.to_owned(),
        island,
        error: None,
    };
    let enrolled = |mac: &str, by: &str, island| Enrolled {
        mac: mac.to_owned(),
        island,
        by: by.to_owned(),
        error: None,
    };
    Evidence {
        skipped: None,
        before: ["a1", "a2", "b1", "b2"].map(str::to_owned).to_vec(),
        enrolled: vec![
            enrolled("a3", "a1", Island::A),
            enrolled("b3", "b1", Island::B),
        ],
        wrote: vec![
            wrote("before-cut", "a1", Island::A),
            wrote("a-in-cut", "a1", Island::A),
            wrote("b-in-cut", "b1", Island::B),
        ],
        failed_exchanges: Vec::new(),
        after: ["a1", "a2", "a3", "b1", "b2", "b3"]
            .iter()
            .map(|m| after(m, "ready", &["h"], &ROWS))
            .collect(),
        heal_ms: 900,
    }
}

fn failed(ev: &Evidence) -> Vec<&'static str> {
    judge(ev)
        .into_iter()
        .filter(|c| !c.pass)
        .map(|c| c.name)
        .collect()
}

#[test]
fn a_whole_commons_passes_every_check() {
    assert_eq!(judge(&whole()).len(), 3);
    assert_eq!(failed(&whole()), Vec::<&str>::new());
}

#[test]
fn two_islands_that_each_let_a_mac_in_and_never_recover_fail_one_history_and_the_rows() {
    // What main's two-authority fork looks like: competing heads, tables refused.
    let mut ev = whole();
    for a in &mut ev.after {
        a.state = "conflict".to_owned();
        a.heads = vec!["h1".to_owned(), "h2".to_owned()];
        a.rows.clear();
        a.unreadable = Some("Commons membership is not ready for table access".to_owned());
    }
    assert_eq!(
        failed(&ev),
        vec![
            "commons: one control history on every Mac after the heal",
            "commons: every row written on either side is on every Mac after the heal",
        ]
    );
}

#[test]
fn macs_each_ready_on_a_different_head_are_not_one_history() {
    let mut ev = whole();
    ev.after[5].heads = vec!["other".to_owned()];
    assert_eq!(
        failed(&ev),
        vec!["commons: one control history on every Mac after the heal"]
    );
}

#[test]
fn a_row_missing_on_one_mac_is_lost() {
    let mut ev = whole();
    ev.after[0].rows.retain(|r| r != "b-in-cut");
    let checks = judge(&ev);
    let rows = checks.iter().find(|c| !c.pass).unwrap();
    assert!(rows.detail.contains("a1 lacks b-in-cut"), "{}", rows.detail);
}

#[test]
fn an_island_that_could_not_let_its_newcomer_in_or_write_did_not_keep_working() {
    let mut ev = whole();
    ev.enrolled[1].error = Some("not a member of the vault".to_owned());
    assert_eq!(
        failed(&ev),
        vec!["commons: each island enrolled and wrote during the cut"]
    );
    let mut ev = whole();
    ev.wrote[2].error = Some("its session: busy".to_owned());
    let got = failed(&ev);
    assert!(
        got.contains(&"commons: each island enrolled and wrote during the cut"),
        "{got:?}"
    );
}

#[test]
fn a_plane_that_did_not_run_is_one_failed_check_naming_why() {
    let ev = Evidence {
        skipped: Some("a1 holds no Commons membership".to_owned()),
        ..Evidence::default()
    };
    let checks = judge(&ev);
    assert_eq!(checks.len(), 1);
    assert!(!checks[0].pass);
    assert!(checks[0].detail.contains("a1"), "{}", checks[0].detail);
}

#[test]
fn a_failed_write_before_the_cut_is_not_blamed_on_the_cut() {
    let mut ev = whole();
    ev.wrote[0].error = Some("its session: busy".to_owned());
    assert_eq!(
        failed(&ev),
        Vec::<&str>::new(),
        "only the in-cut rows say whether an island worked"
    );
}

#[test]
fn macs_read_alike_only_with_the_same_history_heads_rows_and_reason() {
    let ev = whole();
    assert!(alike(&ev.after));
    assert!(alike(&[]));
    let mut short = ev.after.clone();
    short[1].rows.retain(|r| r != "b-in-cut");
    assert!(!alike(&short), "a2 lacks a row the others read");
    let mut forked = ev.after.clone();
    forked[2].heads = vec!["h2".to_owned()];
    assert!(!alike(&forked), "a3 is ready on another head");
    let conflict: Vec<After> = ["a1", "b1"]
        .iter()
        .map(|m| After {
            unreadable: Some("not ready for table access".to_owned()),
            ..after(m, "conflict", &["h", "h2"], &[])
        })
        .collect();
    assert!(
        alike(&conflict),
        "a conflict every Mac holds has nothing left to carry"
    );
    let mut pending = conflict.clone();
    pending[0].state = "pending".to_owned();
    assert!(!alike(&pending), "a1's history is in another state");
}

#[test]
fn a_pass_that_moved_nothing_settles_the_repair_only_where_every_exchange_ran() {
    let ev = whole();
    let mut short = ev.after.clone();
    short[4].rows.retain(|r| r != "b-in-cut");
    assert!(
        !settled(&short, &short, 2),
        "b2's exchanges failed: the next pass may still carry its row"
    );
    assert!(
        settled(&short, &short, 0),
        "every exchange ran and nothing moved"
    );
    let mut fuller = short.clone();
    fuller[1].rows.push("x".to_owned());
    assert!(!settled(&short, &fuller, 0), "a read moved");
    assert!(
        settled(&short, &ev.after, 3),
        "every Mac reads alike, whatever failed on the way"
    );
}

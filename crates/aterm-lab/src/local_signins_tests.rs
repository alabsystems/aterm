// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(pool: &str, label: &str, condition: &str) -> Row {
    Row {
        pool: pool.into(),
        label: label.into(),
        condition: condition.into(),
    }
}

/// Every row of side `side` as the lane lays it out and a correct report says it.
fn as_laid(side: char) -> BTreeSet<Row> {
    let mut rows = BTreeSet::new();
    for (cli, letter) in CLIS {
        for laid in Laid::ALL {
            rows.insert(row(cli.id(), &label(side, letter, laid), expected(laid)[0]));
        }
    }
    rows
}

#[test]
fn a_report_as_laid_has_no_wrong_row_and_an_expired_row_said_unseen_is_wrong() {
    let rows = as_laid('a');
    assert!(wrong_rows('a', &rows).is_empty());
    // The defect E4 found on main: an expired login reported like a signed-in one.
    let mut unseen = rows.clone();
    unseen.retain(|r| r.label != "ag-exp");
    unseen.insert(row("grok", "ag-exp", "unseen"));
    assert_eq!(
        wrong_rows('a', &unseen),
        vec![("ag-exp".to_string(), Laid::Expired, "unseen".to_string())]
    );
    let mut missing = rows;
    missing.retain(|r| r.label != "ac-in");
    assert_eq!(wrong_rows('a', &missing)[0].2, "missing");
}

#[test]
fn another_macs_rows_are_foreign_and_its_own_are_not() {
    let mut rows = as_laid('a');
    assert!(foreign('a', &rows).is_empty());
    rows.insert(row("codex", "bx-in", "unseen"));
    assert_eq!(foreign('a', &rows), vec!["bx-in".to_string()]);
    // A row of no lane CLI letter is no lane label.
    rows.insert(row("kimi", "bk-in", "unseen"));
    assert_eq!(foreign('a', &rows).len(), 1);
}

#[test]
fn survey_rows_are_read_per_host_and_report_rows_carry_their_cli() {
    let reply: aterm_json::Value = aterm_json::from_str(
        r#"[{"instance":{"node":"n1"},"accounts":[
            {"pool":"claude","id":"ac-in","condition":"unseen"},
            {"pool":"grok","id":"ag-exp","condition":"signed-out"}]},
           {"instance":{"node":"n2"},"accounts":[]}]"#,
    )
    .unwrap();
    let rows = survey_rows(&reply);
    assert_eq!(rows["n1"].len(), 2);
    assert!(rows["n2"].is_empty());
    let report: aterm_json::Value = aterm_json::from_str(
        r#"{"pools":[{"cli":"codex","rows":[{"label":"ax-in","condition":"usable"}]}]}"#,
    )
    .unwrap();
    assert_eq!(
        report_rows(&report).into_iter().collect::<Vec<_>>(),
        vec![row("codex", "ax-in", "usable")]
    );
}

#[test]
fn a_proxied_request_is_known_by_its_host_alone() {
    assert_eq!(
        proxied_host("CONNECT api.anthropic.com:443 HTTP/1.1"),
        "api.anthropic.com:443"
    );
    assert_eq!(
        proxied_host("GET http://chatgpt.com/backend-api/wham/usage?token=x HTTP/1.1"),
        "chatgpt.com"
    );
    assert_eq!(proxied_host(""), "?");
}

#[test]
fn the_median_is_the_middle_of_the_sorted_times() {
    assert_eq!(median(&[30, 10, 20]), Some(20));
    assert_eq!(median(&[]), None);
}

#[test]
fn only_a_path_beside_the_mark_is_a_standin() {
    let dir = aterm_tempfile::Builder::new().tempdir().unwrap();
    let link = dir.path().join("codex");
    assert!(!is_standin(&link.to_string_lossy()));
    std::fs::write(dir.path().join(STANDIN_MARK), "ran.log").unwrap();
    assert!(is_standin(&link.to_string_lossy()));
    assert!(!is_standin("codex"));
}

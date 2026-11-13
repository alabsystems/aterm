// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::reveal_tab::{AxEvidence, evidence_json};

/// A run's checks as `judge` names them: the ones off the tree `off` each, and the tree's
/// three as `tree` says (none: not read).
fn checks(off: bool, tree: Option<[bool; 3]>) -> Vec<(String, bool)> {
    let mut checks = vec![
        ("the import sealed the synthetic logins".to_owned(), true),
        ("`text` of the Editor tab refuses".to_owned(), off),
    ];
    if let Some(tree) = tree {
        checks.extend(
            TREE_CHECKS
                .iter()
                .zip(tree)
                .map(|(n, h)| ((*n).to_owned(), h)),
        );
    }
    checks
}

/// The row holds when every check does; its control holds only when "and no value" fails,
/// whatever the grid read. Neither holds with the tree unread or a check off it failed.
#[test]
fn a_row_holds_on_every_check_and_its_control_only_on_a_seen_value() {
    assert_eq!(verdict(&checks(true, Some([true; 3])), false), Ok(()));
    assert!(
        verdict(&checks(true, Some([true; 3])), true)
            .unwrap_err()
            .starts_with("CONTROL:")
    );
    for seen in [
        [false, true, false],
        [false, false, false],
        [true, true, false],
    ] {
        assert_eq!(verdict(&checks(true, Some(seen)), true), Ok(()));
        assert!(
            verdict(&checks(true, Some(seen)), false)
                .unwrap_err()
                .contains("and no value")
        );
    }
    for control in [false, true] {
        assert!(
            verdict(&checks(true, None), control)
                .unwrap_err()
                .contains("not read")
        );
        let off = verdict(&checks(false, Some([false, true, false])), control).unwrap_err();
        assert!(off.contains("`text` of the Editor tab refuses"), "{off}");
    }
}

/// The cells name every tree check, NOT MEASURED with why where the tree was not read, and
/// the failures off it by name.
#[test]
fn the_cells_name_each_tree_check_and_each_failure() {
    let read = Evidence {
        ax: Some(AxEvidence::default()),
        ..Evidence::default()
    };
    let cells = cells(&read, &checks(false, Some([true, true, false])));
    let text: Vec<String> = cells.iter().map(|(n, v)| format!("{n}={v}")).collect();
    assert_eq!(
        text,
        [
            "checks=3 of 5 held".to_owned(),
            format!("tree: {}=PASS", TREE_CHECKS[0]),
            format!("tree: {}=PASS", TREE_CHECKS[1]),
            format!("tree: {}=FAIL", TREE_CHECKS[2]),
            "failed off the tree=`text` of the Editor tab refuses".to_owned(),
        ]
    );
    let unread = cells_unread();
    assert!(
        unread
            .iter()
            .filter(|(n, _)| n.starts_with("tree: "))
            .all(|(_, v)| matches!(v, Value::Missing(why) if why.contains("Accessibility grant")))
    );
}

fn cells_unread() -> Vec<(String, Value)> {
    cells(&Evidence::default(), &checks(true, None))
}

/// The report's evidence is read from its last report line, after anything else the run
/// printed, and only when it was made under the publication the row asked for.
#[test]
fn the_evidence_is_the_report_lines_under_the_rows_publication() {
    let ev = Evidence {
        imported: true,
        ax: Some(AxEvidence {
            grid_read: true,
            logins_read: true,
            clean: false,
        }),
        ..Evidence::default()
    };
    let report = |plain: bool| {
        aterm_json::to_string(&aterm_json::json!({
            "lane": "reveal-tab",
            "pass": false,
            "plain_publication": plain,
            "evidence": evidence_json(&ev),
        }))
        .unwrap()
    };
    let stdout = format!("waiting for a quiet machine\n{}\n", report(true));
    assert_eq!(report_evidence(&stdout, true), Some(ev.clone()));
    assert_eq!(report_evidence(&stdout, false), None);
    assert_eq!(report_evidence(&report(false), false), Some(ev));
    assert_eq!(report_evidence("", false), None);
    assert_eq!(
        report_evidence(
            "{\"lane\":\"reveal-tab\",\"plain_publication\":false}",
            false
        ),
        None
    );
}

/// The run reads its tree through the granted helper, writes its status last, and names the
/// negative control only for the control row; the aterm it runs is named `aterm`.
#[test]
fn the_run_reads_through_the_helper_and_names_the_control_alone() {
    let plain = run_script("/var/tmp/aterm-nux/reveal", false);
    let control = run_script("/var/tmp/aterm-nux/reveal", true);
    for script in [&plain, &control] {
        assert!(script.starts_with(
            "'/var/tmp/aterm-nux/reveal/aterm-lab' reveal-tab --json --ax-helper /usr/local/libexec/aterm-lab-guest"
        ));
        assert!(
            script.contains(" '/var/tmp/aterm-nux/reveal/aterm' > /var/tmp/aterm-nux/reveal.json")
        );
        assert!(script.ends_with("; echo $? > /var/tmp/aterm-nux/reveal.exit"));
        // The agent's plist carries it as text: nothing XML would read as markup.
        assert!(!script.contains('<') && !script.contains('&'));
    }
    assert!(!plain.contains("--plain-publication"));
    assert!(control.contains(" --plain-publication '"));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const FIRST: &str = "s-0a1b2c3d4e5f60718293";

fn row(kind: &str, text: &str, result: Option<&str>) -> Row {
    Row {
        kind: kind.into(),
        text: text.into(),
        result: result.map(str::to_string),
        ..Row::default()
    }
}

/// THE RUN'S SCRIPT KEYS EACH CONVERSATION BY ITS TOKEN: the manager spawns each worker in its
/// own worktree; each worker changes the one file's function and holds on its gate; the first
/// answers Land with its commit and push held on its second gate, then its defense, and the land
/// check with `LANDED`; the second answers with a reply that is no defense.
#[test]
fn the_script_keys_each_conversation_by_its_token() {
    let plan = Plan::new("abc", Path::new("/w"));
    let steps = plan.script();
    let steps = steps.as_arr().expect("a list");
    let when = |t: &str| {
        steps
            .iter()
            .filter(|s| s.get("when").and_then(J::as_str) == Some(t))
            .count()
    };
    assert_eq!(
        (
            when(&plan.mgr),
            when(&plan.first.token),
            when(&plan.second.token)
        ),
        (3, 5, 3)
    );
    let spawn = Plan::spawn_command(&plan.first);
    assert!(
        spawn.starts_with("aterm ctl @self spawn agent=claude cwd=/w/wa - <<'EOF'\n"),
        "{spawn}"
    );
    assert!(spawn.contains(&plan.first.token), "{spawn}");
    let change = plan.change_command(&plan.second);
    assert!(
        change.starts_with("printf 'pub fn shared() -> u32 {\\n    2\\n}\\n' > src/lib.rs && "),
        "{change}"
    );
    assert!(
        change.contains(&format!("[ -e /w/gates/{}-1 ]", plan.second.token)),
        "{change}"
    );
    let land = plan.land_command();
    assert!(
        land.contains(&format!("[ -e /w/gates/{}-2 ]", plan.first.token)),
        "{land}"
    );
    assert!(land.contains("git push -q origin HEAD:main"), "{land}");
    assert!(land.contains("export TMPDIR=/w/tmp && "), "{land}");
    assert!(plan.defense.starts_with("Nothing left:"));
    assert_eq!(plan.landed, "LANDED");
    assert!(!plan.not_yet.starts_with("Nothing left:"));
}

/// THE SECOND'S PROMPT IS READ WHERE ATERM TYPED IT: the first user row with words after the
/// worker's report, a tool result skipped; none before the report is. The typed overlap rows are
/// the ledger's `fact-overlap@v1` rows `typed`, and a fact naming a path, a file or a worktree
/// names a place.
#[test]
fn the_seconds_prompt_and_ledger_rows_are_read_where_aterm_typed_them() {
    let rows = [
        row("user", "the task", None),
        row("assistant", "the report", None),
        row("user", "", Some("ok")),
        row("user", "Land's instruction (aterm: x)", None),
    ];
    assert_eq!(
        typed_after(&rows, "the report").map(|r| r.text.as_str()),
        Some("Land's instruction (aterm: x)")
    );
    assert_eq!(typed_after(&rows[..2], "the report"), None);
    let ledger = format!(
        "{{\"rule_id\":\"fact-overlap@v1\",\"decision\":\"typed\",\"command\":\"{}\"}}\n\
         {{\"rule_id\":\"fact-intent@v1\",\"decision\":\"waiting\",\"command\":\"\"}}\n\
         {{\"rule_id\":\"fact-overlap@v1\",\"decision\":\"skipped\",\"command\":\"y\"}}\n",
        told(FIRST)
    );
    assert_eq!(overlaps_typed(&ledger), [told(FIRST)]);
    assert!(names_no_place(&told(FIRST)));
    for placed in [
        "1 file at src/lib.rs",
        "lib.rs is changed",
        "changed in wa",
        "on main",
    ] {
        assert!(!names_no_place(placed), "{placed}");
    }
}

/// A SURVEY RECORD'S WORK FIELDS ARE READ BY SID: its `work=`, whether `work_paths=` names the
/// file, its `work_ms=`; a session showing no record has no `work=`, and another sid is none.
#[test]
fn a_survey_records_work_fields_are_read_by_sid() {
    let survey = format!(
        r#"[{{"label":"here","sessions":[{{"sid":"{FIRST}","work":"wa","work_paths":"README.md\nsrc/lib.rs","work_ms":"21500"}},{{"sid":"s-2"}}]}}]"#
    );
    let first = session_of(&survey, FIRST).expect("the first");
    assert_eq!(
        work_of(&first),
        (Some("wa".to_string()), true, Some(21_500))
    );
    let bare = session_of(&survey, "s-2").expect("the second");
    assert_eq!(work_of(&bare), (None, false, None));
    assert!(session_of(&survey, "s-3").is_none());
}

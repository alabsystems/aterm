// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEADER: &str = "run v=1 boot=B id=42-7 uniqueid=10 start=100 sid=-\nroot path=/owned\n";
const FIRST: &str =
    "group pgid=50 leader_birth=8 leader_uniqueid=11 stage=first%20stage\nclosed pgid=50\n";
const SECOND: &str =
    "group pgid=50 leader_birth=9 leader_uniqueid=12 stage=second%20stage\nclosed pgid=50\n";

#[test]
fn complete_archives_keep_each_closed_generation_and_zero_group_runs() {
    let text = format!("{HEADER}{FIRST}{SECOND}end at=1\n");
    let record = completed_archive(&text).unwrap();
    assert_eq!(
        record.id,
        Some(RunId {
            pid: 42,
            birth: Birth(7)
        })
    );
    assert_eq!(record.boot, "B");
    assert_eq!(record.roots, vec![PathBuf::from("/owned")]);
    assert_eq!(record.groups.len(), 2);
    assert_eq!(record.groups[0].leader_birth, Birth(8));
    assert_eq!(record.groups[1].leader_birth, Birth(9));
    assert!(record.groups.iter().all(|group| group.closed));
    // Wall time may move backwards; the native writer makes no timestamp-order promise.
    assert!(
        completed_archive(&format!("{HEADER}end at=0\n"))
            .unwrap()
            .groups
            .is_empty()
    );
}

#[test]
fn incomplete_unknown_or_premature_terminal_archives_do_not_qualify() {
    let good = format!("{HEADER}{FIRST}{SECOND}end at=1\n");
    let bad = [
        good.trim_end_matches('\n').to_owned(),
        good.replace("end at=1\n", ""),
        format!("{HEADER}end at=1\n{FIRST}"),
        format!("{good}end at=2\n"),
        format!("{HEADER}future field=value\nend at=1\n"),
        good.replace("run v=1", "run v=2"),
        good.replace("id=42-7", "id=1-7"),
        good.replace("id=42-7", "id=42-0"),
        good.replace("stage=first%20stage", "stage=first%Q0stage"),
        good.replace("root path=/owned", "root path=%FF"),
        good.replace("leader_birth=8", "leader_birth=0"),
        good.replace("leader_uniqueid=11", "leader_uniqueid=0"),
        good.replace("leader_birth=8 ", ""),
    ];
    for text in bad {
        assert!(completed_archive(&text).is_none(), "{text:?}");
    }
}

#[test]
fn a_close_must_follow_its_exact_latest_group_generation() {
    for text in [
        format!("{HEADER}closed pgid=50\nend at=1\n"),
        format!("{HEADER}closed pgid=50\n{FIRST}end at=1\n"),
        format!("{HEADER}{FIRST}closed pgid=50\nend at=1\n"),
        format!(
            "{HEADER}{}{}end at=1\n",
            FIRST.replace("closed pgid=50\n", ""),
            SECOND
        ),
        format!(
            "{HEADER}{FIRST}{}end at=1\n",
            SECOND.replace("closed pgid=50\n", "")
        ),
    ] {
        assert!(completed_archive(&text).is_none(), "{text:?}");
    }
}

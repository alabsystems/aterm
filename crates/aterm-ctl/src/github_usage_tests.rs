// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn missing_and_incomplete_hours_never_become_zero_and_raw_paths_never_cross_the_fleet() {
    let root = aterm_tempfile::tempdir().unwrap();
    let empty = survey(root.path(), 7200);
    assert_eq!(
        empty,
        [
            "github_rate hour=1 status=unrecorded",
            "github_rate hour=2 status=unrecorded"
        ]
    );
    let rate = root.path().join("agh/rate");
    std::fs::create_dir_all(&rate).unwrap();
    let calls = rate.join("calls");
    aterm_uds::private_file::create_private_dir(&calls).unwrap();
    aterm_uds::private_file::write_private(&calls, "1.jsonl", b"{\"as\":\"dev\",\"caller\":\"cli\",\"path\":\"/repos/private/secret/contents/secret-filename\",\"status\":200}\n").unwrap();
    let recorded = survey(root.path(), 7200).join("\n");
    assert!(recorded.contains("status=recorded attempts=1"));
    assert!(!recorded.contains("secret"));
    aterm_uds::private_file::write_private(&calls, "1.incomplete", b"full").unwrap();
    assert!(survey(root.path(), 7200)[0].contains("status=incomplete"));
}

#[test]
fn counts_attempts_not_shared_counter_deltas_and_keeps_missing_evidence() {
    let rows = vec![
        json!({"as":"dev","caller":"cli","path":"/repos/secret-owner/private/contents/password?token=secret","status":200,"rate":{"used":1}}),
        json!({"as":"dev","caller":"cli","path":"/repos/secret-owner/private/contents/password","status":304,"rate":{"used":1171}}),
        json!({"as":"dev","caller":"cli","path":"/repos/secret-owner/private/contents/password","status":null}),
    ];
    let summary = summarize(&rows, 1);
    assert_eq!(summary[0]["counted"], json!(1));
    assert_eq!(summary[0]["resource"], json!("core"));
    assert_eq!(summary[0]["free"], json!(1));
    assert_eq!(summary[0]["unknown"], json!(1));
    assert!(!summary[0].to_string().contains("password"));
    assert!(!summary[0].to_string().contains("secret"));
}

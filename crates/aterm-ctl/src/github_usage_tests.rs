// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The survey reads agh's book under aterm's state root on Unix and Windows alike: a missing
/// hour is `unrecorded`, a recorded one counts its attempts without a raw path, one whose bound
/// was reached is `incomplete`, and an unknown root is `unavailable`, never zero use.
/// NEGATIVE CONTROL (2026-11-17, on Windows): with `survey`'s read gated `cfg(unix)` again (its
/// Windows answer `status=unavailable`), this failed at its first assertion, left
/// `["github_rate status=unavailable"]`.
#[cfg(any(unix, windows))]
#[test]
fn missing_and_incomplete_hours_never_become_zero_and_raw_paths_never_cross_the_fleet() {
    let root = aterm_tempfile::tempdir().unwrap();
    let empty = survey(Some(root.path()), 7200);
    assert_eq!(
        empty,
        [
            "github_rate hour=1 status=unrecorded",
            "github_rate hour=2 status=unrecorded"
        ]
    );
    let calls = calls_dir(root.path());
    std::fs::create_dir_all(calls.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&calls).unwrap();
    aterm_uds::private_file::write_private(&calls, "1.jsonl", b"{\"as\":\"dev\",\"caller\":\"cli\",\"path\":\"/repos/private/secret/contents/secret-filename\",\"status\":200}\n").unwrap();
    let recorded = survey(Some(root.path()), 7200).join("\n");
    assert!(
        recorded.contains("status=recorded attempts=1"),
        "{recorded}"
    );
    assert!(!recorded.contains("secret"));
    aterm_uds::private_file::write_private(&calls, "1.incomplete", b"full").unwrap();
    assert!(survey(Some(root.path()), 7200)[0].contains("status=incomplete"));
    assert_eq!(survey(None, 7200), ["github_rate status=unavailable"]);
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

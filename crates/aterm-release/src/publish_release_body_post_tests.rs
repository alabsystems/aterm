// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn notes_file(label: &str, contents: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = crate::test_scratch_dir(label);
    let path = dir.path().join("notes-0.87.0.md");
    fs::write(&path, contents).expect("write release-notes fixture");
    (dir, path)
}

#[test]
fn an_oversized_notes_file_on_disk_is_bounded_at_the_post() {
    let oversized =
        "### Section\n\n- an entry — with em dashes — long enough to matter\n\n".repeat(4000);
    assert!(
        changelog::github_length(&oversized) > changelog::GITHUB_RELEASE_BODY_LIMIT,
        "fixture must actually overflow: {} UTF-16 units",
        changelog::github_length(&oversized)
    );
    let (_dir, path) = notes_file("oversized", &oversized);
    let posted = release_body_for_post("draft", &path).expect("read notes");
    assert!(
        changelog::github_length(&posted) <= changelog::GITHUB_RELEASE_BODY_LIMIT,
        "the POST would carry {} UTF-16 units ({} bytes), over the {} GitHub takes",
        changelog::github_length(&posted),
        posted.len(),
        changelog::GITHUB_RELEASE_BODY_LIMIT
    );
    let payload = release_create_payload("v0.87.0", "aterm 0.87.0", &posted).unwrap();
    let request: aterm_json::Value =
        aterm_json::from_str(std::str::from_utf8(&payload).unwrap()).unwrap();
    assert_eq!(
        request.get("body").and_then(aterm_json::Value::as_str),
        Some(&*posted)
    );
    assert_eq!(
        request.get("draft").and_then(aterm_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        fs::read_to_string(&path).expect("re-read fixture").len(),
        oversized.len()
    );
}

#[test]
fn a_422_post_failure_judges_body_length_in_utf16_characters_not_utf8_bytes() {
    let out = RunOut {
        status: CURL_EXIT_HTTP_ERROR,
        stdout: br#"{"message":"Validation Failed"}"#.to_vec(),
        stderr: b"curl: (22) The requested URL returned error: 422\n".to_vec(),
    };
    let within = ReleaseBody("—".repeat(changelog::GITHUB_RELEASE_BODY_LIMIT));
    assert!(
        within.len() > changelog::GITHUB_RELEASE_BODY_LIMIT,
        "UTF-8 bytes ({}) exceed the UTF-16 limit ({})",
        within.len(),
        changelog::GITHUB_RELEASE_BODY_LIMIT
    );
    let detail = release_body_post_failure_detail(&out, &within);
    assert!(detail.contains("WITHIN it"), "{detail}");
    assert!(!detail.contains("OVER IT"), "{detail}");

    let over = ReleaseBody("—".repeat(changelog::GITHUB_RELEASE_BODY_LIMIT + 1));
    let detail = release_body_post_failure_detail(&out, &over);
    assert!(detail.contains("OVER IT"), "{detail}");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for one transcript's read ([`super::read_one`]): its deadline cuts it where it is,
//! however far the byte cap would let it go. The history's own tests are in
//! `cli_account_backfill_tests.rs`.

use super::*;

/// A transcript holding one five-hour wall, and what the walk finds of it.
fn one_wall(dir: &Path) -> spend::Found {
    let path = dir.join("s.jsonl");
    let line = r#"{"type":"assistant","isSidechain":false,"timestamp":"2025-09-20T10:00:00Z","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your session limit"}]},"quotaLimits":{"status":"rejected","resetsAt":1758366000,"rateLimitType":"five_hour"},"error":"rate_limit","isApiErrorMessage":true}"#;
    std::fs::write(&path, format!("{line}\n")).expect("write");
    spend::Found {
        rel: "s.jsonl".to_owned(),
        meta: std::fs::metadata(&path).expect("metadata"),
        path,
    }
}

/// A TRANSCRIPT READ PAST ITS DEADLINE IS CUT BY TIME. With room for every byte and a deadline
/// already past, one transcript's read takes no line of it and says `time`; with no bound it
/// reads the wall, whole. Negative controls: a read that does not stop at its deadline (no
/// `Budgeted` reader in `read_one`) reads the wall and says nothing cut, and one that says every
/// short read is the cap's (no `Cut::Time` branch) says `bytes`: the first assertion fails
/// either way.
#[test]
fn a_transcript_read_past_its_deadline_is_cut_by_time() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-backfill-read-")
        .tempdir()
        .expect("temp dir");
    let found = one_wall(tmp.path());
    let past = Limits {
        bytes: u64::MAX,
        until: Some(std::time::Instant::now()),
    };
    let mut out = Walls::default();
    let mut left = u64::MAX;
    assert_eq!(
        read_one(&found, "a@example.test", &mut left, &past, &mut out),
        Some(Cut::Time),
        "{out:?}"
    );
    assert!(out.walls.is_empty(), "{out:?}");
    let mut whole = Walls::default();
    let mut left = u64::MAX;
    assert_eq!(
        read_one(
            &found,
            "a@example.test",
            &mut left,
            &Limits::none(),
            &mut whole
        ),
        None
    );
    assert_eq!(
        whole.walls,
        [Wall {
            at: 1_758_362_400,
            reset: 1_758_366_000,
            w: SESSION_LIMIT,
        }]
    );
}

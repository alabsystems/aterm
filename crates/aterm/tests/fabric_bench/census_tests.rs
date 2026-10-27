// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure census fixtures mounted only by the ordinary fabric test target.

use std::collections::BTreeSet;

use astream_broker::store::{COMMIT_SUBJECT, WILL_SUBJECT};

use super::census::*;

fn rec(seq: u64, subject: &str, text: &str, commit: Option<(&str, u64)>) -> Rec {
    Rec {
        seq,
        producer: (1, seq),
        subject: subject.to_string(),
        text: text.to_string(),
        head: String::new(),
        commit: commit.map(|(g, u)| (g.to_string(), u)),
    }
}

#[test]
fn a_kind_is_the_subject_without_its_identities() {
    assert_eq!(
        class_of("/f/f1/in/n-00ab/s-77/n-00ab/note"),
        "/f/f1/in/<node>/<sid>/<node>/note"
    );
    assert_eq!(
        class_of("/f/f1/pub/n-1/node/presence"),
        "/f/f1/pub/<node>/node/presence"
    );
    assert_eq!(class_of("/a/commit"), "/a/commit");
    assert_eq!(
        class_of("/f/f1/in/n-1/s-2/h-andrew/task"),
        "/f/f1/in/<node>/<sid>/<principal>/task"
    );
}

#[test]
fn a_token_is_read_whole_from_the_head_line() {
    let head = head_line(b"v=1 t=9 state=live inc=3 pid=41 statex=no\nsecond=line");
    assert_eq!(token(&head, "state"), Some("live"));
    assert_eq!(token(&head, "inc"), Some("3"));
    assert_eq!(token(&head, "pid"), Some("41"));
    assert_eq!(token(&head, "second"), None, "the head line only");
    assert_eq!(token(&head, "sta"), None, "a whole key, not a prefix");
}

#[test]
fn a_window_counts_every_record_and_names_the_commits() {
    let group = "/f/f1/cur/n-1/node/inbox";
    let recs = vec![
        rec(4, "/f/f1/in/n-1/s-b/n-1/note", "lat-0", None),
        rec(5, COMMIT_SUBJECT, "", Some((group, 4))),
        rec(6, "/f/f1/in/n-1/s-b/n-1/note", "lat-1", None),
        rec(7, "/f/f1/pub/n-1/s-b/ev", "", None),
        rec(8, COMMIT_SUBJECT, "", Some((group, 6))),
        rec(9, "/a/will", "", None),
    ];
    let c = census(window(&recs, 4, 8));
    assert_eq!((c.total, c.hidden, c.commits), (5, 2, 2));
    assert_eq!(
        c.by_class.get("/f/f1/in/<node>/<sid>/<node>/note"),
        Some(&2)
    );
    assert_eq!(c.by_class.get("/a/commit"), Some(&2));
    assert!(!c.by_class.contains_key("/a/will"), "outside the window");
    assert_eq!(covering_commit(&recs, group, 6, 0).map(|r| r.seq), Some(8));
    assert_eq!(covering_commit(&recs, group, 7, 0).map(|r| r.seq), None);
    assert_eq!(covering_commit(&recs, "other", 4, 0), None);
    let j = to_json(&c, 2, (4, 8));
    assert_eq!(
        j.get("records_per_message")
            .and_then(aterm_json::Value::as_f64),
        Some(2.5)
    );
    assert_eq!(
        j.get("commit_records_per_message")
            .and_then(aterm_json::Value::as_f64),
        Some(1.0)
    );
    let table = rows(&c, 2);
    assert!(
        table.iter().any(|r| r == "| **all records** | 5 | 2.500 |"),
        "{table:?}"
    );
}

#[test]
fn a_fired_will_is_named_by_the_key_its_registration_carried() {
    let node = "/f/f1/pub/n-1/node/presence";
    let mut recs = vec![
        rec(2, WILL_SUBJECT, "", None),
        rec(3, node, "", None),
        rec(10, node, "", None),
        rec(11, WILL_SUBJECT, "", None),
        rec(12, node, "", None),
        rec(13, "/f/f1/pub/n-1/s-a/presence", "", None),
    ];
    let will = |inc| astream_broker::will_seq(inc).expect("in the layout");
    recs[0].producer = (7, will(0));
    recs[2].producer = (7, will(0));
    recs[3].producer = (7, will(1));
    assert_eq!(will_firings(&recs), BTreeSet::from([10]));
    recs[1].producer = (7, will(1));
    assert_eq!(will_firings(&recs), BTreeSet::from([10]));
}

#[test]
fn what_lands_before_a_deliverys_commit_is_named_and_the_commit_found_past_it() {
    let group = "/f/f1/cur/n-1/node/inbox";
    let note = "/f/f1/in/n-1/s-b/n-1/note";
    let ev = "/f/f1/pub/n-1/s-b/ev";
    let recs = vec![
        rec(4, note, "lat-0", None),
        rec(5, COMMIT_SUBJECT, "", Some((group, 4))),
        rec(6, note, "lat-1", None),
        rec(7, ev, "", None),
        rec(8, "/f/f1/pub/n-1/s-a/ev", "", None),
        rec(9, COMMIT_SUBJECT, "", Some((group, 6))),
        rec(10, note, "lat-2", None),
        rec(11, COMMIT_SUBJECT, "", Some(("other", 10))),
    ];
    let seqs = |v: Vec<&Rec>| v.iter().map(|r| r.seq).collect::<Vec<_>>();
    assert_eq!(before_its_commit(&recs, group, 4).map(seqs), Ok(vec![]));
    assert_eq!(
        before_its_commit(&recs, group, 6).map(seqs),
        Ok(vec![7, 8]),
        "both mirror rows sit between the delivery and its commit"
    );
    assert_eq!(
        lands_before(&recs, group, 6, 10),
        Ok(()),
        "and it was visible under the head read before the next post"
    );
    let why = before_its_commit(&recs, group, 10).expect_err("another group's commit");
    assert!(why.contains("offset 10"), "{why}");
    assert!(before_its_commit(&recs, "other", 4).is_ok_and(|v| v.len() == 6));
}

/// One log, two runs. The bridge writes a commit it held just ahead of the post that
/// carries it, so a premature wait — the head past the mirror's `ev` row at 10, the commit
/// still held — leaves exactly the log a correct wait does: the commit at 11, the next post
/// at 12. The post's own offset passes both. Only the visible head read before the post
/// tells them apart: 12 when the commit was visible, 11 when the post carried it.
#[test]
fn a_commit_the_next_post_carries_is_told_by_the_head_read_before_it() {
    let group = "/f/f1/cur/n-1/node/inbox";
    let note = "/f/f1/in/n-1/s-b/n-1/note";
    let recs = vec![
        rec(9, note, "lat-0", None),
        rec(10, "/f/f1/pub/n-1/s-b/ev", "", None),
        rec(11, COMMIT_SUBJECT, "", Some((group, 9))),
        rec(12, note, "lat-1", None),
    ];
    assert_eq!(
        lands_before(&recs, group, 9, 12),
        Ok(()),
        "12 is the head a correct wait reads, and the next post's offset, which passes a \
         carried commit as well"
    );
    let why = lands_before(&recs, group, 9, 11).expect_err("the post carried it");
    assert!(why.contains("offset 11") && why.contains("at 11"), "{why}");
}

#[test]
fn the_fence_holds_only_when_the_bursts_commit_lands_before_it() {
    let group = "/f/f1/cur/n-1/node/inbox";
    let note = "/f/f1/in/n-1/s-b/n-1/note";
    let held = vec![
        rec(10, note, "burst-0", None),
        rec(11, note, "burst-1", None),
        rec(12, COMMIT_SUBJECT, "", Some((group, 11))),
        rec(13, note, "fence-0", None),
        rec(14, COMMIT_SUBJECT, "", Some((group, 13))),
    ];
    assert_eq!(
        lands_before(&held, group, 11, 13),
        Ok(()),
        "visible under the head read before the fence's post"
    );
    let why = lands_before(&held, group, 11, 12).expect_err("the fence's post carried it");
    assert!(why.contains("offset 12") && why.contains("at 12"), "{why}");
    let shared = vec![
        rec(10, note, "burst-0", None),
        rec(11, note, "burst-1", None),
        rec(12, note, "fence-0", None),
        rec(13, COMMIT_SUBJECT, "", Some((group, 12))),
    ];
    let why = lands_before(&shared, group, 11, 12).expect_err("the fence shares it");
    assert!(why.contains("offset 13") && why.contains("at 12"), "{why}");
    let late = vec![
        rec(11, note, "burst-1", None),
        rec(12, note, "fence-0", None),
        rec(13, COMMIT_SUBJECT, "", Some((group, 11))),
    ];
    assert!(lands_before(&late, group, 11, 12).is_err());
    assert!(
        lands_before(&held[..2], group, 11, 13).is_err(),
        "no commit at all"
    );
}

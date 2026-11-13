// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{TopicEntry, parse_topics, render_topics};

#[test]
fn the_carry_round_trips_and_a_malformed_row_invents_nothing() {
    let entry = |since: &str| TopicEntry {
        since: since.to_string(),
    };
    let live = vec![
        ("build.failed".to_string(), entry("head")),
        ("sat-comp".to_string(), entry("@42")),
    ];
    let wire = render_topics(&live);
    assert_eq!(wire, ["build.failed head", "sat-comp @42"]);
    assert_eq!(parse_topics(&wire), live);
    assert_eq!(
        parse_topics(&["old.topic @3 7".to_string()]),
        vec![("old.topic".to_string(), entry("@3"))]
    );

    let hostile: Vec<String> = [
        "Build head",        // not a topic
        "a/b head",          // a subject segment
        "ok yesterday",      // not a `since` token
        "ok @notanumber",    // nor is this
        "ok @+5",            // an offset has no sign
        "ok head x",         // a third field that is not a number
        "noseparator",       // no `since` at all
        "build.failed head", // …and one good row, so the drop is selective
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        parse_topics(&hostile),
        vec![("build.failed".to_string(), entry("head"))],
    );
}

#[test]
fn a_post_on_a_topic_is_the_sessions_last_topic_post_and_an_addressed_one_is_not() {
    let h = crate::test_support::test_handle(1);
    let ctx = &*h.ctx;
    assert!(ctx.fabric.last_topic_post().is_none());
    let post = |rest: &str| super::cmd_post(ctx, rest, None);
    assert_eq!(post("to=say:build kind=note -- crate 3 of 9"), "OK 1\n");
    let first = ctx.fabric.last_topic_post().expect("a topic post");
    assert_eq!(
        (first.topic.as_str(), first.text.as_str()),
        ("build", "crate 3 of 9")
    );
    // Negative control: mail to a session is no channel post, and leaves the last one.
    assert_eq!(post("to=@s-peer kind=report -- to the manager"), "OK 2\n");
    assert_eq!(ctx.fabric.last_topic_post().as_ref(), Some(&first));
    assert_eq!(post("to=say kind=note -- everyone"), "OK 3\n");
    let bare = ctx.fabric.last_topic_post().expect("a topic post");
    assert_eq!(
        (bare.topic.as_str(), bare.text.as_str()),
        ("say", "everyone")
    );
    assert!(bare.at_ms >= first.at_ms);
    // A long body keeps its head, cut where a character ends (each `é` is two bytes, after
    // one `a`, so the cap falls inside one).
    let long = format!("a{}", "é".repeat(super::TOPIC_POST_KEPT));
    assert_eq!(
        super::cmd_post(ctx, "to=say:big kind=note", Some(long.clone().into_bytes())),
        "OK 4\n"
    );
    let kept = ctx.fabric.last_topic_post().expect("a topic post").text;
    assert_eq!(kept.len(), super::TOPIC_POST_KEPT - 1);
    assert!(long.starts_with(&kept));
}

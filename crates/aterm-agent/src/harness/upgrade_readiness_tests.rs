// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::upgrade::{self, Source, Version};

const MARKER: &str = "ATERM-UPGRADE-READY-01234567";

fn row(kind: &str, text: &str) -> String {
    let text = aterm_json::to_string(text).expect("text JSON");
    format!(
        r#"{{"type":"{kind}","message":{{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{{"type":"text","text":{text}}}]}}}}"#
    )
}

fn notice(marker: &str) -> String {
    let from = Version::parse("2.1.281").expect("version");
    let to = Version::parse("2.1.282").expect("version");
    row(
        "user",
        &upgrade::prepare_prompt(&from, &to, Source::Native, marker),
    )
}

#[test]
fn ordinary_answers_need_no_prescribed_output() {
    for text in [
        "Ready for the restart.",
        "Everything is saved. I'm ready to restart now.",
        "Nothing of mine is running. You can restart this session now.",
        "No background jobs are running; this session can safely be restarted.",
        "It’s safe to restart the session.",
        "**I'm ready for the upgrade.**",
        "All work is saved, and I am ready for this restart.",
        "Saved `parser.rs`. I'm ready to restart now.",
        "```text\nExample: not ready for restart.\n```\nI'm ready to restart now.",
    ] {
        assert!(says_ready(text), "{text}");
        assert!(
            upgrade::transcript_has_ready(
                &[notice(MARKER), row("assistant", text)].join("\n"),
                MARKER
            ),
            "{text}"
        );
    }
}

#[test]
fn a_caveat_or_example_is_not_readiness() {
    for text in [
        "I'm not ready for the restart.",
        "I'll be ready once the build finishes.",
        "Ready for the restart?",
        "Ready for the restart, but the build is still running.",
        "Ready for the restart. The build is pending.",
        "Ready for the restart. The tests are still in progress.",
        "Ready for the restart. The tests are in progress.",
        "Ready for the restart. I still need to save my work.",
        "Ready for the restart. I need to save my work.",
        "Ready for the restart. I have unsaved changes.",
        "I'm ready to restart. I haven't saved my work.",
        "Ready for the restart. The result hasn't been saved.",
        "Ready for the restart. The work is not done.",
        "Ready for the restart. Not all changes are saved.",
        "Ready for the restart. Nothing is running except the release watcher.",
        "Ready for the restart. No tests are running other than the release watcher.",
        "Ready for the restart. The build is not running and the deploy is pending.",
        "Ready for the restart. Wait for the upload to finish.",
        "Ready for the restart. Hold off for a moment.",
        "Ready for the restart. The upload isn't complete.",
        "Ready for the restart. Do not restart yet.",
        "If everything is saved, you can restart this session.",
        "Once I say ready, you can restart this session.",
        "> Ready for the restart.",
        "```text\nReady for the restart.\n```",
        "Example: `I'm ready to restart now.`",
        "Example:\n    Ready for the restart.",
        "\tReady for the restart.",
        "The agent said: \"Ready for the restart.\"",
        "All tests passed.",
        "````text\nAn example follows:\n```\nReady for the restart.\n````",
        "```text\n~~~\nReady for the restart.\n```",
        "`Example response:\nReady for the restart.\n`",
        "``Example with a ` inside:\nReady for the restart.\n``",
        "I'm `not` ready for the restart.",
        "Ready for the restart. I'm `not` ready for the restart.",
        "Ready for the restart. `An unfinished example",
        "Ready for the restart.\n````text\nAn unfinished example",
    ] {
        assert!(!says_ready(text), "{text}");
        assert!(
            !upgrade::transcript_has_ready(
                &[notice(MARKER), row("assistant", text)].join("\n"),
                MARKER,
            ),
            "{text}"
        );
    }
}

#[test]
fn readiness_requires_the_current_notice_and_a_final_own_model_row() {
    let answer = row("assistant", "You can restart this session now.");
    assert!(!upgrade::transcript_has_ready(&answer, MARKER));
    assert!(!upgrade::transcript_has_ready(
        &[notice("ATERM-UPGRADE-READY-other"), answer.clone()].join("\n"),
        MARKER
    ));
    assert!(!upgrade::transcript_has_ready(
        &[
            notice(MARKER),
            row("user", "prepare the deploy"),
            answer.clone()
        ]
        .join("\n"),
        MARKER
    ));
    for invalid in [
        answer.replace("claude-opus-5-5", "<synthetic>"),
        answer.replace("\"end_turn\"", "\"tool_use\""),
        answer.replace(
            "\"type\":\"assistant\"",
            "\"type\":\"assistant\",\"isApiErrorMessage\":true",
        ),
        answer.replace(
            "\"type\":\"assistant\"",
            "\"type\":\"assistant\",\"isSidechain\":true",
        ),
    ] {
        assert!(!upgrade::transcript_has_ready(
            &[notice(MARKER), invalid].join("\n"),
            MARKER
        ));
    }
    let split_caveat = answer.replace(
        "You can restart this session now.",
        "You can restart this session now.\"},{\"type\":\"text\",\"text\":\"The tests are still in progress.",
    );
    assert!(!upgrade::transcript_has_ready(
        &[notice(MARKER), split_caveat].join("\n"),
        MARKER
    ));
    let ready = [notice(MARKER), answer].join("\n");
    for later in [
        row("user", "keep going"),
        notice("ATERM-UPGRADE-READY-other"),
        row("assistant", "One more command remains."),
    ] {
        assert!(!upgrade::transcript_has_ready(
            &format!("{ready}\n{later}"),
            MARKER
        ));
    }
}

#[test]
fn ordinary_readiness_keeps_supersession_and_release_ownership() {
    let markers = [MARKER.to_string()];
    let ready = [notice(MARKER), row("assistant", "Ready for the restart.")].join("\n");
    assert!(!upgrade::ready_superseded(&ready, &markers));
    let directed = format!("{ready}\n{}", row("user", "fix the parser first"));
    assert!(upgrade::ready_superseded(&directed, &markers));
    assert!(!upgrade::directed_since_ready(&directed, &markers));
    let taken = format!("{directed}\n{}", row("assistant", "Working on the parser."));
    assert!(upgrade::directed_since_ready(&taken, &markers));
}

#[test]
fn legacy_marker_replies_still_read_without_the_old_notice() {
    assert!(upgrade::transcript_has_ready(
        &row("assistant", MARKER),
        MARKER
    ));
    assert!(upgrade::says_ready(MARKER, MARKER));
}

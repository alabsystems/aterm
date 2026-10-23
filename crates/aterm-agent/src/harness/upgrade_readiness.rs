// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conservative reading of an ordinary answer to an upgrade notice.

/// Checks whether `words` states unconditional readiness for an upgrade restart.
pub(super) fn says_ready(words: &str) -> bool {
    let Some((prose, caution)) = prose(words) else {
        return false;
    };
    if caution.contains('?') {
        return false;
    }
    let clauses = |words: &str| -> Vec<String> {
        words
            .split(['.', '!', ';', ',', '\n'])
            .map(|part| {
                part.to_lowercase()
                    .replace('’', "'")
                    .replace(['*', '_'], "")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|part| !part.is_empty())
            .collect()
    };
    !clauses(&caution).iter().any(|clause| withholds(clause))
        && clauses(&prose)
            .iter()
            .any(|clause| !clause.contains('\0') && affirms(clause))
}

fn prose(words: &str) -> Option<(String, String)> {
    let (mut fenced, mut inline) = (None, None);
    let (mut out, mut caution) = (String::new(), String::new());
    for line in words.lines() {
        if line.starts_with("    ") || line.starts_with('\t') {
            out.push('\n');
            caution.push('\n');
            continue;
        }
        let line = line.trim();
        let fence = line.as_bytes().first().copied().and_then(|byte| {
            if !matches!(byte, b'`' | b'~') {
                return None;
            }
            let count = line.bytes().take_while(|&b| b == byte).count();
            (count >= 3).then_some((byte, count, &line[count..]))
        });
        if let Some((byte, count)) = fenced {
            if fence.is_some_and(|(b, n, rest)| b == byte && n >= count && rest.trim().is_empty()) {
                fenced = None;
            }
        } else if inline.is_none()
            && let Some((byte, count, _)) = fence
        {
            fenced = Some((byte, count));
        } else if !line.starts_with('>') {
            if inline.is_some() {
                out.push('\0');
            }
            let bytes = line.as_bytes();
            let (mut from, mut at) = (0, 0);
            while at < bytes.len() {
                if bytes[at] != b'`' {
                    at += 1;
                    continue;
                }
                if inline.is_none() {
                    out.push_str(&line[from..at]);
                }
                caution.push_str(&line[from..at]);
                out.push_str(" \0 ");
                caution.push(' ');
                let count = bytes[at..].iter().take_while(|&&b| b == b'`').count();
                match inline {
                    Some(open) if open == count => inline = None,
                    None => inline = Some(count),
                    Some(_) => {}
                }
                at += count;
                from = at;
            }
            if inline.is_none() {
                out.push_str(&line[from..]);
            }
            caution.push_str(&line[from..]);
        }
        out.push('\n');
        caution.push('\n');
    }
    (fenced.is_none() && inline.is_none()).then_some((out, caution))
}

fn affirms(clause: &str) -> bool {
    let clause = clause.strip_prefix("and ").unwrap_or(clause);
    let clause = clause.strip_prefix("so ").unwrap_or(clause);
    let clause = clause.strip_suffix(" now").unwrap_or(clause);
    let ready = [
        "i'm ",
        "i am ",
        "we're ",
        "we are ",
        "this session is ",
        "the session is ",
    ]
    .iter()
    .find_map(|subject| clause.strip_prefix(subject))
    .unwrap_or(clause);
    if matches!(
        ready,
        "ready" | "ready to restart" | "ready to be restarted"
    ) {
        return true;
    }
    if let Some(target) = ready.strip_prefix("ready for ") {
        return restart_target(target);
    }
    if let Some(target) = clause.strip_prefix("you can restart ") {
        return matches!(
            target,
            "this session" | "the session" | "me" | "claude code"
        );
    }
    if matches!(
        clause,
        "you can restart" | "it is safe to restart" | "it's safe to restart"
    ) {
        return true;
    }
    if let Some(target) = clause
        .strip_prefix("it is safe to restart ")
        .or_else(|| clause.strip_prefix("it's safe to restart "))
    {
        return matches!(target, "this session" | "the session" | "claude code");
    }
    matches!(
        clause,
        "this session can be restarted"
            | "the session can be restarted"
            | "this session can safely be restarted"
            | "the session can safely be restarted"
    )
}

fn restart_target(target: &str) -> bool {
    let target = target
        .strip_prefix("the ")
        .or_else(|| target.strip_prefix("this "))
        .or_else(|| target.strip_prefix("an "))
        .or_else(|| target.strip_prefix("a "))
        .unwrap_or(target);
    matches!(target, "restart" | "upgrade" | "upgrade restart")
}

fn withholds(clause: &str) -> bool {
    let tokens: Vec<&str> = clause.split_whitespace().collect();
    if tokens.iter().enumerate().any(|(index, word)| {
        *word == "not"
            && !matches!(
                tokens.get(index + 1).copied(),
                Some("running" | "waiting" | "pending" | "ongoing" | "underway")
            )
            && tokens.get(index + 1..index + 3) != Some(&["in", "progress"][..])
    }) {
        return true;
    }
    if tokens.iter().any(|word| {
        word.ends_with("n't")
            || matches!(
                *word,
                "if" | "unless"
                    | "until"
                    | "once"
                    | "when"
                    | "after"
                    | "before"
                    | "but"
                    | "except"
                    | "still"
                    | "unsaved"
                    | "uncommitted"
                    | "wait"
                    | "pause"
            )
    }) || [
        "not ready",
        "not done",
        "not yet",
        "isn't ready",
        "aren't ready",
        "cannot restart",
        "can't restart",
        "do not restart",
        "don't restart",
        "must finish",
        "needs to finish",
        "need to finish",
        "need to save",
        "needs to save",
        "have to save",
        "must save",
        "not finished",
        "not complete",
        "isn't complete",
        "aren't complete",
        "hasn't finished",
        "hasn't completed",
        "not saved",
        "not committed",
        "hold off",
        "hold on",
        "other than",
        "apart from",
        "will be ready",
        "would be ready",
        "should be ready",
        "may be ready",
        "might be ready",
    ]
    .iter()
    .any(|phrase| clause.contains(phrase))
    {
        return true;
    }
    tokens.iter().enumerate().any(|(index, word)| {
        let activity = matches!(
            *word,
            "running"
                | "waiting"
                | "pending"
                | "ongoing"
                | "underway"
                | "unfinished"
                | "remaining"
                | "incomplete"
        ) || (*word == "progress" && index > 0 && tokens[index - 1] == "in");
        if !activity {
            return false;
        }
        let prefix = &tokens[..index];
        let start = prefix
            .iter()
            .rposition(|word| matches!(*word, "and" | "or" | "nor"))
            .map_or(0, |at| at + 1);
        let prefix = &prefix[start..];
        let negated = prefix
            .first()
            .is_some_and(|word| matches!(*word, "no" | "nothing" | "none"))
            || prefix
                .iter()
                .rev()
                .take(2)
                .any(|word| *word == "not" || word.ends_with("n't"));
        !negated
    })
}

#[cfg(test)]
mod tests {
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
}

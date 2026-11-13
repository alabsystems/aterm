// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{parse_story, story_usage};
use crate::presence::{StoryVerb, TOLD_TEXT_MAX_BYTES};

#[test]
fn every_junk_form_is_the_usage_line_and_the_closed_set_parses() {
    let usage = story_usage();
    for junk in [
        "",
        "   ",
        "approve",
        "chosen",
        "CHOSE",
        "choose",
        "42",
        "@s-1 chose",
        "story chose",
        "approved",
        "dismissed",
        "reconnected",
        "timeout",
        "exit",
        "compacted",
        "warned",
    ] {
        assert_eq!(parse_story(junk), Err(usage.clone()), "{junk:?}");
    }
    assert_eq!(parse_story("chose"), Ok((StoryVerb::Chose, String::new())));
    assert_eq!(
        parse_story("  chose   some text here  "),
        Ok((StoryVerb::Chose, "some text here".to_string())),
        "the text is trimmed and kept whole"
    );
    assert_eq!(
        StoryVerb::TOLD_WORDS,
        ["chose"],
        "the closed set is `chose`"
    );
    for word in StoryVerb::TOLD_WORDS {
        assert!(StoryVerb::parse_told(word).is_some(), "{word}");
    }
}

#[test]
fn the_text_is_capped_at_the_wire_cap_and_refuses_control_bytes() {
    let ok = "x".repeat(TOLD_TEXT_MAX_BYTES);
    assert_eq!(
        parse_story(&format!("chose {ok}")),
        Ok((StoryVerb::Chose, ok.clone()))
    );
    let long = "x".repeat(TOLD_TEXT_MAX_BYTES + 1);
    assert_eq!(
        parse_story(&format!("chose {long}")),
        Err(format!(
            "ERR story: text is longer than {TOLD_TEXT_MAX_BYTES} bytes\n"
        ))
    );
    let wide = "é".repeat(TOLD_TEXT_MAX_BYTES / 2 + 1);
    assert!(parse_story(&format!("chose {wide}")).is_err());
    assert_eq!(
        parse_story("chose session gone\nOK forged"),
        Err("ERR story: text carries a control character\n".to_string())
    );
    assert_eq!(
        parse_story("chose tab\there"),
        Err("ERR story: text carries a control character\n".to_string())
    );
}

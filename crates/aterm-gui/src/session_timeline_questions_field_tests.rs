// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    META_QUESTIONS_MAX, MetaEdit, MetaField, MetaWriteError, QUESTIONS_POLICY_WORDS, SessionMeta,
    normalize_questions_value, restore_carried_meta, sanitize_metadata_value, validated_meta_value,
};

#[test]
fn the_policy_words_are_admitted_and_case_folded() {
    for word in QUESTIONS_POLICY_WORDS {
        assert_eq!(normalize_questions_value(word), Some(word));
        assert!(word.len() <= META_QUESTIONS_MAX, "{word}");
    }
    for (typed, stored) in [
        ("Recommended", "recommended"),
        ("  RECOMMENDED ", "recommended"),
        ("Ask", "ask"),
        ("ASK", "ask"),
        ("Long-Term", "long-term"),
        (" SHORT-TERM ", "short-term"),
    ] {
        assert_eq!(normalize_questions_value(typed), Some(stored), "{typed:?}");
    }
}

#[test]
fn anything_outside_the_words_is_refused() {
    for bad in [
        "",
        "long_term",
        "short_term",
        "recommend",
        "long term",
        "longterm",
        "long-term-ish",
        "askx",
        "ask ask",
        "@s-abc123",
        "s-abc123@n-lab",
        "controller",
        "ask\u{202e}",
        "rec\u{200b}ommended",
        "whatever claude thinks",
        "land",
        "Land",
    ] {
        assert_eq!(normalize_questions_value(bad), None, "{bad:?}");
    }
}

#[test]
fn the_write_ladder_normalizes_or_refuses() {
    use MetaField::Questions;
    assert_eq!(
        validated_meta_value(Questions, MetaEdit::Set(" Recommended ")),
        Ok(Some("recommended".to_string()))
    );
    assert_eq!(
        validated_meta_value(Questions, MetaEdit::Set("whatever you think")),
        Err(MetaWriteError::NotAQuestionsPolicy)
    );
    let long = "x".repeat(200);
    assert_eq!(
        validated_meta_value(Questions, MetaEdit::Set(&long)),
        Err(MetaWriteError::NotAQuestionsPolicy),
        "an over-cap value is outside the set first"
    );
    assert_eq!(
        validated_meta_value(Questions, MetaEdit::Set("  ")),
        Err(MetaWriteError::Empty)
    );
    assert_eq!(validated_meta_value(Questions, MetaEdit::Clear), Ok(None));
    assert_eq!(MetaField::parse("questions"), Some(Questions));
    assert_eq!(Questions.wire_name(), "questions");
    assert_eq!(Questions.cap(), META_QUESTIONS_MAX);
}

#[test]
fn the_store_normalizes_counts_and_compares_questions() {
    let mut m = SessionMeta::default();
    assert_eq!(m.set("questions", Some("Recommended".into())), Some(true));
    assert_eq!(m.get("questions"), Some("recommended"));
    assert!(m.any_set(), "a user field: `ls meta=1`");
    assert_ne!(m, SessionMeta::default(), "a value, part of equality");
    assert_eq!(
        m.set("questions", Some("not a policy".into())),
        Some(true),
        "an illegal seed clears rather than stores"
    );
    assert_eq!(m.get("questions"), None);
    assert!(!m.any_set());
    assert_eq!(
        sanitize_metadata_value("questions", "Ask").as_deref(),
        Some("ask")
    );
    assert_eq!(sanitize_metadata_value("questions", "@s-x"), None);
}

#[test]
fn a_carried_identity_restores_the_policy() {
    for word in QUESTIONS_POLICY_WORDS {
        let ctx = crate::stub_session(0).ctx.clone();
        let mut live = SessionMeta::default();
        let _ = live.set("questions", Some(word.into()));
        let carried = live.sanitized();
        assert_eq!(carried.questions.as_deref(), Some(word));
        assert!(restore_carried_meta(&ctx, &carried));
        assert_eq!(ctx.meta.lock().unwrap().questions.as_deref(), Some(word));
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(events, vec![format!("field=questions value={word}")]);
    }
}

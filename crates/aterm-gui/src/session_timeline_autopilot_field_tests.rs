// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AUTOPILOT_USAGE, META_AUTOPILOT_MAX, MetaEdit, MetaField, MetaWriteError, SessionMeta,
    apply_meta_value, normalize_autopilot_value, restore_carried_meta, sanitize_metadata_value,
    validated_meta_value,
};
use aterm_agent::supervise::Autopilot;

#[test]
fn the_modes_and_the_owners_words_store_the_modes_word() {
    for mode in Autopilot::ALL {
        assert_eq!(normalize_autopilot_value(mode.word()), Some(mode.word()));
        assert!(mode.word().len() <= META_AUTOPILOT_MAX, "{mode:?}");
    }
    for (typed, stored) in [
        ("Cruise", "cruise"),
        ("  LAND ", "land"),
        ("Off", "off"),
        ("forever", "cruise"),
        ("Infinity", "cruise"),
        ("wrap-up", "land"),
        ("WrapUp", "land"),
        ("wrap", "land"),
    ] {
        assert_eq!(normalize_autopilot_value(typed), Some(stored), "{typed:?}");
    }
    let words: Vec<String> = Autopilot::ALL
        .iter()
        .map(|m| match m {
            Autopilot::Cruise => format!("{} [until=HH:MM|+DURATION]", m.word()),
            _ => m.word().to_string(),
        })
        .collect();
    assert_eq!(
        AUTOPILOT_USAGE,
        format!("usage: meta set autopilot {}", words.join("|"))
    );
}

#[test]
fn anything_that_names_no_mode_is_refused() {
    for bad in [
        "",
        "on",
        "true",
        "false",
        "no",
        "cruising",
        "land it",
        "of\u{200b}f",
        "off\u{202e}",
        "@s-abc123",
    ] {
        assert_eq!(normalize_autopilot_value(bad), None, "{bad:?}");
    }
}

#[test]
fn the_write_ladder_normalizes_or_refuses() {
    use MetaField::Autopilot as Field;
    assert_eq!(
        validated_meta_value(Field, MetaEdit::Set(" Wrap-Up ")),
        Ok(Some("land".to_string()))
    );
    assert_eq!(
        validated_meta_value(Field, MetaEdit::Set("sometimes")),
        Err(MetaWriteError::NotAnAutopilot)
    );
    assert_eq!(
        validated_meta_value(Field, MetaEdit::Set(&"x".repeat(200))),
        Err(MetaWriteError::NotAnAutopilot)
    );
    assert_eq!(
        validated_meta_value(Field, MetaEdit::Set("  ")),
        Err(MetaWriteError::Empty)
    );
    assert_eq!(validated_meta_value(Field, MetaEdit::Clear), Ok(None));
    assert_eq!(MetaField::parse("autopilot"), Some(Field));
    assert_eq!(Field.wire_name(), "autopilot");
    assert_eq!(Field.cap(), META_AUTOPILOT_MAX);
    assert!(MetaField::NAMES.ends_with("|questions|autopilot"));
}

#[test]
fn the_store_normalizes_counts_compares_and_types_the_word() {
    let mut m = SessionMeta::default();
    assert_eq!(m.autopilot_word(), None);
    assert_eq!(m.set("autopilot", Some("Infinity".into())), Some(true));
    assert_eq!(m.get("autopilot"), Some("cruise"));
    assert_eq!(m.autopilot_word(), Some(Autopilot::Cruise));
    assert!(m.any_set(), "a user field: `ls meta=1`");
    assert_ne!(m, SessionMeta::default(), "a value, part of equality");
    assert_eq!(
        m.set("autopilot", Some("hover".into())),
        Some(true),
        "an illegal seed clears rather than stores"
    );
    assert_eq!(m.get("autopilot"), None);
    assert!(!m.any_set());
    assert_eq!(
        sanitize_metadata_value("autopilot", "OFF").as_deref(),
        Some("off")
    );
    assert_eq!(sanitize_metadata_value("autopilot", "on"), None);
    for mode in Autopilot::ALL {
        m.autopilot = Some(mode.word().into());
        assert_eq!(m.autopilot_word(), Some(mode));
    }
    m.autopilot = Some(" Wrap-Up ".into());
    assert_eq!(m.autopilot_word(), Some(Autopilot::Land));
}

#[test]
fn a_word_that_moves_is_recorded_and_rings_the_host() {
    let ctx = crate::stub_session(0).ctx.clone();
    let rings = crate::harness_host::rings_here();
    assert!(apply_meta_value(
        &ctx,
        MetaField::Autopilot,
        Some("off".into()),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(crate::harness_host::rings_here(), rings + 1, "moved: rung");
    assert!(!apply_meta_value(
        &ctx,
        MetaField::Autopilot,
        Some("off".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(apply_meta_value(
        &ctx,
        MetaField::Role,
        Some("lead".into()),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        crate::harness_host::rings_here(),
        rings + 1,
        "NEGATIVE CONTROLS: a re-set and another field ring nothing"
    );
    let events: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(events[0], "field=autopilot value=off");
}

#[test]
fn a_carried_identity_restores_the_word() {
    for mode in Autopilot::ALL {
        let ctx = crate::stub_session(0).ctx.clone();
        let mut live = SessionMeta::default();
        let _ = live.set("autopilot", Some(mode.word().into()));
        let carried = live.sanitized();
        assert_eq!(carried.autopilot.as_deref(), Some(mode.word()));
        let rings = crate::harness_host::rings_here();
        assert!(restore_carried_meta(&ctx, &carried));
        assert_eq!(crate::harness_host::rings_here(), rings + 1);
        assert_eq!(ctx.meta.lock().unwrap().autopilot_word(), Some(mode));
    }
}

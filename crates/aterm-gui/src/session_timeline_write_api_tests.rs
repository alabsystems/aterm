// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MetaEdit, MetaField, MetaWriteError, apply_meta_value, validated_meta_value};

#[test]
fn the_ladder_refuses_rather_than_repairs() {
    use MetaField::{Icon, Title};
    assert_eq!(
        validated_meta_value(Title, MetaEdit::Set("   ")),
        Err(MetaWriteError::Empty),
        "an empty Set is a refusal, not a clear"
    );
    assert_eq!(
        validated_meta_value(Title, MetaEdit::Set("build\u{202e}agent")),
        Err(MetaWriteError::ForbiddenFormatting)
    );
    let over = "x".repeat(super::META_TITLE_MAX + 1);
    assert_eq!(
        validated_meta_value(Title, MetaEdit::Set(&over)),
        Err(MetaWriteError::TooLong {
            cap: super::META_TITLE_MAX
        }),
        "over-cap is refused, never truncated"
    );
    assert_eq!(
        validated_meta_value(Title, MetaEdit::Set("  build agent  ")),
        Ok(Some("build agent".to_string())),
        "interior whitespace survives; the edges are trimmed"
    );
    assert_eq!(validated_meta_value(Title, MetaEdit::Clear), Ok(None));
    assert_eq!(validated_meta_value(Icon, MetaEdit::Clear), Ok(None));
    assert_eq!(Icon.cap(), super::META_ICON_MAX);
    assert_eq!(MetaField::parse("colour"), None);
}

#[test]
fn a_clear_records_the_cleared_marker_and_only_on_a_real_change() {
    let ctx = crate::stub_session(0).ctx.clone();
    assert!(apply_meta_value(
        &ctx,
        MetaField::Title,
        Some("agent".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(
        !apply_meta_value(
            &ctx,
            MetaField::Title,
            Some("agent".into()),
            aterm_slog::Actor::Ctl
        ),
        "a no-op re-set reports unchanged so the caller stays silent"
    );
    assert!(apply_meta_value(
        &ctx,
        MetaField::Title,
        None,
        aterm_slog::Actor::Ctl
    ));
    assert!(
        !apply_meta_value(&ctx, MetaField::Title, None, aterm_slog::Actor::Ctl),
        "clearing an unset field is a no-op"
    );
    let rows: Vec<crate::session_timeline::TimelineEvent> =
        ctx.timeline.lock().unwrap().since(None).collect();
    let payloads: Vec<&str> = rows
        .iter()
        .filter(|e| e.kind == "meta-change")
        .map(|e| &*e.payload)
        .collect();
    assert_eq!(
        payloads,
        vec!["field=title value=agent", "field=title value=-"],
        "exactly one record per REAL change, and a clear is `-`"
    );
}

#[test]
fn every_accepted_write_marks_its_field_and_a_seed_marks_none() {
    use super::SessionMeta;
    let ctx = crate::stub_session(0).ctx.clone();
    assert!(
        !apply_meta_value(&ctx, MetaField::Attention, None, aterm_slog::Actor::Ctl),
        "clearing an unset field moves nothing"
    );
    assert!(apply_meta_value(
        &ctx,
        MetaField::Role,
        Some("lead".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(!apply_meta_value(
        &ctx,
        MetaField::Role,
        Some("lead".into()),
        aterm_slog::Actor::Ctl
    ));
    let meta = ctx.meta.lock().unwrap().clone();
    let wrote = MetaField::ALL.map(|field| meta.driver_wrote(field));
    assert_eq!(
        wrote,
        [false, false, false, true, true, false, false],
        "title, description, icon, role, attention, questions, autopilot"
    );

    let mut seeded = SessionMeta::default();
    assert_eq!(seeded.set("role", Some("lead".into())), Some(true));
    assert!(
        MetaField::ALL
            .iter()
            .all(|field| !seeded.driver_wrote(*field))
    );
    assert_eq!(seeded, meta, "equality is over the values alone");
}

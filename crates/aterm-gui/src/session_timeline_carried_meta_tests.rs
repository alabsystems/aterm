// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MetaField, SessionMeta, apply_meta_value, restore_carried_meta};

fn meta_events_after(ctx: &crate::SessionCtx, watermark: Option<u64>) -> Vec<String> {
    ctx.timeline
        .lock()
        .unwrap()
        .since(watermark)
        .filter(|event| event.kind == "meta-change")
        .map(|event| event.payload.to_string())
        .collect()
}

#[test]
fn a_driver_write_outranks_the_carried_value_and_every_move_is_announced() {
    let ctx = crate::stub_session(0).ctx.clone();
    let _ = ctx.meta.lock().unwrap().set("icon", Some("🧟".into()));
    assert!(apply_meta_value(
        &ctx,
        MetaField::Role,
        Some("worker:new".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        Some("fresh escalation".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(!apply_meta_value(
        &ctx,
        MetaField::Description,
        None,
        aterm_slog::Actor::Ctl
    ));
    let watermark = ctx.timeline.lock().unwrap().high_id();

    let carried = SessionMeta {
        user_title: Some("fable driver".into()),
        description: Some("carried notes".into()),
        role: Some("worker:old".into()),
        ..SessionMeta::default()
    };
    assert!(restore_carried_meta(&ctx, &carried));

    let meta = ctx.meta.lock().unwrap().clone();
    assert_eq!(
        meta,
        SessionMeta {
            user_title: Some("fable driver".into()),
            role: Some("worker:new".into()),
            attention: Some("fresh escalation".into()),
            ..SessionMeta::default()
        },
        "the driver's role and attention and its clear of description \
         stand; the title is carried; the stale icon is cleared"
    );
    assert_eq!(
        meta_events_after(&ctx, watermark),
        vec![
            "field=title value=fable%20driver".to_string(),
            "field=icon value=-".to_string(),
        ],
        "exactly the fields that moved, each naming what it now stores"
    );

    let watermark = ctx.timeline.lock().unwrap().high_id();
    assert!(!restore_carried_meta(&ctx, &carried));
    assert!(meta_events_after(&ctx, watermark).is_empty());
}

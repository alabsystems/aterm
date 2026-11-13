// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ATTENTION_OWNERS_MAX, AttentionWriteError, BARE_ATTENTION_OWNER, META_ATTENTION_KEYED_MAX,
    MetaEdit, MetaField, MetaWriteError, SessionMeta, apply_meta_value, claim_supervisor,
    release_supervisor, release_supervisor_held_by, write_attention_owned,
};

fn meta_events(ctx: &crate::SessionCtx) -> Vec<String> {
    ctx.timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|event| event.kind == "meta-change")
        .map(|event| event.payload.to_string())
        .collect()
}

fn effective(ctx: &crate::SessionCtx) -> Option<String> {
    ctx.meta.lock().unwrap().attention.clone()
}

#[test]
fn two_owners_set_and_clear_independently() {
    let ctx = crate::stub_session(0).ctx.clone();
    let set = |owner: &str, text: &str| {
        write_attention_owned(&ctx, owner, MetaEdit::Set(text), aterm_slog::Actor::Ctl)
            .expect("accepted")
    };
    assert!(set("sup", "claude needs approval: rm -rf tmp"));
    assert!(set("human", "check the deploy"));
    assert_eq!(effective(&ctx).as_deref(), Some("check the deploy"));
    assert_eq!(
        ctx.meta.lock().unwrap().attention_owners.get("sup"),
        Some("claude needs approval: rm -rf tmp")
    );

    assert!(write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
    assert_eq!(
        effective(&ctx).as_deref(),
        Some("claude needs approval: rm -rf tmp")
    );
    assert!(
        !write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
    );
    assert!(!set("sup", "claude needs approval: rm -rf tmp"));
    assert!(write_attention_owned(&ctx, "sup", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
    assert_eq!(effective(&ctx), None);
    assert!(ctx.meta.lock().unwrap().attention_owners.is_empty());

    assert_eq!(
        meta_events(&ctx),
        [
            "field=attention value=claude%20needs%20approval:%20rm%20-rf%20tmp",
            "field=attention value=check%20the%20deploy",
            "field=attention value=claude%20needs%20approval:%20rm%20-rf%20tmp",
            "field=attention value=-",
        ]
    );

    set("a", "first");
    set("b", "second");
    let before = meta_events(&ctx).len();
    assert!(write_attention_owned(&ctx, "a", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
    assert_eq!(effective(&ctx).as_deref(), Some("second"));
    assert_eq!(meta_events(&ctx).len(), before);
    apply_meta_value(
        &ctx,
        MetaField::Attention,
        Some("lunch".into()),
        aterm_slog::Actor::Ctl,
    );
    set("c", "third");
    let before = meta_events(&ctx).len();
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        None,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(effective(&ctx).as_deref(), Some("third"));
    assert_eq!(meta_events(&ctx).len(), before);
    assert!(!apply_meta_value(
        &ctx,
        MetaField::Attention,
        None,
        aterm_slog::Actor::Ctl
    ));
    assert!(!write_attention_owned(&ctx, "a", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
}

#[test]
fn the_bare_form_is_the_bare_owner_and_keeps_its_contract() {
    let ctx = crate::stub_session(0).ctx.clone();
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        Some("legacy badge".to_string()),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(effective(&ctx).as_deref(), Some("legacy badge"));
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap()
            .attention_owners
            .get(BARE_ATTENTION_OWNER),
        Some("legacy badge")
    );
    write_attention_owned(&ctx, "sup", MetaEdit::Set("keyed"), aterm_slog::Actor::Ctl).unwrap();
    apply_meta_value(&ctx, MetaField::Attention, None, aterm_slog::Actor::Ctl);
    assert_eq!(effective(&ctx).as_deref(), Some("keyed"));
    write_attention_owned(&ctx, "sup", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap();
    assert_eq!(effective(&ctx), None);
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        Some("x".into()),
        aterm_slog::Actor::Ctl
    ));
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        None,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(effective(&ctx), None);
}

#[test]
fn the_map_is_bounded_and_the_keyed_text_capped() {
    let ctx = crate::stub_session(0).ctx.clone();
    for i in 0..ATTENTION_OWNERS_MAX - 1 {
        write_attention_owned(
            &ctx,
            &format!("o{i}"),
            MetaEdit::Set("x"),
            aterm_slog::Actor::Ctl,
        )
        .unwrap();
    }
    assert!(apply_meta_value(
        &ctx,
        MetaField::Attention,
        Some("bare".into()),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        write_attention_owned(
            &ctx,
            "one-too-many",
            MetaEdit::Set("y"),
            aterm_slog::Actor::Ctl
        ),
        Err(AttentionWriteError::OwnersFull)
    );
    assert_eq!(effective(&ctx).as_deref(), Some("bare"), "nothing moved");
    assert!(
        write_attention_owned(&ctx, "o0", MetaEdit::Set("updated"), aterm_slog::Actor::Ctl)
            .unwrap()
    );
    write_attention_owned(&ctx, "o1", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap();
    write_attention_owned(
        &ctx,
        "one-too-many",
        MetaEdit::Set("y"),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();

    let long = "a".repeat(META_ATTENTION_KEYED_MAX + 1);
    assert_eq!(
        write_attention_owned(&ctx, "o0", MetaEdit::Set(&long), aterm_slog::Actor::Ctl),
        Err(AttentionWriteError::Meta(MetaWriteError::TooLong {
            cap: META_ATTENTION_KEYED_MAX
        }))
    );
    write_attention_owned(
        &ctx,
        BARE_ATTENTION_OWNER,
        MetaEdit::Set(&long),
        aterm_slog::Actor::Ctl,
    )
    .expect("the bare owner keeps the bare field's 256-byte cap");
}

#[test]
fn a_restore_carries_the_bare_entry_and_a_literal_seed_is_adopted() {
    let mut meta = SessionMeta::default();
    meta.set("attention", Some("bare".into()));
    meta.set_attention_owned("sup", Some("keyed".into()))
        .unwrap();
    assert_eq!(meta.attention.as_deref(), Some("keyed"));
    assert_eq!(meta.sanitized().attention.as_deref(), Some("bare"));
    meta.set_attention_owned(BARE_ATTENTION_OWNER, None)
        .unwrap();
    assert_eq!(
        meta.sanitized().attention,
        None,
        "a keyed entry never travels"
    );

    let mut seeded = SessionMeta {
        attention: Some("seeded".into()),
        ..SessionMeta::default()
    };
    assert!(
        seeded
            .set_attention_owned("sup", Some("keyed".into()))
            .unwrap()
    );
    assert!(seeded.set_attention_owned("sup", None).unwrap());
    assert_eq!(seeded.attention.as_deref(), Some("seeded"));
}

#[test]
fn a_retired_hook_badge_does_not_travel() {
    let legacy = "claude needs approval: Claude needs your permission - permission_prompt";
    let mut meta = SessionMeta::default();
    meta.set("attention", Some(legacy.into()));
    assert_eq!(
        meta.attention.as_deref(),
        Some(legacy),
        "a live write stands"
    );
    assert_eq!(meta.sanitized().attention, None);
    meta.set_attention_owned("sup", Some("claude bash: ls (x)".into()))
        .unwrap();
    assert_eq!(meta.sanitized().attention, None);

    let seeded = SessionMeta {
        attention: Some(legacy.into()),
        ..SessionMeta::default()
    };
    assert_eq!(seeded.sanitized().attention, None);

    let mut person = SessionMeta::default();
    person.set("attention", Some("needs approval soon".into()));
    assert_eq!(
        person.sanitized().attention.as_deref(),
        Some("needs approval soon")
    );
}

#[test]
fn a_holder_conditional_release_never_clears_another_holders_claim() {
    let ctx = crate::stub_session(0).ctx.clone();
    let shown = || {
        ctx.meta
            .lock()
            .unwrap()
            .live_supervisor(100)
            .map(str::to_owned)
    };
    assert_eq!(
        claim_supervisor(&ctx, "sup-b", Some(8), None, 100),
        Ok(true)
    );
    assert!(!release_supervisor_held_by(
        &ctx,
        "sup-a",
        100,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(shown().as_deref(), Some("sup-b"));
    assert!(release_supervisor_held_by(
        &ctx,
        "sup-b",
        100,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(shown(), None);
    assert!(
        !release_supervisor_held_by(&ctx, "sup-b", 100, aterm_slog::Actor::Ctl),
        "nothing left"
    );
}

#[test]
fn a_supervisor_claim_is_exclusive_lapses_and_releases_by_connection() {
    let ctx = crate::stub_session(0).ctx.clone();
    let shown = |now: u64| {
        ctx.meta
            .lock()
            .unwrap()
            .live_supervisor(now)
            .map(str::to_owned)
    };
    assert_eq!(
        claim_supervisor(&ctx, "sup-a", Some(7), None, 100),
        Ok(true)
    );
    assert_eq!(shown(100).as_deref(), Some("sup-a"));
    assert_eq!(
        claim_supervisor(&ctx, "sup-b", Some(8), None, 100),
        Err("sup-a".to_string())
    );
    assert_eq!(
        claim_supervisor(&ctx, "sup-a", Some(9), None, 100),
        Ok(false)
    );
    assert!(!release_supervisor(
        &ctx,
        Some(7),
        100,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(shown(100).as_deref(), Some("sup-a"));
    assert!(release_supervisor(
        &ctx,
        Some(9),
        100,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(shown(100), None);

    assert_eq!(
        claim_supervisor(&ctx, "sup-a", None, Some(500), 100),
        Ok(true)
    );
    assert_eq!(shown(499).as_deref(), Some("sup-a"));
    assert_eq!(shown(500), None);
    assert_eq!(claim_supervisor(&ctx, "sup-b", None, None, 600), Ok(true));

    assert_eq!(
        meta_events(&ctx),
        [
            "field=supervisor value=sup-a",
            "field=supervisor value=-",
            "field=supervisor value=sup-a",
            "field=supervisor value=sup-b",
        ]
    );
}

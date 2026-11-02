// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh intent list`, `show` and `retract` over journals of drafts a real permit made (WP4),
//! through `common::ctx` over agh-fake's loopback server. The permits come from a client in one
//! scratch root, holding the agh.toml and the token files; the verbs run in another, with no
//! agh.toml and no token, so they show the profile and the grant each draft recorded while
//! loading no credential and sending nothing. The verbs' orders, refusals and damaged
//! journals are their in-crate tests (`src/cli/cmd_intent_tests.rs`); these do not repeat them.

use agh::cli::Ctx;
use agh::journal::IntentDraft;
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::WriteOp;
use agh::{IntentId, IntentStore, Journal, Permit};
use agh_fake::FakeGithub;
use aterm_json::Value;

use crate::common;

/// The dev permit for `fixture-dev/app` and the granted release permit for
/// `alabsystems/aterm`, from a client whose agh.toml and token files live under `root`.
fn permits(root: &std::path::Path, fake: &FakeGithub) -> (Permit, Permit) {
    common::canonical_tokens(root, fake, true, true);
    let (dirs, router) = common::file_router(root, common::WRITERS);
    let gh = common::writer(router, fake, &dirs);
    let dev = common::permit(
        &gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug("fixture-dev/app"))),
    );
    let granted = common::granted(&gh, "alabsystems/aterm");
    let release = common::permit(
        &gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug("alabsystems/aterm")))
            .with_named(granted),
    );
    (dev, release)
}

/// Begin a `release.create` of `v1` on `slug` under `permit` in `ctx`'s journal, as a caller
/// does: `after` is the newest intent of the key.
fn begin(ctx: &Ctx, slug: &str, permit: &Permit) -> IntentId {
    let dirs = ctx.dirs.as_ref().expect("the context's directories");
    let journal = Journal::open(dirs, &common::slug(slug), &ctx.fx)
        .unwrap_or_else(|e| panic!("open: {}", e.why));
    let after = journal
        .latest(WriteOp::ReleaseCreate, "v1")
        .unwrap_or_else(|e| panic!("latest: {}", e.why))
        .map(|view| view.intent.id);
    let draft = IntentDraft::new(
        WriteOp::ReleaseCreate,
        &common::slug(slug),
        permit,
        "fixture-fingerprint",
        "v1",
    )
    .with_request(format!("sha256:{} 812", "0".repeat(64)))
    .with_target(Some("main"))
    .with_after(after);
    journal
        .begin(&draft)
        .unwrap_or_else(|e| panic!("begin: {}", e.why))
}

/// `field` of `doc` as text, or `None` when it is null or absent.
fn text<'a>(doc: &'a Value, field: &str) -> Option<&'a str> {
    doc.get(field).and_then(Value::as_str)
}

#[test]
fn intent_documents_carry_each_permits_profile_and_grant() {
    let permit_root = common::scratch("intents-permits");
    let root = common::scratch("intents-verbs");
    let fake = FakeGithub::start();
    let (dev, release) = permits(permit_root.path(), &fake);

    let (ctx, _) = common::ctx(root.path(), &fake, common::env());
    let app = begin(&ctx, "fixture-dev/app", &dev);
    let aterm = begin(&ctx, "alabsystems/aterm", &release);

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    assert_eq!(common::run_cli(&mut ctx, &["intent", "list"]), 0);
    let listed = common::stdout_json(&captured);
    let items = listed.as_array().expect("a list");
    let rows: Vec<(Option<&str>, Option<&str>, Option<&str>)> = items
        .iter()
        .map(|item| {
            (
                text(item, "repo"),
                text(item, "identity"),
                text(item, "grant"),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                Some("alabsystems/aterm"),
                Some("release"),
                Some("fixture-caller")
            ),
            (Some("fixture-dev/app"), Some("dev"), None),
        ]
    );
    assert_eq!(
        items.get(1).and_then(|item| item.get("grant")),
        Some(&Value::Null)
    );

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let shown = ["intent", "show", "alabsystems/aterm", aterm.as_str()];
    assert_eq!(common::run_cli(&mut ctx, &shown), 0);
    let doc = common::stdout_json(&captured);
    assert_eq!(text(&doc, "id"), Some(aterm.as_str()));
    assert_eq!(text(&doc, "identity"), Some("release"));
    assert_eq!(text(&doc, "grant"), Some("fixture-caller"));
    assert_eq!(text(&doc, "op"), Some("release.create"));
    assert_eq!(text(&doc, "target_commitish"), Some("main"));
    assert_eq!(doc.get("open"), Some(&Value::from(true)));

    let (mut ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let id = app.as_str();
    let retract = ["intent", "retract", "fixture-dev/app", id, "--confirm", id];
    assert_eq!(common::run_cli(&mut ctx, &retract), 0);
    let doc = common::stdout_json(&captured);
    assert_eq!(text(&doc, "identity"), Some("dev"));
    assert_eq!(doc.get("grant"), Some(&Value::Null));
    assert_eq!(doc.get("open"), Some(&Value::from(false)));
    assert_eq!(
        doc.get("retracted").and_then(Value::as_u64),
        Some(common::SIM_START)
    );

    assert!(
        !root.path().join("tokens").exists() && !root.path().join("secrets").exists(),
        "the verbs' root holds no token"
    );
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

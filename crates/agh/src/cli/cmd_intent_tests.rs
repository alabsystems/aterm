// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh intent list`, `show` and `retract` in-crate (plan §8): journals written through
//! `Journal::open` with literal drafts, which need no `Permit`, under a context whose
//! transport is scripted with nothing, so a request would fail the test.

use std::sync::Arc;

use aterm_json::Value;

use super::super::{Captured, Ctx};
use crate::config::Dirs;
use crate::identity::MapEnv;
use crate::journal::{IntentDraft, IntentId, IntentStore, Journal, Settled};
use crate::slug::Slug;
use crate::wire::Endpoints;
use crate::wire::scripted::ScriptedTransport;
use crate::write::WriteOp;

/// A scratch root.
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("agh-intents-")
        .tempdir()
        .expect("a scratch directory")
}

/// A test context under `root` whose transport answers nothing, with no token anywhere.
fn context(root: &std::path::Path) -> (Ctx, Captured, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new());
    let (ctx, captured) = Ctx::for_tests(
        root,
        transport.clone(),
        Endpoints::loopback(9, 9, 9, 9),
        MapEnv::new(),
    );
    (ctx, captured, transport)
}

/// `cli::run` of `args`.
fn run(ctx: &mut Ctx, args: &[&str]) -> u8 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    super::super::run(&args, ctx)
}

/// The journal of `slug` under the context's directories.
fn journal(ctx: &Ctx, slug: &str) -> Journal {
    let dirs: &Dirs = ctx.dirs.as_ref().expect("dirs");
    Journal::open(dirs, &Slug::parse(slug).expect("a slug"), &ctx.fx).expect("open")
}

/// Begin a `release.create` of `key` as a caller does, after the newest intent of its key.
fn begin(journal: &Journal, slug: &str, key: &str) -> IntentId {
    let after = journal
        .latest(WriteOp::ReleaseCreate, key)
        .expect("latest")
        .map(|view| view.intent.id);
    journal
        .begin(&IntentDraft {
            op: WriteOp::ReleaseCreate,
            slug: Slug::parse(slug).expect("a slug"),
            identity: "dev".to_string(),
            fingerprint: "fixture-fingerprint".to_string(),
            key: key.to_string(),
            request: format!("sha256:{} 812", "0".repeat(64)),
            target_commitish: Some("main".to_string()),
            body_digest: None,
            grant: None,
            after,
        })
        .expect("begin")
}

/// Stdout as one JSON value.
fn stdout_json(captured: &Captured) -> Value {
    aterm_json::from_str(captured.stdout_text().trim_end()).expect("JSON on stdout")
}

/// A created settle.
fn created(id: u64) -> Settled {
    Settled::Created {
        id,
        url: format!("https://github.com/fixture-dev/app/releases/{id}"),
    }
}

/// The document's thirteen keys, sorted.
const KEYS: [&str; 13] = [
    "at_secs",
    "grant",
    "id",
    "identity",
    "key",
    "observed",
    "op",
    "open",
    "repo",
    "request",
    "retracted",
    "settled",
    "target_commitish",
];

#[test]
fn intent_list_prints_every_journal_in_order() {
    let root = scratch();
    let (mut ctx, captured, _) = context(root.path());
    for slug in ["fixture-dev/app", "alabsystems/aterm"] {
        let journal = journal(&ctx, slug);
        let first = begin(&journal, slug, "v1");
        journal.settle(&first, &created(5000)).expect("settle");
        begin(&journal, slug, "v2");
    }
    assert_eq!(run(&mut ctx, &["intent", "list"]), 0);
    let listed = stdout_json(&captured);
    let items = listed.as_array().expect("a list");
    let order: Vec<(String, String)> = items
        .iter()
        .map(|item| {
            let keys: Vec<&str> = item
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, KEYS);
            let text = |k: &str| item.get(k).and_then(Value::as_str).expect(k).to_string();
            (text("repo"), text("key"))
        })
        .collect();
    let want = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(r, k)| ((*r).to_string(), (*k).to_string()))
            .collect()
    };
    assert_eq!(
        order,
        want(&[
            ("alabsystems/aterm", "v1"),
            ("alabsystems/aterm", "v2"),
            ("fixture-dev/app", "v1"),
            ("fixture-dev/app", "v2"),
        ])
    );

    let (mut ctx, captured, _) = context(root.path());
    assert_eq!(run(&mut ctx, &["intent", "list", "--open"]), 0);
    let keys: Vec<String> = stdout_json(&captured)
        .as_array()
        .expect("a list")
        .iter()
        .map(|item| {
            item.get("key")
                .and_then(Value::as_str)
                .expect("key")
                .to_string()
        })
        .collect();
    assert_eq!(keys, ["v2", "v2"]);

    let (mut ctx, captured, _) = context(root.path());
    assert_eq!(run(&mut ctx, &["intent", "list", "fixture-dev/app"]), 0);
    let repos: Vec<String> = stdout_json(&captured)
        .as_array()
        .expect("a list")
        .iter()
        .map(|item| {
            item.get("repo")
                .and_then(Value::as_str)
                .expect("repo")
                .to_string()
        })
        .collect();
    assert_eq!(repos, ["fixture-dev/app", "fixture-dev/app"]);
}

#[test]
fn intent_show_prints_the_document() {
    let root = scratch();
    let (mut ctx, captured, _) = context(root.path());
    let journal = journal(&ctx, "fixture-dev/app");
    let id = begin(&journal, "fixture-dev/app", "v1");
    journal
        .settle(
            &id,
            &Settled::Unknown {
                why: "no answer".to_string(),
            },
        )
        .expect("settle");
    ctx.fx
        .sleep(std::time::Duration::from_secs(60))
        .expect("sleep");
    journal.observe(&id, None).expect("observe");
    assert_eq!(
        run(
            &mut ctx,
            &["intent", "show", "fixture-dev/app", id.as_str()]
        ),
        0
    );
    let doc = stdout_json(&captured);
    assert_eq!(doc.get("settled").and_then(Value::as_str), Some("unknown"));
    assert_eq!(doc.get("open"), Some(&Value::from(true)));
    let observed = doc
        .get("observed")
        .and_then(Value::as_array)
        .expect("observed");
    let [row] = observed.as_slice() else {
        panic!("one observed row: {observed:?}");
    };
    assert_eq!(
        row.get("at_secs").and_then(Value::as_u64),
        Some(1_760_000_060)
    );
    assert_eq!(row.get("found"), Some(&Value::Null));
    assert!(doc.get("fingerprint").is_none() && doc.get("body_digest").is_none());

    // A missing id names its repository, as retract's NotFound does.
    for verb in ["show", "retract"] {
        let (mut ctx, captured, _) = context(root.path());
        let mut args = vec!["intent", verb, "fixture-dev/app", "99-000000000000"];
        if verb == "retract" {
            args.extend(["--confirm", "99-000000000000"]);
        }
        assert_eq!(run(&mut ctx, &args), 6, "{verb}");
        let stderr = captured.stderr_text();
        assert!(stderr.contains("repo=fixture-dev/app"), "{verb}: {stderr}");
    }
    let (mut ctx, _, _) = context(root.path());
    assert_eq!(
        run(
            &mut ctx,
            &["intent", "show", "fixture-dev/app", "not-an-id"]
        ),
        2
    );
}

#[test]
fn intent_retract_runs_without_a_terminal_and_needs_the_id_again() {
    let root = scratch();
    let (ctx0, _, _) = context(root.path());
    let journal = journal(&ctx0, "fixture-dev/app");
    let id = begin(&journal, "fixture-dev/app", "v1");
    let other = IntentId::parse("7-000000000000").expect("an id");
    let before = std::fs::read(journal.path()).expect("bytes");
    for confirm in [None, Some(other.as_str())] {
        let (mut ctx, _, _) = context(root.path());
        let mut args = vec!["intent", "retract", "fixture-dev/app", id.as_str()];
        args.extend(confirm.iter().flat_map(|c| ["--confirm", *c]));
        assert_eq!(run(&mut ctx, &args), 1, "{args:?}");
        assert_eq!(std::fs::read(journal.path()).expect("bytes"), before);
    }

    let (mut ctx, captured, _) = context(root.path());
    let args = [
        "intent",
        "retract",
        "fixture-dev/app",
        id.as_str(),
        "--confirm",
        id.as_str(),
    ];
    assert_eq!(run(&mut ctx, &args), 0);
    let doc = stdout_json(&captured);
    assert_eq!(doc.get("open"), Some(&Value::from(false)));
    assert_eq!(
        doc.get("retracted").and_then(Value::as_u64),
        Some(1_760_000_000)
    );

    let (mut ctx, captured, _) = context(root.path());
    assert_eq!(run(&mut ctx, &args), 1);
    let stderr = captured.stderr_text();
    assert!(stderr.contains("not_open"), "{stderr}");
}

#[test]
fn intent_verbs_send_nothing_and_load_no_token() {
    let root = scratch();
    let (ctx0, _, _) = context(root.path());
    let journal = journal(&ctx0, "fixture-dev/app");
    let id = begin(&journal, "fixture-dev/app", "v1");
    let id = id.as_str();
    let runs: [&[&str]; 3] = [
        &["intent", "list"],
        &["intent", "show", "fixture-dev/app", id],
        &["intent", "retract", "fixture-dev/app", id, "--confirm", id],
    ];
    for args in runs {
        let (mut ctx, _, transport) = context(root.path());
        assert_eq!(run(&mut ctx, args), 0, "{args:?}");
        assert!(transport.requests().is_empty(), "{args:?}");
    }
}

#[test]
fn a_damaged_journal_fails_the_listing_after_what_it_read() {
    let root = scratch();
    let (mut ctx, captured, _) = context(root.path());
    let clean = journal(&ctx, "alabsystems/aterm");
    begin(&clean, "alabsystems/aterm", "v1");
    let damaged = journal(&ctx, "fixture-dev/app");
    begin(&damaged, "fixture-dev/app", "v1");
    begin(&damaged, "fixture-dev/app", "v2");
    let mut bytes = std::fs::read(damaged.path()).expect("bytes");
    bytes[30] ^= 0x01;
    std::fs::write(damaged.path(), &bytes).expect("the damage written");

    assert_eq!(run(&mut ctx, &["intent", "list"]), 1);
    let listed = stdout_json(&captured);
    let repos: Vec<&str> = listed
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|item| item.get("repo").and_then(Value::as_str))
        .collect();
    assert_eq!(repos, ["alabsystems/aterm"]);
    let stderr = captured.stderr_text();
    assert!(stderr.contains("journal_fault"), "{stderr}");
    assert!(
        stderr.contains(&damaged.path().display().to_string()),
        "{stderr}"
    );
}

#[test]
fn a_damaged_journal_keeps_its_own_items_before_the_fault_and_shows_nothing() {
    let root = scratch();
    let (mut ctx, captured, _) = context(root.path());
    let damaged = journal(&ctx, "fixture-dev/app");
    let first = begin(&damaged, "fixture-dev/app", "v1");
    let first_len = std::fs::read(damaged.path()).expect("bytes").len();
    begin(&damaged, "fixture-dev/app", "v2");
    begin(&damaged, "fixture-dev/app", "v3");
    let mut bytes = std::fs::read(damaged.path()).expect("bytes");
    // A byte inside the second record's body, past its frame and step headers.
    bytes[first_len + 30] ^= 0x01;
    std::fs::write(damaged.path(), &bytes).expect("the damage written");

    assert_eq!(run(&mut ctx, &["intent", "list", "fixture-dev/app"]), 1);
    let listed = stdout_json(&captured);
    let keys: Vec<&str> = listed
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|item| item.get("key").and_then(Value::as_str))
        .collect();
    assert_eq!(keys, ["v1"]);
    assert!(captured.stderr_text().contains("journal_fault"));

    let (mut ctx, captured, _) = context(root.path());
    assert_eq!(
        run(
            &mut ctx,
            &["intent", "show", "fixture-dev/app", first.as_str()]
        ),
        1
    );
    assert!(captured.stdout_text().is_empty());
    let stderr = captured.stderr_text();
    assert!(stderr.contains("journal_fault"), "{stderr}");
    assert_eq!(std::fs::read(damaged.path()).expect("bytes"), bytes);
}

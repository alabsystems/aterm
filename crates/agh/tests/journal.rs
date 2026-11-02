// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write journal over drafts a real permit made (WP4). `common::permit` mints each permit
//! through WP1's `Router::permit`, and `IntentDraft::new` takes the profile and the grant from
//! it. The journal's frames, lock, torn tails, faults, bound and settle window are its
//! in-crate tests (`src/journal_tests.rs`), whose drafts are literals; these do not repeat them.

use std::sync::Arc;

use agh::config::Dirs;
use agh::journal::{self, IntentDraft, IntentId, JournalEnd, JournalRecord, Settled};
use agh::policy::{Named, RouteRequest, RouteTarget};
use agh::write::WriteOp;
use agh::{Fx, Github, IntentStore, Journal, Kind, Permit, Sub};
use agh_fake::FakeGithub;

use crate::common;

/// The request digest every draft here names: `sha256:` and 64 zeros, then ` 812`.
fn request() -> String {
    format!("sha256:{} 812", "0".repeat(64))
}

/// A client over `fake` whose agh.toml (the canonical one, with `common::WRITERS`) and token
/// files live under `root`, and the directories it read them from.
fn client(root: &std::path::Path, fake: &FakeGithub) -> (Dirs, Github) {
    common::canonical_tokens(root, fake, true, true);
    let (dirs, router) = common::file_router(root, common::WRITERS);
    let gh = common::writer(router, fake, &dirs);
    (dirs, gh)
}

/// The dev-tier write permit for `fixture-dev/app`, with no grant.
fn dev_permit(gh: &Github) -> Permit {
    common::permit(
        gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug("fixture-dev/app"))),
    )
}

/// The release-tier write permit for `alabsystems/aterm`, its literal from
/// `Router::grant("fixture-caller", ..)`.
fn granted_permit(gh: &Github) -> Permit {
    let granted: Named = common::granted(gh, "alabsystems/aterm");
    common::permit(
        gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug("alabsystems/aterm")))
            .with_named(granted),
    )
}

/// A `release.create` draft of `key` on `slug` under `permit`, with its request and target.
fn draft(slug: &str, permit: &Permit, key: &str) -> IntentDraft {
    IntentDraft::new(
        WriteOp::ReleaseCreate,
        &common::slug(slug),
        permit,
        "fixture-fingerprint",
        key,
    )
    .with_request(request())
    .with_target(Some("main"))
}

/// `begin` as a caller does: `after` is the id of `latest(op, key)` at the time.
fn begin(journal: &Journal, draft: IntentDraft) -> IntentId {
    let after = journal
        .latest(draft.op, &draft.key)
        .unwrap_or_else(|e| panic!("latest: {}", e.why))
        .map(|view| view.intent.id);
    journal
        .begin(&draft.with_after(after))
        .unwrap_or_else(|e| panic!("begin: {}", e.why))
}

/// The journal of `slug` under `dirs`.
fn open(dirs: &Dirs, slug: &str, fx: &Arc<Fx>) -> Journal {
    Journal::open(dirs, &common::slug(slug), fx).unwrap_or_else(|e| panic!("open: {}", e.why))
}

/// The one Intent record of a journal, its step, and how the file ended.
fn only_intent(journal: &Journal) -> (u64, journal::Intent, JournalEnd) {
    let (records, end) = journal.read().unwrap_or_else(|e| panic!("read: {}", e.why));
    let mut intents: Vec<(u64, journal::Intent)> = records
        .into_iter()
        .filter_map(|(step, record)| match record {
            JournalRecord::Intent(intent) => Some((step, *intent)),
            _ => None,
        })
        .collect();
    assert_eq!(intents.len(), 1, "{intents:?}");
    let (step, intent) = intents.remove(0);
    (step, intent, end)
}

#[test]
fn a_dev_permit_journals_its_profile_and_no_grant() {
    let root = common::scratch("journal-dev");
    let fake = FakeGithub::start();
    let (dirs, gh) = client(root.path(), &fake);
    let permit = dev_permit(&gh);
    let fx = Arc::new(Fx::simulated(common::SIM_START));
    let journal = open(&dirs, "fixture-dev/app", &fx);
    assert_eq!(
        journal.path(),
        dirs.journal().join("fixture-dev").join("app.steps")
    );

    let id = begin(
        &journal,
        draft("fixture-dev/app", &permit, "v1")
            .with_body_digest(Some(format!("sha256:{}", "1".repeat(64)))),
    );
    let (step, intent, end) = only_intent(&journal);
    assert_eq!(end, JournalEnd::Clean);
    assert_eq!(step, 0);
    assert_eq!(intent.id, id);
    assert_eq!(intent.identity, "dev");
    assert_eq!(intent.identity, permit.profile());
    assert_eq!(intent.grant, None);
    assert_eq!(intent.fingerprint, "fixture-fingerprint");
    assert_eq!(intent.key, "v1");
    assert_eq!(intent.request, request());
    assert_eq!(intent.target_commitish.as_deref(), Some("main"));
    assert_eq!(intent.at_secs, common::SIM_START);

    let record = JournalRecord::Intent(Box::new(intent));
    let body = journal::encode(&record);
    let text = String::from_utf8(body.clone()).expect("a UTF-8 body");
    assert!(text.contains(r#""grant":null"#), "{text}");
    assert!(text.contains(r#""identity":"dev""#), "{text}");
    assert_eq!(
        journal::decode(0, 0, &body).unwrap_or_else(|e| panic!("decode: {}", e.why)),
        record
    );
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_granted_permit_journals_the_release_profile_and_its_caller() {
    let root = common::scratch("journal-granted");
    let fake = FakeGithub::start();
    let (dirs, gh) = client(root.path(), &fake);
    let permit = granted_permit(&gh);
    assert_eq!(permit.grant(), Some("fixture-caller"));
    let fx = Arc::new(Fx::simulated(common::SIM_START));
    let journal = open(&dirs, "alabsystems/aterm", &fx);

    let id = begin(&journal, draft("alabsystems/aterm", &permit, "v1"));
    let (_, intent, _) = only_intent(&journal);
    assert_eq!(intent.id, id);
    assert_eq!(intent.identity, "release");
    assert_eq!(intent.grant.as_deref(), Some("fixture-caller"));
    let body = journal::encode(&JournalRecord::Intent(Box::new(intent)));
    let text = String::from_utf8(body).expect("a UTF-8 body");
    assert!(text.contains(r#""grant":"fixture-caller""#), "{text}");
    assert!(text.contains(r#""identity":"release""#), "{text}");

    let dev = open(&dirs, "fixture-dev/app", &fx);
    begin(&dev, draft("fixture-dev/app", &dev_permit(&gh), "v1"));
    let listed: Vec<String> = journal::journals(&dirs)
        .unwrap_or_else(|e| panic!("journals: {}", e.why))
        .into_iter()
        .map(|(slug, _)| slug.to_string())
        .collect();
    assert_eq!(listed, ["alabsystems/aterm", "fixture-dev/app"]);
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn two_journal_values_begin_one_permitted_write_once() {
    let root = common::scratch("journal-two");
    let fake = FakeGithub::start();
    let (dirs, gh) = client(root.path(), &fake);
    let permit = dev_permit(&gh);
    let fx = Arc::new(Fx::simulated(common::SIM_START));
    // Two values over one file, as two agh processes hold it.
    let lane = open(&dirs, "fixture-dev/app", &fx);
    let person = open(&dirs, "fixture-dev/app", &fx);

    let first = begin(&lane, draft("fixture-dev/app", &permit, "v1"));
    let after = person
        .latest(WriteOp::ReleaseCreate, "v1")
        .unwrap_or_else(|e| panic!("latest: {}", e.why))
        .expect("the lane's intent")
        .intent
        .id;
    assert_eq!(after, first);
    let refused = person
        .begin(&draft("fixture-dev/app", &permit, "v1").with_after(Some(after)))
        .expect_err("an open intent of the key refuses a second");
    assert_eq!(refused.kind, Kind::OutcomeUnknown);
    assert_eq!(refused.exit(), 13);
    assert_eq!(
        refused.sub,
        Sub::IntentOpen {
            intent: first.to_string()
        }
    );

    lane.settle(
        &first,
        &Settled::Unknown {
            why: "no answer".to_string(),
        },
    )
    .unwrap_or_else(|e| panic!("settle: {}", e.why));
    assert_eq!(
        person
            .open()
            .unwrap_or_else(|e| panic!("open: {}", e.why))
            .len(),
        1
    );
    person
        .observe(&first, Some(5000))
        .unwrap_or_else(|e| panic!("observe: {}", e.why));
    assert!(
        lane.open()
            .unwrap_or_else(|e| panic!("open: {}", e.why))
            .is_empty()
    );

    let second = begin(&person, draft("fixture-dev/app", &permit, "v1"));
    assert_eq!(second.step(), 3, "intent, settled, observed, then this");
    let open: Vec<IntentId> = lane
        .open()
        .unwrap_or_else(|e| panic!("open: {}", e.why))
        .into_iter()
        .map(|intent| intent.id)
        .collect();
    assert_eq!(open, [second]);
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

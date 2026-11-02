// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write pipeline: call bodies, pacing, login binding and one-shot writes (WP3).

use agh::github::raw::RawSend;
use agh::journal::{JournalRecord, Settled};
use agh::policy::{RouteRequest, RouteTarget};
use agh::wire::Method;
use agh::write::{Outcome, WriteOp};
use agh::{Github, IntentStore as _, Journal};
use agh_fake::{FakeGithub, Fault, Match};

use crate::common;

const REFS: &str = "/repos/fixture-dev/app/git/refs";

struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
}

fn world(label: &str) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, true, true);
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router, &fake, &dirs);
    World {
        _root: root,
        fake,
        gh,
    }
}

fn app() -> agh::Slug {
    common::slug("fixture-dev/app")
}

fn send(method: Method, path: &str, tag: &str) -> RawSend {
    let mut send = RawSend::new(method, path, RouteRequest::write(RouteTarget::Repo(app())));
    let mut body = aterm_json::Map::new();
    body.insert(
        "ref".into(),
        aterm_json::Value::from(format!("refs/tags/{tag}")),
    );
    body.insert("sha".into(), aterm_json::Value::from(common::MAIN_SHA));
    send.body = Some(aterm_json::Value::Object(body));
    send
}

fn run(world: &World, send: &RawSend) -> (Outcome<agh::github::RawOutcome>, Journal) {
    let permit = common::permit(&world.gh, &send.route);
    let journal = world.gh.journal(&app()).expect("the journal");
    let outcome = world
        .gh
        .raw()
        .send(&permit, send, &journal)
        .expect("an outcome");
    (outcome, journal)
}

fn records(journal: &Journal) -> Vec<JournalRecord> {
    let (records, _) = journal.read().expect("the journal reads");
    records.into_iter().map(|(_, record)| record).collect()
}

#[test]
fn a_release_tier_write_without_a_literal_sends_nothing() {
    let world = world("release-default");
    let route = RouteRequest::write(RouteTarget::Repo(common::slug("alabsystems/aterm")));
    let error = world
        .gh
        .permit(&route)
        .expect_err("a release tier needs its literal");
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("policy.release_needs_literal"),
        "{error:?}"
    );
    assert!(world.fake.seen().is_empty(), "{:?}", world.fake.seen());
}

#[test]
fn a_post_is_journaled_once() {
    let world = world("post-journaled");
    let (outcome, journal) = run(&world, &send(Method::Post, REFS, "v1"));
    assert!(outcome.done().is_some(), "the POST is done");
    let records = records(&journal);
    assert_eq!(records.len(), 2, "{records:?}");
    let JournalRecord::Intent(intent) = &records[0] else {
        panic!("an Intent first: {records:?}");
    };
    assert_eq!(intent.op, WriteOp::Api);
    assert_eq!(intent.key, format!("api POST {REFS}"));
    // A ref's answer has no numeric id: `created` is id 0 and the ref's url (plan_deviations).
    let JournalRecord::Settled {
        outcome: Settled::Created { id: 0, url },
        ..
    } = &records[1]
    else {
        panic!("then Settled created: {records:?}");
    };
    assert!(url.ends_with("/git/refs/tags/v1"), "{url}");
    let posts = world
        .fake
        .seen()
        .iter()
        .filter(|s| s.method == "POST")
        .count();
    assert_eq!(posts, 1);
}

#[test]
fn a_lost_post_answer_leaves_the_intent_open() {
    let world = world("post-lost");
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::LandThenClose);
    let (outcome, journal) = run(&world, &send(Method::Post, REFS, "v1"));
    assert!(matches!(outcome, Outcome::Unknown { .. }), "{outcome:?}");
    assert_eq!(journal.open().expect("the open intents").len(), 1);
    assert!(
        world
            .fake
            .git_refs("fixture-dev/app")
            .iter()
            .any(|(name, _)| name == "refs/tags/v1"),
        "the ref landed"
    );
}

#[test]
fn a_refused_post_closes_its_intent() {
    let world = world("post-refused");
    world.fake.fault(
        Match::api(REFS).method("POST"),
        Fault::Status {
            status: 422,
            body: "{\"message\":\"Validation Failed\"}".into(),
            headers: Vec::new(),
        },
    );
    let (outcome, journal) = run(&world, &send(Method::Post, REFS, "v1"));
    assert!(matches!(outcome, Outcome::Refused(_)), "{outcome:?}");
    assert!(journal.open().expect("the open intents").is_empty());
}

#[test]
fn a_post_outside_its_permit_is_usage_before_the_journal() {
    let world = world("post-other-repo");
    let permit = common::permit(&world.gh, &RouteRequest::write(RouteTarget::Repo(app())));
    let other = common::slug("fixture-dev/other");
    let journal = world.gh.journal(&other).expect("the journal");
    let error = world
        .gh
        .raw()
        .send(
            &permit,
            &send(Method::Post, "/repos/fixture-dev/other/git/refs", "v1"),
            &journal,
        )
        .expect_err("the permit covers fixture-dev/app");
    assert_eq!(error.kind, agh::Kind::Usage, "{error:?}");
    assert!(records(&journal).is_empty(), "{:?}", records(&journal));
    assert!(world.fake.seen().is_empty(), "{:?}", world.fake.seen());
}

#[test]
fn a_patch_is_not_journaled() {
    let world = world("patch");
    world.fake.fault(
        Match::api("/repos/fixture-dev/app/git/refs/tags/v1").method("PATCH"),
        Fault::Status {
            status: 200,
            body: "{}".into(),
            headers: Vec::new(),
        },
    );
    let mut patch = send(
        Method::Patch,
        "/repos/fixture-dev/app/git/refs/tags/v1",
        "v1",
    );
    patch.body = Some(aterm_json::Value::Object(aterm_json::Map::new()));
    let (outcome, journal) = run(&world, &patch);
    assert!(outcome.done().is_some(), "the PATCH is done");
    assert!(records(&journal).is_empty());
}

#[test]
fn a_done_write_whose_body_is_not_json_is_still_done() {
    let world = world("not-json");
    let not_json = |status| Fault::Status {
        status,
        body: "not json".into(),
        headers: Vec::new(),
    };
    world
        .fake
        .fault(Match::api(REFS).method("POST"), not_json(201));
    let (outcome, journal) = run(&world, &send(Method::Post, REFS, "v1"));
    let done = outcome.done().expect("a 2xx is done");
    assert_eq!(
        done.answer,
        agh::github::RawAnswer::Bytes(b"not json".to_vec())
    );
    let settled = records(&journal).into_iter().any(|record| {
        matches!(
            record,
            JournalRecord::Settled {
                outcome: Settled::Created { .. },
                ..
            }
        )
    });
    assert!(settled, "the intent settles created");

    let tag = "/repos/fixture-dev/app/git/refs/tags/v1";
    world
        .fake
        .fault(Match::api(tag).method("PATCH"), not_json(200));
    let (outcome, _) = run(&world, &send(Method::Patch, tag, "v1"));
    assert!(outcome.done().is_some(), "the PATCH is done");
}

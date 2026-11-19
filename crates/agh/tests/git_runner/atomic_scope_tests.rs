// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use agh::journal::{IntentDraft, IntentId, IntentStore, IntentView, Settled};
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::{Outcome, WriteOp};
use agh_fake::git_backend::GitHttpBackend;

struct NoJournalAccess;
impl IntentStore for NoJournalAccess {
    fn begin(&self, _: &IntentDraft) -> agh::Result<IntentId> {
        unreachable!("unauthorized journal begin")
    }
    fn settle(&self, _: &IntentId, _: &Settled) -> agh::Result<()> {
        unreachable!("unauthorized settlement")
    }
    fn observe(&self, _: &IntentId, _: Option<u64>) -> agh::Result<()> {
        unreachable!("unauthorized observation")
    }
    fn open(&self) -> agh::Result<Vec<agh::journal::Intent>> {
        unreachable!("unauthorized journal read")
    }
    fn latest(&self, _: WriteOp, _: &str) -> agh::Result<Option<IntentView>> {
        unreachable!("unauthorized journal lookup")
    }
}

#[test]
fn real_native_main_can_land_beside_an_unknown_line_without_replaying_that_line() {
    let world = reader_world("native-disjoint-intent");
    let source = bare_repo(world.path(), "source");
    host_git(
        &source,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture-dev/app.git",
        ],
    );
    let before = host_git(&source, &["rev-parse", "main"]);
    let backend = GitHttpBackend::start(
        "fixture-dev/app",
        &source,
        &agh_fake::token("reader"),
        &agh_fake::token("dev"),
    )
    .unwrap();
    let tree = host_git(&source, &["rev-parse", "main^{tree}"]);
    let after = host_git(
        &source,
        &[
            "commit-tree",
            &tree,
            "-p",
            &before,
            "-m",
            "independent main",
        ],
    );
    host_git(&source, &["update-ref", "refs/heads/main", &after]);
    let ports = world.fake.ports();
    let fx = Arc::new(Fx::simulated(live_start()));
    let gh = git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, backend.port(), ports.cdn),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .fx(fx.clone())
    .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let journal = gh.journal(&slug).unwrap();
    let line = push_batch(&[("refs/aterm-release/line/0.207.0", ABSENT, &after)]);
    let canonical =
        agh::fx::canon::git_request(&GitRun::push_refs(&slug, &source, line.clone(), false));
    // Import the old begin protocol: same durable key/bytes, no live scoped
    // judgment. The owner remains live and its Unknown must never be retracted.
    let held = journal
        .begin(
            &IntentDraft::new(
                WriteOp::RefUpdateAtomic,
                &slug,
                &permit,
                "fixture-fingerprint",
                "atomic refs",
            )
            .with_request(agh::fx::canon::request_digest(&canonical))
            .with_body_digest(Some(agh::model::Sha256Digest::of(&canonical).to_string()))
            .with_refs(line.updates().to_vec()),
        )
        .unwrap();
    journal
        .settle(
            &held,
            &Settled::Unknown {
                why: "original receive unacknowledged".into(),
            },
        )
        .unwrap();
    let original = journal.view(&held).unwrap().unwrap();
    let prefix = std::fs::read(journal.path()).unwrap();

    let main = push_batch(&[("refs/heads/main", &before, &after)]);
    assert!(matches!(
        gh.refs(&slug)
            .push_atomic(&permit, &source, &main, &journal)
            .unwrap(),
        Outcome::Done(_)
    ));
    assert_eq!(
        host_git(backend.repository(), &["rev-parse", "refs/heads/main"]),
        after
    );
    let completed = journal
        .latest(WriteOp::RefUpdateAtomic, "atomic refs")
        .unwrap()
        .unwrap();
    assert!(!completed.open);
    assert!(matches!(
        completed.intent.writer,
        Some(agh::journal::IntentWriter::Tracked { git: Some(_), .. })
    ));
    assert_eq!(journal.view(&held).unwrap().unwrap(), original);
    assert!(std::fs::read(journal.path()).unwrap().starts_with(&prefix));
    let children = fx.children_run().len();
    let error = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &line, &journal)
        .unwrap_err();
    assert!(matches!(&error.sub, Sub::IntentOpen { intent } if intent == &held.to_string()));
    assert_eq!(
        fx.children_run().len(),
        children,
        "an overlapping unknown must not dispatch Git"
    );
    assert!(
        host_git(
            backend.repository(),
            &["for-each-ref", "refs/aterm-release/line/"]
        )
        .is_empty()
    );

    let other = common::slug("fixture-dev/other");
    assert_eq!(
        gh.refs(&other)
            .push_atomic(&permit, &source, &main, &NoJournalAccess)
            .unwrap_err()
            .kind,
        Kind::Usage
    );
    assert_eq!(fx.children_run().len(), children);
}

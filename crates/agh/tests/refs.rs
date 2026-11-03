// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Refs: `ref create` over the API and the leased `ref delete` (WP5).

use agh::config::Dirs;
use agh::github::refs::Refs;
use agh::journal::{IntentId, JournalRecord, Settled};
use agh::model::RefObject;
use agh::policy::{Permit, RouteRequest, RouteTarget};
use agh::write::{Deleted, Effect, Ensured, Outcome, WriteOp};
use agh::{Error, Github, IntentStore as _, Journal, Kind};
use agh_fake::{CommitSpec, FakeGithub, FakeOrigin, Fault, Match};

use crate::common;

const APP: &str = "fixture-dev/app";
const REFS: &str = "/repos/fixture-dev/app/git/refs";
/// A commit `add_commit` gives the fake, which is not `main`'s head.
const OTHER_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";

struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    _dirs: Dirs,
    gh: Github,
    journal: Journal,
}

fn world(label: &str) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, true, true);
    fake.add_commit(
        APP,
        CommitSpec {
            sha: OTHER_SHA.to_string(),
            message: "other".to_string(),
            author_login: None,
            author_name: "fixture".to_string(),
            date: "2026-10-01T00:00:00Z".to_string(),
            paths: Vec::new(),
        },
    );
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router, &fake, &dirs);
    let journal = gh.journal(&common::slug(APP)).expect("the journal");
    World {
        _root: root,
        fake,
        _dirs: dirs,
        gh,
        journal,
    }
}

impl World {
    fn refs(&self) -> Refs<'_> {
        self.gh.refs(&common::slug(APP))
    }

    fn permit(&self) -> Permit {
        common::permit(
            &self.gh,
            &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
        )
    }

    fn create(&self, reference: &str, sha: &str) -> agh::Result<Outcome<Ensured<RefObject>>> {
        self.refs()
            .create(&self.permit(), reference, sha, &self.journal)
    }

    fn records(&self) -> Vec<JournalRecord> {
        let (records, _) = self.journal.read().expect("the journal reads");
        records.into_iter().map(|(_, record)| record).collect()
    }

    /// The Observed records of `intent`, what each found.
    fn observed(&self, intent: &IntentId) -> Vec<Option<u64>> {
        self.records()
            .into_iter()
            .filter_map(|record| match record {
                JournalRecord::Observed {
                    intent: id, found, ..
                } if &id == intent => Some(found),
                _ => None,
            })
            .collect()
    }

    fn posts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.method == "POST" && s.path_and_query == REFS)
            .count()
    }

    fn git_requests(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.origin == FakeOrigin::Web)
            .count()
    }
}

fn done(outcome: Outcome<Ensured<RefObject>>, effect: Effect) -> RefObject {
    let Outcome::Done(ensured) = outcome else {
        panic!("done, {effect:?}: {outcome:?}");
    };
    assert_eq!(ensured.effect, effect);
    ensured.object
}

fn assert_conflict(error: &Error, sub: &str) {
    assert_eq!(error.kind, Kind::Conflict, "{error:?}");
    assert_eq!(error.exit(), 9);
    assert_eq!(error.sub.slug().as_deref(), Some(sub), "{error:?}");
}

#[test]
fn ref_create_posts_once_and_journals() {
    let world = world("refs-create");
    let made = done(
        world
            .create("refs/tags/v1", common::MAIN_SHA)
            .expect("an outcome"),
        Effect::Created,
    );
    assert_eq!(made.object.sha, common::MAIN_SHA);
    let bodies = world.fake.bodies();
    assert_eq!(
        bodies.last().and_then(|b| b.json.as_deref()),
        Some(format!(r#"{{"ref":"refs/tags/v1","sha":"{}"}}"#, common::MAIN_SHA).as_str())
    );
    let records = world.records();
    assert_eq!(records.len(), 2, "{records:?}");
    let JournalRecord::Intent(intent) = &records[0] else {
        panic!("an Intent first: {records:?}");
    };
    assert_eq!(intent.op, WriteOp::RefCreate);
    assert_eq!(intent.op.slug(), "ref.create");
    assert_eq!(intent.key, "refs/tags/v1");
    let JournalRecord::Settled {
        outcome: Settled::Created { id, url },
        ..
    } = &records[1]
    else {
        panic!("then Settled created: {records:?}");
    };
    assert_eq!((*id, url.as_str()), (0, made.url.as_str()));
    assert_eq!(world.posts(), 1);
}

#[test]
fn ref_create_converges_on_the_same_sha() {
    let world = world("refs-same");
    world.fake.set_tag(APP, "v1", common::MAIN_SHA);
    let there = done(
        world
            .create("refs/tags/v1", common::MAIN_SHA)
            .expect("an outcome"),
        Effect::Converged,
    );
    assert_eq!(there.object.sha, common::MAIN_SHA);
    let records = world.records();
    assert!(
        records.iter().any(|r| matches!(
            r,
            JournalRecord::Settled {
                outcome: Settled::Refused { status: 422 },
                ..
            }
        )),
        "{records:?}"
    );
    let gets = world
        .fake
        .seen()
        .iter()
        .filter(|s| s.method == "GET" && s.path_and_query.ends_with("/git/ref/tags/v1"))
        .count();
    assert_eq!(gets, 1);
}

#[test]
fn ref_create_refuses_another_sha() {
    let world = world("refs-other");
    world.fake.set_tag(APP, "v1", OTHER_SHA);
    let outcome = world
        .create("refs/tags/v1", common::MAIN_SHA)
        .expect("an outcome");
    let Outcome::Refused(error) = outcome else {
        panic!("refused: {outcome:?}");
    };
    assert_conflict(&error, "already_exists");
}

#[test]
fn a_lost_ref_create_is_discovered() {
    let world = world("refs-lost");
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::LandThenClose);
    done(
        world
            .create("refs/tags/v1", common::MAIN_SHA)
            .expect("an outcome"),
        Effect::Converged,
    );
    let records = world.records();
    let JournalRecord::Intent(intent) = &records[0] else {
        panic!("an Intent first: {records:?}");
    };
    assert_eq!(world.observed(&intent.id), [Some(0)]);
    assert!(world.journal.open().expect("the journal reads").is_empty());
}

#[test]
fn an_older_open_intent_is_closed_by_a_later_create() {
    let world = world("refs-older");
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::Close);
    let Outcome::Unknown { intent, .. } = world
        .create("refs/tags/v1", common::MAIN_SHA)
        .expect("an outcome")
    else {
        panic!("a lost answer");
    };
    done(
        world
            .create("refs/tags/v1", common::MAIN_SHA)
            .expect("an outcome"),
        Effect::Created,
    );
    assert_eq!(world.observed(&intent).last(), Some(&Some(0)));
    assert!(world.journal.open().expect("the journal reads").is_empty());
    assert_eq!(world.posts(), 2);
}

#[test]
fn an_older_intent_for_another_sha_stays_open() {
    let world = world("refs-older-other");
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::Close);
    let Outcome::Unknown { intent, .. } =
        world.create("refs/tags/v1", OTHER_SHA).expect("an outcome")
    else {
        panic!("a lost answer");
    };
    done(
        world
            .create("refs/tags/v1", common::MAIN_SHA)
            .expect("an outcome"),
        Effect::Created,
    );
    assert!(
        world.observed(&intent).iter().all(Option::is_none),
        "the first intent is never recorded found"
    );
    let open = world.journal.open().expect("the journal reads");
    assert!(open.iter().any(|o| o.id == intent), "{open:?}");
}

#[test]
fn refs_and_shas_are_checked_before_anything_is_sent() {
    let world = world("refs-rule");
    let bad = [
        "refs/pull/1/head",
        "refs/tags/../x",
        "heads/main",
        "refs/heads/a:b",
        "refs/tags/v1^{}",
        "refs/heads/x.lock",
        "refs/heads/a b",
        "refs/heads/*",
        "refs/heads/x.",
        "refs/heads//x",
        "refs/heads/.x",
        "refs/heads/",
        "refs/heads/a@{b",
    ];
    for reference in bad {
        let error = world.refs().get(reference).expect_err(reference);
        assert_eq!(error.kind, Kind::Usage, "{reference}: {error:?}");
        let error = world
            .create(reference, common::MAIN_SHA)
            .expect_err(reference);
        assert_eq!(error.kind, Kind::Usage, "{reference}: {error:?}");
    }
    let short = &common::MAIN_SHA[..39];
    let error = world
        .create("refs/tags/v1", short)
        .expect_err("a 39-hex sha");
    assert_eq!(error.kind, Kind::Usage, "{error:?}");
    assert!(world.fake.seen().is_empty(), "{:?}", world.fake.seen());
    let accepted = world
        .refs()
        .get("refs/heads/release-1.2/x")
        .expect_err("no such branch");
    assert_eq!(accepted.kind, Kind::NotFound, "{accepted:?}");
}

#[test]
fn ref_get_reads_one_ref() {
    let world = world("refs-get");
    let main = world.refs().get("refs/heads/main").expect("main");
    assert_eq!(main.object.sha, common::MAIN_SHA);
    assert_eq!(main.object.kind, "commit");
    let error = world.refs().get("refs/tags/none").expect_err("no such tag");
    assert_eq!((error.kind, error.exit()), (Kind::NotFound, 6), "{error:?}");
}

#[test]
fn delete_expect_of_a_missing_ref_is_gone() {
    let world = world("refs-gone");
    let gone = world
        .refs()
        .delete_expect(&world.permit(), "refs/tags/v1", common::MAIN_SHA)
        .expect("a missing ref");
    assert_eq!(gone, Deleted::Gone);
    assert_eq!(world.git_requests(), 0);
}

/// Design §6.4's push check comes first: under a profile that covers every repository, a
/// repository that is not there is NotFound, never `Gone`, and nothing is pushed.
#[test]
fn delete_expect_of_a_missing_repository_is_not_found() {
    let root = common::scratch("refs-no-repo");
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, true, true);
    let dirs = Dirs::under(root.path());
    let config = common::canonical_config(root.path()).replacen(
        "login = \"fixture-dev-bot\"\n",
        "login = \"fixture-dev-bot\"\ncovers_all_repos = true\n",
        1,
    );
    common::write_config(&dirs, &config);
    let validated = agh::config::load_for_host(&dirs)
        .unwrap_or_else(|e| panic!("agh.toml did not load: {}", e.why))
        .expect("an agh.toml");
    let gh = common::writer(agh::policy::Router::from_validated(validated), &fake, &dirs);
    let gone = common::slug("fixture-dev/gone");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(gone.clone())));
    let error = gh
        .refs(&gone)
        .delete_expect(&permit, "refs/tags/v1", common::MAIN_SHA)
        .expect_err("no repository");
    assert_eq!(error.kind, Kind::NotFound, "{error:?}");
    assert!(
        !fake.seen().iter().any(|s| s.origin == FakeOrigin::Web),
        "{:?}",
        fake.seen()
    );
}

/// A writer without push on the repository is Forbidden `no_push` before the ref is read,
/// and nothing is pushed.
#[test]
fn delete_expect_without_push_is_forbidden() {
    let world = world("refs-no-push");
    world.fake.add_repo(
        "fixture-dev/read-only",
        agh_fake::RepoSpec::private().pull("fixture-dev-bot"),
    );
    let slug = common::slug("fixture-dev/read-only");
    let permit = common::permit(
        &world.gh,
        &RouteRequest::write(RouteTarget::Repo(slug.clone())),
    );
    let error = world
        .gh
        .refs(&slug)
        .delete_expect(&permit, "refs/tags/v1", common::MAIN_SHA)
        .expect_err("no push");
    assert_eq!(error.kind, Kind::Forbidden, "{error:?}");
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("forbid.no_push"),
        "{error:?}"
    );
    assert_eq!(world.git_requests(), 0);
}

#[test]
fn delete_expect_refuses_a_moved_ref_before_the_push() {
    let world = world("refs-moved");
    world.fake.set_tag(APP, "v1", OTHER_SHA);
    let error = world
        .refs()
        .delete_expect(&world.permit(), "refs/tags/v1", common::MAIN_SHA)
        .expect_err("the ref moved");
    assert_conflict(&error, "guard_failed");
    let agh::Sub::GuardFailed { what } = &error.sub else {
        panic!("{error:?}");
    };
    assert!(
        what.contains(OTHER_SHA) && what.contains(common::MAIN_SHA),
        "{what}"
    );
    assert_eq!(world.git_requests(), 0);
}

/// A ref create GitHub answered 201 whose body is not a ref took effect: its error says so.
#[test]
fn a_created_ref_whose_answer_does_not_parse_is_applied() {
    let world = world("refs-garbled");
    world.fake.fault(
        Match::api(REFS).method("POST"),
        Fault::Status {
            status: 201,
            body: r#"{"url":"https://api.invalid/refs/tags/v1"}"#.to_string(),
            headers: Vec::new(),
        },
    );
    let error = world
        .create("refs/tags/v1", common::MAIN_SHA)
        .expect_err("an answer that is not a ref");
    assert_eq!(error.side_effect, agh::SideEffect::Applied, "{error:?}");
}

/// A ref POST whose answer was lost, then a ref read that cannot connect: the POST may have
/// landed, so the outcome is unknown and the intent stays open, never the read's own error.
#[test]
fn a_lost_ref_create_whose_read_fails_is_unknown() {
    let world = world("refs-lost-unread");
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::Close);
    world.fake.fault_always(
        Match::api("/repos/fixture-dev/app/git/ref/").method("GET"),
        Fault::Close,
    );
    let outcome = world
        .create("refs/tags/v1", common::MAIN_SHA)
        .expect("an outcome, not an error");
    let Outcome::Unknown { intent, error } = outcome else {
        panic!("unknown: {outcome:?}");
    };
    assert_eq!(error.side_effect, agh::SideEffect::Unknown, "{error:?}");
    let open = world.journal.open().expect("the journal reads");
    assert_eq!(
        open.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        [intent]
    );
    assert_eq!(world.posts(), 1);
}

/// A ref POST whose answer was lost, then a ref found at another sha: the POST may have made
/// the ref before something moved it, so the Conflict says the effect is unknown and names
/// the open intent.
#[test]
fn a_lost_ref_create_found_elsewhere_is_of_unknown_effect() {
    let world = world("refs-lost-elsewhere");
    world.fake.set_tag(APP, "v1", OTHER_SHA);
    world
        .fake
        .fault(Match::api(REFS).method("POST"), Fault::Close);
    let error = world
        .create("refs/tags/v1", common::MAIN_SHA)
        .expect_err("the ref points elsewhere");
    assert_conflict(&error, "already_exists");
    assert_eq!(error.side_effect, agh::SideEffect::Unknown, "{error:?}");
    let open = world.journal.open().expect("the journal reads");
    assert_eq!(open.len(), 1, "{open:?}");
    let next = &error.next;
    assert!(next.contains(&open[0].id.to_string()), "{next}");
    assert_eq!(world.posts(), 1);
}

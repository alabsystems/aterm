// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `release ensure`: each step once, a rerun converging (WP5, bound to AghEnsureOnce).
//!
//! The composed `Releases::create` along the model's paths, on `fixture-dev/app`.

use std::sync::Arc;

use agh::config::Dirs;
use agh::fx::Fx;
use agh::github::releases::{DraftRequest, Releases};
use agh::identity::MapEnv;
use agh::journal::{JournalRecord, Settled};
use agh::model::ReleaseObject;
use agh::policy::{Permit, RouteRequest, RouteTarget, Router};
use agh::rate::RateBook;
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::write::{Effect, Ensured, Outcome};
use agh::{Error, Github, GithubBuilder, IntentStore as _, Journal, Kind};
use agh_fake::{Change, FakeGithub, Fault, Match, ReleaseSpec};

use crate::common;

const APP: &str = "fixture-dev/app";
const RELEASES: &str = "/repos/fixture-dev/app/releases";
/// agh-fake's 422 body A.
const BODY_A: &str = r#"{"message":"Validation Failed","errors":[{"resource":"Release","code":"already_exists","field":"tag_name"}]}"#;

struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    dirs: Dirs,
    router: Router,
    gh: Github,
    journal: Journal,
}

fn world(label: &str) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, true, true);
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router.clone(), &fake, &dirs);
    let journal = gh.journal(&common::slug(APP)).expect("the journal");
    World {
        _root: root,
        fake,
        dirs,
        router,
        gh,
        journal,
    }
}

impl World {
    fn releases(&self) -> Releases<'_> {
        self.gh.releases(&common::slug(APP))
    }

    fn permit(&self) -> Permit {
        common::permit(
            &self.gh,
            &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
        )
    }

    fn create(&self) -> Outcome<Ensured<ReleaseObject>> {
        self.releases()
            .create(&self.permit(), &request(), &self.journal)
            .expect("an outcome")
    }

    fn posts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|s| s.method == "POST" && s.path_and_query == RELEASES)
            .count()
    }

    fn drafts(&self) -> usize {
        self.fake.releases_of(APP).len()
    }

    fn records(&self) -> Vec<JournalRecord> {
        let (records, _) = self.journal.read().expect("the journal reads");
        records.into_iter().map(|(_, record)| record).collect()
    }

    fn post_fault(&self, fault: Fault) {
        self.fake.fault(Match::api(RELEASES).method("POST"), fault);
    }

    /// The one release the fake holds for `fixture-dev/app` (hidden ones included).
    fn draft_id(&self) -> u64 {
        let held = self.fake.releases_of(APP);
        assert_eq!(held.len(), 1, "{held:?}");
        held[0].id
    }
}

fn request() -> DraftRequest {
    DraftRequest {
        target_commitish: Some("main".to_string()),
        body: "notes".to_string(),
        ..DraftRequest::new("v1")
    }
}

fn done(outcome: Outcome<Ensured<ReleaseObject>>, effect: Effect) -> ReleaseObject {
    let Outcome::Done(ensured) = outcome else {
        panic!("done, {effect:?}: {outcome:?}");
    };
    assert_eq!(ensured.effect, effect);
    ensured.object
}

/// `Outcome::Unknown`'s error.
fn unknown(outcome: Outcome<Ensured<ReleaseObject>>) -> Error {
    let Outcome::Unknown { error, .. } = outcome else {
        panic!("unknown: {outcome:?}");
    };
    error
}

fn assert_still_open(error: &Error) {
    assert_eq!(error.kind, Kind::OutcomeUnknown, "{error:?}");
    assert_eq!(error.exit(), 13);
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("intent_open"),
        "{error:?}"
    );
}

/// The records' kinds, in order: `intent`, `settled:{slug}`, `observed:{found}`, `retracted`.
fn kinds(records: &[JournalRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| match record {
            JournalRecord::Intent(_) => "intent".to_string(),
            JournalRecord::Settled { outcome, .. } => match outcome {
                Settled::NeverSent => "settled:never_sent".to_string(),
                Settled::Refused { status } => format!("settled:refused {status}"),
                Settled::Created { .. } => "settled:created".to_string(),
                Settled::Unknown { .. } => "settled:unknown".to_string(),
                other => format!("settled:{other:?}"),
            },
            JournalRecord::Observed { found, .. } => match found {
                Some(_) => "observed:found".to_string(),
                None => "observed:none".to_string(),
            },
            JournalRecord::Retracted { .. } => "retracted".to_string(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn begin_send_answer_created_settle() {
    let world = world("eo-created");
    let made = done(world.create(), Effect::Created);
    let again = done(world.create(), Effect::Converged);
    assert_eq!(again.id, made.id);
    assert_eq!(world.posts(), 1);
    assert_eq!(world.drafts(), 1);
}

/// The wire refuses the POST before a byte leaves: the client's rate book says the core
/// budget is spent, which the guard reads (`rate_book(false)`) pass and the POST does not.
#[test]
fn begin_not_sent() {
    let world = world("eo-not-sent");
    let gh = GithubBuilder::new(world.router.clone())
        .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
        .endpoints(common::endpoints(&world.fake))
        .env(Arc::new(MapEnv::new()))
        .fx(Arc::new(Fx::simulated(common::SIM_START)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .rate_book(RateBook::at(world.dirs.rate()))
        .journal(world.dirs.clone())
        .build();
    let reset = (common::SIM_START + 3600).to_string();
    world.fake.fault(
        Match::api(&format!("{RELEASES}?")).method("GET"),
        Fault::Status {
            status: 200,
            body: "[]".to_string(),
            headers: common::headers(&[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &reset),
                ("x-ratelimit-used", "5000"),
                ("x-ratelimit-resource", "core"),
            ]),
        },
    );
    let permit = common::permit(
        &gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
    );
    let journal = gh.journal(&common::slug(APP)).expect("the journal");
    let outcome = gh
        .releases(&common::slug(APP))
        .create(&permit, &request(), &journal)
        .expect("an outcome");
    assert!(matches!(outcome, Outcome::NeverSent(_)), "{outcome:?}");
    assert_eq!(
        kinds(&world.records()),
        ["intent", "settled:never_sent"],
        "{:?}",
        world.records()
    );
    assert_eq!(world.posts(), 0);
    assert_eq!(world.drafts(), 0);
}

/// A client under the production policy, `RetryPolicy::STANDARD`, on the world's fake.
fn standard(world: &World) -> Github {
    GithubBuilder::new(world.router.clone())
        .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
        .endpoints(common::endpoints(&world.fake))
        .env(Arc::new(MapEnv::new()))
        .fx(Arc::new(Fx::simulated(common::SIM_START)))
        .caller("test")
        .retry(RetryPolicy::STANDARD)
        .journal(world.dirs.clone())
        .build()
}

/// The create POST is one-shot under the production policy too: a 502, and an answer lost
/// after the draft landed, are each posted once, with no pause slept for a retry. Every other
/// create test runs under `RetryPolicy::NONE`, where no retry is ever asked about, so only
/// this one tells a create that would be re-posted (two drafts) from one that is not.
#[test]
fn a_create_answered_502_under_the_standard_policy_posts_once() {
    for (label, fault, landed) in [
        (
            "eo-standard-502",
            Fault::Status {
                status: 502,
                body: "{}".to_string(),
                headers: Vec::new(),
            },
            false,
        ),
        ("eo-standard-lost", Fault::LandThenClose, true),
    ] {
        let world = world(label);
        let gh = standard(&world);
        let permit = common::permit(
            &gh,
            &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
        );
        let journal = gh.journal(&common::slug(APP)).expect("the journal");
        world.post_fault(fault);
        let outcome = gh
            .releases(&common::slug(APP))
            .create(&permit, &request(), &journal)
            .expect("an outcome");
        if landed {
            let found = done(outcome, Effect::Converged);
            assert_eq!(found.id, world.draft_id(), "{label}");
        } else {
            let error = unknown(outcome);
            assert_eq!(
                error.side_effect,
                agh::SideEffect::Unknown,
                "{label}: {error:?}"
            );
            assert_eq!(world.drafts(), 0, "{label}");
        }
        assert_eq!(world.posts(), 1, "{label}");
        assert!(gh.fx().slept().is_empty(), "{label}: {:?}", gh.fx().slept());
    }
}

#[test]
fn begin_send_answer_refused_settle_begin() {
    let world = world("eo-refused");
    world.post_fault(Fault::Status {
        status: 422,
        body: BODY_A.to_string(),
        headers: Vec::new(),
    });
    let refused = world.create();
    assert!(matches!(refused, Outcome::Refused(_)), "{refused:?}");
    done(world.create(), Effect::Created);
    assert_eq!(world.posts(), 2);
    assert_eq!(world.drafts(), 1);
}

#[test]
fn begin_send_landed_lost_settle_discover() {
    let world = world("eo-landed-lost");
    world.post_fault(Fault::LandThenClose);
    let found = done(world.create(), Effect::Converged);
    assert_eq!(found.id, world.draft_id());
    assert_eq!(
        kinds(&world.records()),
        ["intent", "settled:unknown", "observed:found"]
    );
    assert_eq!(world.posts(), 1);
    assert_eq!(world.drafts(), 1);
}

#[test]
fn a_landed_lost_post_under_the_listing_lag_waits_for_the_reveal() {
    let world = world("eo-landed-lag");
    world.fake.set_listing_lag(APP, true);
    world.post_fault(Fault::LandThenClose);
    unknown(world.create());
    assert_still_open(&unknown(world.create()));
    assert_eq!(world.posts(), 1);
    world.fake.apply(Change::Reveal {
        release_id: world.draft_id(),
    });
    done(world.create(), Effect::Converged);
    assert_eq!(world.posts(), 1);
    assert_eq!(world.drafts(), 1);
}

#[test]
fn begin_send_failed_lost_settle_discover_rediscover() {
    let world = world("eo-failed-lost");
    world.post_fault(Fault::Close);
    unknown(world.create());
    assert_still_open(&unknown(world.create()));
    assert_eq!(world.posts(), 1);
    assert_eq!(world.drafts(), 0);
}

#[test]
fn a_crash_after_the_post_leaves_the_intent_open() {
    let world = world("eo-crash");
    world.fake.set_listing_lag(APP, true);
    let mut spec = ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z");
    spec.body = Some("notes".to_string());
    let draft = world.fake.add_release(APP, spec);
    // The Intent the create would make, begun by the run that then crashed.
    let permit = world.permit();
    let intent = agh::journal::IntentDraft::new(
        agh::write::WriteOp::ReleaseCreate,
        &common::slug(APP),
        &permit,
        "taped",
        "v1",
    )
    .with_target(Some("main"))
    .with_body_digest(Some(notes_digest()))
    .with_request("sha256:00".to_string());
    world
        .journal
        .begin(&intent)
        .expect("the crashed run's intent");
    assert_still_open(&unknown(world.create()));
    assert_eq!(world.posts(), 0);
    world.fake.apply(Change::Reveal { release_id: draft });
    let found = done(world.create(), Effect::Converged);
    assert_eq!(found.id, draft);
    assert_eq!(
        kinds(&world.records()).last().map(String::as_str),
        Some("observed:found")
    );
    assert_eq!(world.posts(), 0);
}

#[test]
fn a_retracted_intent_lets_the_next_create_post() {
    let world = world("eo-retract");
    world.post_fault(Fault::Close);
    let Outcome::Unknown { intent, .. } = world.create() else {
        panic!("a lost answer is unknown");
    };
    world.journal.retract(&intent).expect("the retract");
    done(world.create(), Effect::Created);
    assert_eq!(world.posts(), 2);
    assert_eq!(world.drafts(), 1);
}

#[test]
fn a_person_deleted_draft_is_posted_again() {
    let world = world("eo-deleted");
    let made = done(world.create(), Effect::Created);
    world.fake.apply(Change::DeleteRelease {
        release_id: made.id,
    });
    let again = done(world.create(), Effect::Created);
    assert_ne!(again.id, made.id);
    assert_eq!(world.posts(), 2);
    assert_eq!(world.drafts(), 1);
}

/// `sha256:` and the hex of sha256 of `notes`.
fn notes_digest() -> String {
    format!(
        "sha256:{}",
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b"notes"))
    )
}

/// The journal with every `observe` refused: a discovery that fails at its journal step.
struct ObserveRefused<'a>(&'a Journal);

impl agh::IntentStore for ObserveRefused<'_> {
    fn begin(&self, draft: &agh::journal::IntentDraft) -> agh::Result<agh::IntentId> {
        self.0.begin(draft)
    }

    fn settle(&self, id: &agh::IntentId, outcome: &Settled) -> agh::Result<()> {
        self.0.settle(id, outcome)
    }

    fn observe(&self, _: &agh::IntentId, _: Option<u64>) -> agh::Result<()> {
        Err(Error::could_not_run("the journal is full (test)"))
    }

    fn open(&self) -> agh::Result<Vec<agh::journal::Intent>> {
        self.0.open()
    }

    fn latest(
        &self,
        op: agh::write::WriteOp,
        key: &str,
    ) -> agh::Result<Option<agh::journal::IntentView>> {
        self.0.latest(op, key)
    }
}

/// A POST that landed with its answer lost, then a discovery that fails: the outcome is
/// unknown and the intent stays open (exit 13), never a refusal or `not_sent`.
#[test]
fn a_failed_discovery_after_a_lost_post_is_unknown() {
    let world = world("eo-discovery-failed");
    world.post_fault(Fault::LandThenClose);
    let outcome = world
        .releases()
        .create(&world.permit(), &request(), &ObserveRefused(&world.journal))
        .expect("an outcome, not an error");
    let Outcome::Unknown { intent, error } = outcome else {
        panic!("unknown: {outcome:?}");
    };
    assert_ne!(error.side_effect, agh::SideEffect::None, "{error:?}");
    let open = world.journal.open().expect("the journal reads");
    assert_eq!(
        open.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        [intent]
    );
    assert_eq!(world.posts(), 1);
}

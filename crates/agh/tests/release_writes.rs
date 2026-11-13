// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The release protocol: create, upload, publish, clobber, edit and delete over agh-fake (WP5).

use std::collections::BTreeMap;

use agh::config::Dirs;
use agh::github::releases::{
    DraftRequest, EnsureSpec, MakeLatest, PublishGuard, PublishPatch, ReleaseEdit, Releases,
    Replace, UploadFile,
};
use agh::journal::{JournalRecord, Settled};
use agh::model::{ReleaseObject, Sha256Digest};
use agh::policy::{Access, Permit, RouteRequest, RouteTarget};
use agh::retry::RetryPolicy;
use agh::write::{Deleted, Effect, Ensured, Outcome};
use agh::{Error, Github, How, IntentStore as _, Journal, Kind};
use agh_fake::{AssetSpec, Change, FakeGithub, Fault, Match, ReleaseSpec};

use crate::common;

const APP: &str = "fixture-dev/app";
const ATERM: &str = "alabsystems/aterm";
const RELEASES: &str = "/repos/fixture-dev/app/releases";
/// The push check's read (design §6.4).
const REPO: &str = "/repos/fixture-dev/app";
/// agh-fake's 422 body N: an asset of the name exists.
const BODY_N: &str = r#"{"message":"Validation Failed","errors":[{"resource":"ReleaseAsset","code":"already_exists","field":"name"}]}"#;

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    _dirs: Dirs,
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
        root,
        fake,
        _dirs: dirs,
        gh,
    }
}

impl World {
    fn releases(&self) -> Releases<'_> {
        self.gh.releases(&common::slug(APP))
    }

    /// The releases under the standard retry policy, so a Converging write tries again.
    fn retrying(&self) -> Releases<'_> {
        self.releases()
            .how(How::read().retry_policy(RetryPolicy::STANDARD))
    }

    fn permit(&self) -> Permit {
        common::permit(
            &self.gh,
            &RouteRequest::write(RouteTarget::Repo(common::slug(APP))),
        )
    }

    fn journal(&self) -> Journal {
        self.gh.journal(&common::slug(APP)).expect("the journal")
    }

    fn create(&self, req: &DraftRequest) -> agh::Result<Outcome<Ensured<ReleaseObject>>> {
        self.releases().create(&self.permit(), req, &self.journal())
    }

    /// A draft `v1` made through `create`.
    fn draft(&self) -> ReleaseObject {
        done(
            self.create(&DraftRequest::new("v1")).expect("an outcome"),
            Effect::Created,
        )
    }

    /// The release as the writer reads it now.
    fn reread(&self, id: u64) -> ReleaseObject {
        self.releases()
            .how(How::writer())
            .get(id)
            .expect("the release")
    }

    fn file(&self, name: &str, bytes: &[u8]) -> UploadFile {
        let path = common::file(self.root.path(), name, bytes);
        UploadFile::open(&path, Some(name)).expect("the upload file")
    }

    fn mark(&self) -> usize {
        self.fake.seen().len()
    }

    /// `(method, path_and_query)` of each request since `mark`.
    fn since(&self, mark: usize) -> Vec<(String, String)> {
        self.fake
            .seen()
            .into_iter()
            .skip(mark)
            .map(|s| (s.method, s.path_and_query))
            .collect()
    }

    /// The write requests since `mark`.
    fn writes_since(&self, mark: usize) -> Vec<(String, String)> {
        self.since(mark)
            .into_iter()
            .filter(|(method, _)| method != "GET" && method != "HEAD")
            .collect()
    }

    /// The methods since `mark` after the write's push check (design §6.4), which must be
    /// the first request: `GET /repos/fixture-dev/app`.
    fn checked_since(&self, mark: usize) -> Vec<String> {
        let requests = self.since(mark);
        assert_eq!(
            requests.first(),
            Some(&("GET".to_string(), REPO.to_string())),
            "the push check first: {requests:?}"
        );
        requests.into_iter().skip(1).map(|(m, _)| m).collect()
    }

    fn records(&self) -> Vec<JournalRecord> {
        let (records, _) = self.journal().read().expect("the journal reads");
        records.into_iter().map(|(_, record)| record).collect()
    }

    fn assets(&self, release: u64) -> Vec<(u64, String, String, String)> {
        self.fake
            .releases_of(APP)
            .into_iter()
            .find(|r| r.id == release)
            .expect("the release")
            .assets
    }
}

fn done<T: std::fmt::Debug>(outcome: Outcome<Ensured<T>>, effect: Effect) -> T {
    let Outcome::Done(ensured) = outcome else {
        panic!("done, {effect:?}: {outcome:?}");
    };
    assert_eq!(ensured.effect, effect);
    ensured.object
}

fn notes(text: &str) -> DraftRequest {
    DraftRequest {
        body: text.to_string(),
        ..DraftRequest::new("v1")
    }
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::parse(&sha(bytes)).expect("a digest")
}

fn sha(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
    )
}

fn assert_conflict(error: &Error, sub: &str) {
    assert_eq!(error.kind, Kind::Conflict, "{error:?}");
    assert_eq!(error.exit(), 9);
    assert_eq!(error.sub.slug().as_deref(), Some(sub), "{error:?}");
}

fn guard_what(error: &Error) -> String {
    let agh::Sub::GuardFailed { what } = &error.sub else {
        panic!("guard_failed: {error:?}");
    };
    what.clone()
}

#[test]
fn recovery_only_lookup_never_creates_an_absent_release_or_an_intent() {
    let world = world("recover-absent");
    let mark = world.mark();
    assert!(
        world
            .releases()
            .recover_created(&world.permit(), &notes("A"), &world.journal())
            .unwrap()
            .is_none()
    );
    assert!(world.writes_since(mark).is_empty());
    assert!(world.records().is_empty());
}

#[test]
fn recovery_only_lookup_returns_the_journals_existing_draft_without_posting() {
    let world = world("recover-owned");
    let request = notes("A");
    let original = done(world.create(&request).unwrap(), Effect::Created);
    let mark = world.mark();
    let recovered = world
        .releases()
        .recover_created(&world.permit(), &request, &world.journal())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.object.id, original.id);
    assert_eq!(recovered.effect, Effect::Converged);
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn recovery_only_lookup_refuses_a_foreign_draft_and_another_body_without_writes() {
    let world = world("recover-foreign");
    world
        .fake
        .add_release(APP, ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z"));
    let mark = world.mark();
    assert!(
        world
            .releases()
            .recover_created(&world.permit(), &notes("A"), &world.journal())
            .is_err()
    );
    assert!(world.writes_since(mark).is_empty());

    let owned = self::world("recover-other-body");
    done(owned.create(&notes("A")).unwrap(), Effect::Created);
    let mark = owned.mark();
    assert!(
        owned
            .releases()
            .recover_created(&owned.permit(), &notes("B"), &owned.journal())
            .is_err()
    );
    assert!(owned.writes_since(mark).is_empty());
}

#[test]
fn create_posts_one_draft_with_the_exact_body() {
    let world = world("rw-create");
    let made = done(
        world.create(&notes("notes")).expect("an outcome"),
        Effect::Created,
    );
    let bodies = world.fake.bodies();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert_eq!(
        bodies[0].json.as_deref(),
        Some(
            r#"{"body":"notes","draft":true,"make_latest":"false","name":"v1","prerelease":false,"tag_name":"v1"}"#
        )
    );
    let records = world.records();
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        matches!(&records[0], JournalRecord::Intent(_)),
        "{records:?}"
    );
    assert!(
        matches!(&records[1], JournalRecord::Settled { outcome: Settled::Created { id, .. }, .. } if *id == made.id),
        "{records:?}"
    );
    let held = world.fake.releases_of(APP);
    assert_eq!(held.len(), 1);
    assert!(held[0].draft);

    let targeted = DraftRequest {
        target_commitish: Some("main".to_string()),
        ..DraftRequest::new("v2")
    };
    done(
        world.create(&targeted).expect("an outcome"),
        Effect::Created,
    );
    let bodies = world.fake.bodies();
    assert_eq!(
        bodies[1].json.as_deref(),
        Some(
            r#"{"body":"","draft":true,"make_latest":"false","name":"v2","prerelease":false,"tag_name":"v2","target_commitish":"main"}"#
        )
    );
    let records = world.records();
    let JournalRecord::Intent(intent) = &records[2] else {
        panic!("the second Intent: {records:?}");
    };
    assert_eq!(intent.target_commitish.as_deref(), Some("main"));
}

#[test]
fn foreign_published_and_ambiguous_tags_are_refused() {
    let world = world("rw-foreign");
    let foreign = world
        .fake
        .add_release(APP, ReleaseSpec::draft("d1", "2026-10-01T00:00:00Z"));
    world
        .fake
        .add_release(APP, ReleaseSpec::published("p1", "2026-10-01T00:00:01Z"));
    let first = world
        .fake
        .add_release(APP, ReleaseSpec::draft("a1", "2026-10-01T00:00:02Z"));
    let second = world
        .fake
        .add_release(APP, ReleaseSpec::draft("a1", "2026-10-01T00:00:03Z"));
    let mark = world.mark();

    let error = world
        .create(&DraftRequest::new("d1"))
        .expect_err("a foreign draft");
    assert_conflict(&error, "foreign_draft");
    assert_eq!(error.sub, agh::Sub::ForeignDraft { id: foreign });
    assert!(error.next.contains(&foreign.to_string()), "{error:?}");
    let error = world
        .create(&DraftRequest::new("p1"))
        .expect_err("a published tag");
    assert_conflict(&error, "already_exists");
    let error = world
        .create(&DraftRequest::new("a1"))
        .expect_err("two drafts");
    assert_conflict(&error, "ambiguous_tag");
    let agh::Sub::AmbiguousTag { ids } = &error.sub else {
        panic!("{error:?}");
    };
    let mut ids = ids.clone();
    ids.sort_unstable();
    assert_eq!(ids, vec![first, second]);
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn a_draft_matches_on_its_target_and_body() {
    let world = world("rw-match");
    world
        .fake
        .fault(Match::api(RELEASES).method("POST"), Fault::Close);
    let req = DraftRequest {
        target_commitish: Some("main".to_string()),
        ..notes("notes")
    };
    assert!(matches!(
        world.create(&req).expect("an outcome"),
        Outcome::Unknown { .. }
    ));
    let mut other = ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z");
    other.body = Some("other".to_string());
    world.fake.add_release(APP, other);
    let mark = world.mark();
    let Outcome::Unknown { error, .. } = world.create(&req).expect("an outcome") else {
        panic!("a draft with other notes is not the intent's");
    };
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("intent_open"),
        "{error:?}"
    );
    assert!(world.writes_since(mark).is_empty());

    // An intent with no target matches a draft whose target is `rel`.
    world
        .fake
        .fault(Match::api(RELEASES).method("POST"), Fault::Close);
    let untargeted = DraftRequest {
        body: "notes".to_string(),
        ..DraftRequest::new("v2")
    };
    assert!(matches!(
        world.create(&untargeted).expect("an outcome"),
        Outcome::Unknown { .. }
    ));
    let mut rel = ReleaseSpec::draft("v2", "2026-10-01T00:00:01Z");
    rel.body = Some("notes".to_string());
    rel.target_commitish = "rel".to_string();
    let id = world.fake.add_release(APP, rel);
    let found = done(
        world.create(&untargeted).expect("an outcome"),
        Effect::Converged,
    );
    assert_eq!(found.id, id);
}

#[test]
fn a_rerun_with_other_notes_is_a_conflict() {
    let world = world("rw-other-notes");
    let made = done(
        world.create(&notes("A")).expect("an outcome"),
        Effect::Created,
    );
    let mark = world.mark();
    let error = world.create(&notes("B")).expect_err("other notes");
    assert_conflict(&error, "already_exists");
    let held = world.fake.releases_of(APP);
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].id, held[0].body.as_str()), (made.id, "A"));

    world
        .fake
        .fault(Match::api(RELEASES).method("POST"), Fault::Close);
    let v2 = |text: &str| DraftRequest {
        body: text.to_string(),
        ..DraftRequest::new("v2")
    };
    let Outcome::Unknown { intent, .. } = world.create(&v2("A")).expect("an outcome") else {
        panic!("a lost answer");
    };
    let after_lost = world.mark();
    let error = world
        .create(&v2("B"))
        .expect_err("the open intent asked for A");
    assert_conflict(&error, "guard_failed");
    assert!(guard_what(&error).contains(intent.as_str()), "{error:?}");
    let open = world.journal().open().expect("the journal reads");
    assert!(open.iter().any(|o| o.id == intent));
    let Outcome::Unknown { error, .. } = world.create(&v2("A")).expect("an outcome") else {
        panic!("the open intent is still open");
    };
    assert_eq!(error.sub.slug().as_deref(), Some("intent_open"));
    assert!(world.writes_since(after_lost).is_empty());

    let v3 = |target: &str| DraftRequest {
        target_commitish: Some(target.to_string()),
        ..DraftRequest::new("v3")
    };
    done(
        world.create(&v3("main")).expect("an outcome"),
        Effect::Created,
    );
    let before = world.mark();
    let error = world.create(&v3("rel")).expect_err("another target");
    assert_conflict(&error, "already_exists");
    assert!(world.writes_since(before).is_empty());
    let posts = world
        .writes_since(mark)
        .iter()
        .filter(|(m, _)| m == "POST")
        .count();
    assert_eq!(posts, 2, "v2's lost POST and v3's");
}

#[test]
fn a_by_id_read_of_another_tag_is_not_converged() {
    let world = world("rw-retagged");
    let made = world.draft();
    let mut raw = made.raw.clone();
    if let aterm_json::Value::Object(fields) = &mut raw {
        fields.insert("tag_name".into(), aterm_json::Value::from("v2"));
    }
    world.fake.fault(
        Match::api(&format!("{RELEASES}/{}", made.id)).method("GET"),
        Fault::Status {
            status: 200,
            body: aterm_json::to_string(&raw).expect("the release's JSON"),
            headers: Vec::new(),
        },
    );
    let mark = world.mark();
    let error = world
        .create(&DraftRequest::new("v1"))
        .expect_err("the by-id read names another tag");
    assert_conflict(&error, "foreign_draft");
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn a_tag_read_is_bounded_on_a_long_listing() {
    let world = world("rw-long");
    for i in 0..2100_u32 {
        let (day, rest) = (i / 1440, i % 1440);
        let at = format!(
            "2025-01-{:02}T{:02}:{:02}:00Z",
            day + 1,
            rest / 60,
            rest % 60
        );
        world
            .fake
            .add_release(APP, ReleaseSpec::published(&format!("r{i}"), &at));
    }
    let mark = world.mark();
    done(
        world.create(&DraftRequest::new("v1")).expect("an outcome"),
        Effect::Created,
    );
    let before_post: Vec<(String, String)> = world
        .since(mark)
        .into_iter()
        .take_while(|(method, _)| method != "POST")
        .collect();
    // The push check (design §6.4), whose answer also gives M1's walk the repository's id for
    // GitHub's numeric next links (`/repositories/{id}/releases`); one tag read; the listing's
    // pages 1 to 5, each twice (the pass and the revalidation).
    assert_eq!(before_post.len(), 12, "{before_post:?}");
    assert_eq!(before_post[0], ("GET".to_string(), REPO.to_string()));
    assert_eq!(
        before_post[1],
        ("GET".to_string(), format!("{RELEASES}/tags/v1"))
    );
    let repo_reads = before_post.iter().filter(|(_, path)| path == REPO).count();
    assert_eq!(repo_reads, 1, "{before_post:?}");
    let mut pages: BTreeMap<u32, usize> = BTreeMap::new();
    for (_, path) in &before_post[2..] {
        assert!(path.contains("/releases?per_page=100"), "{path}");
        let page = path
            .split(['?', '&'])
            .find_map(|pair| pair.strip_prefix("page="))
            .map_or(1, |n| n.parse().expect("a page number"));
        *pages.entry(page).or_default() += 1;
    }
    assert_eq!(pages, (1..=5).map(|p| (p, 2)).collect::<BTreeMap<_, _>>());

    world
        .fake
        .add_release(APP, ReleaseSpec::draft("v2", "2025-12-01T00:00:00Z"));
    let error = world
        .create(&DraftRequest::new("v2"))
        .expect_err("a newest draft");
    assert_conflict(&error, "foreign_draft");
    let error = world
        .create(&DraftRequest::new("r0"))
        .expect_err("the oldest release");
    assert_conflict(&error, "already_exists");
}

#[test]
fn a_body_over_125000_characters_is_invalid() {
    let world = world("rw-long-body");
    let mark = world.mark();
    let error = world
        .create(&notes(&"é".repeat(125_001)))
        .expect_err("too many characters");
    assert_eq!((error.kind, error.exit()), (Kind::Invalid, 10), "{error:?}");
    assert!(world.since(mark).is_empty());
}

#[test]
fn a_release_tier_create_names_its_grant() {
    let world = world("rw-grant");
    let slug = common::slug(ATERM);
    let route = RouteRequest::write(RouteTarget::Repo(slug.clone()))
        .with_named(common::granted(&world.gh, ATERM));
    let permit = common::permit(&world.gh, &route);
    let journal = world.gh.journal(&slug).expect("the journal");
    let mark = world.mark();
    done(
        world
            .gh
            .releases(&slug)
            .create(&permit, &DraftRequest::new("v1"), &journal)
            .expect("an outcome"),
        Effect::Created,
    );
    let (records, _) = journal.read().expect("the journal reads");
    let JournalRecord::Intent(intent) = &records[0].1 else {
        panic!("an Intent: {records:?}");
    };
    assert_eq!(intent.grant.as_deref(), Some("fixture-caller"));
    assert_eq!(intent.identity, "release");
    let post = world
        .fake
        .seen()
        .into_iter()
        .skip(mark)
        .find(|s| s.method == "POST")
        .expect("the POST");
    assert_eq!(post.as_login.as_deref(), Some("fixture-release-bot"));
}

#[test]
fn upload_never_creates_converges_and_conflicts() {
    let world = world("rw-upload");
    let release = world.draft();
    let file = world.file("pkg.tar", b"one");
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("the upload");
    assert_eq!(made.effect, Effect::Created);
    assert_eq!(made.object.digest.as_deref(), Some(sha(b"one").as_str()));
    let mark = world.mark();
    let again = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("the same file again");
    assert_eq!(again.effect, Effect::Converged);
    std::fs::remove_file(&file.path).expect("the old file");
    let other = world.file("pkg.tar", b"two");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &other, Replace::Never)
        .expect_err("other bytes under the name");
    assert_conflict(&error, "already_exists");
    assert!(world.writes_since(mark).is_empty());
}

/// Offline protocol accounting, with actual HTTP and no uplink or real credentials.
#[test]
fn upload_protocol_accounting_keeps_the_digest_guard() {
    let world = world("upload-accounting");
    let release = world.draft();
    let file = world.file("account.zip", &[7; 1 << 20]);
    let permit = world.permit();
    let mark = world.mark();
    let began = std::time::Instant::now();
    let made = world
        .releases()
        .upload(&permit, &release, &file, Replace::Never)
        .unwrap();
    let elapsed = began.elapsed();
    assert_eq!(made.effect, Effect::Created);
    let requests = world.since(mark);
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _)| method == "POST")
            .count(),
        1
    );
    println!("agh upload offline: 1048576 bytes in {elapsed:?}; requests {requests:?}");
    // Negative control: identical size and name do not authorize different bytes.
    std::fs::remove_file(&file.path).unwrap();
    let other = world.file("account.zip", &[8; 1 << 20]);
    let mark = world.mark();
    let error = world
        .releases()
        .upload(&permit, &release, &other, Replace::Never)
        .unwrap_err();
    assert_conflict(&error, "already_exists");
    assert!(world.writes_since(mark).is_empty());
    world.fake.assert_clean();
}

#[test]
fn a_lost_upload_converges_on_the_guard_read() {
    let world = world("rw-upload-lost");
    let release = world.draft();
    world.fake.fault(
        Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST"),
        Fault::LandThenStatus { status: 502 },
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let made = world
        .retrying()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("the upload");
    assert_eq!(made.effect, Effect::Converged);
    assert_eq!(world.writes_since(mark).len(), 1);
}

#[test]
fn a_starter_asset_is_deleted_and_the_upload_sent_once_more() {
    let world = world("rw-starter");
    let release = world.draft();
    world.fake.fault(
        Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST"),
        Fault::StarterThen { status: 502 },
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let made = world
        .retrying()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("the upload");
    assert_eq!(made.effect, Effect::Created);
    // The first guard read, then: the POST, the guard's read, its DELETE, the POST again.
    assert_eq!(
        world.checked_since(mark),
        ["GET", "POST", "GET", "DELETE", "POST"]
    );
}

#[test]
fn a_starter_asset_on_attempt_one_is_another_upload() {
    let world = world("rw-starter-first");
    let release = world.draft();
    world.fake.add_asset(
        release.id,
        AssetSpec {
            state: "starter".into(),
            ..AssetSpec::uploaded("pkg.tar", b"other")
        },
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect_err("another upload in progress");
    assert_conflict(&error, "guard_failed");
    let what = guard_what(&error);
    assert!(
        what.contains("pkg.tar")
            && what.contains(&release.id.to_string())
            && what.contains("starter"),
        "{what}"
    );
    assert_eq!(world.checked_since(mark), ["GET"]);
}

#[test]
fn a_starter_after_a_422_is_left_alone() {
    let world = world("rw-starter-422");
    let release = world.draft();
    let post = Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST");
    world.fake.fault(
        post.clone(),
        Fault::Status {
            status: 422,
            body: BODY_N.to_string(),
            headers: Vec::new(),
        },
    );
    world.fake.after(
        post,
        1,
        Change::AddAsset {
            release_id: release.id,
            spec: AssetSpec {
                state: "starter".into(),
                ..AssetSpec::uploaded("pkg.tar", b"other")
            },
        },
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect_err("another uploader's starter");
    assert_conflict(&error, "guard_failed");
    assert_eq!(world.checked_since(mark), ["GET", "POST", "GET"]);
    let assets = world.assets(release.id);
    assert!(
        assets
            .iter()
            .any(|(_, name, state, _)| name == "pkg.tar" && state == "starter"),
        "{assets:?}"
    );
}

#[test]
fn the_upload_digest_is_checked() {
    let world = world("rw-digest");
    let release = world.draft();
    let wrong = sha(b"not these bytes");
    let body = format!(
        r#"{{"id":9,"name":"pkg.tar","size":3,"digest":"{wrong}","state":"uploaded","content_type":"application/octet-stream"}}"#
    );
    world.fake.fault(
        Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST"),
        Fault::Status {
            status: 201,
            body,
            headers: Vec::new(),
        },
    );
    let file = world.file("pkg.tar", b"one");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect_err("another digest");
    assert_conflict(&error, "digest_mismatch");
    assert_eq!(
        error.sub,
        agh::Sub::DigestMismatch {
            expected: sha(b"one"),
            got: wrong,
        }
    );
    // GitHub answered 201: the asset is on the release, so the write took effect.
    assert_eq!(error.side_effect, agh::SideEffect::Applied, "{error:?}");
}

#[test]
fn a_name_with_a_plus_is_sent_as_2b() {
    let world = world("rw-plus");
    let release = world.draft();
    let mark = world.mark();
    let file = world.file("pkg+x.tar", b"one");
    world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("the upload");
    let writes = world.writes_since(mark);
    assert_eq!(
        writes,
        [(
            "POST".to_string(),
            format!("{RELEASES}/{}/assets?name=pkg%2Bx.tar", release.id)
        )]
    );
    let assets = world.assets(release.id);
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].1, "pkg+x.tar");
}

#[test]
fn clobber_replaces_in_three_steps() {
    let world = world("rw-clobber");
    let release = world.draft();
    let old = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"old"));
    let mark = world.mark();
    let file = world.file("pkg.tar", b"new");
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the clobber");
    assert_eq!(made.effect, Effect::Created);
    let hex = &sha(b"new")["sha256:".len()..];
    let writes = world.writes_since(mark);
    assert_eq!(writes.len(), 3, "{writes:?}");
    assert_eq!(
        writes[0],
        (
            "POST".to_string(),
            format!(
                "{RELEASES}/{}/assets?name=pkg.tar.agh-{}",
                release.id,
                &hex[..8]
            )
        )
    );
    assert_eq!(
        writes[1],
        ("DELETE".to_string(), format!("{RELEASES}/assets/{old}"))
    );
    assert_eq!(
        writes[2],
        (
            "PATCH".to_string(),
            format!("{RELEASES}/assets/{}", made.object.id)
        )
    );
    let bodies = world.fake.bodies();
    assert_eq!(
        bodies.last().and_then(|b| b.json.as_deref()),
        Some(r#"{"name":"pkg.tar"}"#)
    );
    let assets = world.assets(release.id);
    assert_eq!(assets.len(), 1, "{assets:?}");
    assert_eq!(
        (assets[0].1.as_str(), assets[0].3.as_str()),
        ("pkg.tar", sha(b"new").as_str())
    );
}

#[test]
fn clobber_of_identical_bytes_is_unchanged() {
    let world = world("rw-clobber-same");
    let release = world.draft();
    world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"same"));
    let mark = world.mark();
    let file = world.file("pkg.tar", b"same");
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the clobber");
    assert_eq!(made.effect, Effect::Unchanged);
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn a_clobber_cut_after_its_delete_finishes_with_the_rename() {
    let world = world("rw-clobber-cut");
    let release = world.draft();
    let hex = &sha(b"new")["sha256:".len()..];
    world.fake.add_asset(
        release.id,
        AssetSpec::uploaded(&format!("pkg.tar.agh-{}", &hex[..8]), b"new"),
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"new");
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the clobber");
    assert_eq!(made.effect, Effect::Created);
    let writes = world.writes_since(mark);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].0, "PATCH");
}

#[test]
fn a_clobber_whose_temporary_name_is_too_long_is_invalid() {
    let world = world("rw-clobber-long");
    let release = world.draft();
    let name = format!("{}.tar", "p".repeat(246));
    assert_eq!(name.len(), 250);
    let file = world.file(&name, b"new");
    let mark = world.mark();
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect_err("a temporary name past 255 bytes");
    assert_eq!((error.kind, error.exit()), (Kind::Invalid, 10), "{error:?}");
    let sent = world.since(mark);
    assert!(sent.iter().all(|(method, _)| method == "GET"), "{sent:?}");
    // Once the same bytes are there, the clobber is `unchanged`: it writes nothing, so its
    // temporary name is never judged.
    world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect("a 250-byte name is GitHub's to keep");
    let release = world.reread(release.id);
    let again = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the same bytes");
    assert_eq!(again.effect, Effect::Unchanged);
}

/// The DELETE is answered once without deleting (a one-shot fault, so the rerun's DELETE is
/// served).
#[test]
fn a_clobber_whose_old_asset_lingers_is_guard_failed() {
    let world = world("rw-clobber-linger");
    let release = world.draft();
    let old = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"old"));
    world.fake.fault(
        Match::api(&format!("{RELEASES}/assets/{old}")).method("DELETE"),
        Fault::Status {
            status: 204,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let mark = world.mark();
    let slept = world.gh.fx().slept().len();
    let file = world.file("pkg.tar", b"new");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect_err("the old asset lingers");
    assert_conflict(&error, "guard_failed");
    assert!(guard_what(&error).contains(&old.to_string()), "{error:?}");
    let requests = world.since(mark);
    let delete = requests
        .iter()
        .position(|(m, _)| m == "DELETE")
        .expect("the DELETE");
    let after: Vec<&String> = requests[delete + 1..].iter().map(|(m, _)| m).collect();
    assert_eq!(after, ["GET", "GET", "GET"]);
    // The pacing before each of the two writes (the temporary POST and the DELETE), then the
    // two 1 s sleeps between the three listings; no write follows the DELETE.
    let sleeps: Vec<std::time::Duration> = world.gh.fx().slept()[slept..].to_vec();
    assert_eq!(sleeps.len(), 4, "{sleeps:?}");
    assert!(
        sleeps.ends_with(&[std::time::Duration::from_secs(1); 2]),
        "{sleeps:?}"
    );
    assert!(!requests.iter().any(|(m, _)| m == "PATCH"));
    let hex = &sha(b"new")["sha256:".len()..];
    let temporary = format!("pkg.tar.agh-{}", &hex[..8]);
    assert!(world.assets(release.id).iter().any(|a| a.1 == temporary));

    let parked = world
        .assets(release.id)
        .into_iter()
        .find(|a| a.1 == temporary)
        .expect("the temporary asset")
        .0;
    let mark = world.mark();
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the rerun");
    assert_eq!(made.effect, Effect::Created);
    // Step 1 converges on the parked temporary asset (no POST); step 2 deletes the old asset
    // and step 3 renames the temporary one.
    assert_eq!(
        world.writes_since(mark),
        [
            ("DELETE".to_string(), format!("{RELEASES}/assets/{old}")),
            ("PATCH".to_string(), format!("{RELEASES}/assets/{parked}")),
        ]
    );
    let assets = world.assets(release.id);
    assert_eq!(assets.len(), 1, "{assets:?}");
    assert_eq!(assets[0].1, "pkg.tar");
}

/// A clobber cut in step 1 leaves its temporary asset `starter`: the rerun deletes it, then
/// takes the three steps.
#[test]
fn a_clobber_cut_in_its_upload_clears_its_temporary_asset() {
    let world = world("rw-clobber-cut-upload");
    let release = world.draft();
    let old = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"old"));
    let hex = &sha(b"new")["sha256:".len()..];
    let cut = world.fake.add_asset(
        release.id,
        AssetSpec {
            state: "starter".into(),
            ..AssetSpec::uploaded(&format!("pkg.tar.agh-{}", &hex[..8]), b"new")
        },
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"new");
    let made = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect("the clobber");
    assert_eq!(made.effect, Effect::Created);
    let writes = world.writes_since(mark);
    assert_eq!(
        writes,
        [
            ("DELETE".to_string(), format!("{RELEASES}/assets/{cut}")),
            (
                "POST".to_string(),
                format!(
                    "{RELEASES}/{}/assets?name=pkg.tar.agh-{}",
                    release.id,
                    &hex[..8]
                )
            ),
            ("DELETE".to_string(), format!("{RELEASES}/assets/{old}")),
            (
                "PATCH".to_string(),
                format!("{RELEASES}/assets/{}", made.object.id)
            ),
        ]
    );
    let assets = world.assets(release.id);
    assert_eq!(assets.len(), 1, "{assets:?}");
    assert_eq!(
        (assets[0].1.as_str(), assets[0].3.as_str()),
        ("pkg.tar", sha(b"new").as_str())
    );
}

/// A clobber's DELETE of its cut temporary asset whose answer was lost, retried into a 404,
/// may have removed that asset: a later 422 on the upload says the outcome is unknown, not
/// that nothing was written.
#[test]
fn a_lost_delete_of_a_cut_temporary_asset_leaves_a_later_error_unknown() {
    let world = world("rw-clobber-cut-lost");
    let release = world.draft();
    let hex = &sha(b"new")["sha256:".len()..];
    let cut = world.fake.add_asset(
        release.id,
        AssetSpec {
            state: "starter".into(),
            ..AssetSpec::uploaded(&format!("pkg.tar.agh-{}", &hex[..8]), b"new")
        },
    );
    world.fake.fault(
        Match::api(&format!("{RELEASES}/assets/{cut}")).method("DELETE"),
        Fault::LandThenClose,
    );
    world.fake.fault(
        Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST"),
        Fault::Status {
            status: 422,
            body: r#"{"message":"Validation Failed"}"#.to_string(),
            headers: Vec::new(),
        },
    );
    let (dirs, router) = common::file_router(world.root.path(), common::WRITERS);
    let gh = agh::GithubBuilder::new(router)
        .transport(std::sync::Arc::new(agh::wire::http::HttpTransport::direct(
            std::sync::Arc::new(common::env()),
        )))
        .endpoints(common::endpoints(&world.fake))
        .env(std::sync::Arc::new(common::env()))
        .fx(std::sync::Arc::new(agh::fx::Fx::simulated(
            common::SIM_START,
        )))
        .caller("test")
        .retry(RetryPolicy::STANDARD)
        .journal(dirs)
        .build();
    let mark = world.mark();
    let file = world.file("pkg.tar", b"new");
    let error = gh
        .releases(&common::slug(APP))
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect_err("the upload's 422");
    assert_eq!(error.side_effect, agh::SideEffect::Unknown, "{error:?}");
    let deletes = world
        .writes_since(mark)
        .into_iter()
        .filter(|(method, _)| method == "DELETE")
        .count();
    assert_eq!(deletes, 2, "the lost DELETE and its retry");
}

#[test]
fn immutable_releases_refuse_asset_changes_locally() {
    let world = world("rw-immutable");
    let draft = world.draft();
    world
        .fake
        .add_asset(draft.id, AssetSpec::uploaded("pkg.tar", b"old"));
    world.fake.set_immutable(draft.id, true);
    let release = world.reread(draft.id);
    assert!(release.immutable);
    let file = world.file("pkg.tar", b"new");
    let mark = world.mark();
    let refused = |error: Error| {
        assert_eq!((error.kind, error.exit()), (Kind::Refused, 1), "{error:?}");
        assert_eq!(error.sub.slug().as_deref(), Some("immutable"), "{error:?}");
    };
    for replace in [Replace::Never, Replace::Clobber] {
        refused(
            world
                .releases()
                .upload(&world.permit(), &release, &file, replace)
                .expect_err("immutable"),
        );
    }
    refused(
        world
            .releases()
            .delete_asset(&world.permit(), &release, "pkg.tar")
            .expect_err("immutable"),
    );
    assert!(world.since(mark).is_empty());
}

fn patch(prerelease: bool) -> PublishPatch {
    PublishPatch {
        prerelease,
        make_latest: MakeLatest::False,
    }
}

#[test]
fn publish_patches_once() {
    let world = world("rw-publish");
    let draft = world.draft();
    let mark = world.mark();
    let published = world
        .releases()
        .publish(
            &world.permit(),
            draft.id,
            &PublishGuard::default(),
            &patch(true),
        )
        .expect("the publish");
    assert_eq!(published.effect, Effect::Created);
    assert!(!published.object.draft);
    let writes = world.writes_since(mark);
    assert_eq!(
        writes,
        [("PATCH".to_string(), format!("{RELEASES}/{}", draft.id))]
    );
    assert_eq!(
        world
            .fake
            .bodies()
            .last()
            .and_then(|b| b.json.clone())
            .as_deref(),
        Some(r#"{"draft":false,"make_latest":"false","prerelease":true}"#)
    );
    let held = world.fake.releases_of(APP);
    assert!(!held[0].draft);
    assert_eq!(held[0].make_latest.as_deref(), Some("false"));
}

#[test]
fn publish_guards_the_asset_set_it_first_saw() {
    let world = world("rw-publish-set");
    let draft = world.draft();
    world.fake.fault(
        Match::api(&format!("{RELEASES}/{}", draft.id)).method("PATCH"),
        Fault::Status {
            status: 502,
            body: r#"{"message":"Server Error"}"#.to_string(),
            headers: Vec::new(),
        },
    );
    world.fake.after(
        Match::api(&format!("{RELEASES}/{}", draft.id)).method("GET"),
        1,
        Change::AddAsset {
            release_id: draft.id,
            spec: AssetSpec::uploaded("late.tar", b"late"),
        },
    );
    let mark = world.mark();
    let error = world
        .retrying()
        .publish(
            &world.permit(),
            draft.id,
            &PublishGuard::default(),
            &patch(false),
        )
        .expect_err("the asset set changed");
    assert_conflict(&error, "guard_failed");
    assert!(guard_what(&error).contains("late.tar"), "{error:?}");
    assert_eq!(world.writes_since(mark).len(), 1);
}

#[test]
fn publish_after_a_lost_answer_converges() {
    let world = world("rw-publish-lost");
    let draft = world.draft();
    world.fake.fault(
        Match::api(&format!("{RELEASES}/{}", draft.id)).method("PATCH"),
        Fault::LandThenClose,
    );
    let mark = world.mark();
    let published = world
        .releases()
        .publish(
            &world.permit(),
            draft.id,
            &PublishGuard::default(),
            &patch(false),
        )
        .expect("the publish");
    assert_eq!(published.effect, Effect::Converged);
    assert_eq!(world.writes_since(mark).len(), 1);
}

#[test]
fn publish_refuses_assets_not_uploaded_or_not_expected() {
    let world = world("rw-publish-refuse");
    let draft = world.draft();
    world.fake.add_asset(
        draft.id,
        AssetSpec {
            state: "starter".into(),
            ..AssetSpec::uploaded("half.tar", b"half")
        },
    );
    let mark = world.mark();
    let error = world
        .releases()
        .publish(
            &world.permit(),
            draft.id,
            &PublishGuard::default(),
            &patch(false),
        )
        .expect_err("a starter asset");
    assert_conflict(&error, "guard_failed");

    let other = world.draft_of("v2");
    world
        .fake
        .add_asset(other.id, AssetSpec::uploaded("pkg.tar", b"one"));
    let guard = PublishGuard::expecting(
        [("pkg.tar".to_string(), digest(b"two"))]
            .into_iter()
            .collect(),
    );
    let error = world
        .releases()
        .publish(&world.permit(), other.id, &guard, &patch(false))
        .expect_err("another digest");
    assert_conflict(&error, "guard_failed");
    assert!(
        !world.writes_since(mark).iter().any(|(m, _)| m == "PATCH"),
        "no PATCH"
    );
}

impl World {
    fn draft_of(&self, tag: &str) -> ReleaseObject {
        done(
            self.create(&DraftRequest::new(tag)).expect("an outcome"),
            Effect::Created,
        )
    }
}

/// An edit sends the fields it changes, and a draft's tag besides: GitHub drops a draft's tag
/// when an edit names none.
#[test]
fn edit_sends_only_what_differs() {
    let world = world("rw-edit");
    let draft = world.draft();
    let mut edit = ReleaseEdit::default();
    edit.name = Some("v1 final".to_string());
    let mark = world.mark();
    let edited = world
        .releases()
        .edit(&world.permit(), draft.id, &edit)
        .expect("the edit");
    assert_eq!(edited.effect, Effect::Created);
    assert_eq!(
        world
            .fake
            .bodies()
            .last()
            .and_then(|b| b.json.clone())
            .as_deref(),
        Some(r#"{"name":"v1 final","tag_name":"v1"}"#)
    );
    let again = world
        .releases()
        .edit(&world.permit(), draft.id, &edit)
        .expect("the same edit");
    assert_eq!(again.effect, Effect::Unchanged);
    assert_eq!(world.writes_since(mark).len(), 1);
    let mut long = ReleaseEdit::default();
    long.body = Some("x".repeat(125_001));
    let error = world
        .releases()
        .edit(&world.permit(), draft.id, &long)
        .expect_err("too many characters");
    assert_eq!(error.exit(), 10, "{error:?}");
}

#[test]
fn delete_reads_deletes_and_reads_again() {
    let world = world("rw-delete");
    let draft = world.draft();
    let mark = world.mark();
    let deleted = world
        .releases()
        .delete(&world.permit(), draft.id)
        .expect("the delete");
    assert_eq!(deleted, Deleted::Removed);
    assert_eq!(world.checked_since(mark), ["GET", "DELETE", "GET"]);
    let mark = world.mark();
    let gone = world
        .releases()
        .delete(&world.permit(), draft.id)
        .expect("the delete again");
    assert_eq!(gone, Deleted::Gone);
    assert_eq!(world.checked_since(mark), ["GET"]);
}

#[test]
fn delete_asset_lists_first() {
    let world = world("rw-delete-asset");
    let release = world.draft();
    world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"one"));
    let removed = world
        .releases()
        .delete_asset(&world.permit(), &release, "pkg.tar")
        .expect("the delete");
    assert_eq!(removed, Deleted::Removed);
    assert!(world.assets(release.id).is_empty());
    let mark = world.mark();
    let gone = world
        .releases()
        .delete_asset(&world.permit(), &release, "pkg.tar")
        .expect("the delete again");
    assert_eq!(gone, Deleted::Gone);
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn ensure_creates_uploads_and_publishes_once() {
    let world = world("rw-ensure");
    let files = vec![world.file("a.tar", b"a"), world.file("b.tar", b"b")];
    let spec = EnsureSpec::new(DraftRequest::new("v1"))
        .with_assets(files)
        .with_publish(patch(false));
    let report = world
        .releases()
        .ensure(&world.permit(), &spec, &world.journal())
        .expect("an outcome")
        .done()
        .expect("done");
    assert_eq!(report.release.effect, Effect::Created);
    assert_eq!(
        report.assets.iter().map(|a| a.effect).collect::<Vec<_>>(),
        [Effect::Created, Effect::Created]
    );
    assert_eq!(
        report.published.as_ref().map(|p| p.effect),
        Some(Effect::Created)
    );
    let mark = world.mark();
    let again = world
        .releases()
        .ensure(&world.permit(), &spec, &world.journal())
        .expect("an outcome")
        .done()
        .expect("done");
    assert_eq!(again.release.effect, Effect::Converged);
    assert_eq!(
        again.assets.iter().map(|a| a.effect).collect::<Vec<_>>(),
        [Effect::Converged, Effect::Converged]
    );
    assert_eq!(
        again.published.as_ref().map(|p| p.effect),
        Some(Effect::Converged)
    );
    assert!(world.writes_since(mark).is_empty());
}

#[test]
fn observe_open_closes_an_intent_a_writer_listing_shows() {
    let world = world("rw-observe");
    world.fake.set_listing_lag(APP, true);
    world
        .fake
        .fault(Match::api(RELEASES).method("POST"), Fault::LandThenClose);
    let journal = world.journal();
    let Outcome::Unknown { intent, .. } = world
        .releases()
        .create(&world.permit(), &DraftRequest::new("v1"), &journal)
        .expect("an outcome")
    else {
        panic!("a lost answer under the lag is unknown");
    };
    let id = world.fake.releases_of(APP)[0].id;
    world.fake.apply(Change::Reveal { release_id: id });
    let writer = world.releases().how(How::writer());
    let seen = writer.objects_for_tag("v1").expect("the writer's listing");
    let error = writer
        .observe_open("v1", &seen, Access::Read, &journal)
        .expect_err("a Read listing");
    assert_eq!(error.exit(), 2, "{error:?}");
    let observed = writer
        .observe_open("v1", &seen, Access::WriterRead, &journal)
        .expect("the observe");
    assert_eq!(observed, Some(intent));
    assert!(journal.open().expect("the journal reads").is_empty());
}

#[test]
fn a_permit_for_another_repository_is_usage() {
    let world = world("rw-other-repo");
    let slug = common::slug(ATERM);
    let id = world
        .fake
        .add_release(ATERM, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    let aterm = world.gh.releases(&slug);
    let release = aterm.get(id).expect("the release");
    let file = world.file("pkg.tar", b"one");
    let permit = world.permit();
    let journal = world.gh.journal(&slug).expect("the journal");
    let mark = world.mark();
    let usage = |error: Error| assert_eq!(error.exit(), 2, "{error:?}");
    usage(
        aterm
            .create(&permit, &DraftRequest::new("v2"), &journal)
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .upload(&permit, &release, &file, Replace::Never)
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .publish(&permit, id, &PublishGuard::default(), &patch(false))
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .edit(&permit, id, &ReleaseEdit::default())
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .delete(&permit, id)
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .delete_asset(&permit, &release, "pkg.tar")
            .expect_err("another repository's permit"),
    );
    usage(
        aterm
            .ensure(&permit, &EnsureSpec::new(DraftRequest::new("v2")), &journal)
            .expect_err("another repository's permit"),
    );
    assert!(world.since(mark).is_empty());
}

/// The world of `world`, with the dev writer's token a fine-grained one that no `drafts_fp`
/// vouches for.
fn fine_grained_world(label: &str) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    common::canonical_repos(&fake);
    common::canonical_tokens(root.path(), &fake, false, true);
    let token = agh_fake::fine_grained_token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    common::write_token(&root.path().join("tokens"), "dev.token", &token);
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router, &fake, &dirs);
    World {
        root,
        fake,
        _dirs: dirs,
        gh,
    }
}

/// Design §6.4's fail-closed gate holds for writes: an unvouched fine-grained writer's
/// listing may not show drafts, so `create` (which would post a duplicate) and `delete`
/// (which would read a hidden draft as gone) refuse before anything is sent.
#[test]
fn an_unvouched_fine_grained_writer_writes_nothing() {
    let world = fine_grained_world("rw-drafts-unproven");
    let id = world
        .fake
        .add_release(APP, ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z"));
    let unproven = |error: Error| {
        assert_eq!(error.kind, Kind::Forbidden, "{error:?}");
        assert_eq!(
            error.sub.slug().as_deref(),
            Some("forbid.drafts_unproven"),
            "{error:?}"
        );
    };
    let mark = world.mark();
    unproven(
        world
            .create(&DraftRequest::new("v2"))
            .expect_err("an unvouched fine-grained writer"),
    );
    unproven(
        world
            .releases()
            .delete(&world.permit(), id)
            .expect_err("an unvouched fine-grained writer"),
    );
    assert!(world.since(mark).is_empty(), "{:?}", world.since(mark));
    assert_eq!(world.fake.releases_of(APP).len(), 1);
}

/// A writer without push cannot see drafts, so its 404 on a draft's id is no proof the draft
/// is gone: the delete is Forbidden `no_push`, not `Gone`.
#[test]
fn a_no_push_writers_delete_is_no_push_not_gone() {
    let world = world("rw-no-push");
    let pulled = "fixture-dev/pulled";
    world.fake.add_repo(
        pulled,
        agh_fake::RepoSpec::private().pull("fixture-dev-bot"),
    );
    let id = world
        .fake
        .add_release(pulled, ReleaseSpec::draft("v1", "2026-10-01T00:00:00Z"));
    let slug = common::slug(pulled);
    let permit = common::permit(
        &world.gh,
        &RouteRequest::write(RouteTarget::Repo(slug.clone())),
    );
    let mark = world.mark();
    let error = world
        .gh
        .releases(&slug)
        .delete(&permit, id)
        .expect_err("no push");
    assert_eq!(error.kind, Kind::Forbidden, "{error:?}");
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("forbid.no_push"),
        "{error:?}"
    );
    assert!(world.writes_since(mark).is_empty());
    assert_eq!(world.fake.releases_of(pulled).len(), 1);
}

/// A repository the writer cannot see is not_visible (exit 7), never not_found or `Gone`, and
/// nothing is written to it.
#[test]
fn an_unseen_repository_is_not_visible_and_nothing_is_written() {
    let world = world("rw-unseen");
    let slug = common::slug("fixture-dev/hidden");
    let permit = common::permit(
        &world.gh,
        &RouteRequest::write(RouteTarget::Repo(slug.clone())),
    );
    let hidden = world.gh.releases(&slug);
    let journal = world.gh.journal(&slug).expect("the journal");
    let mark = world.mark();
    let not_visible = |error: Error| {
        assert_eq!(
            (error.kind, error.exit()),
            (Kind::NotVisible, 7),
            "{error:?}"
        );
    };
    not_visible(hidden.delete(&permit, 1).expect_err("an unseen repository"));
    not_visible(
        hidden
            .publish(&permit, 1, &PublishGuard::default(), &patch(false))
            .expect_err("an unseen repository"),
    );
    let mut edit = ReleaseEdit::default();
    edit.name = Some("v1 final".to_string());
    not_visible(
        hidden
            .edit(&permit, 1, &edit)
            .expect_err("an unseen repository"),
    );
    not_visible(
        hidden
            .create(&permit, &DraftRequest::new("v1"), &journal)
            .expect_err("an unseen repository"),
    );
    let requests = world.since(mark);
    assert!(
        requests
            .iter()
            .all(|(method, path)| method == "GET" && path == "/repos/fixture-dev/hidden"),
        "{requests:?}"
    );
    assert!(journal.open().expect("the journal reads").is_empty());
}

/// A publish GitHub answered 200 whose body does not parse took effect: its error says so.
#[test]
fn a_publish_answer_that_does_not_parse_is_applied() {
    let world = world("rw-publish-garbled");
    let draft = world.draft();
    world.fake.fault(
        Match::api(&format!("{RELEASES}/{}", draft.id)).method("PATCH"),
        Fault::Status {
            status: 200,
            body: "not json".to_string(),
            headers: Vec::new(),
        },
    );
    let error = world
        .releases()
        .publish(
            &world.permit(),
            draft.id,
            &PublishGuard::default(),
            &patch(false),
        )
        .expect_err("an answer that does not parse");
    assert_eq!(error.side_effect, agh::SideEffect::Applied, "{error:?}");
}

/// `Fault::Status` 200 with `body`, a canned answer for one read.
fn ok_json(body: String) -> Fault {
    Fault::Status {
        status: 200,
        body,
        headers: Vec::new(),
    }
}

/// An outcome-unknown error (§4.6.7): exit 13, `side_effect` unknown.
fn assert_outcome_unknown(error: &Error) {
    assert_eq!(
        (error.kind, error.exit()),
        (Kind::OutcomeUnknown, 13),
        "{error:?}"
    );
    assert_eq!(error.side_effect, agh::SideEffect::Unknown, "{error:?}");
}

/// An upload POST that lost its answer, then a guard read that cannot connect, may have
/// landed: OutcomeUnknown, never the read's own not-sent error. The first guard read is
/// answered once, so only the read after the lost answer fails.
#[test]
fn a_lost_upload_whose_guard_read_fails_is_outcome_unknown() {
    let world = world("rw-upload-unread");
    let release = world.draft();
    let assets = Match::api(&format!("{RELEASES}/{}/assets", release.id)).method("GET");
    world.fake.fault(assets.clone(), ok_json("[]".to_string()));
    world.fake.fault_always(assets, Fault::Close);
    world.fake.fault(
        Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST"),
        Fault::Close,
    );
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let error = world
        .retrying()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect_err("a lost answer the guard cannot settle");
    assert_outcome_unknown(&error);
    assert_eq!(world.writes_since(mark).len(), 1);
}

/// An asset DELETE that lost its answer, then a last read that cannot connect:
/// OutcomeUnknown (the loop ends on the lost answer, and its last look fails).
#[test]
fn a_lost_asset_delete_whose_last_read_fails_is_outcome_unknown() {
    let world = world("rw-asset-delete-unread");
    let release = world.draft();
    let asset = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"one"));
    let listed = format!(
        r#"[{{"id":{asset},"name":"pkg.tar","size":3,"digest":"{}","state":"uploaded","content_type":"application/octet-stream"}}]"#,
        sha(b"one")
    );
    let assets = Match::api(&format!("{RELEASES}/{}/assets", release.id)).method("GET");
    world.fake.fault(assets.clone(), ok_json(listed));
    world.fake.fault_always(assets, Fault::Close);
    world.fake.fault(
        Match::api(&format!("{RELEASES}/assets/{asset}")).method("DELETE"),
        Fault::Close,
    );
    let error = world
        .releases()
        .delete_asset(&world.permit(), &release, "pkg.tar")
        .expect_err("a lost answer the last read cannot settle");
    assert_outcome_unknown(&error);
}

/// GitHub answered 204 to a release DELETE that did not delete it: the error after that
/// answer says a write took effect.
#[test]
fn a_delete_answered_but_still_there_is_applied() {
    let world = world("rw-delete-still");
    let draft = world.draft();
    world.fake.fault(
        Match::api(&format!("{RELEASES}/{}", draft.id)).method("DELETE"),
        Fault::Status {
            status: 204,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    let error = world
        .releases()
        .delete(&world.permit(), draft.id)
        .expect_err("still there after its DELETE");
    assert_conflict(&error, "guard_failed");
    assert_eq!(error.side_effect, agh::SideEffect::Applied, "{error:?}");
}

/// A release deleted after the caller read it is not_found (exit 6), or `Gone` for an asset
/// delete, never not_visible: the push check read the repository in the same call.
#[test]
fn a_release_gone_since_it_was_read_is_not_found() {
    let world = world("rw-stale-release");
    let release = world.draft();
    world.fake.apply(Change::DeleteRelease {
        release_id: release.id,
    });
    let file = world.file("pkg.tar", b"one");
    let not_found = |error: Error| {
        assert_eq!((error.kind, error.exit()), (Kind::NotFound, 6), "{error:?}");
    };
    let mark = world.mark();
    not_found(
        world
            .releases()
            .upload(&world.permit(), &release, &file, Replace::Never)
            .expect_err("a release that is gone"),
    );
    not_found(
        world
            .releases()
            .upload(&world.permit(), &release, &file, Replace::Clobber)
            .expect_err("a release that is gone"),
    );
    assert_eq!(
        world
            .releases()
            .delete_asset(&world.permit(), &release, "pkg.tar")
            .expect("a release that is gone"),
        Deleted::Gone
    );
    assert!(world.writes_since(mark).is_empty());
}

/// GitHub renames a name ending in `.`, so agh refuses it before anything is read or sent.
#[test]
fn an_asset_name_ending_in_a_period_is_invalid() {
    let world = world("rw-trailing-period");
    let path = common::file(world.root.path(), "pkg", b"one");
    let error = UploadFile::open(&path, Some("pkg.")).expect_err("a trailing period");
    assert_eq!((error.kind, error.exit()), (Kind::Invalid, 10), "{error:?}");
}

/// `Fault::Status` 404 with GitHub's not-found body.
fn not_found_answer() -> Fault {
    Fault::Status {
        status: 404,
        body: r#"{"message":"Not Found"}"#.to_string(),
        headers: Vec::new(),
    }
}

/// A clobber that DELETEd its own cut temporary asset (GitHub answered 204) and then fails
/// on the next listing took effect: its error says applied, never not-sent.
#[test]
fn a_clobber_that_cleared_its_temporary_asset_then_fails_is_applied() {
    let world = world("rw-clobber-cleared-unread");
    let release = world.draft();
    let hex = &sha(b"new")["sha256:".len()..];
    let temporary = format!("pkg.tar.agh-{}", &hex[..8]);
    let cut = world.fake.add_asset(
        release.id,
        AssetSpec {
            state: "starter".into(),
            ..AssetSpec::uploaded(&temporary, b"new")
        },
    );
    let listed = format!(
        r#"[{{"id":{cut},"name":"{temporary}","size":3,"digest":"{}","state":"starter","content_type":"application/octet-stream"}}]"#,
        sha(b"new")
    );
    let assets = Match::api(&format!("{RELEASES}/{}/assets", release.id)).method("GET");
    // The first listing holds the cut temporary asset, the listing after its DELETE does
    // not, and the upload's guard read cannot connect.
    world.fake.fault(assets.clone(), ok_json(listed));
    world.fake.fault(assets.clone(), ok_json("[]".to_string()));
    world.fake.fault_always(assets, Fault::Close);
    let mark = world.mark();
    let file = world.file("pkg.tar", b"new");
    let error = world
        .releases()
        .upload(&world.permit(), &release, &file, Replace::Clobber)
        .expect_err("the guard read after the DELETE fails");
    assert_eq!(error.side_effect, agh::SideEffect::Applied, "{error:?}");
    assert_eq!(
        world.writes_since(mark),
        [("DELETE".to_string(), format!("{RELEASES}/assets/{cut}"))]
    );
}

/// An upload whose first POST lost its answer, whose second was answered (a secondary rate
/// limit, retried), and whose next guard read fails: the first POST may have landed, so the
/// call is OutcomeUnknown, never the read's own not-sent error.
#[test]
fn a_lost_answer_is_kept_past_an_answered_attempt() {
    let world = world("rw-lost-kept");
    let release = world.draft();
    let post = Match::uploads(&format!("{RELEASES}/{}/assets", release.id)).method("POST");
    world.fake.fault(post.clone(), Fault::Close);
    world
        .fake
        .fault(post, Fault::SecondaryLimit { retry_after: 1 });
    let assets = Match::api(&format!("{RELEASES}/{}/assets", release.id)).method("GET");
    world.fake.fault(assets.clone(), ok_json("[]".to_string()));
    world.fake.fault(assets.clone(), ok_json("[]".to_string()));
    world.fake.fault_always(assets, Fault::Close);
    let mark = world.mark();
    let file = world.file("pkg.tar", b"one");
    let error = world
        .retrying()
        .upload(&world.permit(), &release, &file, Replace::Never)
        .expect_err("a lost answer the guard cannot settle");
    assert_outcome_unknown(&error);
    assert_eq!(world.writes_since(mark).len(), 2);
}

/// A release DELETE answered 404 (another DELETE won after the guard read) is `Gone`, and
/// one whose first answer was lost and whose second is 404 is `Removed` by the last look.
#[test]
fn a_delete_answered_404_is_gone_or_removed() {
    let world = world("rw-delete-404");
    let draft = world.draft();
    let delete = Match::api(&format!("{RELEASES}/{}", draft.id)).method("DELETE");
    world.fake.fault(delete.clone(), not_found_answer());
    let mark = world.mark();
    let gone = world
        .releases()
        .delete(&world.permit(), draft.id)
        .expect("a 404 on the DELETE");
    assert_eq!(gone, Deleted::Gone);
    assert_eq!(world.checked_since(mark), ["GET", "DELETE"]);

    world.fake.fault(delete.clone(), Fault::Close);
    world.fake.fault(delete.clone(), not_found_answer());
    world.fake.after(
        delete,
        1,
        Change::DeleteRelease {
            release_id: draft.id,
        },
    );
    let mark = world.mark();
    let removed = world
        .retrying()
        .delete(&world.permit(), draft.id)
        .expect("a lost answer, then a 404");
    assert_eq!(removed, Deleted::Removed);
    assert_eq!(
        world.checked_since(mark),
        ["GET", "DELETE", "GET", "DELETE", "GET"]
    );
}

/// An asset DELETE answered 404 is `Gone`.
#[test]
fn an_asset_delete_answered_404_is_gone() {
    let world = world("rw-asset-delete-404");
    let release = world.draft();
    let asset = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"one"));
    world.fake.fault(
        Match::api(&format!("{RELEASES}/assets/{asset}")).method("DELETE"),
        not_found_answer(),
    );
    let gone = world
        .releases()
        .delete_asset(&world.permit(), &release, "pkg.tar")
        .expect("a 404 on the DELETE");
    assert_eq!(gone, Deleted::Gone);
}

/// An asset of the same name that appears after the DELETE is another asset: the DELETE
/// removed the id it targeted, so the call is `Removed`.
#[test]
fn an_asset_delete_judges_the_id_it_deleted() {
    let world = world("rw-asset-delete-id");
    let release = world.draft();
    let asset = world
        .fake
        .add_asset(release.id, AssetSpec::uploaded("pkg.tar", b"one"));
    world.fake.after(
        Match::api(&format!("{RELEASES}/assets/{asset}")).method("DELETE"),
        1,
        Change::AddAsset {
            release_id: release.id,
            spec: AssetSpec::uploaded("pkg.tar", b"two"),
        },
    );
    let removed = world
        .releases()
        .delete_asset(&world.permit(), &release, "pkg.tar")
        .expect("the delete");
    assert_eq!(removed, Deleted::Removed);
    let assets = world.assets(release.id);
    assert_eq!(assets.len(), 1, "{assets:?}");
    assert!(
        assets[0].0 != asset && assets[0].1 == "pkg.tar",
        "{assets:?}"
    );
}

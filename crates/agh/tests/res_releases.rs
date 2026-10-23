// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `releases()` over agh-fake: list, view, tags, assets and their objects.

#![cfg(unix)]

use agh::download::{Expect, Sink};
use agh::error::{Forbid, IdentityRef, Tried};
use agh::github::TagObjects;
use agh::page::{End, ListOpts};
use agh::{Github, How, Kind, Sub};
use agh_fake::{AssetSpec, Change, FakeGithub, ReleaseSpec, RepoSpec};

use crate::common;

/// The repository most tests read: public, the dev bot holding push.
const APP: &str = "fixture-dev/app";
/// A public repository on which the dev bot holds pull only.
const PULLED: &str = "fixture-dev/pulled";

/// A fake, its scratch root and a client over the canonical config with both token files.
struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
}

impl World {
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, true);
        fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
        fake.add_repo(PULLED, RepoSpec::public().pull("fixture-dev-bot"));
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        World {
            _root: root,
            fake,
            gh,
        }
    }

    fn release(&self, repo: &str, spec: ReleaseSpec) -> u64 {
        self.fake.add_release(repo, spec)
    }

    /// `GET /repos/{repo}` requests seen so far.
    fn repo_reads(&self, repo: &str) -> usize {
        let path = format!("/repos/{repo}");
        self.fake
            .seen()
            .iter()
            .filter(|seen| seen.method == "GET" && seen.path_and_query == path)
            .count()
    }
}

fn tags(items: &[agh::model::ReleaseObject]) -> Vec<&str> {
    items.iter().map(|r| r.tag_name.as_str()).collect()
}

#[test]
fn list_drops_drafts_under_read_and_orders_by_created_then_id() {
    let w = World::new("rel-list");
    w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.release(APP, ReleaseSpec::published("v2", "2026-02-01T00:00:00Z"));
    w.release(APP, ReleaseSpec::published("v3", "2026-02-01T00:00:00Z"));
    w.release(APP, ReleaseSpec::draft("v4", "2026-03-01T00:00:00Z"));
    let listing =
        w.gh.releases(&common::slug(APP))
            .list(&ListOpts::STANDARD)
            .expect("the list");
    assert_eq!(tags(&listing.items), ["v3", "v2", "v1"]);
    assert_eq!(listing.end, End::Complete);
    assert_eq!(listing.as_.label(), "dev");
    w.fake.assert_clean();
}

#[test]
fn a_limit_that_cuts_is_limited_and_one_that_does_not_is_complete() {
    let w = World::new("rel-limit");
    for (i, day) in ["01", "02", "03"].iter().enumerate() {
        w.release(
            APP,
            ReleaseSpec::published(&format!("v{i}"), &format!("2026-01-{day}T00:00:00Z")),
        );
    }
    let releases = w.gh.releases(&common::slug(APP));
    let cut = releases
        .list(&ListOpts::STANDARD.with_limit(Some(2)))
        .expect("the list");
    assert_eq!(tags(&cut.items), ["v2", "v1"]);
    assert_eq!(cut.end, End::Limited { at: 2 });
    let whole = releases
        .list(&ListOpts::STANDARD.with_limit(Some(3)))
        .expect("the list");
    assert_eq!(whole.items.len(), 3);
    assert_eq!(whole.end, End::Complete);
    w.fake.assert_clean();
}

#[test]
fn githubs_result_cap_is_incomplete_or_capped_under_partial_never_invalid() {
    let w = World::new("rel-result-cap");
    for i in 0..1001 {
        w.release(
            APP,
            ReleaseSpec::published(&format!("v{i}"), "2026-01-01T00:00:00Z"),
        );
    }
    let releases = w.gh.releases(&common::slug(APP));
    let refused = releases
        .list(&ListOpts::STANDARD)
        .expect_err("past the result cap");
    assert_eq!(refused.kind, Kind::Incomplete, "{refused:?}");
    assert_eq!(refused.sub, Sub::Capped { pages: 10 });
    assert_eq!(refused.next, "--partial");
    assert_eq!(refused.http.as_ref().map(|h| h.status), Some(422));
    let capped = releases
        .list(&ListOpts::STANDARD.with_partial(true).with_limit(Some(3)))
        .expect("the capped list");
    assert_eq!(capped.end, End::Capped { pages: 10 });
    assert_eq!(capped.items.len(), 3);
    let first = releases
        .list(&ListOpts::STANDARD.with_max_pages(1).with_partial(true))
        .expect("one page");
    assert_eq!(first.end, End::Capped { pages: 1 });
    w.fake.assert_clean();
}

#[test]
fn the_writer_sees_drafts_and_a_pull_only_writer_is_no_push() {
    let w = World::new("rel-writer");
    w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.release(APP, ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"));
    w.release(PULLED, ReleaseSpec::draft("v1", "2026-02-01T00:00:00Z"));
    let listing =
        w.gh.releases(&common::slug(APP))
            .how(How::writer())
            .list(&ListOpts::STANDARD)
            .expect("the writer's list");
    assert_eq!(tags(&listing.items), ["v2", "v1"]);
    assert!(listing.items[0].draft);

    let refused =
        w.gh.releases(&common::slug(PULLED))
            .how(How::writer())
            .list(&ListOpts::STANDARD)
            .expect_err("pull only");
    assert_eq!(refused.kind, Kind::Forbidden);
    assert_eq!(refused.sub, Sub::Forbid(Forbid::NoPush));
    assert_eq!(refused.repo.as_deref(), Some(PULLED));
    w.fake.assert_clean();
}

#[test]
fn a_renamed_repository_under_writer_read_is_moved() {
    let w = World::new("rel-moved");
    w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake.rename_repo(APP, "fixture-dev/app2");
    let moved =
        w.gh.releases(&common::slug(APP))
            .how(How::writer())
            .list(&ListOpts::STANDARD)
            .expect_err("moved");
    assert_eq!(moved.kind, Kind::Conflict);
    assert!(matches!(moved.sub, Sub::Moved { .. }), "{:?}", moved.sub);
    w.fake.assert_clean();
}

#[test]
fn get_writer_pull_only_is_no_push() {
    let w = World::new("rel-get-nopush");
    let draft = w.release(PULLED, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let error =
        w.gh.releases(&common::slug(PULLED))
            .how(How::writer())
            .get(draft)
            .expect_err("no push");
    assert_eq!(error.kind, Kind::Forbidden);
    assert_eq!(error.sub.slug().as_deref(), Some("forbid.no_push"));
    w.fake.assert_clean();
}

#[test]
fn asset_writer_pull_only_is_no_push() {
    let w = World::new("rel-asset-nopush");
    let draft = w.release(PULLED, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let asset = w
        .fake
        .add_asset(draft, AssetSpec::uploaded("a.bin", b"bytes"));
    let error =
        w.gh.releases(&common::slug(PULLED))
            .how(How::writer())
            .asset(asset)
            .expect_err("no push");
    assert_eq!(error.kind, Kind::Forbidden);
    assert_eq!(error.sub.slug().as_deref(), Some("forbid.no_push"));
    w.fake.assert_clean();
}

#[test]
fn the_push_check_is_sent_on_every_writer_call_and_once_per_method() {
    let w = World::new("rel-check");
    let draft = w.release(APP, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP)).how(How::writer());
    releases.get(draft).expect("the draft");
    releases.get(draft).expect("the draft again");
    assert_eq!(w.repo_reads(APP), 2);
    w.fake.clear_seen();
    let found = releases.objects_for_tag("v1").expect("the tag's objects");
    assert!(matches!(found, TagObjects::One(_)));
    assert_eq!(w.repo_reads(APP), 1);
    w.fake.assert_clean();
}

#[test]
fn a_missing_id_after_the_check_is_not_found() {
    let w = World::new("rel-missing");
    let releases = w.gh.releases(&common::slug(APP)).how(How::writer());
    let missing = releases.get(4_999).expect_err("no such release");
    assert_eq!(missing.kind, Kind::NotFound);
    let unprobed =
        w.gh.releases(&common::slug(APP))
            .how(How::writer().no_probe(true))
            .get(4_999)
            .expect_err("no such release");
    assert_eq!(unprobed.kind, Kind::NotFound);
    assert_eq!(unprobed.http.as_ref().map(|http| http.status), Some(404));
    assert_eq!(unprobed.repo.as_deref(), Some(APP));
    w.fake.assert_clean();
}

#[test]
fn newest_under_a_writer_named_with_as_reads_through_the_read_chain() {
    let w = World::new("rel-newest-as");
    w.fake.add_repo(
        "alabsystems/aterm",
        RepoSpec::public().push("fixture-release-bot"),
    );
    w.release(
        "alabsystems/aterm",
        ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"),
    );
    let newest =
        w.gh.releases(&common::slug("alabsystems/aterm"))
            .how(How::writer().as_profile(Some("release".to_string())))
            .newest(None, None)
            .expect("the newest release")
            .expect("one release");
    assert_eq!(newest.tag_name, "v1");
    w.fake.assert_clean();
}

#[test]
fn a_download_of_an_asset_gone_after_the_check_is_not_found() {
    let w = World::new("rel-download-gone");
    let release = w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    let id = w
        .fake
        .add_asset(release, AssetSpec::uploaded("a.bin", b"twelve bytes"));
    let asset =
        w.gh.releases(&common::slug(APP))
            .asset(id)
            .expect("the asset");
    w.fake.apply(Change::DeleteRelease {
        release_id: release,
    });
    let expect = Expect::from_asset(&asset).expect("an expectation");
    let gone =
        w.gh.releases(&common::slug(APP))
            .how(How::writer().no_probe(true))
            .download(&asset, Sink::Bounded(1024), expect)
            .expect_err("the asset is gone");
    assert_eq!(gone.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn get_hides_a_draft_under_read_and_shows_it_to_the_writer() {
    let w = World::new("rel-get");
    let draft = w.release(APP, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let hidden =
        w.gh.releases(&common::slug(APP))
            .get(draft)
            .expect_err("a draft");
    assert_eq!(hidden.kind, Kind::NotFound);
    assert!(hidden.why.contains(&draft.to_string()), "{}", hidden.why);
    let seen =
        w.gh.releases(&common::slug(APP))
            .how(How::writer())
            .get(draft)
            .expect("the writer sees it");
    assert_eq!(seen.id, draft);
    assert!(seen.draft);
    w.fake.assert_clean();
}

#[test]
fn published_by_tag_finds_published_and_judges_the_rest() {
    let w = World::new("rel-tag");
    let v1 = w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.release(APP, ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP));
    assert_eq!(releases.published_by_tag("v1").expect("v1").id, v1);

    let draft_only = releases.published_by_tag("v2").expect_err("a draft only");
    assert_eq!(draft_only.kind, Kind::NotFound);
    assert!(draft_only.why.contains("v2"), "{}", draft_only.why);
    assert_eq!(
        draft_only.tried,
        vec![
            Tried::Sent {
                as_: "dev".into(),
                status: 404
            },
            Tried::Probe {
                path: "repos/fixture-dev/app".into(),
                status: 200
            },
        ]
    );

    w.fake.add_repo(
        "fixture-dev/secret",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    w.release(
        "fixture-dev/secret",
        ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"),
    );
    let hidden =
        w.gh.releases(&common::slug("fixture-dev/secret"))
            .how(How::read().anonymous(true))
            .published_by_tag("v1")
            .expect_err("hidden from anonymous");
    assert_eq!(hidden.kind, Kind::NotVisible);
    assert_eq!(hidden.identity, Some(IdentityRef::Anonymous));
    w.fake.assert_clean();
}

#[test]
fn published_by_tag_refuses_a_tag_no_release_can_have_before_any_request() {
    let w = World::new("rel-bad-tag");
    w.release(APP, ReleaseSpec::published("a/b", "2026-01-01T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP));
    for tag in ["", "a//b", "v1/", "/v1", "a/../b", "."] {
        let error = releases.published_by_tag(tag).expect_err("a bad tag");
        assert_eq!(error.kind, Kind::Usage, "{tag:?}");
        assert_eq!(error.repo.as_deref(), Some(APP), "{tag:?}");
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn objects_for_tag_answers_none_one_and_many() {
    let w = World::new("rel-objects");
    let one = w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    let a = w.release(APP, ReleaseSpec::draft("v2", "2026-02-01T00:00:00Z"));
    let b = w.release(APP, ReleaseSpec::draft("v2", "2026-02-02T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP));
    assert_eq!(
        releases.objects_for_tag("v9").expect("none"),
        TagObjects::None
    );
    match releases.objects_for_tag("v1").expect("one") {
        TagObjects::One(release) => assert_eq!(release.id, one),
        other => panic!("not one: {other:?}"),
    }
    match releases.objects_for_tag("v2").expect("many") {
        TagObjects::Many(found) => {
            let mut ids: Vec<u64> = found.iter().map(|r| r.id).collect();
            ids.sort_unstable();
            assert_eq!(ids, [a, b]);
        }
        other => panic!("not many: {other:?}"),
    }
    w.fake.assert_clean();
}

#[test]
fn latest_follows_the_pointer_and_latest_id_reads_none_as_absent() {
    let w = World::new("rel-latest");
    let older = w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    let newer = w.release(APP, ReleaseSpec::published("v2", "2026-02-01T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP));
    assert_eq!(releases.latest().expect("latest").id, newer);
    w.fake.set_latest(APP, Some(older));
    assert_eq!(releases.latest().expect("latest").id, older);
    assert_eq!(releases.latest_id().expect("an id"), Some(older));

    w.fake.add_repo(
        "fixture-dev/drafts",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    w.release(
        "fixture-dev/drafts",
        ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"),
    );
    let none = w.gh.releases(&common::slug("fixture-dev/drafts"));
    let absent = none.latest().expect_err("no latest");
    assert_eq!(absent.kind, Kind::NotFound);
    assert_eq!(none.latest_id().expect("absent is none"), None);
    w.fake.assert_clean();
}

#[test]
fn newest_filters_on_prerelease_and_the_tag_regex() {
    let w = World::new("rel-newest");
    let stable = w.release(
        APP,
        ReleaseSpec::published("v1.0.0", "2026-01-01T00:00:00Z"),
    );
    let pre = w.release(
        APP,
        ReleaseSpec::published("v1.1.0-rc1", "2026-02-01T00:00:00Z").prerelease(true),
    );
    let other = w.release(
        APP,
        ReleaseSpec::published("tools-1", "2026-03-01T00:00:00Z"),
    );
    w.release(APP, ReleaseSpec::draft("v2.0.0", "2026-04-01T00:00:00Z"));
    let releases = w.gh.releases(&common::slug(APP));
    let id = |found: Option<agh::model::ReleaseObject>| found.map(|r| r.id);
    assert_eq!(
        id(releases.newest(None, None).expect("newest")),
        Some(other)
    );
    let regex = aterm_regex::Regex::new("^v").expect("a regex");
    assert_eq!(
        id(releases.newest(None, Some(&regex)).expect("newest v")),
        Some(pre)
    );
    assert_eq!(
        id(releases
            .newest(Some(false), Some(&regex))
            .expect("newest stable")),
        Some(stable)
    );
    let nothing = aterm_regex::Regex::new("^nothing").expect("a regex");
    assert_eq!(
        id(releases.newest(None, Some(&nothing)).expect("none")),
        None
    );
    w.fake.assert_clean();
}

#[test]
fn asset_reads_one_asset_by_id() {
    let w = World::new("rel-asset");
    let release = w.release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    let id = w
        .fake
        .add_asset(release, AssetSpec::uploaded("a.bin", b"twelve bytes"));
    let asset =
        w.gh.releases(&common::slug(APP))
            .asset(id)
            .expect("the asset");
    assert_eq!(asset.id, id);
    assert_eq!(asset.name, "a.bin");
    assert_eq!(asset.size, 12);
    assert_eq!(asset.state, "uploaded");
    assert!(asset.sha256().expect("a digest").is_some());
    let missing =
        w.gh.releases(&common::slug(APP))
            .asset(id + 100)
            .expect_err("absent");
    assert_eq!(missing.kind, Kind::NotFound);
    w.fake.assert_clean();
}

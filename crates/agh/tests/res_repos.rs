// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `repos()` over agh-fake: a repository, its permissions and an owner's listing.

#![cfg(unix)]

use agh::github::RepoListKind;
use agh::page::{End, ListOpts};
use agh::policy::Access;
use agh::{Github, How, Kind};
use agh_fake::{FakeGithub, FakeOrigin, RepoSpec};

use crate::common;

/// Public, the dev bot holding push.
const APP: &str = "fixture-dev/app";
/// Public, the dev bot holding pull only.
const PULLED: &str = "fixture-dev/pulled";

struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
}

impl World {
    /// The canonical config with classic tokens.
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        let text = common::canonical_config(root.path());
        World::over(root, fake, &text)
    }

    /// The dev profile's token file holds a fine-grained token; its row carries
    /// `drafts_fp = "{fp}"` when `fp` is given.
    fn fine_grained(label: &str, vouch: bool) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        let token = agh_fake::fine_grained_token("dev");
        fake.add_token(&token, "fixture-dev-bot");
        common::write_token(&root.path().join("tokens"), "dev.token", &token);
        let mut text = common::canonical_config(root.path());
        if vouch {
            let fp = agh::identity::fingerprint(token.as_bytes());
            text = text.replacen(
                "login = \"fixture-dev-bot\"",
                &format!("login = \"fixture-dev-bot\"\ndrafts_fp = \"{fp}\""),
                1,
            );
        }
        World::over(root, fake, &text)
    }

    fn over(root: aterm_tempfile::TempDir, fake: FakeGithub, text: &str) -> World {
        fake.add_repo(APP, RepoSpec::public().push("fixture-dev-bot"));
        fake.add_repo(PULLED, RepoSpec::public().pull("fixture-dev-bot"));
        let router = common::router(text, root.path());
        let gh = common::github(router, &fake);
        World {
            _root: root,
            fake,
            gh,
        }
    }
}

fn full_names(items: &[agh::model::RepoObject]) -> Vec<&str> {
    items.iter().map(|r| r.full_name.as_str()).collect()
}

#[test]
fn get_reads_the_repository() {
    let w = World::new("repo-get");
    let repo =
        w.gh.repos()
            .get(&common::slug(APP))
            .expect("the repository");
    assert_eq!(repo.full_name, APP);
    assert_eq!(repo.default_branch.as_deref(), Some("main"));
    assert!(!repo.private);
    assert_eq!(
        repo.raw.get("full_name").and_then(|v| v.as_str()),
        Some(APP)
    );
    let missing =
        w.gh.repos()
            .get(&common::slug("fixture-dev/nothing"))
            .expect_err("absent");
    assert_eq!(missing.kind, Kind::NotVisible);
    w.fake.assert_clean();
}

#[test]
fn permissions_for_a_push_login_a_pull_login_and_anonymous() {
    let w = World::new("repo-perms");
    let repos = w.gh.repos();
    let (_, push) = repos
        .permissions(&common::slug(APP), Access::WriterRead)
        .expect("push");
    assert!(push.push && push.pull);
    let (_, pull) = repos
        .permissions(&common::slug(PULLED), Access::WriterRead)
        .expect("pull");
    assert!(pull.pull && !pull.push);
    let (_, anonymous) =
        w.gh.repos()
            .how(How::read().anonymous(true))
            .permissions(&common::slug(APP), Access::Read)
            .expect("anonymous");
    assert!(anonymous.pull && !anonymous.push && !anonymous.admin);
    w.fake.assert_clean();
}

#[test]
fn an_unvouched_fine_grained_writer_is_drafts_unproven_before_any_request() {
    let w = World::fine_grained("repo-fg", false);
    let error =
        w.gh.repos()
            .permissions(&common::slug(APP), Access::WriterRead)
            .expect_err("unproven");
    assert_eq!(error.kind, Kind::Forbidden);
    assert_eq!(error.sub.slug().as_deref(), Some("forbid.drafts_unproven"));
    assert!(error.next.contains("auth add dev "), "{}", error.next);
    assert!(w.fake.seen().is_empty());

    let (_, read) =
        w.gh.repos()
            .permissions(&common::slug(APP), Access::Read)
            .expect("a read never meets the gate");
    assert!(read.push);
    w.fake.assert_clean();
}

#[test]
fn a_vouched_fine_grained_writer_is_sent_and_answered() {
    let w = World::fine_grained("repo-fg-vouched", true);
    let (_, permissions) =
        w.gh.repos()
            .permissions(&common::slug(APP), Access::WriterRead)
            .expect("vouched");
    assert!(permissions.push);
    let sent = w.fake.seen();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].path_and_query, format!("/repos/{APP}"));
    assert_eq!(sent[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn an_organization_s_listing_follows_its_numeric_links_and_sorts_by_full_name() {
    let w = World::new("repo-org");
    w.fake.add_repo("fixture-dev/Gamma", RepoSpec::public());
    w.fake.add_repo(
        "fixture-dev/Beta",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    w.fake.add_repo("fixture-dev/hidden", RepoSpec::private());
    let repos = w.gh.repos();
    let opts = ListOpts::STANDARD.with_per_page(2);
    let all = repos
        .list_for_owner("fixture-dev", RepoListKind::All, &opts)
        .expect("the listing");
    assert_eq!(
        full_names(&all.items),
        [APP, "fixture-dev/Beta", "fixture-dev/Gamma", PULLED]
    );
    assert_eq!(all.pages, 2);
    let paths: Vec<String> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.origin == FakeOrigin::Api)
        .map(|s| s.path_and_query)
        .collect();
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with("/orgs/fixture-dev/repos?")),
        "{paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.starts_with("/organizations/")),
        "{paths:?}"
    );

    let private = repos
        .list_for_owner("fixture-dev", RepoListKind::Private, &opts)
        .expect("private");
    assert_eq!(full_names(&private.items), ["fixture-dev/Beta"]);
    let public = repos
        .list_for_owner("fixture-dev", RepoListKind::Public, &opts)
        .expect("public");
    assert_eq!(public.items.len(), 3);
    let forks = repos
        .list_for_owner("fixture-dev", RepoListKind::Forks, &opts)
        .expect("forks");
    assert!(forks.items.is_empty());
    let sources = repos
        .list_for_owner("fixture-dev", RepoListKind::Sources, &opts)
        .expect("sources");
    assert_eq!(sources.items.len(), 4);
    let two = repos
        .list_for_owner("fixture-dev", RepoListKind::All, &opts.with_limit(Some(2)))
        .expect("two");
    assert_eq!(full_names(&two.items), [APP, "fixture-dev/Beta"]);
    assert_eq!(two.end, End::Limited { at: 2 });
    w.fake.assert_clean();
}

#[test]
fn a_user_s_listing_is_their_public_repositories() {
    let w = World::new("repo-user");
    w.fake
        .add_token(&agh_fake::token("reader"), "fixture-reader");
    w.fake.add_repo("fixture-reader/tools", RepoSpec::public());
    w.fake.add_repo(
        "fixture-reader/private",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    let listing =
        w.gh.repos()
            .list_for_owner("fixture-reader", RepoListKind::All, &ListOpts::STANDARD)
            .expect("the listing");
    assert_eq!(full_names(&listing.items), ["fixture-reader/tools"]);
    assert!(
        w.fake
            .seen()
            .iter()
            .any(|s| s.path_and_query.starts_with("/users/fixture-reader/repos?"))
    );
    w.fake.assert_clean();
}

#[test]
fn the_profile_s_own_login_lists_through_user_repos() {
    let w = World::new("repo-own");
    w.fake.add_repo(
        "fixture-dev-bot/one",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    w.fake.add_repo(
        "fixture-dev-bot/Two",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    let repos = w.gh.repos();
    let all = repos
        .list_for_owner("fixture-dev-bot", RepoListKind::All, &ListOpts::STANDARD)
        .expect("the listing");
    assert_eq!(
        full_names(&all.items),
        ["fixture-dev-bot/one", "fixture-dev-bot/Two"]
    );
    w.fake.clear_seen();
    let private = repos
        .list_for_owner(
            "fixture-dev-bot",
            RepoListKind::Private,
            &ListOpts::STANDARD,
        )
        .expect("private");
    assert_eq!(full_names(&private.items), ["fixture-dev-bot/Two"]);
    let own: Vec<_> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query.starts_with("/user/repos"))
        .collect();
    assert_eq!(own.len(), 1);
    assert!(own[0].path_and_query.contains("affiliation=owner"));
    assert!(own[0].path_and_query.contains("visibility=private"));
    assert_eq!(own[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn a_writer_listing_looks_the_owner_up_as_a_read() {
    let root = common::scratch("repo-writer-owner");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, true);
    let text = common::canonical_config(root.path());
    let w = World::over(root, fake, &text);
    w.fake.add_repo(
        "alabsystems/aterm",
        RepoSpec::public().push("fixture-release-bot"),
    );
    let listing =
        w.gh.repos()
            .how(How::writer())
            .list_for_owner("alabsystems", RepoListKind::All, &ListOpts::STANDARD)
            .expect("the listing");
    assert_eq!(full_names(&listing.items), ["alabsystems/aterm"]);
    let lookups: Vec<_> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query == "/users/alabsystems")
        .collect();
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn a_writer_named_with_as_still_looks_the_owner_up_through_the_read_chain() {
    let root = common::scratch("repo-writer-as-owner");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, true);
    let text = common::canonical_config(root.path());
    let w = World::over(root, fake, &text);
    w.fake.add_repo(
        "alabsystems/aterm",
        RepoSpec::public().push("fixture-release-bot"),
    );
    let listing =
        w.gh.repos()
            .how(How::writer().as_profile(Some("release".to_string())))
            .list_for_owner("alabsystems", RepoListKind::All, &ListOpts::STANDARD)
            .expect("the listing");
    assert_eq!(full_names(&listing.items), ["alabsystems/aterm"]);
    let lookups: Vec<_> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query == "/users/alabsystems")
        .collect();
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn a_writer_listing_of_the_writer_s_own_login_reads_user_repos() {
    let root = common::scratch("repo-writer-own");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, false);
    let text = common::canonical_config(root.path()).replacen(
        "[default]",
        "[[owners]]\nname = \"fixture-dev-bot\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n\n[default]",
        1,
    );
    let w = World::over(root, fake, &text);
    w.fake.add_repo(
        "fixture-dev-bot/mine",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    let listing =
        w.gh.repos()
            .how(How::writer())
            .list_for_owner("fixture-dev-bot", RepoListKind::All, &ListOpts::STANDARD)
            .expect("the listing");
    assert_eq!(full_names(&listing.items), ["fixture-dev-bot/mine"]);
    let own: Vec<_> = w
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.path_and_query.starts_with("/user/repos"))
        .collect();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].as_login.as_deref(), Some("fixture-dev-bot"));
    w.fake.assert_clean();
}

#[test]
fn a_limit_cut_of_a_capped_owner_listing_stays_capped() {
    let w = World::new("repo-cap-limit");
    for name in ["c", "a", "b"] {
        w.fake.add_repo(
            &format!("fixture-dev/{name}"),
            RepoSpec::public().pull("fixture-dev-bot"),
        );
    }
    let opts = ListOpts::STANDARD
        .with_per_page(2)
        .with_max_pages(1)
        .with_partial(true)
        .with_limit(Some(1));
    let listing =
        w.gh.repos()
            .list_for_owner("fixture-dev", RepoListKind::All, &opts)
            .expect("a partial listing");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.end, End::Capped { pages: 1 });
    w.fake.assert_clean();
}

#[test]
fn an_owner_no_github_account_can_hold_is_usage_before_any_request() {
    let w = World::new("repo-owner-dots");
    for owner in ["..", ".", "fixture-dev/app", ""] {
        let error =
            w.gh.repos()
                .list_for_owner(owner, RepoListKind::All, &ListOpts::STANDARD)
                .expect_err("not an owner");
        assert_eq!(error.kind, Kind::Usage, "{owner:?}");
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

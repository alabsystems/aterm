// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Seed and lineage reads through the real Agh host with local Git/fake HTTP authority.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agh::config::Dirs;
use agh::git::HelperCommand;
use agh::identity::MapEnv;
use agh::policy::Router;
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh_fake::{FakeGithub, ReleaseSpec, RepoSpec};
use atpkg_publish::artifact::lineage_switch::{self, Switch};
use atpkg_publish::channel::seed::{self, GENERATED_MARKER};
use atpkg_publish::ctx::{Ctx, HostChoice};
use atpkg_publish::host::Hosts;
use atpkg_publish::host::agh::AghHost;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;

use super::{git, scratch};
use crate::shared::write_config;

const SLUG: &str = "fixture-dev/trust";
const COMPILER: &str = "1234567890123456789012345678901234567890";
const TAG: &str = "seed-native-fixture";

fn stage0(archives: &str) -> String {
    format!("compiler_git_commit_hash={COMPILER}\n{GENERATED_MARKER}\ndist/{archives}.tar.xz=abc\n")
}

fn ledger(minted: bool) -> String {
    format!(
        "[release]\nrepo = \"{SLUG}\"\ntag = \"{TAG}\"\n{}",
        if minted {
            "[release.mint]\ncompiler = \"fixture\"\n"
        } else {
            ""
        }
    )
}

fn commit(repo: &Path, archives: &str, minted: bool) -> String {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("bootstrap/trust-stage0")).unwrap();
    std::fs::write(repo.join("src/stage0"), stage0(archives)).unwrap();
    std::fs::write(repo.join(seed::LEDGER), ledger(minted)).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", archives]);
    git(repo, &["rev-parse", "HEAD"])
}

struct World {
    fake: FakeGithub,
    ctx: Ctx,
    checkout: PathBuf,
    authority: PathBuf,
    repin: String,
    receipt: String,
    _tmp: aterm_tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let tmp = scratch("native-seed");
        let origin = tmp.path().join("origin/fixture-dev/trust");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        commit(&origin, "old", false);
        let checkout = tmp.path().join("checkout");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        // A clone has no FETCH_HEAD. Create a real stale fetch record so the
        // host read must preserve its complete bytes as well as tracking refs.
        git(&checkout, &["fetch", "-q", "origin", "main"]);
        let authority = tmp.path().join("authority");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                authority.to_str().unwrap(),
            ],
        );
        let repin = commit(&authority, "minted", true);

        let fake = FakeGithub::start();
        let token = agh_fake::token("native-seed");
        fake.add_token(&token, "fixture-dev-bot");
        fake.add_repo(SLUG, RepoSpec::private().pull("fixture-dev-bot"));
        fake.set_branch(SLUG, "main", &repin);
        fake.add_release(SLUG, ReleaseSpec::published(TAG, "2026-11-06T00:00:00Z"));
        let tokens = tmp.path().join("tokens");
        aterm_uds::private_file::create_private_dir(&tokens).unwrap();
        aterm_uds::private_file::write_private(&tokens, "dev", token.as_bytes()).unwrap();
        let token_path = tokens.join("dev");
        let config = format!(
            "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
            token_path.display()
        );
        let dirs = Dirs::under(tmp.path());
        write_config(&dirs, &config);
        let validated = agh::config::load_for_host(&dirs).unwrap().unwrap();
        let ports = fake.ports();
        let env = MapEnv::new().with("PATH", &std::env::var("PATH").unwrap());
        let github = agh::GithubBuilder::new(Router::from_validated(validated))
            .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
            .endpoints(Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .env(Arc::new(env))
            .retry(RetryPolicy::NONE)
            .journal(dirs)
            .git_helper_for_tests(HelperCommand {
                exe: "/usr/bin/false".into(),
                args: Vec::new(),
            })
            .git_remote_for_tests(SLUG, &authority)
            .build();
        let host = AghHost::new(github, agh::Slug::new("fixture-dev", "trust").unwrap())
            .with_retry_pause(Duration::ZERO);
        let mut ctx = Keys::mint("m3", &[]).ctx(tmp.path(), Arc::new(FakeHost::new()));
        ctx.accounts.staging = "fixture-dev".into();
        ctx.hosts = Hosts::single(Arc::new(host));
        let ctx = ctx.with_host_choice(HostChoice::Agh);
        let receipt = format!(
            "repin_commit={repin}\nrepin_release_tag={TAG}\nrepin_compiler_git_commit_hash={COMPILER}\n"
        );
        Self {
            _tmp: tmp,
            fake,
            ctx,
            checkout,
            authority,
            repin,
            receipt,
        }
    }

    fn refusals(&self) -> Vec<String> {
        seed::publish_refusals_for_receipt(&self.ctx, &self.checkout, &self.receipt)
    }

    fn refs(&self) -> String {
        git(&self.checkout, &["for-each-ref"])
    }
}

#[test]
fn native_seed_uses_host_main_and_preserves_the_checkout_and_inventory_checks() {
    let world = World::new();
    let refs = world.refs();
    let head = git(&world.checkout, &["rev-parse", "HEAD"]);
    let fetch_head = std::fs::read(world.checkout.join(".git/FETCH_HEAD")).unwrap();
    let reasons = world.refusals();
    assert!(reasons.is_empty(), "{reasons:?}");
    assert_eq!(world.refs(), refs);
    assert_eq!(git(&world.checkout, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(world.checkout.join(".git/FETCH_HEAD")).unwrap(),
        fetch_head
    );
    assert_ne!(
        git(&world.checkout, &["rev-parse", "origin/main"]),
        world.repin
    );
    let changed = commit(&world.authority, "other-archives", true);
    world.fake.set_branch(SLUG, "main", &changed);
    assert!(
        world
            .refusals()
            .iter()
            .any(|why| why.contains("generated section"))
    );
    world.fake.assert_clean();
}

#[test]
fn native_seed_does_not_fall_back_when_the_host_head_is_unavailable() {
    let world = World::new();
    git(
        &world.checkout,
        &["fetch", "-q", world.authority.to_str().unwrap(), "main"],
    );
    git(&world.checkout, &["reset", "--hard", "FETCH_HEAD"]);
    // The local graph contains the exact re-pin, but the selected authority is absent.
    world
        .fake
        .apply(agh_fake::Change::Hide { slug: SLUG.into() });
    let reasons = world.refusals();
    assert!(
        reasons.iter().any(|why| why.contains("owner-bound main")),
        "{reasons:?}"
    );
    world.fake.assert_clean();
}

#[test]
fn native_lineage_reads_the_current_owner_ledger_without_moving_tracking_refs() {
    let world = World::new();
    let refs = world.refs();
    assert_eq!(
        lineage_switch::read(&world.ctx, &world.checkout).unwrap(),
        Switch::Live
    );
    assert_eq!(world.refs(), refs);
    world
        .fake
        .apply(agh_fake::Change::Hide { slug: SLUG.into() });
    assert!(matches!(
        lineage_switch::read(&world.ctx, &world.checkout).unwrap(),
        Switch::Unknown(_)
    ));
    assert_eq!(world.refs(), refs);
    world.fake.assert_clean();
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agh_fake::FakeGithub;

use super::*;
use crate::identity::vault::Slot;
use crate::identity::{MapEnv, fingerprint};
use crate::test_fixtures::{setup_file, write_config};
use crate::wire::http::HttpTransport;

const GRANT: &str =
    "vault = true\n[[writers]]\ncaller = 'atpkg-publish'\nrepos = ['alabsystems/aterm']\n";

struct Fixture {
    _root: aterm_tempfile::TempDir,
    dirs: Dirs,
    fake: FakeGithub,
    base: Github,
    dev: String,
    release: String,
}

impl Fixture {
    fn new(recorded: bool) -> Self {
        let root = crate::test_fixtures::scratch_dir("shared-routing");
        let dirs = Dirs::under(root.path());
        let fake = FakeGithub::start();
        let dev = agh_fake::token("routing-dev");
        let release = agh_fake::token("routing-release");
        fake.add_token(&dev, "fixture-dev");
        fake.add_token(&release, "fixture-release-bot");
        aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
        aterm_uds::private_file::create_private_dir(&dirs.home.join(".secrets")).unwrap();
        setup_file(&Slot::Dev.path(&dirs.home), dev.as_bytes());
        setup_file(&Slot::Release.path(&dirs.home), release.as_bytes());
        let env = Arc::new(MapEnv::new());
        let ports = fake.ports();
        let base = crate::GithubBuilder::new(Router::empty())
            .env(env.clone())
            .transport(Arc::new(HttpTransport::direct(env)))
            .endpoints(crate::wire::Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .fx(Arc::new(crate::Fx::simulated(1_760_000_000)))
            .caller("atpkg-publish")
            .build();
        if recorded {
            vault::record_for_tests(
                &dirs,
                &[
                    (Slot::Dev, &fingerprint(dev.as_bytes()), "fixture-dev"),
                    (
                        Slot::Release,
                        &fingerprint(release.as_bytes()),
                        "fixture-release-bot",
                    ),
                ],
            )
            .unwrap();
        }
        Self {
            _root: root,
            dirs,
            fake,
            base,
            dev,
            release,
        }
    }
}

fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut entries = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        let mut content = format!(
            "{:?}",
            (metadata.mode(), metadata.mtime(), metadata.mtime_nsec())
        )
        .into_bytes();
        if entry.file_type().unwrap().is_dir() {
            entries.insert(entry.path(), content);
            entries.extend(snapshot(&entry.path()));
        } else {
            content.extend(std::fs::read(entry.path()).unwrap());
            entries.insert(entry.path(), content);
        }
    }
    entries
}

#[test]
fn recorded_view_routes_vault_identities_without_manufacturing_a_grant_or_writing() {
    let fixture = Fixture::new(true);
    let before = snapshot(&fixture.dirs.home);
    let resolved = view_for_host(&fixture.dirs, fixture.base.env()).unwrap();
    let client = resolved.bind(&fixture.base).unwrap();
    assert_eq!(client.router().writer_of("fixture-dev"), Some("dev"));
    assert_eq!(client.router().writer_of("alabsystems"), Some("release"));
    assert!(
        client
            .router()
            .grant(
                "atpkg-publish",
                &crate::Slug::parse("alabsystems/aterm").unwrap()
            )
            .is_err()
    );
    assert!(client.router().writers().is_empty());
    assert!(fixture.fake.seen().is_empty());
    assert_eq!(before, snapshot(&fixture.dirs.home));
}

#[test]
fn only_exact_file_grants_survive_composition_with_vault_routes() {
    let fixture = Fixture::new(true);
    write_config(&fixture.dirs, GRANT);
    let resolved = view_for_host(&fixture.dirs, fixture.base.env()).unwrap();
    let client = resolved.bind(&fixture.base).unwrap();
    let target = crate::Slug::parse("alabsystems/aterm").unwrap();
    assert!(client.router().grant("atpkg-publish", &target).is_ok());
    assert!(client.router().grant("other-caller", &target).is_err());
    assert!(
        client
            .router()
            .grant(
                "atpkg-publish",
                &crate::Slug::parse("alabsystems/other").unwrap()
            )
            .is_err()
    );
    let text = crate::config::parse(GRANT).unwrap();
    let validated =
        crate::config::validate(&text, &fixture.dirs.home, &crate::config::file_id).unwrap();
    let forged = client.router().clone().with_file_authority(&validated);
    assert!(forged.grant("atpkg-publish", &target).is_err());
    assert!(fixture.fake.seen().is_empty());
}

#[test]
fn malformed_explicit_configuration_never_falls_back_to_healthy_vault_tokens() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(true);
    write_config(&fixture.dirs, "[profiles.invalid\n");
    let before = snapshot(&fixture.dirs.home);
    assert!(view_for_host(&fixture.dirs, fixture.base.env()).is_err());
    assert!(adopt_for_host(&fixture.base, &fixture.dirs).is_err());
    assert!(fixture.fake.seen().is_empty());
    assert_eq!(before, snapshot(&fixture.dirs.home));
    std::fs::write(fixture.dirs.config_file(), GRANT).unwrap();
    std::fs::set_permissions(
        fixture.dirs.config_file(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let before = snapshot(&fixture.dirs.home);
    assert!(view_for_host(&fixture.dirs, fixture.base.env()).is_err());
    assert!(adopt_for_host(&fixture.base, &fixture.dirs).is_err());
    assert!(fixture.fake.seen().is_empty());
    assert_eq!(before, snapshot(&fixture.dirs.home));
}

#[test]
fn unlearned_and_duplicate_tokens_do_not_gain_routes_in_a_view() {
    let fixture = Fixture::new(false);
    let before = snapshot(&fixture.dirs.home);
    let resolved = view(&fixture.dirs, fixture.base.env()).unwrap();
    assert_eq!(resolved.router().unwrap().writer_of("fixture-dev"), None);
    assert_eq!(before, snapshot(&fixture.dirs.home));
    std::fs::write(Slot::Release.path(&fixture.dirs.home), &fixture.dev).unwrap();
    let resolved = view(&fixture.dirs, fixture.base.env()).unwrap();
    assert_eq!(resolved.router().unwrap().writer_of("alabsystems"), None);
    assert!(fixture.fake.seen().is_empty());
}

#[test]
fn live_adoption_is_counted_once_and_clients_keep_the_same_judged_tokens() {
    let fixture = Fixture::new(false);
    write_config(&fixture.dirs, GRANT);
    let resolved = adopt_for_host(&fixture.base, &fixture.dirs).unwrap();
    assert_eq!(fixture.fake.seen().len(), 2);
    let first = resolved.bind(&fixture.base).unwrap();
    let second = first.clone();
    assert_eq!(first.meter().requests, 2);
    let replacement = agh_fake::token("replacement");
    fixture.fake.add_token(&replacement, "unexpected-login");
    std::fs::write(Slot::Release.path(&fixture.dirs.home), &replacement).unwrap();
    assert_eq!(
        first.users().me_once("release").unwrap().login,
        "fixture-release-bot"
    );
    assert_eq!(
        second.users().me_once("release").unwrap().login,
        "fixture-release-bot"
    );
    assert!(
        !first
            .redactor()
            .apply(&fixture.release)
            .contains(&fixture.release)
    );
    assert!(
        first
            .router()
            .grant(
                "atpkg-publish",
                &crate::Slug::parse("alabsystems/aterm").unwrap()
            )
            .is_ok()
    );
}

#[test]
fn a_bound_vault_dev_identity_cannot_write_the_release_tier() {
    let fixture = Fixture::new(true);
    write_config(&fixture.dirs, GRANT);
    let client = view_for_host(&fixture.dirs, fixture.base.env())
        .unwrap()
        .bind(&fixture.base)
        .unwrap();
    let target = crate::Slug::parse("alabsystems/aterm").unwrap();
    let granted = client.router().grant("atpkg-publish", &target).unwrap();
    let request = crate::policy::RouteRequest::write(crate::policy::RouteTarget::Repo(target))
        .with_named(crate::policy::Named::Literal(granted))
        .with_as(Some("dev".into()));
    assert!(client.permit(&request).is_err());
    assert!(fixture.fake.seen().is_empty());
}

#[test]
fn a_vault_join_preserves_only_the_host_admitted_explicit_legacy_profile() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(true);
    let path = fixture.dirs.home.join("explicit.token");
    setup_file(&path, agh_fake::token("explicit-loose").as_bytes());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_config(
        &fixture.dirs,
        &format!(
            "vault = true\n[profiles.lanes]\ntoken_file = '{}'\ntier = 'dev'\nlogin = 'fixture-explicit'\nlegacy_loose = true\n[[owners]]\nname = 'fixture-explicit'\ntier = 'dev'\nread = ['lanes']\nwrite = 'lanes'\n",
            path.display()
        ),
    );
    let host = view_for_host(&fixture.dirs, fixture.base.env())
        .unwrap()
        .bind(&fixture.base)
        .unwrap();
    assert_eq!(host.presence("lanes"), crate::policy::Presence::Present);
    assert!(!host.router().profile("dev").unwrap().legacy_loose());
    let cli = view(&fixture.dirs, fixture.base.env())
        .unwrap()
        .bind(&fixture.base)
        .unwrap();
    assert_ne!(cli.presence("lanes"), crate::policy::Presence::Present);
    assert!(fixture.fake.seen().is_empty());
}

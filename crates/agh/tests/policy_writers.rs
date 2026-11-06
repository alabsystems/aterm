// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The `[[writers]]` rows and L11, `Router::grant` and `policy grant` and `policy revoke` (WP1).

use std::path::Path;

use agh::config::{self, Dirs};
use agh::error::{Error, Kind, Sub};
use agh::identity::{self, MapEnv};
use agh::policy::{
    Named, PermitTarget, Presence, Provenance, RefusalCode, RouteRequest, RouteTarget, Router, Tier,
};
use agh_fake::FakeGithub;

use crate::common;

/// Each profile's presence on `router`, judged as the client judges it, with no environment.
fn presence_on(router: &Router) -> impl Fn(&str) -> Presence + '_ {
    |name: &str| {
        router.profile(name).map_or(Presence::Absent, |profile| {
            identity::presence(profile, &MapEnv::new())
        })
    }
}

/// The dev and release token files under `root`, as `canonical_tokens` writes them.
fn tokens(root: &Path, dev: bool, release: bool) {
    if dev {
        common::write_token(&root.join("tokens"), "dev.token", &agh_fake::token("dev"));
    }
    if release {
        common::write_token(
            &root.join("secrets"),
            "release.token",
            &agh_fake::token("release"),
        );
    }
}

fn assert_policy(error: &Error, code: RefusalCode) {
    assert_eq!(error.sub, Sub::Policy(code), "{}", error.why);
}

#[test]
fn a_file_grants_and_text_does_not() {
    let root = common::scratch("writers-grant");
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let target = router
        .grant("fixture-caller", &common::slug("alabsystems/aterm"))
        .expect("the row grants alabsystems/aterm");
    assert_eq!(target.provenance(), Provenance::Granted);
    assert_eq!(target.granted_to(), Some("fixture-caller"));
    assert_eq!(target.slug().to_string(), "alabsystems/aterm");
    let cased = router
        .grant("fixture-caller", &common::slug("ALABSYSTEMS/ATERM"))
        .expect("a grant compares ASCII case-insensitively");
    assert_eq!(cased.slug().to_string(), "ALABSYSTEMS/ATERM");

    let text = format!(
        "{}{}",
        common::canonical_config(root.path()),
        common::WRITERS
    );
    let from_text = common::router(&text, root.path());
    for router in [from_text, Router::empty()] {
        let refused = router
            .grant("fixture-caller", &common::slug("alabsystems/aterm"))
            .expect_err("only a file grants");
        assert_eq!(refused.kind, Kind::Refused);
        assert_eq!(refused.exit(), 1);
        assert_eq!(
            refused.sub.slug().as_deref(),
            Some("policy.not_granted"),
            "{}",
            refused.why
        );
        assert_eq!(
            refused.next,
            "agh policy grant fixture-caller alabsystems/aterm"
        );
    }
}

#[test]
fn grant_refuses_another_caller_repository_or_name() {
    let root = common::scratch("writers-other");
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let other_caller = router
        .grant("other-caller", &common::slug("alabsystems/aterm"))
        .expect_err("another caller has no row");
    assert_policy(&other_caller, RefusalCode::NotGranted);
    let other_repo = router
        .grant("fixture-caller", &common::slug("alabsystems/other"))
        .expect_err("the row lists one repository");
    assert_policy(&other_repo, RefusalCode::NotGranted);
    let bad = router
        .grant("Bad_Caller", &common::slug("alabsystems/aterm"))
        .expect_err("not a caller name");
    assert_eq!(bad.kind, Kind::Usage);
    assert_eq!(bad.exit(), 2);
    let legacy = agh_fake::token("caller")[4..].to_string();
    let pasted = router
        .grant(&legacy, &common::slug("alabsystems/aterm"))
        .expect_err("a token is not a caller name");
    assert_eq!(pasted.kind, Kind::Usage);
    assert!(!pasted.why.contains(&legacy), "{}", pasted.why);
}

#[test]
fn a_granted_write_permits_as_release() {
    let root = common::scratch("writers-permit");
    tokens(root.path(), true, true);
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let slug = common::slug("alabsystems/aterm");
    let granted = router.grant("fixture-caller", &slug).expect("granted");
    let req =
        RouteRequest::write(RouteTarget::Repo(slug.clone())).with_named(Named::Literal(granted));
    let permit = router
        .permit(&req, &presence_on(&router))
        .expect("a granted write permits");
    assert_eq!(permit.target(), &PermitTarget::Repo(slug));
    assert_eq!(permit.profile(), "release");
    assert_eq!(permit.tier(), Tier::Release);
    assert_eq!(permit.provenance(), Some(Provenance::Granted));
    assert_eq!(permit.grant(), Some("fixture-caller"));
}

#[test]
fn a_grant_names_one_repository() {
    let root = common::scratch("writers-one");
    tokens(root.path(), true, true);
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let granted = router
        .grant("fixture-caller", &common::slug("alabsystems/aterm"))
        .expect("granted");
    let req = RouteRequest::write(RouteTarget::Repo(common::slug("alabsystems/other")))
        .with_named(Named::Literal(granted.clone()));
    let refused = router
        .permit(&req, &presence_on(&router))
        .expect_err("the grant is for another repository");
    assert_policy(&refused, RefusalCode::ReleaseNeedsLiteral);
    let dev = router
        .permit(
            &RouteRequest::write(RouteTarget::Repo(common::slug("fixture-dev/app")))
                .with_named(Named::Literal(granted)),
            &presence_on(&router),
        )
        .expect("a dev write needs no literal");
    assert_eq!(dev.profile(), "dev");
    assert_eq!(
        dev.provenance(),
        None,
        "another repository's grant is not this one's"
    );
    assert_eq!(dev.grant(), None);
}

#[test]
fn permit_refuses_what_decide_refuses() {
    let root = common::scratch("writers-refuse");
    tokens(root.path(), true, true);
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let presence = presence_on(&router);
    let aterm = || RouteTarget::Repo(common::slug("alabsystems/aterm"));

    let default = router
        .permit(&RouteRequest::write(aterm()), &presence)
        .expect_err("a release write needs a literal");
    assert_policy(&default, RefusalCode::ReleaseNeedsLiteral);

    let channel = router
        .permit(
            &RouteRequest::write(aterm()).with_named(Named::Target("channel".into())),
            &presence,
        )
        .expect_err("a target is no literal");
    assert_policy(&channel, RefusalCode::ReleaseTargetForWrite);

    let bare = common::scratch("writers-absent");
    tokens(bare.path(), true, false);
    let (_bare_dirs, bare_router) = common::file_router(bare.path(), common::WRITERS);
    let granted = bare_router
        .grant("fixture-caller", &common::slug("alabsystems/aterm"))
        .expect("granted");
    let absent = bare_router
        .permit(
            &RouteRequest::write(aterm()).with_named(Named::Literal(granted)),
            &presence_on(&bare_router),
        )
        .expect_err("no release token");
    assert_policy(&absent, RefusalCode::WriterAbsent);
    assert_eq!(absent.exit(), 4);

    let writer_read = router
        .permit(&RouteRequest::writer_read(aterm()), &presence)
        .expect_err("a permit is for a write");
    assert_eq!(writer_read.kind, Kind::Usage);
    assert!(
        writer_read.why.contains("writer-read"),
        "{}",
        writer_read.why
    );

    let owner = router
        .permit(
            &RouteRequest::write(RouteTarget::Owner("alabsystems".into())),
            &presence,
        )
        .expect_err("an owner is no write target");
    assert_policy(&owner, RefusalCode::UnscopedNotRead);

    let identity = router
        .permit(&RouteRequest::write(RouteTarget::Identity), &presence)
        .expect_err("the file router writes no identity");
    assert_policy(&identity, RefusalCode::UnscopedNotRead);
}

#[test]
fn a_dev_write_needs_no_literal() {
    let root = common::scratch("writers-dev");
    tokens(root.path(), true, false);
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let permit = router
        .permit(
            &RouteRequest::write(RouteTarget::Repo(common::slug("fixture-dev/app"))),
            &presence_on(&router),
        )
        .expect("a dev write permits");
    assert_eq!(permit.profile(), "dev");
    assert_eq!(permit.tier(), Tier::Dev);
    assert_eq!(permit.provenance(), None);
    assert_eq!(permit.grant(), None);
}

#[test]
fn a_permit_covers_its_repository_and_below() {
    let root = common::scratch("writers-covers");
    tokens(root.path(), true, true);
    let (_dirs, router) = common::file_router(root.path(), common::WRITERS);
    let slug = common::slug("alabsystems/aterm");
    let granted = router.grant("fixture-caller", &slug).expect("granted");
    let permit = router
        .permit(
            &RouteRequest::write(RouteTarget::Repo(slug)).with_named(Named::Literal(granted)),
            &presence_on(&router),
        )
        .expect("a permit");
    for path in [
        "/repos/alabsystems/aterm",
        "/repos/alabsystems/aterm/releases/1",
        "/repos/ALABSYSTEMS/aterm/releases",
        "/repos/alabsystems/%61term/releases",
        "/repos/alab%73ystems/aterm",
    ] {
        assert!(permit.covers(path), "{path}");
    }
    for path in [
        "/repos/alabsystems/aterm2",
        "/repos/alabsystems/aterm/../other",
        "/repos/alabsystems/aterm/%2E%2E/other",
        "/repos/alabsystems//aterm",
        "/repos/alabsystems%2Faterm",
        "/repos/alabsystems/aterm/git/refs/tags%2F..%2F..",
        "/repos/alabsystems/at%zzerm",
        "/repos/alabsystems/aterm/releases/1%zz",
        "/repos/alabsystems/aterm/releases%2",
        "/repos/alabsystems",
        "/user",
    ] {
        assert!(!permit.covers(path), "{path}");
    }
}

/// The canonical agh.toml with `legacy_loose = true` on the dev profile.
fn loose_config(root: &Path) -> String {
    common::canonical_config(root).replacen(
        "login = \"fixture-dev-bot\"\n",
        "login = \"fixture-dev-bot\"\nlegacy_loose = true\n",
        1,
    )
}

/// The dev profile's `legacy_loose()` and presence under `config::load` and `load_for_host`.
fn dev_under_both(dirs: &Dirs) -> [(bool, Presence); 2] {
    let judge = |validated: Option<config::Validated>| {
        let validated = validated.expect("agh.toml is there");
        let dev = &validated.profiles()["dev"];
        (dev.legacy_loose(), identity::presence(dev, &MapEnv::new()))
    };
    [
        judge(config::load(dirs).expect("load reads it")),
        judge(config::load_for_host(dirs).expect("load_for_host reads it")),
    ]
}

#[test]
fn legacy_loose_is_honoured_by_the_host_loader_only() {
    let root = common::scratch("writers-loose");
    let dirs = Dirs::under(root.path());
    common::write_config(&dirs, &loose_config(root.path()));
    let file = common::write_token(
        &root.path().join("tokens"),
        "dev.token",
        &agh_fake::token("dev"),
    );
    common::chmod(&file, 0o644);
    assert_eq!(
        dev_under_both(&dirs),
        [
            (false, Presence::Unusable("loose")),
            (true, Presence::Present)
        ]
    );
    let loose_read = |mode: u32| {
        common::chmod(&file, mode);
        let validated = config::load_for_host(&dirs)
            .expect("load_for_host reads it")
            .expect("agh.toml is there");
        identity::load_for_tests(&validated.profiles()["dev"], &MapEnv::new())
            .expect("a credential")
            .loose_file()
    };
    assert!(
        loose_read(0o644),
        "a loose read is reported, for the lane's warning"
    );
    assert!(!loose_read(0o600), "a private file reads as private");
    common::chmod(&file, 0o664);
    assert_eq!(
        dev_under_both(&dirs),
        [
            (false, Presence::Unusable("loose")),
            (true, Presence::Unusable("loose"))
        ]
    );
    common::chmod(&file, 0o600);
    let real = root.path().join("tokens").join("dev.real");
    std::fs::rename(&file, &real).expect("move the token aside");
    std::os::unix::fs::symlink(&real, &file).expect("a link in its place");
    assert_eq!(
        dev_under_both(&dirs),
        [
            (false, Presence::Unusable("symlink")),
            (true, Presence::Unusable("symlink"))
        ]
    );
}

/// The canonical agh.toml with a comment line, for the CLI's edits.
fn commented(root: &Path) -> String {
    format!(
        "# kept: the owner's own note\n{}",
        common::canonical_config(root)
    )
}

fn run(
    root: &Path,
    fake: &FakeGithub,
    terminals: (bool, bool),
    args: &[&str],
) -> (u8, String, String) {
    let (ctx, captured) = common::ctx(root, fake, common::env());
    let mut ctx = ctx.with_terminals(terminals.0, terminals.1);
    let exit = common::run_cli(&mut ctx, args);
    (exit, captured.stdout_text(), captured.stderr_text())
}

fn grant_vault(root: &Path, fake: &FakeGithub, recorded: bool) -> Dirs {
    use agh::identity::vault::{self, Slot};
    let dirs = Dirs::under(root);
    aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
    let dev = agh_fake::token("first-grant-dev");
    let release = agh_fake::token("first-grant-release");
    fake.add_token(&dev, "fixture-dev");
    fake.add_token(&release, "fixture-release-bot");
    common::write_token(&dirs.home.join(".secrets"), "gh_access_token", &dev);
    common::write_token(
        &dirs.home.join(".secrets"),
        "gh_access_token_alabsystems",
        &release,
    );
    if recorded {
        vault::record_for_tests(
            &dirs,
            &[
                (
                    Slot::Dev,
                    &identity::fingerprint(dev.as_bytes()),
                    "fixture-dev",
                ),
                (
                    Slot::Release,
                    &identity::fingerprint(release.as_bytes()),
                    "fixture-release-bot",
                ),
            ],
        )
        .unwrap();
    }
    dirs
}

#[test]
fn first_grant_preserves_recorded_vault_routing_without_learning() {
    use std::os::unix::fs::PermissionsExt;
    let root = common::scratch("first-grant");
    let fake = FakeGithub::start();
    let dirs = grant_vault(root.path(), &fake, true);
    let recorded: Vec<_> = std::fs::read_dir(&dirs.config)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let args = ["policy", "grant", "atpkg-publish", "alabsystems/aterm"];
    {
        let _held = identity::provision::hold_auth_edit_for_tests(&dirs).unwrap();
        let (exit, _, _) = run(root.path(), &fake, (false, false), &args);
        assert_eq!(exit, 1, "first creation respects the auth edit lock");
        assert!(!dirs.config_file().exists());
    }
    let (exit, stdout, stderr) = run(root.path(), &fake, (false, false), &args);
    assert_eq!(exit, 0, "{stdout}{stderr}");
    let value: aterm_toml::Value =
        aterm_toml::from_str(&std::fs::read_to_string(dirs.config_file()).unwrap()).unwrap();
    assert_eq!(
        value.get("vault").and_then(aterm_toml::Value::as_bool),
        Some(true)
    );
    for absent in ["profiles", "owners", "targets", "default"] {
        assert!(value.get(absent).is_none(), "no implicit {absent}");
    }
    assert_eq!(
        granted(&dirs),
        [row("atpkg-publish", &["alabsystems/aterm"])]
    );
    assert_eq!(
        std::fs::metadata(dirs.config_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&dirs.config)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let view = agh::routing::view(&dirs, &common::env()).unwrap();
    let router = view.router().unwrap();
    assert_eq!(router.writer_of("fixture-dev"), Some("dev"));
    assert_eq!(router.writer_of("alabsystems"), Some("release"));
    assert!(
        router
            .grant("atpkg-publish", &common::slug("alabsystems/aterm"))
            .is_ok()
    );
    assert!(
        router
            .grant("atpkg-publish", &common::slug("alabsystems/other"))
            .is_err()
    );
    let (exit, stdout, stderr) = run(root.path(), &fake, (false, false), &args);
    assert_eq!(exit, 0, "{stderr}");
    assert!(stdout.contains("\"change\":\"unchanged\""));
    assert!(
        fake.seen().is_empty(),
        "grant must never learn credentials over HTTP"
    );
    for (path, before) in recorded {
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "adoption record unchanged"
        );
    }
}

#[test]
fn first_grant_allows_one_recorded_vault_identity_without_its_partner() {
    use agh::identity::vault::Slot;
    for absent in Slot::ALL {
        let root = common::scratch("first-grant-single-identity");
        let fake = FakeGithub::start();
        let dirs = grant_vault(root.path(), &fake, true);
        std::fs::remove_file(absent.path(&dirs.home)).unwrap();
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", "grant", "atpkg-publish", "alabsystems/aterm"],
        );
        assert_eq!(exit, 0, "{absent:?}: {stdout}{stderr}");
        assert_eq!(
            granted(&dirs),
            [row("atpkg-publish", &["alabsystems/aterm"])]
        );
        assert!(fake.seen().is_empty());
    }
}

#[test]
fn first_grant_refuses_unrecorded_rotated_or_unsafe_vault_credentials() {
    use agh::identity::vault::Slot;
    use std::os::unix::fs::PermissionsExt;
    for state in ["unrecorded", "rotated", "loose", "absent", "damaged"] {
        let root = common::scratch("first-grant-refused");
        let fake = FakeGithub::start();
        let dirs = if state == "absent" {
            Dirs::under(root.path())
        } else {
            grant_vault(root.path(), &fake, state != "unrecorded")
        };
        if state == "rotated" {
            std::fs::write(Slot::Dev.path(&dirs.home), agh_fake::token("rotated")).unwrap();
        } else if state == "loose" {
            std::fs::set_permissions(
                Slot::Dev.path(&dirs.home),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        } else if state == "damaged" {
            std::fs::write(
                dirs.config.join(agh::identity::vault::RECORD_FILE),
                "[broken\n",
            )
            .unwrap();
        }
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", "grant", "atpkg-publish", "alabsystems/aterm"],
        );
        assert_eq!(exit, 1, "{state}: {stdout}{stderr}");
        assert!(!dirs.config_file().exists(), "{state}");
        assert!(fake.seen().is_empty(), "{state}: no credential learning");
    }
}

#[test]
fn revoke_and_malformed_grants_do_not_initialize_a_vault_config() {
    let root = common::scratch("first-grant-input");
    let fake = FakeGithub::start();
    let dirs = grant_vault(root.path(), &fake, true);
    for (args, expected) in [
        (
            ["policy", "revoke", "atpkg-publish", "alabsystems/aterm"],
            1,
        ),
        (["policy", "grant", "Bad_Caller", "alabsystems/aterm"], 2),
        (["policy", "grant", "atpkg-publish", "@target"], 2),
    ] {
        let (exit, stdout, stderr) = run(root.path(), &fake, (false, false), &args);
        assert_eq!(exit, expected, "{stdout}{stderr}");
        assert!(!dirs.config_file().exists());
    }
    assert!(fake.seen().is_empty());
}

#[test]
fn first_grant_does_not_replace_unsafe_or_malformed_existing_config() {
    use std::os::unix::fs::PermissionsExt;
    for state in ["symlink", "loose", "invalid", "not-utf8"] {
        let root = common::scratch("first-grant-config");
        let fake = FakeGithub::start();
        let dirs = grant_vault(root.path(), &fake, true);
        let content: &[u8] = match state {
            "invalid" => b"[broken\n",
            "not-utf8" => &[0xff],
            _ => b"vault = true\n",
        };
        let protected = if state == "symlink" {
            root.path().join("elsewhere")
        } else {
            dirs.config_file()
        };
        std::fs::write(&protected, content).unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o600)).unwrap();
        if state == "symlink" {
            std::os::unix::fs::symlink(&protected, dirs.config_file()).unwrap();
        } else if state == "loose" {
            std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let (exit, _, _) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", "grant", "atpkg-publish", "alabsystems/aterm"],
        );
        assert_ne!(exit, 0, "{state}");
        assert_eq!(std::fs::read(protected).unwrap(), content, "{state}");
        assert!(fake.seen().is_empty());
    }
}

#[test]
fn policy_grant_and_revoke_edit_writers() {
    let root = common::scratch("writers-cli");
    let dirs = Dirs::under(root.path());
    common::write_config(&dirs, &commented(root.path()));
    let fake = FakeGithub::start();
    let no_terminal = (false, false);
    let grant = ["policy", "grant", "atpkg-publish", "alabsystems/aterm"];

    let (exit, stdout, stderr) = run(root.path(), &fake, no_terminal, &grant);
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(
        stdout,
        "{\"caller\":\"atpkg-publish\",\"change\":\"added\",\"repos\":[\"alabsystems/aterm\"]}\n"
    );
    let validated = config::load_for_host(&dirs)
        .expect("agh.toml loads")
        .expect("agh.toml is there");
    assert_eq!(
        validated.writers(),
        [(
            "atpkg-publish".to_string(),
            vec![common::slug("alabsystems/aterm")]
        )]
    );
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(text.contains("# kept: the owner's own note"), "{text}");

    let (exit, stdout, stderr) = run(root.path(), &fake, no_terminal, &grant);
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(
        stdout,
        "{\"caller\":\"atpkg-publish\",\"change\":\"unchanged\",\"repos\":[\"alabsystems/aterm\"]}\n"
    );
    let (exit, stdout, stderr) = run(root.path(), &fake, (false, true), &grant);
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(
        stdout,
        "{\"caller\":\"atpkg-publish\",\"change\":\"unchanged\",\"repos\":[\"alabsystems/aterm\"]}\n"
    );

    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        no_terminal,
        &["policy", "revoke", "atpkg-publish", "ALABSYSTEMS/ATERM"],
    );
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(
        stdout,
        "{\"caller\":\"atpkg-publish\",\"change\":\"removed\",\"repos\":[]}\n"
    );
    let validated = config::load_for_host(&dirs)
        .expect("agh.toml loads")
        .expect("agh.toml is there");
    assert!(validated.writers().is_empty(), "{:?}", validated.writers());
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(!text.contains("[[writers]]"), "{text}");
    assert!(text.contains("# kept: the owner's own note"), "{text}");

    let before = std::fs::read(dirs.config_file()).expect("agh.toml");
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        no_terminal,
        &["policy", "grant", "atpkg-publish", "fixture-dev/app"],
    );
    assert_eq!(exit, 1, "{stdout}{stderr}");
    let error = common::error_json(&stderr);
    let why = error["why"].as_str().expect("a why");
    assert!(why.contains("L11"), "{why}");
    assert!(why.contains("fixture-dev/app"), "{why}");
    assert_eq!(std::fs::read(dirs.config_file()).expect("agh.toml"), before);
}

#[test]
fn policy_check_lists_writers_and_legacy_loose() {
    let root = common::scratch("writers-check");
    let dirs = Dirs::under(root.path());
    common::write_config(
        &dirs,
        &format!("{}{}", loose_config(root.path()), common::WRITERS),
    );
    let fake = FakeGithub::start();
    let (ctx, captured) = common::ctx(root.path(), &fake, common::env());
    let mut ctx = ctx;
    let exit = common::run_cli(&mut ctx, &["policy", "check"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let report = common::stdout_json(&captured);
    assert_eq!(report["counts"]["writers"], aterm_json::Value::from(1u64));
    assert_eq!(
        aterm_json::to_string(&report["legacy_loose"]).expect("JSON"),
        "[\"dev\"]"
    );
    assert_eq!(
        aterm_json::to_string(&report["writers"]).expect("JSON"),
        "[{\"caller\":\"fixture-caller\",\"repos\":[\"alabsystems/aterm\"]}]"
    );
}

/// The token's body is not in `text`.
fn assert_unleaked(text: &str, body: &str) {
    assert!(!text.contains(body), "the token's body in {text:?}");
}

#[test]
fn a_token_pasted_into_writers_is_never_printed() {
    let root = common::scratch("writers-leak");
    let dirs = Dirs::under(root.path());
    let fake = FakeGithub::start();
    let legacy = agh_fake::token("leak")[4..].to_string();
    let no_terminal = (false, false);

    let faulty = format!(
        "{}\n[profiles.{legacy}]\nenv = \"AGH_LEAK_TOKEN\"\ntier = \"dev\"\n\
         legacy_loose = true\n\n[[writers]]\ncaller = \"leaky\"\nrepos = [\"{legacy}\"]\n",
        common::canonical_config(root.path())
    );
    common::write_config(&dirs, &faulty);
    let (exit, stdout, stderr) = run(root.path(), &fake, no_terminal, &["policy", "check"]);
    assert_ne!(exit, 0, "{stdout}");
    assert!(stdout.contains("L11"), "{stdout}");
    assert_unleaked(&format!("{stdout}{stderr}"), &legacy);
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        no_terminal,
        &["policy", "grant", "leaky", "alabsystems/aterm"],
    );
    assert_eq!(exit, 1, "{stdout}{stderr}");
    assert_unleaked(&format!("{stdout}{stderr}"), &legacy);

    let valid = format!(
        "{}\n[[writers]]\ncaller = \"leaky\"\nrepos = [\"alabsystems/{legacy}\"]\n",
        common::canonical_config(root.path())
    );
    common::write_config(&dirs, &valid);
    let (exit, stdout, stderr) = run(root.path(), &fake, no_terminal, &["policy", "check"]);
    assert_eq!(exit, 0, "{stderr}");
    assert_unleaked(&format!("{stdout}{stderr}"), &legacy);
    let slug = format!("alabsystems/{legacy}");
    for caller in ["leaky", "other"] {
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            no_terminal,
            &["policy", "grant", caller, &slug],
        );
        assert_eq!(exit, 0, "{stderr}");
        assert!(stdout.contains("\"repos\":[\""), "{stdout}");
        assert_unleaked(&format!("{stdout}{stderr}"), &legacy);
    }

    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        no_terminal,
        &["policy", "grant", &legacy, "alabsystems/aterm"],
    );
    assert_eq!(exit, 2, "{stdout}{stderr}");
    assert_unleaked(&format!("{stdout}{stderr}"), &legacy);

    let before = std::fs::read(dirs.config_file()).expect("agh.toml");
    for verb in ["grant", "revoke"] {
        for repo in [
            legacy.clone(),
            format!("@{legacy}"),
            format!("alabsystems/aterm/{legacy}"),
        ] {
            let (exit, stdout, stderr) = run(
                root.path(),
                &fake,
                no_terminal,
                &["policy", verb, "leaky", &repo],
            );
            assert_eq!(exit, 2, "{verb} {stdout}{stderr}");
            assert_unleaked(&format!("{stdout}{stderr}"), &legacy);
        }
    }
    assert_eq!(std::fs::read(dirs.config_file()).expect("agh.toml"), before);
}

/// The canonical agh.toml with its release profile moved to the end, after the `[[owners]]`
/// rows: tables whose order in the file is not their keys' order.
fn interleaved(root: &Path) -> String {
    let canonical = common::canonical_config(root);
    let start = canonical
        .find("[profiles.release]")
        .expect("the release row");
    let end = canonical.find("[[owners]]").expect("the owner rows");
    let release = &canonical[start..end];
    format!(
        "{}{}# the release profile, kept last on purpose\n{}",
        &canonical[..start],
        &canonical[end..],
        release.trim_end()
    )
}

#[test]
fn a_new_writers_row_goes_after_the_last_table() {
    let root = common::scratch("writers-last");
    let dirs = Dirs::under(root.path());
    common::write_config(&dirs, &interleaved(root.path()));
    let fake = FakeGithub::start();
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        (false, false),
        &["policy", "grant", "ci", "alabsystems/aterm"],
    );
    assert_eq!(exit, 0, "{stdout}{stderr}");
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(
        text.ends_with("[[writers]]\ncaller = \"ci\"\nrepos = [\"alabsystems/aterm\"]\n"),
        "{text}"
    );
    let release = text.find("[profiles.release]").expect("the release row");
    assert!(
        release < text.find("[[writers]]").expect("the row"),
        "{text}"
    );
    assert!(
        text.contains("# the release profile, kept last on purpose"),
        "{text}"
    );
}

#[test]
fn an_edit_never_drops_an_entry_it_cannot_read() {
    let root = common::scratch("writers-entry");
    let dirs = Dirs::under(root.path());
    common::write_config(
        &dirs,
        &format!(
            "{}\n[[writers]]\ncaller = \"ci\"\nrepos = [\"alabsystems/aterm\", 7]\n",
            common::canonical_config(root.path())
        ),
    );
    let before = std::fs::read(dirs.config_file()).expect("agh.toml");
    let fake = FakeGithub::start();
    for args in [
        ["policy", "grant", "ci", "alabsystems/other"],
        ["policy", "revoke", "ci", "alabsystems/aterm"],
    ] {
        let (exit, stdout, stderr) = run(root.path(), &fake, (false, false), &args);
        assert_eq!(exit, 10, "{stdout}{stderr}");
        assert_eq!(std::fs::read(dirs.config_file()).expect("agh.toml"), before);
    }
}

/// The writers each `agh.toml` under `dirs` grants, as `load_for_host` reads them.
fn granted(dirs: &Dirs) -> Vec<(String, Vec<String>)> {
    config::load_for_host(dirs)
        .expect("agh.toml loads")
        .expect("agh.toml is there")
        .writers()
        .iter()
        .map(|(caller, repos)| {
            (
                caller.clone(),
                repos.iter().map(ToString::to_string).collect(),
            )
        })
        .collect()
}

fn row(caller: &str, repos: &[&str]) -> (String, Vec<String>) {
    (
        caller.to_string(),
        repos.iter().map(ToString::to_string).collect(),
    )
}

#[test]
fn an_edit_keeps_the_comments_inside_repos() {
    let root = common::scratch("writers-comments");
    let dirs = Dirs::under(root.path());
    let block = "[[writers]]\ncaller = \"ci\"\nrepos = [\n  \"alabsystems/aterm\", # the release \
                 lane\n  \"alabsystems/site\",  # until the docs move\n]\n";
    common::write_config(
        &dirs,
        &format!("{}\n{block}", common::canonical_config(root.path())),
    );
    let fake = FakeGithub::start();
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        (false, false),
        &["policy", "grant", "ci", "alabsystems/docs"],
    );
    assert_eq!(exit, 0, "{stdout}{stderr}");
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(
        text.ends_with(
            "repos = [\n  \"alabsystems/aterm\", # the release lane\n  \"alabsystems/site\",  \
             # until the docs move\n  \"alabsystems/docs\",\n]\n"
        ),
        "{text}"
    );
    assert_eq!(
        granted(&dirs),
        [row(
            "ci",
            &["alabsystems/aterm", "alabsystems/site", "alabsystems/docs"]
        )]
    );

    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        (false, false),
        &["policy", "revoke", "ci", "alabsystems/site"],
    );
    assert_eq!(exit, 0, "{stdout}{stderr}");
    let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
    assert!(
        text.ends_with(
            "repos = [\n  \"alabsystems/aterm\", # the release lane\n  \"alabsystems/docs\",\n]\n"
        ),
        "{text}"
    );
    assert_eq!(
        granted(&dirs),
        [row("ci", &["alabsystems/aterm", "alabsystems/docs"])]
    );

    common::write_config(
        &dirs,
        &format!(
            "{}\n[[writers]]\ncaller = \"ci\"\nrepos = [\"alabsystems/aterm\", \
             \"alabsystems/site\"] # one line\n",
            common::canonical_config(root.path())
        ),
    );
    for (verb, repo, after) in [
        (
            "grant",
            "alabsystems/docs",
            "[\"alabsystems/aterm\", \"alabsystems/site\", \"alabsystems/docs\"] # one line\n",
        ),
        (
            "revoke",
            "alabsystems/aterm",
            "[\"alabsystems/site\", \"alabsystems/docs\"] # one line\n",
        ),
    ] {
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", verb, "ci", repo],
        );
        assert_eq!(exit, 0, "{stdout}{stderr}");
        let text = std::fs::read_to_string(dirs.config_file()).expect("agh.toml");
        assert!(text.ends_with(&format!("repos = {after}")), "{text}");
    }
}

#[test]
fn inline_and_empty_writers_are_edited_in_their_form() {
    let root = common::scratch("writers-inline");
    let dirs = Dirs::under(root.path());
    let fake = FakeGithub::start();
    let edit = |verb: &str, caller: &str, repo: &str| {
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", verb, caller, repo],
        );
        assert_eq!(exit, 0, "{verb} {caller} {repo}: {stdout}{stderr}");
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml")
    };

    common::write_config(
        &dirs,
        &format!(
            "writers = [{{ caller = \"ci\", repos = [\"alabsystems/aterm\"] }}]\n\n{}",
            common::canonical_config(root.path())
        ),
    );
    let text = edit("grant", "ci", "alabsystems/docs");
    assert!(
        text.starts_with(
            "writers = [{ caller = \"ci\", repos = [\"alabsystems/aterm\", \"alabsystems/docs\"] \
             }]\n"
        ),
        "{text}"
    );
    assert_eq!(
        granted(&dirs),
        [row("ci", &["alabsystems/aterm", "alabsystems/docs"])]
    );
    edit("grant", "other", "alabsystems/aterm");
    assert_eq!(
        granted(&dirs),
        [
            row("ci", &["alabsystems/aterm", "alabsystems/docs"]),
            row("other", &["alabsystems/aterm"])
        ]
    );
    edit("revoke", "ci", "alabsystems/aterm");
    let text = edit("revoke", "ci", "alabsystems/docs");
    assert!(!text.contains("[[writers]]"), "{text}");
    assert_eq!(granted(&dirs), [row("other", &["alabsystems/aterm"])]);

    common::write_config(
        &dirs,
        &format!("writers = []\n\n{}", common::canonical_config(root.path())),
    );
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        (false, false),
        &["policy", "revoke", "ci", "alabsystems/aterm"],
    );
    assert_eq!(exit, 0, "{stdout}{stderr}");
    assert!(stdout.contains("\"change\":\"unchanged\""), "{stdout}");
    let text = edit("grant", "ci", "alabsystems/aterm");
    assert!(!text.contains("writers = []"), "{text}");
    assert_eq!(granted(&dirs), [row("ci", &["alabsystems/aterm"])]);

    // The owner's comment lines above an empty `writers = []` survive the first grant, and
    // the revoke that empties a commented inline row and the grant after it.
    let note = "# owner note: the lanes write the channel";
    common::write_config(
        &dirs,
        &format!(
            "{note}\n# a second line\nwriters = []\n\n{}",
            common::canonical_config(root.path())
        ),
    );
    let text = edit("grant", "ci", "alabsystems/aterm");
    assert!(
        text.contains(note) && text.contains("# a second line"),
        "{text}"
    );
    assert!(!text.contains("writers = []"), "{text}");
    assert_eq!(granted(&dirs), [row("ci", &["alabsystems/aterm"])]);

    common::write_config(
        &dirs,
        &format!(
            "{note}\nwriters = [{{ caller = \"ci\", repos = [\"alabsystems/aterm\"] }}]\n\n{}",
            common::canonical_config(root.path())
        ),
    );
    let text = edit("revoke", "ci", "alabsystems/aterm");
    assert!(text.contains(note), "{text}");
    assert!(granted(&dirs).is_empty(), "{text}");
    let text = edit("grant", "ci", "alabsystems/aterm");
    assert!(text.contains(note), "{text}");
    assert_eq!(granted(&dirs), [row("ci", &["alabsystems/aterm"])]);
}

#[test]
fn an_edit_of_a_crlf_file_writes_crlf_lines() {
    let root = common::scratch("writers-crlf");
    let dirs = Dirs::under(root.path());
    let fake = FakeGithub::start();
    let edit = |verb: &str, caller: &str, repo: &str| {
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", verb, caller, repo],
        );
        assert_eq!(exit, 0, "{verb} {caller} {repo}: {stdout}{stderr}");
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml")
    };
    let bare_lf = |text: &str| {
        text.match_indices('\n')
            .any(|(at, _)| at.checked_sub(1).and_then(|cr| text.as_bytes().get(cr)) != Some(&b'\r'))
    };
    let lf = format!(
        "{}\n[[writers]]\ncaller = \"ci\"\nrepos = [\n  \"alabsystems/aterm\", # lane\n  \
         \"alabsystems/site\"\n]\n",
        common::canonical_config(root.path())
    );
    common::write_config(&dirs, &lf.replace('\n', "\r\n"));
    // An entry spliced into a multi-line `repos`, a row the rebuild adds, and a removal.
    let text = edit("grant", "ci", "alabsystems/docs");
    assert!(!bare_lf(&text), "{text:?}");
    let text = edit("grant", "other", "alabsystems/aterm");
    assert!(!bare_lf(&text), "{text:?}");
    let text = edit("revoke", "ci", "alabsystems/site");
    assert!(!bare_lf(&text), "{text:?}");
    assert_eq!(
        granted(&dirs),
        [
            row("ci", &["alabsystems/aterm", "alabsystems/docs"]),
            row("other", &["alabsystems/aterm"])
        ]
    );

    // A first row appended to a CRLF file with none.
    common::write_config(
        &dirs,
        &common::canonical_config(root.path()).replace('\n', "\r\n"),
    );
    let text = edit("grant", "ci", "alabsystems/aterm");
    assert!(!bare_lf(&text), "{text:?}");
    assert_eq!(granted(&dirs), [row("ci", &["alabsystems/aterm"])]);
}

#[test]
fn a_grant_never_makes_agh_toml_too_long_to_load() {
    let root = common::scratch("writers-cap");
    let dirs = Dirs::under(root.path());
    let canonical = common::canonical_config(root.path());
    let pad = config::CONFIG_MAX - canonical.len() - 20;
    let full = format!("{canonical}#{}\n", "x".repeat(pad - 2));
    common::write_config(&dirs, &full);
    let fake = FakeGithub::start();
    let (exit, stdout, stderr) = run(
        root.path(),
        &fake,
        (false, false),
        &["policy", "grant", "ci", "alabsystems/aterm"],
    );
    assert_eq!(exit, 1, "{stdout}{stderr}");
    assert_eq!(
        std::fs::read_to_string(dirs.config_file()).expect("agh.toml"),
        full
    );
    let (exit, _, stderr) = run(root.path(), &fake, (false, false), &["policy", "check"]);
    assert_eq!(exit, 0, "{stderr}");
}

#[test]
fn a_held_edit_lock_names_the_policy_verb_to_rerun() {
    let root = common::scratch("writers-lock");
    let dirs = Dirs::under(root.path());
    common::write_config(&dirs, &common::canonical_config(root.path()));
    let before = std::fs::read(dirs.config_file()).expect("agh.toml");
    let fake = FakeGithub::start();
    let _held = identity::provision::hold_auth_edit_for_tests(&dirs).expect("the auth lock");
    for verb in ["grant", "revoke"] {
        let (exit, stdout, stderr) = run(
            root.path(),
            &fake,
            (false, false),
            &["policy", verb, "ci", "alabsystems/aterm"],
        );
        assert_eq!(exit, 1, "{stdout}{stderr}");
        let error = common::error_json(&stderr);
        let next = error["next"].as_str().expect("a next step");
        assert!(next.contains(&format!("agh policy {verb}")), "{next}");
        assert_eq!(std::fs::read(dirs.config_file()).expect("agh.toml"), before);
    }
}

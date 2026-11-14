// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The host choice (plan §16): `gh` stays the default everywhere, `--host agh*` reads the
//! agh.toml the unit will read or refuses by name and raises its need, `lane install --host`
//! records the choice and is undone by a reinstall, and `hosts check` judges a builder's
//! agh.toml and sends nothing. Every home, agh.toml, token file and need is a scratch one.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use agh::config::Dirs;
use agh::needs::{self, Need};
use atpkg_publish::ctx::{Accounts, Ctx, HostChoice};
use atpkg_publish::error::Error;
use atpkg_publish::host::{HostRole, gh_serves};
use atpkg_publish::hosts_check::{self, HostCheck};
use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::install::InstallFlags;
use atpkg_publish::testkit::NOW;
use atpkg_publish::triple::Os;

use super::fixture::{
    DORMANT_LEDGER, Fx, MINTED_LEDGER, dev_config, git, public_repos, release_config,
    trust_checkout, trust_ledger, unit_dirs,
};
use super::install::{Box_, install, machine};
use crate::shared::{toml_path, write_config};

/// The runbook every provisioning text names.
const RUNBOOK: &str = "docs/RUNBOOK-atpkg-republish.md";

/// The staging owner every judging test uses, which `dev_config` writes.
const STAGING: &str = "fixture-dev";

/// The owners every judging test uses, in the dev build and the public build alike, and a
/// trust checkout whose origin names the staging owner's `trust` repository.
fn fixture_accounts(ctx: &mut Ctx) {
    ctx.accounts = Accounts {
        staging: STAGING.into(),
        public: "alabsystems".into(),
    };
    trust_checkout(ctx);
}

/// A lane unit's environment under `ctx`'s home: `XDG_CONFIG_HOME` is `~/.config`, as
/// `install.rs`'s `machine` pins it, and `XDG_STATE_HOME` the state root's parent.
fn unit_env(ctx: &Ctx) -> (PathBuf, PathBuf) {
    (
        ctx.home.join(".config"),
        ctx.state_dir
            .parent()
            .expect("the state root's parent")
            .to_path_buf(),
    )
}

/// `from_home_hosts` over `fx`'s home and its unit environment.
fn built(fx: &Fx, choice: HostChoice) -> atpkg_publish::error::Result<Ctx> {
    let (config, state) = unit_env(&fx.ctx);
    Ctx::from_home_hosts(&fx.ctx.home, Some(&config), Some(&state), NOW, choice)
}

/// The scratch token directory of `fx`, 0700.
fn tokens(fx: &Fx) -> PathBuf {
    let dir = fx.path("tokens");
    aterm_uds::private_file::create_private_dir(&dir).expect("the token directory");
    dir
}

/// Write `token` as `<name>.token` in `dir`, 0600.
fn write_token(dir: &Path, name: &str, token: &str) {
    aterm_uds::private_file::write_private(
        dir,
        &format!("{name}.token"),
        format!("{token}\n").as_bytes(),
    )
    .expect("the token file");
}

fn named<'a>(checks: &'a [HostCheck], name: &str) -> &'a HostCheck {
    checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} check in {checks:?}"))
}

fn all_hold(checks: &[HostCheck]) {
    assert!(checks.iter().all(|c| c.ok), "{checks:?}");
    assert_eq!(
        checks.iter().map(|c| c.name).collect::<Vec<_>>(),
        [
            "config",
            "staging_writer",
            "public_writer",
            "grants",
            "drafts",
            "trust_origin"
        ]
    );
}

/// The `ExecStart` words of the armed Linux service.
fn exec_words(unit: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(unit).expect("the armed unit");
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .unwrap_or_else(|| panic!("no ExecStart in {text}"));
    line.trim_matches('"')
        .split("\" \"")
        .map(str::to_string)
        .collect()
}

/// The `ProgramArguments` strings of the armed Alab plist.
fn program_arguments(unit: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(unit).expect("the armed unit");
    let (_, after) = text
        .split_once("<key>ProgramArguments</key>")
        .unwrap_or_else(|| panic!("no ProgramArguments in {text}"));
    let (array, _) = after
        .split_once("</array>")
        .unwrap_or_else(|| panic!("no </array> after ProgramArguments in {text}"));
    array
        .split("<string>")
        .skip(1)
        .map(|s| {
            s.split_once("</string>")
                .unwrap_or_else(|| panic!("an unclosed <string> in {array}"))
                .0
                .to_string()
        })
        .collect()
}

/// Every `lane.*` snapshot directory under the state directory.
fn snapshots(state_dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(state_dir) {
        Ok(entries) => entries
            .map(|e| e.expect("an entry").path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("lane."))
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn the_default_host_is_gh_everywhere() {
    assert_eq!(HostChoice::default(), HostChoice::Gh);
    assert_eq!(InstallFlags::default().host, HostChoice::Gh);

    let fx = Fx::new("host-default");
    let ctx = built(&fx, HostChoice::Gh).expect("the default needs no agh.toml");
    assert_eq!(ctx.host_choice(), HostChoice::Gh);
    assert!(
        !unit_dirs(&ctx).needs().exists(),
        "the default raised a need"
    );

    let b = machine("host-default-install", Os::Linux);
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let done = r.expect("installed");
    let words = exec_words(&done.armed[0]);
    assert_eq!(words.len(), 8, "{words:?}");
    assert_eq!(words[0], done.binary.display().to_string());
    assert_eq!(words[1..4], ["lane", "run", "linux"]);
    let from = std::fs::read_to_string(done.snapshot.join("INSTALLED-FROM")).expect("record");
    assert!(!from.contains("host "), "{from}");
}

#[test]
fn missing_config_builds_without_manufacturing_writers_or_needs() {
    for choice in [HostChoice::AghStaging, HostChoice::Agh] {
        let fx = Fx::new("host-missing");
        let dirs = unit_dirs(&fx.ctx);
        assert_eq!(built(&fx, choice).unwrap().host_choice(), choice);
        let checks = hosts_check::check(
            &fx.ctx,
            &dirs,
            choice,
            false,
            &hosts_check::alab_program_repos(),
        );
        assert!(named(&checks, "config").ok);
        assert!(!named(&checks, "staging_writer").ok);
        assert!(needs::list(&dirs).expect("the needs").is_empty());
    }
}

/// Explicit profiles still route when their file also opts into vault adoption.
#[test]
fn a_vault_join_keeps_its_explicit_profile_and_clears_the_config_need() {
    let mut fx = Fx::new("host-vault");
    fixture_accounts(&mut fx.ctx);
    let dirs = unit_dirs(&fx.ctx);
    let tokens = tokens(&fx);
    write_token(&tokens, "lanes", &agh_fake::token("dev"));
    write_config(
        &dirs,
        &format!(
            "vault = true\n\n[profiles.lanes]\ntoken_file = \"{}/lanes.token\"\ntier = \"dev\"\n\
             login = \"fixture-dev-bot\"\n\n\
             [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"lanes\"]\n\
             write = \"lanes\"\n",
            toml_path(&tokens)
        ),
    );

    let checks = hosts_check::check(
        &fx.ctx,
        &dirs,
        HostChoice::AghStaging,
        false,
        &hosts_check::alab_program_repos(),
    );
    all_hold(&checks);

    needs::raise(&dirs, &Need::config_invalid("L1 bogus", 0)).expect("the invalid need");
    assert!(built(&fx, HostChoice::Agh).is_ok());
    assert!(needs::list(&dirs).unwrap().is_empty());
}

#[test]
fn an_invalid_agh_toml_is_named_and_raises_its_need() {
    let fx = Fx::new("host-invalid");
    let dirs = unit_dirs(&fx.ctx);
    write_config(&dirs, "bogus = true\n");
    let e = built(&fx, HostChoice::Agh)
        .err()
        .expect("an invalid agh.toml refuses");
    assert_eq!(e.exit_code(), 3, "{e}");
    let shown = e.to_string();
    assert!(shown.contains("agh.toml"), "{shown}");
    assert!(shown.contains("bogus"), "{shown}");
    let raised = needs::list(&dirs).expect("the needs");
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert_eq!(raised[0].key, "agh.config.invalid");
    assert!(raised[0].why.contains("bogus"), "{raised:?}");
}

#[test]
fn a_valid_agh_toml_builds_and_clears_the_need() {
    let fx = Fx::new("host-valid");
    let dirs = unit_dirs(&fx.ctx);
    needs::raise(&dirs, &Need::config_invalid("L1 bogus", 0)).expect("the invalid need");
    let tokens = tokens(&fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&dirs, &dev_config(&tokens, STAGING));
    let ctx = built(&fx, HostChoice::Agh).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ctx.host_choice(), HostChoice::Agh);
    assert!(needs::list(&dirs).expect("the needs").is_empty());
}

#[test]
fn install_records_the_host_in_the_unit_and_installed_from() {
    let mut b: Box_ = machine("host-install", Os::Darwin);
    fixture_accounts(&mut b.fx.ctx);
    let tokens = tokens(&b.fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&unit_dirs(&b.fx.ctx), &dev_config(&tokens, STAGING));

    let (r, tx) = install(
        &b,
        LaneKind::Alab,
        InstallFlags {
            host: HostChoice::AghStaging,
            ..InstallFlags::default()
        },
    );
    let done = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert!(
        tx.text().contains(
            "hosts check --host agh-staging: every check holds (~/.config/aterm/agh/agh.toml)"
        ),
        "{}",
        tx.text()
    );
    let unit = std::fs::read_to_string(&done.armed[0]).expect("the armed unit");
    let at = |word: &str, from: usize| {
        unit[from..]
            .find(word)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("no {word} after {from} in {unit}"))
    };
    let binary = at(&done.binary.display().to_string(), 0);
    let host = at("<string>--host</string>", binary);
    let choice = at("<string>agh-staging</string>", host);
    at("<string>lane</string>", choice);
    let from = std::fs::read_to_string(done.snapshot.join("INSTALLED-FROM")).expect("record");
    assert!(from.ends_with("host agh-staging\n"), "{from}");

    // The rollback: a reinstall with no --host arms M1's eight words again, and clears the
    // config need an agh unit raised under the unit's directories, which no longer hold.
    let dirs = unit_dirs(&b.fx.ctx);
    needs::raise(&dirs, &Need::config_invalid("L1 bogus", 0)).expect("the invalid need");
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    let again = r.expect("reinstalled");
    let words = program_arguments(&again.armed[0]);
    assert_eq!(words.len(), 8, "{words:?}");
    assert_eq!(words[0], again.binary.display().to_string());
    assert_eq!(words[1..4], ["lane", "run", "alab"]);
    assert_eq!(words[4], "--sha256");
    assert_eq!(words[6], "--log");
    let from = std::fs::read_to_string(again.snapshot.join("INSTALLED-FROM")).expect("record");
    assert!(!from.contains("host "), "{from}");
    assert!(needs::list(&dirs).expect("the needs").is_empty());
}

/// `lane install`'s own `--host`, each spelling, and its refusals.
#[test]
fn install_flags_parse_the_host() {
    let parse = |args: &[&str]| {
        InstallFlags::parse(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>())
    };
    let host = |args: &[&str]| parse(args).unwrap_or_else(|e| panic!("{args:?}: {e}")).host;
    assert_eq!(host(&["--host", "agh"]), HostChoice::Agh);
    assert_eq!(host(&["--host=agh-staging"]), HostChoice::AghStaging);
    assert_eq!(host(&["--host", "gh"]), HostChoice::Gh);
    assert_eq!(host(&[]), HostChoice::Gh);
    let both = parse(&["--keyless", "--host=agh"]).expect("--keyless and --host");
    assert!(both.keyless, "{both:?}");
    assert_eq!(both.host, HostChoice::Agh);
    for (args, named) in [
        (&["--host"][..], "(missing)"),
        (&["--host", "github"][..], "github"),
        (&["--host="][..], "--host takes"),
        (&["--bogus"][..], "--bogus"),
    ] {
        match parse(args) {
            Err(e @ Error::Usage(_)) => assert!(e.to_string().contains(named), "{args:?}: {e}"),
            other => panic!("{args:?}: {other:?}"),
        }
    }
}

/// A mistyped verb and a local command both avoid identity-state writes.
#[test]
fn a_mistyped_verb_after_a_global_host_raises_nothing() {
    let fx = Fx::new("host-typo");
    let (config, state) = unit_env(&fx.ctx);
    let dirs = unit_dirs(&fx.ctx);
    let exe = std::fs::canonicalize(aterm_tempfile::bin_exe("atpkg-publish")).expect("the binary");
    let run = |args: &[&str]| {
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args(args)
            .env_clear()
            .env("HOME", &fx.ctx.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state);
        // A hang detector, not a budget: the child refuses at once.
        let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = aterm_uds::deadline::output_until(&mut cmd, hang).expect("the child runs");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, stderr) = run(&["--host", "agh", "statsu"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("unknown verb `statsu`"), "{stderr}");
    assert!(!dirs.needs().exists(), "a typo raised a need");

    let (code, stderr) = run(&["--host", "agh", "sign"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(needs::list(&dirs).expect("the needs").is_empty());
}

#[test]
fn an_install_whose_hosts_check_fails_arms_nothing() {
    let b = machine("host-install-refused", Os::Linux);
    std::fs::remove_file(unit_dirs(&b.fx.ctx).config.join("agh.toml")).expect("no agh.toml");
    let (r, _) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            host: HostChoice::Agh,
            ..InstallFlags::default()
        },
    );
    let e = r.expect_err("no agh.toml refuses the install");
    assert_eq!(e.exit_code(), 1, "{e}");
    let shown = e.to_string();
    assert!(
        shown.contains("hosts check failed (staging_writer, public_writer, grants)"),
        "{shown}"
    );
    assert!(
        shown.contains("atpkg-publish hosts check --host agh --unit-env"),
        "{shown}"
    );
    assert!(b.rec.calls().is_empty(), "{:?}", b.rec.calls());
    let state = &b.fx.ctx.state_dir;
    assert!(std::fs::symlink_metadata(state.join("lane")).is_err());
    assert!(snapshots(state).is_empty(), "{:?}", snapshots(state));
}

#[test]
fn hosts_check_reports_each_check() {
    let mut fx = Fx::new("hosts-check");
    fixture_accounts(&mut fx.ctx);
    let ctx = &fx.ctx;
    let dirs = unit_dirs(ctx);
    let repos = hosts_check::alab_program_repos();
    let check = |choice, keyless| hosts_check::check(ctx, &dirs, choice, keyless, &repos);

    // 1. No agh.toml.
    let checks = check(HostChoice::Agh, false);
    let config = named(&checks, "config");
    assert!(config.ok, "{config:?}");
    for name in ["staging_writer", "public_writer", "grants"] {
        let c = named(&checks, name);
        assert!(!c.ok, "{c:?}");
    }
    assert!(named(&checks, "drafts").ok);

    // 2. The dev profile alone, under agh-staging.
    let tokens = tokens(&fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&dirs, &dev_config(&tokens, STAGING));
    let checks = check(HostChoice::AghStaging, false);
    all_hold(&checks);
    assert_eq!(
        named(&checks, "public_writer").detail,
        "not needed under --host agh-staging"
    );
    assert!(
        named(&checks, "grants")
            .detail
            .contains("--host agh-staging"),
        "{checks:?}"
    );

    // 3. The same file under agh.
    let checks = check(HostChoice::Agh, false);
    let public = named(&checks, "public_writer");
    assert!(
        !public.ok && public.detail.contains("alabsystems"),
        "{public:?}"
    );
    let grants = named(&checks, "grants");
    assert!(
        !grants.ok && grants.detail.contains("alabsystems/aterm"),
        "{grants:?}"
    );

    // 4. A release profile writing alabsystems, its token file absent, and every grant.
    write_config(&dirs, &release_config(&tokens, STAGING, &public_repos()));
    let checks = check(HostChoice::Agh, false);
    let public = named(&checks, "public_writer");
    assert!(!public.ok, "{public:?}");
    assert!(
        public
            .detail
            .starts_with("release writes alabsystems, and its token is absent: ")
            && public.detail.contains("aterm secrets need gh:release"),
        "{public:?}"
    );
    assert!(named(&checks, "grants").ok, "{checks:?}");

    // 5. With its token file written.
    write_token(&tokens, "release", &agh_fake::token("release"));
    all_hold(&check(HostChoice::Agh, false));

    // 6. One repository's grant dropped.
    let narrowed: Vec<String> = public_repos()
        .into_iter()
        .filter(|r| r != "alabsystems/trust")
        .collect();
    write_config(&dirs, &release_config(&tokens, STAGING, &narrowed));
    let checks = check(HostChoice::Agh, false);
    let grants = named(&checks, "grants");
    assert!(!grants.ok, "{grants:?}");
    assert!(
        grants.detail.starts_with("missing: alabsystems/trust; "),
        "{grants:?}"
    );

    // 7. A fine-grained dev token nobody vouched for drafts.
    write_config(&dirs, &release_config(&tokens, STAGING, &public_repos()));
    write_token(&tokens, "dev", &agh_fake::fine_grained_token("dev"));
    let checks = check(HostChoice::Agh, false);
    let drafts = named(&checks, "drafts");
    assert!(!drafts.ok, "{drafts:?}");
    assert!(
        drafts.detail.contains("dev") && drafts.detail.contains("--drafts"),
        "{drafts:?}"
    );

    // 8. The dev profile alone, under agh, for a keyless lane.
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&dirs, &dev_config(&tokens, STAGING));
    let checks = check(HostChoice::Agh, true);
    all_hold(&checks);
    assert_eq!(
        named(&checks, "public_writer").detail,
        "not needed by a keyless lane"
    );
    assert!(
        !named(&checks, "grants").detail.contains("alabsystems/"),
        "{checks:?}"
    );

    // 9. A writer whose token is a variable: no lane unit carries one, and git's helper reads
    // only token files, so it fails before any variable is read.
    write_config(
        &dirs,
        "[profiles.dev]\nenv = \"AGH_FIXTURE_NEVER_SET\"\ntier = \"dev\"\n\
         login = \"fixture-dev-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
    );
    let checks = check(HostChoice::AghStaging, false);
    let staging = named(&checks, "staging_writer");
    assert!(!staging.ok, "{staging:?}");
    assert!(
        staging
            .detail
            .starts_with("dev writes fixture-dev, and its token is not in a token file"),
        "{staging:?}"
    );
    assert!(
        staging.detail.contains("aterm secrets need gh:dev") && staging.detail.contains(RUNBOOK),
        "{staging:?}"
    );
}

/// The forwarding verb as git's credential helper, end to end: agh's runner mounts the built
/// `atpkg-publish` with the word `agh`, as `HelperCommand::current()` names it in a lane, git
/// asks it for `fixture-dev/app` on agh-fake's loopback endpoint, and the read is answered as
/// the dev writer. NEGATIVE CONTROL: the same binary without the word `agh` answers nothing,
/// so the private read is refused and agh-fake never sees the dev login.
#[test]
fn the_forwarding_verb_answers_git_as_the_credential_helper() {
    use std::sync::Arc;

    use agh::git::{GitRun, HelperCommand};
    use agh::identity::MapEnv;
    use agh_fake::{FakeGithub, FakeOrigin, RepoSpec};

    const MAIN: &str = "0123456789abcdef0123456789abcdef01234567";

    let fx = Fx::new("host-helper");
    let fake = FakeGithub::start();
    let dirs = Dirs::under(&fx.path("agh"));
    let tokens = tokens(&fx);
    let token = agh_fake::token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    write_token(&tokens, "dev", &token);
    write_config(&dirs, &dev_config(&tokens, STAGING));
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    fake.set_branch("fixture-dev/app", "main", MAIN);
    let router = agh::policy::Router::from_validated(
        agh::config::load_for_host(&dirs)
            .unwrap_or_else(|e| panic!("agh.toml did not load: {}", e.why))
            .expect("agh.toml"),
    );
    let ports = fake.ports();
    let endpoints = agh::wire::Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn);
    let env = ["PATH", "USER", "LOGNAME"]
        .into_iter()
        .fold(MapEnv::new(), |env, name| match std::env::var(name) {
            Ok(value) => env.with(name, &value),
            Err(_) => env,
        });
    // The helper judges by the host's clock, so the client's starts there too.
    let start = agh::fx::Fx::live()
        .wall_secs()
        .unwrap_or_else(|e| panic!("the host's clock: {}", e.why));
    let exe = std::fs::canonicalize(aterm_tempfile::bin_exe("atpkg-publish")).expect("the binary");
    let slug = agh::Slug::parse("fixture-dev/app").expect("a slug");
    let read = |args: Vec<String>| {
        agh::GithubBuilder::new(router.clone())
            .transport(Arc::new(agh::wire::http::HttpTransport::direct(Arc::new(
                env.clone(),
            ))))
            .endpoints(endpoints.clone())
            .env(Arc::new(env.clone()))
            .fx(Arc::new(agh::fx::Fx::simulated(start)))
            .caller("atpkg-publish")
            .retry(agh::retry::RetryPolicy::NONE)
            .journal(dirs.clone())
            .git_helper_for_tests(HelperCommand {
                exe: exe.clone(),
                args,
            })
            .build()
            .run_git(&GitRun::ls_remote(&slug, None))
    };
    let as_dev = |fake: &FakeGithub| {
        fake.seen().iter().any(|s| {
            s.origin == FakeOrigin::Web
                && s.path_and_query.starts_with("/fixture-dev/app.git/")
                && s.as_login.as_deref() == Some("fixture-dev-bot")
        })
    };

    let out = read(vec!["agh".to_string()])
        .unwrap_or_else(|e| panic!("the read through the forwarding verb: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert!(out.stdout.contains(MAIN), "{}", out.stdout);
    assert!(as_dev(&fake), "{:?}", fake.seen());

    let fake_seen = fake.seen().len();
    assert!(
        read(Vec::new()).is_err(),
        "the read without the forwarding word was answered"
    );
    assert!(
        !fake.seen()[fake_seen..]
            .iter()
            .any(|s| s.as_login.as_deref() == Some("fixture-dev-bot")),
        "{:?}",
        fake.seen()
    );
}

#[test]
fn a_keyless_install_under_agh_needs_no_public_writer() {
    let mut b: Box_ = machine("host-keyless", Os::Linux);
    fixture_accounts(&mut b.fx.ctx);
    let tokens = tokens(&b.fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&unit_dirs(&b.fx.ctx), &dev_config(&tokens, STAGING));
    std::fs::remove_file(&b.fx.ctx.identity.key).expect("rm key");
    b.fx.publish_head(1, &[("git", 7)]);

    let (r, tx) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            keyless: true,
            host: HostChoice::Agh,
            ..InstallFlags::default()
        },
    );
    let done = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    let words = exec_words(&done.armed[0]);
    assert!(
        words.windows(2).any(|w| w[0] == "--host" && w[1] == "agh"),
        "{words:?}"
    );

    std::fs::write(&b.fx.ctx.identity.key, &b.fx.keys.machine_key).expect("the machine key");
    // A keyed builder's run signs and writes the public owner whether or not its install said
    // --keyless (the run reads the key, not the flag), so both installs need the public writer.
    for keyless in [false, true] {
        let (r, _) = install(
            &b,
            LaneKind::Linux,
            InstallFlags {
                keyless,
                host: HostChoice::Agh,
                ..InstallFlags::default()
            },
        );
        let shown = r
            .expect_err("a signing install needs the public writer")
            .to_string();
        assert!(shown.contains("public_writer"), "{keyless}: {shown}");
        assert!(
            shown.ends_with("--unit-env --lane linux; nothing installed"),
            "{keyless}: {shown}"
        );
        assert!(!shown.contains(" --keyless"), "{keyless}: {shown}");
    }
}

#[test]
fn the_tip_lanes_check_asks_no_grant_on_a_program_repository_it_cannot_know_is_open() {
    let mut fx = Fx::new("hosts-check-tip");
    fixture_accounts(&mut fx.ctx);
    let ctx = &fx.ctx;
    let dirs = unit_dirs(ctx);
    let tokens = tokens(&fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_token(&tokens, "release", &agh_fake::token("release"));
    // The release writer is granted the index repository and one program's, and no other.
    write_config(
        &dirs,
        &release_config(
            &tokens,
            STAGING,
            &[
                "alabsystems/aterm".to_string(),
                "alabsystems/ay".to_string(),
            ],
        ),
    );
    // The alab lane's check names every ungranted repository it tracks as missing, the closed
    // tiers among them, with a remedy that would grant them.
    let whole = hosts_check::check(
        ctx,
        &dirs,
        HostChoice::Agh,
        false,
        &hosts_check::alab_program_repos(),
    );
    let grants = named(&whole, "grants");
    assert!(
        !grants.ok && grants.detail.contains("alabsystems/claude-code-alab"),
        "{grants:?}"
    );
    // The tip lane's check holds: the index repository's grant is the one it needs to
    // publish at all.
    let tip = hosts_check::check_tip(ctx, &dirs, HostChoice::Agh);
    assert!(tip.iter().all(|c| c.ok), "{tip:?}");
    let note = named(&tip, "program_grants");
    assert!(note.ok, "{note:?}");
    // It names what has no grant without asking for one: which repositories are open is the
    // lifecycle registry's to say, and the granted one is not listed.
    assert!(
        note.detail.contains("alabsystems/nn")
            && !note.detail.contains("alabsystems/ay,")
            && !note.detail.contains("alabsystems/ay."),
        "{note:?}"
    );
    assert!(
        note.detail
            .contains("only when the lifecycle registry holds it active")
            && !note.detail.contains("agh policy grant"),
        "{note:?}"
    );
    // A program the queue never carries (a coherence-group member, a program built from
    // another product's repository) is not the tip lane's to write at all.
    let repos = hosts_check::tip_program_repos();
    assert!(repos.iter().any(|r| r == "ay") && !repos.iter().any(|r| r == "trust"));
    // Without the index repository's grant nothing can publish: that still refuses.
    write_config(
        &dirs,
        &release_config(&tokens, STAGING, &["alabsystems/ay".to_string()]),
    );
    let tip = hosts_check::check_tip(ctx, &dirs, HostChoice::Agh);
    let grants = named(&tip, "grants");
    assert!(
        !grants.ok && grants.detail.starts_with("missing: alabsystems/aterm;"),
        "{grants:?}"
    );
}

/// Every file and identity here is owned fixture data; no live token or policy is read.
fn linux_selection_box(label: &str, grants: &[&str]) -> Box_ {
    let mut b = machine(label, Os::Linux);
    fixture_accounts(&mut b.fx.ctx);
    let tokens = tokens(&b.fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_token(&tokens, "release", &agh_fake::token("release"));
    write_config(
        &unit_dirs(&b.fx.ctx),
        &release_config(
            &tokens,
            STAGING,
            &grants.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        ),
    );
    b
}

#[test]
fn linux_install_grants_follow_signed_pins_not_unselected_table_entries() {
    let b = linux_selection_box("linux-selected-install", &["alabsystems/aterm"]);
    b.fx.publish_head(1, &[("git", 7)]);
    let (result, tx) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            host: HostChoice::Agh,
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    let done = result.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert!(done.binary.is_file());
    assert!(done.armed.is_empty());
    assert!(tx.text().contains("every check holds"));
}

#[test]
fn linux_preflight_requires_the_selected_repo_and_keeps_the_index_grant() {
    let b = linux_selection_box("linux-selected-grants", &["alabsystems/aterm"]);
    let text = b.fx.keys.index_toml(
        1,
        &[
            ("git", "selected-git-assets"),
            ("claude-code-alab", "closed-unselected"),
        ],
        &[("git", 7)],
    );
    b.fx.publish_index_text(1, &text);
    let dirs = unit_dirs(&b.fx.ctx);
    let checks =
        hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false).expect("signed head");
    assert!(!named(&checks, "grants").ok);
    assert!(
        named(&checks, "grants")
            .detail
            .contains("alabsystems/selected-git-assets")
    );
    assert!(
        !named(&checks, "grants")
            .detail
            .contains("closed-unselected")
    );
    let tokens = tokens(&b.fx);
    write_config(
        &dirs,
        &release_config(
            &tokens,
            STAGING,
            &[
                "alabsystems/aterm".into(),
                "alabsystems/selected-git-assets".into(),
            ],
        ),
    );
    all_hold(
        &hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false)
            .expect("selected grant"),
    );
    write_config(
        &dirs,
        &release_config(
            &tokens,
            STAGING,
            &["alabsystems/selected-git-assets".into()],
        ),
    );
    let checks = hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false)
        .expect("index grant refusal");
    assert!(!named(&checks, "grants").ok);
    assert!(
        named(&checks, "grants")
            .detail
            .contains("alabsystems/aterm")
    );
}

#[test]
fn linux_preflight_refetches_and_authenticates_the_head() {
    let b = linux_selection_box("linux-selected-fresh-head", &["alabsystems/aterm"]);
    b.fx.publish_head(1, &[("git", 7)]);
    let dirs = unit_dirs(&b.fx.ctx);
    all_hold(
        &hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false)
            .expect("first signed head"),
    );
    b.fx.publish_head(2, &[("git", 7), ("ay", 8)]);
    let checks = hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false)
        .expect("new signed head");
    assert!(named(&checks, "grants").detail.contains("alabsystems/ay"));
    b.fx.host.put_asset(
        &b.fx.ctx.index_slug(),
        "atpkg-index-2",
        "index.toml.sig",
        vec![0; 64],
    );
    assert!(hosts_check::check_linux(&b.fx.ctx, &dirs, HostChoice::Agh, false).is_err());
}

#[test]
fn linux_preflight_resolves_linux_overlays_without_darwin_grants() {
    let b = linux_selection_box("linux-selected-overlay", &["alabsystems/aterm"]);
    let mut text = b.fx.keys.index_toml(
        1,
        &[("git", "aterm"), ("ay", "ay"), ("nn", "nn")],
        &[("git", 7)],
    );
    text.push_str("\n[channels.pin_by_target.aarch64-unknown-linux-gnu]\nay = 8\n[channels.pin_by_target.aarch64-apple-darwin]\nnn = 9\n");
    b.fx.publish_index_text(1, &text);
    let checks = hosts_check::check_linux(&b.fx.ctx, &unit_dirs(&b.fx.ctx), HostChoice::Agh, false)
        .expect("target-resolved head");
    let missing = &named(&checks, "grants").detail;
    assert!(missing.contains("alabsystems/ay"), "{missing}");
    assert!(!missing.contains("alabsystems/nn"), "{missing}");
}

#[test]
fn the_hosts_verb_refuses_what_it_cannot_judge() {
    let fx = Fx::new("hosts-usage");
    for args in [
        &["check", "--host", "gh"][..],
        &["status"][..],
        &[][..],
        &["check", "--bogus"][..],
    ] {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let e = hosts_check::run(&fx.ctx, &args).expect_err("a usage error");
        assert!(matches!(e, Error::Usage(_)), "{args:?}: {e}");
        assert_eq!(e.exit_code(), 2, "{args:?}: {e}");
    }
}

#[test]
fn one_owner_for_both_accounts_is_judged_by_role() {
    let fx = Fx::new("host-one-owner");
    for (choice, serves) in [
        (HostChoice::Gh, (true, true)),
        (HostChoice::AghStaging, (false, true)),
        (HostChoice::Agh, (false, false)),
    ] {
        let mut ctx = fx.ctx.clone().with_host_choice(choice);
        ctx.accounts = Accounts {
            staging: "alabsystems".into(),
            public: "alabsystems".into(),
        };
        let host = ctx
            .hosts
            .for_owner(&ctx.accounts, "alabsystems")
            .expect("a host serves alabsystems");
        assert!(std::ptr::eq(host, &ctx.hosts.staging), "{choice:?}");
        assert_eq!(
            (
                gh_serves(&ctx, HostRole::Staging),
                gh_serves(&ctx, HostRole::Channel)
            ),
            serves,
            "{choice:?}"
        );
    }
}

/// The alab lane's check asks a grant on exactly the repositories that lane preflights: the
/// index and each lane-tracked program's. A repository only package or bottle rows name enters
/// the channel through an explicit pin, never through this lane, so it is never asked for.
/// NEGATIVE CONTROL: `alab_program_repos` over the unfiltered table names `codex-alab` too.
#[test]
fn the_alab_check_names_no_package_or_bottle_repository() {
    use atpkg_publish::programs::PROGRAMS;
    let mut tracked: Vec<&str> = Vec::new();
    for def in PROGRAMS.iter().filter(|d| d.lane_tracked()) {
        if !tracked.contains(&def.repo) {
            tracked.push(def.repo);
        }
    }
    let untracked: Vec<&str> = PROGRAMS
        .iter()
        .map(|d| d.repo)
        .filter(|repo| !tracked.contains(repo))
        .collect();
    assert!(
        !untracked.is_empty(),
        "no repository is named only by a package or bottle row, so this proves nothing"
    );
    let repos = hosts_check::alab_program_repos();
    assert_eq!(
        repos, tracked,
        "the lane-tracked repositories, in table order"
    );

    let mut fx = Fx::new("hosts-check-alab-repos");
    fixture_accounts(&mut fx.ctx);
    let dirs = unit_dirs(&fx.ctx);
    let tokens = tokens(&fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_token(&tokens, "release", &agh_fake::token("release"));
    write_config(
        &dirs,
        &release_config(&tokens, STAGING, &["alabsystems/aterm".to_string()]),
    );
    let checks = hosts_check::check(&fx.ctx, &dirs, HostChoice::Agh, false, &repos);
    let grants = named(&checks, "grants");
    assert!(!grants.ok, "{grants:?}");
    let missing: Vec<&str> = grants
        .detail
        .strip_prefix("missing: ")
        .and_then(|rest| rest.split_once("; "))
        .map(|(list, _)| list.split(", ").collect())
        .unwrap_or_else(|| panic!("no missing list in {grants:?}"));
    let wanted: Vec<String> = tracked
        .iter()
        .filter(|repo| **repo != "aterm")
        .map(|repo| format!("alabsystems/{repo}"))
        .collect();
    assert_eq!(missing, wanted, "{grants:?}");
}

/// A trust checkout whose origin names another repository fails `trust_origin`, and no other
/// row, exactly where that origin decides the lineage switch: its origin/main carries a dormant
/// seed ledger, so the switch reads the trust repository's main through that origin. Where the
/// checkout alone settles the switch (no checkout, no origin/main, a minted ledger there), the
/// row holds and names the origin that would not serve. The row names the origin it found, its
/// credential masked, and the repository it wants, whatever the lane; it is judged without an
/// agh.toml. NEGATIVE CONTROLS: without the row, `named` finds no `trust_origin` check; a row
/// that fails on every mismatched origin fails the settled halves.
#[test]
fn a_mismatched_trust_origin_fails_trust_origin_where_it_decides_the_switch() {
    let mut fx = Fx::new("hosts-check-trust-origin");
    fixture_accounts(&mut fx.ctx);
    let ctx = &fx.ctx;
    let dirs = unit_dirs(ctx);
    let tokens = tokens(&fx);
    write_token(&tokens, "dev", &agh_fake::token("dev"));
    write_config(&dirs, &dev_config(&tokens, STAGING));
    let check = |ctx: &Ctx| {
        hosts_check::check(
            ctx,
            &dirs,
            HostChoice::AghStaging,
            false,
            &hosts_check::alab_program_repos(),
        )
    };
    let failed = |checks: &[HostCheck]| -> Vec<&'static str> {
        checks.iter().filter(|c| !c.ok).map(|c| c.name).collect()
    };
    let held = check(ctx);
    all_hold(&held);
    assert_eq!(
        named(&held, "trust_origin").detail,
        "$HOME/trust's origin names fixture-dev/trust"
    );

    git(
        &ctx.trust_repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://someone:hunter2@github.com/someone/trust.git",
        ],
    );
    let names_it = |row: &str| {
        assert!(
            row.contains("does not name fixture-dev/trust")
                && row.contains("(it names https://***@github.com/someone/trust.git)")
                && !row.contains("hunter2"),
            "{row}"
        );
    };
    // Settled by the checkout alone: no origin/main reads unknown, a minted ledger there live.
    let checks = check(ctx);
    all_hold(&checks);
    let row = &named(&checks, "trust_origin").detail;
    names_it(row);
    assert!(
        row.contains("decides nothing")
            && row.contains("reads the lineage switch unknown")
            && row.contains("has no origin/main"),
        "{row}"
    );
    let mut absent = ctx.clone();
    absent.trust_repo = ctx.home.join("no-trust");
    let checks = check(&absent);
    all_hold(&checks);
    let row = &named(&checks, "trust_origin").detail;
    assert!(
        row.contains("reads the lineage switch unknown")
            && row.contains("there is no trust checkout at ~/no-trust"),
        "{row}"
    );
    trust_ledger(ctx, MINTED_LEDGER);
    let checks = check(ctx);
    all_hold(&checks);
    let row = &named(&checks, "trust_origin").detail;
    names_it(row);
    assert!(
        row.contains("reads the lineage switch live: its origin/main carries a minted re-pin"),
        "{row}"
    );

    // Deferred to the trust repository's main: the origin decides, and the row fails.
    trust_ledger(ctx, DORMANT_LEDGER);
    let checks = check(ctx);
    assert_eq!(failed(&checks), ["trust_origin"], "{checks:?}");
    let row = &named(&checks, "trust_origin").detail;
    names_it(row);
    assert!(row.contains("carries a dormant ledger"), "{row}");
    let tip = hosts_check::check_tip(ctx, &dirs, HostChoice::AghStaging);
    assert_eq!(failed(&tip), ["trust_origin"], "{tip:?}");

    write_config(&dirs, "bogus = true\n");
    let checks = check(ctx);
    assert!(!named(&checks, "config").ok, "{checks:?}");
    assert_eq!(named(&checks, "trust_origin").detail, *row);
    write_config(&dirs, &dev_config(&tokens, STAGING));

    // Removing the remote drops its tracking refs too; the dormant origin/main is laid again.
    git(&ctx.trust_repo, &["remote", "remove", "origin"]);
    git(
        &ctx.trust_repo,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let checks = check(ctx);
    assert_eq!(failed(&checks), ["trust_origin"], "{checks:?}");
    let row = &named(&checks, "trust_origin").detail;
    assert!(
        row.contains("has no origin remote; it must name fixture-dev/trust"),
        "{row}"
    );

    // The Linux check reads the head past a failed trust origin: the grant its pin selects
    // is still judged.
    let b = linux_selection_box("linux-trust-origin", &["alabsystems/aterm"]);
    let text =
        b.fx.keys
            .index_toml(1, &[("git", "selected-git-assets")], &[("git", 7)]);
    b.fx.publish_index_text(1, &text);
    git(
        &b.fx.ctx.trust_repo,
        &["remote", "set-url", "origin", "/elsewhere/trust"],
    );
    trust_ledger(&b.fx.ctx, DORMANT_LEDGER);
    let checks = hosts_check::check_linux(&b.fx.ctx, &unit_dirs(&b.fx.ctx), HostChoice::Agh, false)
        .expect("signed head");
    assert_eq!(failed(&checks), ["grants", "trust_origin"], "{checks:?}");
    assert!(
        named(&checks, "grants")
            .detail
            .contains("alabsystems/selected-git-assets"),
        "{checks:?}"
    );
}

/// A Linux check whose signed head cannot be read after only `trust_origin` failed refuses
/// with the head's own error, and that error names the failed row too: neither fault hides the
/// other, in `hosts check --lane linux` or in `lane install --lane linux`. With the origin
/// right, the same unreadable head names no trust row. NEGATIVE CONTROL: the head's error
/// returned alone, as before, fails the first half.
#[test]
fn an_unreadable_head_still_names_a_failed_trust_origin() {
    let b = linux_selection_box("linux-trust-origin-head", &["alabsystems/aterm"]);
    let public = b.fx.ctx.accounts.public.clone();
    b.fx.host.set_unreachable(&public, true);
    let refused = |b: &Box_| {
        hosts_check::check_linux(&b.fx.ctx, &unit_dirs(&b.fx.ctx), HostChoice::Agh, false)
            .expect_err("the head cannot be read")
    };
    let control = refused(&b);
    assert_eq!(control.exit_code(), 3, "{control}");
    let head = control.to_string();
    assert!(!head.contains("trust_origin"), "{head}");

    git(
        &b.fx.ctx.trust_repo,
        &["remote", "set-url", "origin", "/elsewhere/trust"],
    );
    trust_ledger(&b.fx.ctx, DORMANT_LEDGER);
    let both = refused(&b);
    assert_eq!(both.exit_code(), 3, "the head's kind is kept: {both}");
    let text = both.to_string();
    assert!(
        text.starts_with(&head)
            && text.contains("; trust_origin failed too: $HOME/trust cannot serve as")
            && text.contains("does not name fixture-dev/trust (it names /elsewhere/trust)"),
        "{text}"
    );
}

/// The install fixture's own routing (`install::machine`) is ready for every lane: `hosts check
/// --host agh` holds for alab, tip and Linux over it as it stands, so an install that runs the
/// check finds nothing to provision and the head's pins select no ungranted repository.
#[test]
fn the_install_fixture_holds_every_lane_s_hosts_check() {
    let b = machine("host-fixture-ready", Os::Linux);
    let ctx = &b.fx.ctx;
    let dirs = unit_dirs(ctx);
    all_hold(&hosts_check::check(
        ctx,
        &dirs,
        HostChoice::Agh,
        false,
        &hosts_check::alab_program_repos(),
    ));
    let tip = hosts_check::check_tip(ctx, &dirs, HostChoice::Agh);
    assert!(tip.iter().all(|c| c.ok), "{tip:?}");
    assert!(
        named(&tip, "program_grants")
            .detail
            .contains("may write every program repository the tip lane can queue"),
        "{tip:?}"
    );
    all_hold(
        &hosts_check::check_linux(ctx, &dirs, HostChoice::Agh, false).expect("the signed head"),
    );
}

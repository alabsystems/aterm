// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The isolated git runner and its `git` door: local bare repositories in a scratch
//! directory, the host's own `git`, and agh-fake on 127.0.0.1 (plan §11).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agh::config::{self, Dirs};
use agh::fx::Fx;
use agh::fx::child::ChildSpec;
use agh::git::{GitRun, HelperCommand};
use agh::identity::MapEnv;
use agh::policy::{Named, RefusalCode, Router};
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh::{Github, GithubBuilder, Kind, SideEffect, Sub};
use agh_fake::{FakeGithub, FakeOrigin, RepoSpec};

use crate::common;

/// A host `git` command in `dir`, under a cleared environment holding only what makes it
/// hermetic: no system or global config, a fixed author, committer and date.
pub(crate) fn host_git(dir: &Path, args: &[&str]) -> String {
    let git = host_git_path();
    let mut cmd = aterm_uds::spawnfd::command(&git);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000");
    let until = std::time::Instant::now() + common::HANG;
    let out = aterm_uds::deadline::output_until(&mut cmd, until)
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e:?}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// The host's `git`: the first absolute `PATH` entry holding one, as the runner finds it.
fn host_git_path() -> PathBuf {
    let path = std::env::var_os("PATH").expect("the host has a PATH");
    std::env::split_paths(&path)
        .filter(|entry| entry.is_absolute())
        .map(|entry| entry.join("git"))
        .find(|git| git.is_file())
        .expect("git is on the host's PATH")
}

/// A local bare repository `{root}/{name}.git` holding one commit on `main`, built with the
/// host's `git`: `git init` of a work tree, one `commit --allow-empty`, then `clone --bare`.
pub(crate) fn bare_repo(root: &Path, name: &str) -> PathBuf {
    let work = root.join(format!("{name}-work"));
    std::fs::create_dir_all(&work).expect("the work tree");
    host_git(&work, &["init", "--quiet", "-b", "main"]);
    host_git(&work, &["commit", "--quiet", "--allow-empty", "-m", "one"]);
    let bare = root.join(format!("{name}.git"));
    let bare_text = bare.to_string_lossy().into_owned();
    host_git(
        root,
        &[
            "clone",
            "--quiet",
            "--bare",
            &work.to_string_lossy(),
            &bare_text,
        ],
    );
    bare
}

/// `MapEnv::new()` with each of `PATH`, `USER` and `LOGNAME` that `std::env::var` reads on
/// the host. Nothing is set.
pub(crate) fn git_env() -> MapEnv {
    ["PATH", "USER", "LOGNAME"]
        .into_iter()
        .fold(MapEnv::new(), |env, name| match std::env::var(name) {
            Ok(value) => env.with(name, &value),
            Err(_) => env,
        })
}

/// `Fx::live().wall_secs()`: the host's Unix seconds, read once.
pub(crate) fn live_start() -> u64 {
    Fx::live()
        .wall_secs()
        .unwrap_or_else(|e| panic!("the host's clock: {}", e.why))
}

/// The client a test of the runner builds: simulated time from `start`, `env` as its
/// environment, `dirs` as its journal, and the agh binary as the helper.
pub(crate) fn git_builder(
    router: Router,
    endpoints: Endpoints,
    dirs: &Dirs,
    env: MapEnv,
    start: u64,
) -> GithubBuilder {
    GithubBuilder::new(router)
        .transport(Arc::new(HttpTransport::direct(Arc::new(env.clone()))))
        .endpoints(endpoints)
        .env(Arc::new(env))
        .fx(Arc::new(Fx::simulated(start)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .journal(dirs.clone())
        .git_helper_for_tests(common::helper())
}

/// A scratch root with the canonical agh.toml and token files, its router, and agh-fake.
struct World {
    root: aterm_tempfile::TempDir,
    dirs: Dirs,
    router: Router,
    fake: FakeGithub,
}

fn world(label: &str, text: impl Fn(&Path) -> String) -> World {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    let text = text(root.path());
    common::write_config(&dirs, &text);
    common::canonical_tokens(root.path(), &fake, true, true);
    let router = common::router(&text, root.path());
    World {
        root,
        dirs,
        router,
        fake,
    }
}

impl World {
    fn canonical(label: &str) -> World {
        world(label, common::canonical_config)
    }

    /// The canonical world whose agh.toml grants `fixture-caller` `alabsystems/aterm` and
    /// `alabsystems/secret`, its router read back from the file by `config::load`, so
    /// `Router::grant` counts the `[[writers]]` row and a test holds a release-tier literal.
    fn release(label: &str) -> World {
        let mut world = world(label, |root| {
            format!(
                "{}\n[[writers]]\ncaller = \"fixture-caller\"\n\
                 repos = [\"alabsystems/aterm\", \"alabsystems/secret\"]\n",
                common::canonical_config(root)
            )
        });
        let validated = config::load(&world.dirs)
            .unwrap_or_else(|e| panic!("agh.toml did not load: {}", e.why))
            .unwrap_or_else(|| panic!("no agh.toml at {}", world.dirs.config.display()));
        world.router = Router::from_validated(validated);
        world
    }

    fn client(&self, env: MapEnv) -> GithubBuilder {
        git_builder(
            self.router.clone(),
            common::endpoints(&self.fake),
            &self.dirs,
            env,
            live_start(),
        )
    }

    fn path(&self) -> &Path {
        self.root.path()
    }
}

/// The files in `dir`, none when it does not exist.
fn files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn env_of<'a>(spec: &'a ChildSpec, name: &str) -> Option<&'a OsString> {
    spec.env
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

#[test]
fn the_helper_command_is_quoted() {
    let plain = HelperCommand {
        exe: PathBuf::from("/a b/agh"),
        args: Vec::new(),
    };
    assert_eq!(plain.config_value(), "!\"/a b/agh\" auth git-credential");
    let aterm = HelperCommand {
        exe: PathBuf::from("/x/aterm"),
        args: vec!["agh".to_string()],
    };
    assert_eq!(
        aterm.config_value(),
        "!\"/x/aterm\" \"agh\" auth git-credential"
    );
    let dollar = HelperCommand {
        exe: PathBuf::from("/x/$y/agh"),
        args: Vec::new(),
    };
    assert_eq!(
        dollar.config_value(),
        "!\"/x/\\$y/agh\" auth git-credential"
    );
    let quotes = HelperCommand {
        exe: PathBuf::from("/x/\"`\\/agh"),
        args: Vec::new(),
    };
    assert_eq!(
        quotes.config_value(),
        "!\"/x/\\\"\\`\\\\/agh\" auth git-credential"
    );
}

#[test]
fn a_clone_ignores_the_parent_s_git_environment() {
    let world = World::canonical("git-clone-env");
    let bare = bare_repo(world.path(), "app");
    let home = world.path().join("parent-home");
    std::fs::create_dir_all(&home).expect("the parent's home");
    std::fs::write(
        home.join(".gitconfig"),
        "[url \"file:///nowhere\"]\n\tinsteadOf = file://\n",
    )
    .expect("the parent's .gitconfig");
    let env = git_env()
        .with("GIT_DIR", "/nonexistent")
        .with("GIT_SSH_COMMAND", "false")
        .with("GIT_CONFIG_PARAMETERS", "'url.x.insteadOf=y'")
        .with("HOME", &home.to_string_lossy());
    let gh = world
        .client(env)
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let dest = world.path().join("dest");
    let out = gh
        .run_git(&GitRun::new_clone(&common::slug("fixture-dev/app"), &dest))
        .unwrap_or_else(|e| panic!("the clone: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert_eq!(out.grant, None);
    let head = host_git(&bare, &["rev-parse", "main"]);
    assert_eq!(host_git(&dest, &["rev-parse", "HEAD"]), head);
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn a_fetch_refuses_a_rewriting_config() {
    let world = World::canonical("git-fetch-screen");
    let bare = bare_repo(world.path(), "app");
    let other = bare_repo(world.path(), "other");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let clone = world.path().join("clone");
    gh.run_git(&GitRun::new_clone(&slug, &clone))
        .unwrap_or_else(|e| panic!("the clone: {}", e.why));
    // A second commit on the remote, so a fetch that ran would move the clone's refs.
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let first = host_git(&bare, &["rev-parse", "main"]);
    let second = host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    host_git(&bare, &["update-ref", "refs/heads/main", &second]);
    let refs = || host_git(&clone, &["for-each-ref"]);
    let before = refs();
    let url = format!(
        "file://{}",
        std::path::absolute(&bare)
            .expect("the remote's path")
            .display()
    );
    let other_url = format!("file://{}", other.display());
    let rows: Vec<(String, String, String)> = vec![
        ("url.https://x/.insteadOf", "y", "url.https://x/.insteadof"),
        (
            "remote.origin.pushurl",
            "file:///nowhere",
            "remote.origin.pushurl",
        ),
        ("core.hooksPath", "/nowhere", "core.hookspath"),
        (
            "core.alternateRefsCommand",
            "false",
            "core.alternaterefscommand",
        ),
        (
            "remote.origin.uploadpack",
            "false",
            "remote.origin.uploadpack",
        ),
        ("credential.helper", "false", "credential.helper"),
        ("include.path", "/nowhere", "include.path"),
        ("protocol.ext.allow", "always", "protocol.ext.allow"),
        (
            "extensions.worktreeConfig",
            "true",
            "extensions.worktreeconfig",
        ),
        ("core.gitProxy", "false", "core.gitproxy"),
        ("fetch.bundleURI", "file:///nowhere", "fetch.bundleuri"),
        ("bundle.version", "1", "bundle.version"),
        (
            "extensions.partialClone",
            "origin",
            "extensions.partialclone",
        ),
        ("remote.origin.promisor", "true", "remote.origin.promisor"),
        (
            "remote.origin.partialCloneFilter",
            "blob:none",
            "remote.origin.partialclonefilter",
        ),
        ("hook.x.command", "true", "hook.x.command"),
        ("hook.x.event", "reference-transaction", "hook.x.event"),
    ]
    .into_iter()
    .map(|(k, v, e)| (k.to_string(), v.to_string(), e.to_string()))
    .chain(std::iter::once((
        format!("remote.{url}.url"),
        other_url,
        format!("remote.{url}.url"),
    )))
    .collect();
    for (key, value, listed) in &rows {
        host_git(&clone, &["config", "--local", key, value]);
        let err = gh
            .run_git(&GitRun::fetch(&slug, &clone))
            .expect_err("a rewriting config is refused")
            .into_data();
        assert_eq!(err.kind, Kind::Refused, "{key}: {}", err.why);
        assert_eq!(
            err.sub,
            Sub::GitConfig {
                key: listed.clone()
            },
            "{key}"
        );
        assert_eq!(refs(), before, "{key} moved the clone's refs");
        host_git(&clone, &["config", "--local", "--unset", key]);
    }
    // A config-based hook: `core.hooksPath=/dev/null` does not stop it, so the screen must.
    // The command is a shell redirection, which needs no program on the hook's PATH.
    let marker = world.path().join("hook-ran");
    let command = format!(": > '{}'", marker.display());
    host_git(
        &clone,
        &["config", "--local", "hook.x.event", "reference-transaction"],
    );
    host_git(&clone, &["config", "--local", "hook.x.command", &command]);
    let err = gh
        .run_git(&GitRun::fetch(&slug, &clone))
        .expect_err("a config-based hook is refused")
        .into_data();
    assert_eq!(err.kind, Kind::Refused, "{}", err.why);
    assert_eq!(
        err.sub,
        Sub::GitConfig {
            key: "hook.x.event".to_string()
        }
    );
    assert_eq!(refs(), before, "the hooked fetch moved the clone's refs");
    assert!(!marker.exists(), "the clone's hook ran inside the run");
    // The control: the host's own git, with no screen, runs that hook on a ref update when it
    // knows config-based hooks, which its `help --config` key list then names. A git that
    // predates them (Apple Git 2.50) ignores the keys, so the hook stays unrun; the screen
    // refuses them on every git.
    let config_hooks = host_git(&clone, &["help", "--config"])
        .lines()
        .any(|key| key.starts_with("hook."));
    let head = host_git(&clone, &["rev-parse", "HEAD"]);
    host_git(&clone, &["update-ref", "refs/agh-hook-probe", &head]);
    assert_eq!(
        marker.exists(),
        config_hooks,
        "the control's hook ran exactly when the host's git knows config-based hooks"
    );
    host_git(&clone, &["config", "--local", "--remove-section", "hook.x"]);
    host_git(&clone, &["update-ref", "-d", "refs/agh-hook-probe"]);
    if config_hooks {
        std::fs::remove_file(&marker).expect("the control's marker");
    }
    assert_eq!(refs(), before, "the control's probe ref is gone");
    // A rewriting key followed by more than the child door keeps of a stream: the screen
    // would see only the padding, so it refuses the listing's length.
    let config = clone.join(".git").join("config");
    let kept = std::fs::read_to_string(&config).expect("the clone's config");
    let pad = "a".repeat(1_100_000);
    std::fs::write(
        &config,
        format!(
            "{kept}[url \"file://{}\"]\n\tinsteadOf = {url}\n[pad \"{pad}\"]\n\tv = 1\n",
            other.display()
        ),
    )
    .expect("the padded config");
    let err = gh
        .run_git(&GitRun::fetch(&slug, &clone))
        .expect_err("a config too long to screen is refused")
        .into_data();
    assert_eq!(err.kind, Kind::Refused, "{}", err.why);
    assert!(err.why.contains("too long"), "{}", err.why);
    assert_eq!(refs(), before, "the padded config moved the clone's refs");
    std::fs::write(&config, kept).expect("the clone's config, restored");
    host_git(&clone, &["config", "--local", "submodule.recurse", "true"]);
    host_git(
        &clone,
        &["config", "--local", "submodule.sub.url", "file:///nowhere"],
    );
    gh.run_git(&GitRun::fetch(&slug, &clone))
        .unwrap_or_else(|e| panic!("a submodule's keys do not refuse a fetch: {}", e.why));
    assert_eq!(
        host_git(&clone, &["rev-parse", "refs/remotes/origin/main"]),
        second,
        "the fetch ran"
    );
    let fetched = gh
        .fx()
        .children_run()
        .into_iter()
        .rev()
        .find(|spec| spec.args.iter().any(|arg| arg == "fetch"))
        .expect("the fetch's spec");
    assert!(
        fetched
            .args
            .iter()
            .any(|arg| arg == "--recurse-submodules=no"),
        "{:?}",
        fetched.args
    );
    // No detached auto-maintenance outlives the run: `maintenance.auto` is off at command
    // scope, over whatever the clone says.
    let count: usize = env_of(&fetched, "GIT_CONFIG_COUNT")
        .and_then(|count| count.to_str())
        .and_then(|count| count.parse().ok())
        .expect("GIT_CONFIG_COUNT");
    let off = (0..count).any(|i| {
        env_of(&fetched, &format!("GIT_CONFIG_KEY_{i}"))
            .is_some_and(|key| key == "maintenance.auto")
            && env_of(&fetched, &format!("GIT_CONFIG_VALUE_{i}"))
                .is_some_and(|value| value == "false")
    });
    assert!(off, "{:?}", fetched.env);
}

#[test]
fn a_push_delete_with_a_bad_reference_runs_nothing() {
    let world = World::canonical("git-bad-ref");
    let gh = world.client(git_env()).build();
    let slug = common::slug("fixture-dev/app");
    for (reference, expect) in [
        ("refs/heads/a:b", common::MAIN_SHA),
        ("refs/tags/*", common::MAIN_SHA),
        ("refs/tags/v1", "main"),
    ] {
        let err = gh
            .run_git(&GitRun::push_delete(&slug, reference, expect))
            .expect_err("refused before anything runs")
            .into_data();
        assert_eq!(err.kind, Kind::Usage, "{reference} {expect}: {}", err.why);
        assert_eq!(err.kind.exit(), 2);
    }
    assert!(gh.fx().children_run().is_empty());
    assert!(files(&world.dirs.grants()).is_empty());
}

/// `FetchMain` writes `+refs/heads/main:{into}` into a person's checkout, so an `into` that is
/// a branch, a tag, a remote-tracking ref or no ref name is Usage before anything runs; one
/// outside them fetches main alone, with no tag and no other ref.
#[test]
fn a_fetch_of_main_writes_only_its_record_ref() {
    let world = World::canonical("git-fetch-main");
    let bare = bare_repo(world.path(), "app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let checkout = world.path().join("checkout");
    gh.run_git(&GitRun::new_clone(&slug, &checkout))
        .unwrap_or_else(|e| panic!("the clone: {}", e.why));
    let ran = gh.fx().children_run().len();
    for into in [
        "refs/heads/main",
        "refs/tags/v1",
        "refs/remotes/origin/main",
        "main",
        "refs/x/a:b",
    ] {
        let err = gh
            .run_git(&GitRun::fetch_main(&slug, &checkout, into))
            .expect_err("refused before anything runs")
            .into_data();
        assert_eq!(err.kind, Kind::Usage, "{into}: {}", err.why);
    }
    assert_eq!(
        gh.fx().children_run().len(),
        ran,
        "a refused fetch ran a child"
    );
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let first = host_git(&bare, &["rev-parse", "main"]);
    let second = host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    host_git(&bare, &["update-ref", "refs/heads/main", &second]);
    host_git(&bare, &["tag", "v9", &second]);
    let refs = || host_git(&checkout, &["for-each-ref"]);
    let before = refs();
    gh.run_git(&GitRun::fetch_main(&slug, &checkout, "refs/record/main"))
        .unwrap_or_else(|e| panic!("the fetch of main: {}", e.why));
    assert!(gh.fx().children_run().len() > ran);
    assert_eq!(
        host_git(&checkout, &["rev-parse", "refs/record/main"]),
        second
    );
    host_git(&checkout, &["update-ref", "-d", "refs/record/main"]);
    assert_eq!(refs(), before, "the fetch of main moved another ref");
}

#[test]
fn an_exact_fetch_keeps_tag_objects_and_leaves_checkout_state_alone() {
    let world = World::canonical("git-exact-fetch");
    let bare = bare_repo(world.path(), "app");
    let checkout = world.path().join("checkout");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            &bare.to_string_lossy(),
            &checkout.to_string_lossy(),
        ],
    );
    let first = host_git(&bare, &["rev-parse", "main"]);
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let second = host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    host_git(&bare, &["tag", "-a", "v2", &second, "-m", "two"]);
    let tag = host_git(&bare, &["rev-parse", "refs/tags/v2"]);
    let before = host_git(&checkout, &["for-each-ref"]);
    let config = std::fs::read(checkout.join(".git/config")).unwrap();
    std::fs::write(checkout.join(".git/FETCH_HEAD"), b"keep fetch head\n").unwrap();
    std::fs::write(checkout.join("local-work"), b"keep local work\n").unwrap();
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    for (reference, expected) in [
        ("refs/tags/v2", tag.as_str()),
        (second.as_str(), second.as_str()),
    ] {
        let result = gh
            .run_git(&GitRun::fetch_ref(
                &slug,
                &checkout,
                reference,
                "refs/agh/exact",
                true,
            ))
            .unwrap_or_else(|error| panic!("{}", error.why));
        assert_eq!(result.as_, None);
        assert_eq!(result.grant, None);
        assert_eq!(
            host_git(&checkout, &["rev-parse", "refs/agh/exact"]),
            expected
        );
    }
    // A syntactically private cache ref may already be a symbolic reference to
    // a branch. Replacing the cache must not follow it into that branch.
    host_git(&checkout, &["update-ref", "-d", "refs/agh/exact"]);
    host_git(
        &checkout,
        &["symbolic-ref", "refs/agh/exact", "refs/heads/main"],
    );
    gh.run_git(&GitRun::fetch_ref(
        &slug,
        &checkout,
        &second,
        "refs/agh/exact",
        true,
    ))
    .unwrap();
    assert_eq!(
        host_git(&checkout, &["rev-parse", "refs/heads/main"]),
        first
    );
    assert_eq!(
        host_git(&checkout, &["rev-parse", "refs/agh/exact"]),
        second
    );
    host_git(&checkout, &["update-ref", "-d", "refs/agh/exact"]);
    assert_eq!(host_git(&checkout, &["for-each-ref"]), before);
    assert_eq!(host_git(&checkout, &["rev-parse", "HEAD"]), first);
    assert_eq!(
        std::fs::read(checkout.join(".git/FETCH_HEAD")).unwrap(),
        b"keep fetch head\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("local-work")).unwrap(),
        b"keep local work\n"
    );
    assert_eq!(std::fs::read(checkout.join(".git/config")).unwrap(), config);
    assert!(
        gh.fx()
            .children_run()
            .iter()
            .all(|spec| !runs_agh_s_helper(spec))
    );
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn exact_fetch_refuses_filesystem_symlinks_in_refs_and_reflogs() {
    use std::os::unix::fs::symlink;
    let world = World::canonical("git-exact-fetch-symlink");
    let bare = bare_repo(world.path(), "app");
    let first = host_git(&bare, &["rev-parse", "main"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    for root in ["refs", "logs/refs"] {
        let checkout = world.path().join(root.replace('/', "-"));
        std::fs::create_dir(&checkout).unwrap();
        host_git(&checkout, &["init", "--quiet", "--bare"]);
        host_git(
            &checkout,
            &[
                "fetch",
                "--quiet",
                &bare.to_string_lossy(),
                "main:refs/heads/victim",
            ],
        );
        let parent = checkout.join(root);
        std::fs::create_dir_all(&parent).unwrap();
        symlink(checkout.join("refs/heads"), parent.join("agh")).unwrap();
        let before = gh.fx().children_run().len();
        let error = gh
            .run_git(&GitRun::fetch_ref(
                &common::slug("fixture-dev/app"),
                &checkout,
                &first,
                "refs/agh/victim",
                true,
            ))
            .unwrap_err();
        assert!(error.why.contains("filesystem symlink"), "{}", error.why);
        assert_eq!(
            host_git(&checkout, &["rev-parse", "refs/heads/victim"]),
            first
        );
        assert!(
            gh.fx().children_run()[before..]
                .iter()
                .all(|spec| !spec.args.iter().any(|arg| arg == "fetch"))
        );
    }
    let checkout = world.path().join("hardlink.git");
    std::fs::create_dir(&checkout).unwrap();
    host_git(&checkout, &["init", "--quiet", "--bare"]);
    let logs = checkout.join("logs/refs/agh");
    std::fs::create_dir_all(&logs).unwrap();
    let unrelated = world.path().join("unrelated-file");
    std::fs::write(&unrelated, b"keep unchanged\n").unwrap();
    std::fs::hard_link(&unrelated, logs.join("exact")).unwrap();
    let error = gh
        .run_git(&GitRun::fetch_ref(
            &common::slug("fixture-dev/app"),
            &checkout,
            &first,
            "refs/agh/exact",
            true,
        ))
        .unwrap_err();
    assert!(error.why.contains("hard link"), "{}", error.why);
    assert_eq!(std::fs::read(unrelated).unwrap(), b"keep unchanged\n");
}

#[test]
fn exact_fetch_rejects_ambiguous_sources_and_non_owned_destinations_before_running() {
    let world = World::canonical("git-exact-fetch-refusals");
    let gh = world.client(git_env()).build();
    let slug = common::slug("fixture-dev/app");
    for (source, into) in [
        ("main", "refs/agh/exact"),
        ("--upload-pack=other", "refs/agh/exact"),
        ("refs/heads/*", "refs/agh/exact"),
        ("refs/heads/main:refs/heads/main", "refs/agh/exact"),
        ("refs/heads/main", "refs/heads/main"),
        ("refs/heads/main", "refs/tags/v1"),
        ("refs/heads/main", "refs/remotes/origin/main"),
        ("refs/heads/main", "refs/record/main"),
        ("refs/heads/main", "refs/agh/../main"),
    ] {
        let error = gh
            .run_git(&GitRun::fetch_ref(&slug, world.path(), source, into, true))
            .unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{source} {into}: {}", error.why);
    }
    assert!(gh.fx().children_run().is_empty());
    assert!(world.fake.seen().is_empty());
}

#[test]
fn anonymous_release_fetch_cannot_use_available_writer_credentials() {
    let world = World::release("git-anonymous-release-fetch");
    world.fake.add_repo(
        "alabsystems/secret",
        RepoSpec::private().pull("fixture-release-bot"),
    );
    world
        .fake
        .set_branch("alabsystems/secret", "main", common::MAIN_SHA);
    let checkout = world.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    host_git(&checkout, &["init", "--quiet"]);
    let gh = world.client(git_env()).build();
    let named = common::granted(&gh, "alabsystems/secret");
    gh.run_git(
        &GitRun::fetch_ref(
            &common::slug("alabsystems/secret"),
            &checkout,
            "refs/heads/main",
            "refs/agh/anonymous",
            true,
        )
        .with_named(named),
    )
    .expect_err("a private repository cannot be fetched anonymously");
    let seen = git_seen(&world.fake, "alabsystems/secret");
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|request| !request.had_authorization));
    assert!(
        gh.fx()
            .children_run()
            .iter()
            .all(|spec| !runs_agh_s_helper(spec))
    );
    assert!(files(&world.dirs.grants()).is_empty());
    assert!(host_git(&checkout, &["for-each-ref"]).is_empty());
}

#[test]
fn a_release_tier_run_needs_a_literal_or_a_grant() {
    let world = World::canonical("git-release-named");
    let gh = world.client(git_env()).build();
    let slug = common::slug("alabsystems/aterm");
    for named in [Named::Default, Named::Target("channel".to_string())] {
        let err = gh
            .run_git(&GitRun::ls_remote(&slug, None).with_named(named.clone()))
            .expect_err("a release-tier run named by default or @target")
            .into_data();
        assert_eq!(err.kind, Kind::Refused, "{named:?}: {}", err.why);
        assert_eq!(
            err.sub,
            Sub::Policy(RefusalCode::ReleaseNeedsLiteral),
            "{named:?}"
        );
    }
    assert!(!world.dirs.grants().exists());
    assert!(gh.fx().children_run().is_empty());
}

#[test]
fn every_child_runs_in_the_run_s_scratch() {
    let world = World::canonical("git-scratch");
    let bare = bare_repo(world.path(), "app");
    host_git(&bare, &["tag", "v1", "main"]);
    let sha = host_git(&bare, &["rev-parse", "main"]);
    let parent_home = world.path().join("parent-home");
    let env = git_env().with("HOME", &parent_home.to_string_lossy());
    let gh = world
        .client(env)
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let cwd = std::env::current_dir().expect("the test's working directory");
    let runs = [
        GitRun::ls_remote(&slug, None),
        GitRun::push_delete(&slug, "refs/tags/v1", &sha),
    ];
    for run in &runs {
        let before = gh.fx().children_run().len();
        let out = gh
            .run_git(run)
            .unwrap_or_else(|e| panic!("{:?}: {}", run.op, e.why));
        assert_eq!(out.as_.as_deref(), Some("dev"));
        let specs: Vec<ChildSpec> = gh.fx().children_run().split_off(before);
        assert!(!specs.is_empty());
        let scratch = env_of(&specs[0], "GIT_CEILING_DIRECTORIES")
            .map(PathBuf::from)
            .expect("the ceiling");
        assert!(scratch.is_absolute());
        assert_ne!(scratch, cwd);
        for spec in &specs {
            let home = env_of(spec, "HOME").map(PathBuf::from).expect("HOME");
            assert!(home.starts_with(&scratch), "{home:?}");
            assert_ne!(home, parent_home);
            let dir = spec.cwd.clone().expect("a working directory");
            assert!(dir.starts_with(&scratch), "{dir:?}");
            assert!(!dir.starts_with(&cwd) || cwd.starts_with(&scratch));
            assert_eq!(
                env_of(spec, "GIT_CEILING_DIRECTORIES").map(PathBuf::from),
                Some(scratch.clone())
            );
            for (name, want) in [
                ("AGH_GIT_CONFIG_DIR", &world.dirs.config),
                ("AGH_GIT_STATE_DIR", &world.dirs.state),
                ("AGH_GIT_HOME", &world.dirs.home),
            ] {
                assert_eq!(env_of(spec, name).map(PathBuf::from).as_ref(), Some(want));
            }
            assert!(env_of(spec, "GIT_DIR").is_none());
        }
        assert!(!scratch.exists(), "the scratch outlived its run");
    }
    assert_eq!(
        host_git(&bare, &["tag", "--list"]),
        "",
        "the tag was deleted"
    );
    assert!(files(&world.dirs.grants()).is_empty());
}

/// A leased delete against a local repository: the ref at the expected sha goes, and one at
/// another sha is Conflict `guard_failed` and stays. The grant the push wrote is gone after
/// each run.
#[test]
fn a_leased_delete_of_a_local_repository_is_guarded() {
    let world = World::canonical("git-lease");
    let bare = bare_repo(world.path(), "app");
    host_git(&bare, &["tag", "v1", "main"]);
    host_git(&bare, &["tag", "v2", "main"]);
    let sha = host_git(&bare, &["rev-parse", "main"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let out = gh
        .run_git(&GitRun::push_delete(&slug, "refs/tags/v1", &sha))
        .unwrap_or_else(|e| panic!("the leased delete: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert!(out.grant.as_deref().is_some_and(|nonce| nonce.len() == 32));
    assert_eq!(host_git(&bare, &["tag", "--list"]), "v2");
    let err = gh
        .run_git(&GitRun::push_delete(
            &slug,
            "refs/tags/v2",
            common::MAIN_SHA,
        ))
        .expect_err("another sha is refused")
        .into_data();
    assert_eq!(err.kind, Kind::Conflict, "{}", err.why);
    assert!(
        matches!(&err.sub, Sub::GuardFailed { what } if what.contains("stale info")),
        "{:?}",
        err.sub
    );
    assert_eq!(host_git(&bare, &["tag", "--list"]), "v2");
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn a_profile_without_a_token_file_is_refused_for_git() {
    let text = |_: &Path| {
        "[profiles.envdev]\nenv = \"AGH_TEST_TOKEN\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"envdev\"]\nwrite = \"envdev\"\n"
            .to_string()
    };
    let world = world("git-env-profile", text);
    common::canonical_repos(&world.fake);
    let env = git_env().with("AGH_TEST_TOKEN", &agh_fake::token("dev"));
    let gh = world.client(env).build();
    let slug = common::slug("fixture-dev/app");
    for run in [
        GitRun::push_delete(&slug, "refs/tags/v1", common::MAIN_SHA),
        GitRun::ls_remote(&slug, None),
    ] {
        let err = gh
            .run_git(&run)
            .expect_err("an env profile cannot answer git")
            .into_data();
        assert_eq!(err.kind, Kind::Refused, "{:?}: {}", run.op, err.why);
        assert!(err.why.contains("envdev"), "{}", err.why);
        assert!(err.why.contains("env:AGH_TEST_TOKEN"), "{}", err.why);
    }
    assert!(!world.dirs.grants().exists());
    assert!(gh.fx().children_run().is_empty());
    assert!(
        world
            .fake
            .seen()
            .iter()
            .all(|seen| !seen.path_and_query.contains(".git/")),
        "a git request reached the fake"
    );
}

/// A client with no `Dirs`, an empty environment and the clock at `SIM_START` (what
/// `common::github` builds) stops before any child: a dev read routed as `dev` at `a git run
/// as dev needs agh's directories` (its helper would read another agh.toml, or none, and
/// `GitOut.as_` would name a profile that never answered), and a leased delete, which needs a
/// grant, at `a git grant needs agh's state directory`. With its `Dirs` and no `PATH`, the dev
/// read stops at `git is not on PATH`.
#[test]
fn a_client_without_dirs_or_path_runs_no_git() {
    let world = World::canonical("git-bare-client");
    let gh: Github = common::github(world.router.clone(), &world.fake);
    let slug = common::slug("fixture-dev/app");
    let read = gh
        .run_git(&GitRun::ls_remote(&slug, None))
        .expect_err("no Dirs")
        .into_data();
    assert_eq!(
        (read.kind, read.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(read.why.contains("directories"), "{}", read.why);
    assert!(read.why.contains("dev"), "{}", read.why);
    let pathless = world.client(MapEnv::new()).build();
    let read = pathless
        .run_git(&GitRun::ls_remote(&slug, None))
        .expect_err("no PATH")
        .into_data();
    assert_eq!(
        (read.kind, read.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(read.why.contains("PATH"), "{}", read.why);
    assert!(pathless.fx().children_run().is_empty());
    let push = gh
        .run_git(&GitRun::push_delete(
            &slug,
            "refs/tags/v1",
            common::MAIN_SHA,
        ))
        .expect_err("no state directory")
        .into_data();
    assert_eq!(
        (push.kind, push.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(push.why.contains("state directory"), "{}", push.why);
    assert!(gh.fx().children_run().is_empty());
}

/// Whether any `GIT_CONFIG_VALUE_*` of `spec` runs agh's credential helper.
fn runs_agh_s_helper(spec: &ChildSpec) -> bool {
    let count: usize = env_of(spec, "GIT_CONFIG_COUNT")
        .and_then(|count| count.to_str())
        .and_then(|count| count.parse().ok())
        .expect("GIT_CONFIG_COUNT");
    (0..count).any(|i| {
        env_of(spec, &format!("GIT_CONFIG_KEY_{i}")).is_some_and(|key| key == "credential.helper")
            && env_of(spec, &format!("GIT_CONFIG_VALUE_{i}"))
                .is_some_and(|value| value.to_string_lossy().contains("auth git-credential"))
    })
}

/// An anonymous run gets no helper of agh's, only the reset of every other helper, so nothing
/// answers a token for a run whose route said nobody, even on a client with no `Dirs` whose
/// child would find another agh.toml through `XDG_CONFIG_HOME`. NEGATIVE CONTROL: the same
/// read routed as `dev` runs agh's helper.
#[test]
fn an_anonymous_run_runs_no_helper_of_agh() {
    let world = World::canonical("git-anonymous");
    let bare = bare_repo(world.path(), "app");
    let env = git_env().with(
        "XDG_CONFIG_HOME",
        &world.path().join("config").to_string_lossy(),
    );
    std::fs::remove_file(world.path().join("tokens").join("dev.token"))
        .expect("the dev token file");
    let bare_client = GithubBuilder::new(world.router.clone())
        .transport(Arc::new(HttpTransport::direct(Arc::new(env.clone()))))
        .endpoints(common::endpoints(&world.fake))
        .env(Arc::new(env.clone()))
        .fx(Arc::new(Fx::simulated(live_start())))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .git_helper_for_tests(common::helper())
        .git_remote_for_tests("fixture-anon/app", &bare)
        .build();
    let out = bare_client
        .run_git(&GitRun::ls_remote(&common::slug("fixture-anon/app"), None))
        .unwrap_or_else(|e| panic!("the anonymous read: {}", e.why));
    assert_eq!(out.as_, None);
    let specs = bare_client.fx().children_run();
    assert!(!specs.is_empty());
    assert!(specs.iter().all(|spec| !runs_agh_s_helper(spec)));
    common::write_token(
        &world.path().join("tokens"),
        "dev.token",
        &agh_fake::token("dev"),
    );
    let gh = world
        .client(env)
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let out = gh
        .run_git(&GitRun::ls_remote(&common::slug("fixture-dev/app"), None))
        .unwrap_or_else(|e| panic!("the dev read: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert!(gh.fx().children_run().iter().all(runs_agh_s_helper));
}

/// A fetch into a folder that is not itself a repository never reaches one that encloses it:
/// git looks at the folder alone, so the run is could_not_run, and the enclosing repository's
/// tag and refs stay as they were. The remote's `v1` points elsewhere, so a fetch that found
/// the enclosing repository would have moved it.
#[test]
fn a_fetch_into_a_folder_that_is_no_repository_touches_no_enclosing_one() {
    let world = World::canonical("git-fetch-ceiling");
    let bare = bare_repo(world.path(), "app");
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let first = host_git(&bare, &["rev-parse", "main"]);
    let second = host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    host_git(&bare, &["tag", "v1", &second]);
    let home = world.path().join("home-repo");
    std::fs::create_dir_all(&home).expect("the enclosing repository");
    host_git(&home, &["init", "--quiet", "-b", "main"]);
    host_git(&home, &["commit", "--quiet", "--allow-empty", "-m", "home"]);
    host_git(&home, &["tag", "v1"]);
    let tag = host_git(&home, &["rev-parse", "v1"]);
    assert_ne!(tag, second);
    let dir = home.join("clones").join("app");
    std::fs::create_dir_all(&dir).expect("the empty folder");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let err = gh
        .run_git(&GitRun::fetch(&common::slug("fixture-dev/app"), &dir))
        .expect_err("no repository at the folder")
        .into_data();
    assert_eq!(err.kind, Kind::CouldNotRun, "{}", err.why);
    assert_eq!(host_git(&home, &["rev-parse", "v1"]), tag, "the tag moved");
    assert_eq!(host_git(&home, &["for-each-ref", "refs/remotes"]), "");
    assert!(
        gh.fx()
            .children_run()
            .iter()
            .all(|spec| !spec.args.iter().any(|arg| arg == "fetch")),
        "the fetch ran"
    );
}

/// A leased delete's scratch `git init --bare`, killed at the run's bound, sent nothing: no
/// `git push` started, so the error carries no side effect and the journal can settle the
/// intent as never sent. `git` here is a stand-in that never exits.
#[test]
fn a_leased_delete_whose_init_ends_at_its_bound_sent_nothing() {
    let world = World::canonical("git-init-bound");
    let bin = world.path().join("hung-bin");
    std::fs::create_dir_all(&bin).expect("the stand-in's folder");
    let yes = aterm_uds::deadline::stand_in(Path::new("/usr/bin/yes"));
    std::os::unix::fs::symlink(&yes, bin.join("git")).expect("the stand-in git");
    let env = git_env().with("PATH", &bin.to_string_lossy());
    let gh = world.client(env).build();
    let run = GitRun::push_delete(
        &common::slug("fixture-dev/app"),
        "refs/tags/v1",
        common::MAIN_SHA,
    )
    .with_budget(Duration::from_secs(1));
    let (tx, rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let ended = gh.run_git(&run).map(|_| ()).map_err(|e| e.into_data());
        let specs = gh.fx().children_run();
        let _ = tx.send((ended, specs));
    });
    let (ended, specs) = rx
        .recv_timeout(common::HANG)
        .expect("the run ended within the hang detector");
    runner.join().expect("the runner thread");
    let err = ended.expect_err("the init never exits");
    assert_eq!(
        (err.kind, err.side_effect),
        (Kind::CouldNotRun, SideEffect::None),
        "{}",
        err.why
    );
    assert_eq!(specs.len(), 1, "{specs:?}");
    assert!(specs[0].args.iter().any(|arg| arg == "init"));
    assert!(
        specs
            .iter()
            .all(|spec| !spec.args.iter().any(|arg| arg == "push"))
    );
    assert!(files(&world.dirs.grants()).is_empty());
}

/// The helper routes by agh.toml, not by the client's router: when the agh.toml it reads names
/// another writer for the owner, it would answer a leased delete by its read chain, not as the
/// writer the run names, so the run is Refused before any grant or child, with the helper's
/// generic why (the profile is not one vault adoption gave it), and the ref stays.
#[test]
fn a_helper_with_another_writer_refuses_the_leased_delete() {
    let text = |root: &Path| {
        let canonical = common::canonical_config(root);
        let root = root
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!(
            "[profiles.other]\ntoken_file = \"{root}/tokens/other.token\"\ntier = \"dev\"\n\
             login = \"fixture-other-bot\"\n\n{}",
            canonical.replace("write = \"dev\"", "write = \"other\"")
        )
    };
    let world = world("git-helper-writer", text);
    common::write_token(
        &world.path().join("tokens"),
        "other.token",
        &agh_fake::token("other"),
    );
    let client_router = common::router(&common::canonical_config(world.path()), world.path());
    let bare = bare_repo(world.path(), "app");
    host_git(&bare, &["tag", "v1", "main"]);
    let sha = host_git(&bare, &["rev-parse", "main"]);
    let gh = git_builder(
        client_router,
        common::endpoints(&world.fake),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .git_remote_for_tests("fixture-dev/app", &bare)
    .build();
    let err = gh
        .run_git(&GitRun::push_delete(
            &common::slug("fixture-dev/app"),
            "refs/tags/v1",
            &sha,
        ))
        .expect_err("the helper would not answer as dev")
        .into_data();
    assert_eq!(err.kind, Kind::Refused, "{}", err.why);
    assert!(err.why.contains("dev"), "{}", err.why);
    assert!(!err.why.contains("vault"), "{}", err.why);
    assert_eq!(err.next, "agh auth status");
    assert!(gh.fx().children_run().is_empty());
    assert!(!world.dirs.grants().exists(), "a refused run wrote a grant");
    assert_eq!(host_git(&bare, &["tag", "--list"]), "v1");
}

// The loopback tests: agh-fake's smart-HTTP git endpoints on 127.0.0.1, the agh binary as the
// helper, and the host's clock (`live_start`), which the helper judges a grant by.

/// The `seen()` requests to `slug`'s git endpoints on the web origin, in order.
fn git_seen(fake: &FakeGithub, slug: &str) -> Vec<agh_fake::Seen> {
    let prefix = format!("/{slug}.git/");
    fake.seen()
        .into_iter()
        .filter(|seen| seen.origin == FakeOrigin::Web && seen.path_and_query.starts_with(&prefix))
        .collect()
}

/// The canonical world with a `reader` profile (`fixture-reader`, its own token file) heading
/// `fixture-dev`'s read chain, `dev` still its writer; `fixture-dev/app` is private, push for
/// `fixture-dev-bot` and pull for `fixture-reader`, with `refs/tags/v1` at `MAIN_SHA`.
fn reader_world(label: &str) -> World {
    let world = world(label, |root| {
        common::write_token(
            &root.join("tokens"),
            "reader.token",
            &agh_fake::token("reader"),
        );
        let reader = format!(
            "[profiles.reader]\ntoken_file = \"{}/tokens/reader.token\"\ntier = \"dev\"\n\
             login = \"fixture-reader\"\n\n",
            root.display()
        );
        let canonical = common::canonical_config(root);
        let owners = canonical.replacen(
            "name = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]",
            "name = \"fixture-dev\"\ntier = \"dev\"\nread = [\"reader\", \"dev\"]",
            1,
        );
        assert_ne!(owners, canonical, "fixture-dev's read chain was rewritten");
        format!("{reader}{owners}")
    });
    world
        .fake
        .add_token(&agh_fake::token("reader"), "fixture-reader");
    world.fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private()
            .push("fixture-dev-bot")
            .pull("fixture-reader"),
    );
    world
        .fake
        .set_branch("fixture-dev/app", "main", common::MAIN_SHA);
    world
        .fake
        .set_tag("fixture-dev/app", "v1", common::MAIN_SHA);
    world
}

/// `repo clone {slug} {dir} --meta` through the CLI over the loopback endpoints, the agh
/// binary as the helper and the clock at `start`: its exit and stderr.
fn cli_clone(world: &World, slug: &str, dir: &Path, start: u64) -> (u8, String) {
    let (ctx, captured) = common::ctx(world.path(), &world.fake, git_env());
    let mut ctx = ctx.with_git_helper(common::helper());
    ctx.fx = Arc::new(Fx::simulated(start));
    let dir_text = dir.display().to_string();
    let exit = common::run_cli(
        &mut ctx,
        &["repo", "clone", slug, dir_text.as_str(), "--meta"],
    );
    (exit, captured.stderr_text())
}

/// `alabsystems/secret`, private with pull for `fixture-release-bot` and `main` at `MAIN_SHA`,
/// read by `ls_remote` with a granted literal on a client whose clock starts at `start`:
/// the run's outcome, after the grant it wrote is checked gone.
fn ls_remote_secret(world: &World, start: u64) -> agh::Result<agh::git::GitOut> {
    world.fake.add_repo(
        "alabsystems/secret",
        RepoSpec::private().pull("fixture-release-bot"),
    );
    world
        .fake
        .set_branch("alabsystems/secret", "main", common::MAIN_SHA);
    let gh = git_builder(
        world.router.clone(),
        common::endpoints(&world.fake),
        &world.dirs,
        git_env(),
        start,
    )
    .build();
    let named = common::granted(&gh, "alabsystems/secret");
    let out =
        gh.run_git(&GitRun::ls_remote(&common::slug("alabsystems/secret"), None).with_named(named));
    assert!(
        files(&world.dirs.grants()).is_empty(),
        "a grant outlived the run"
    );
    out
}

#[test]
fn a_private_release_repository_is_read_with_the_grant() {
    let world = World::release("git-release-grant");
    let out = ls_remote_secret(&world, live_start())
        .unwrap_or_else(|e| panic!("the release read: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("release"));
    assert!(out.stdout.contains(common::MAIN_SHA), "{}", out.stdout);
    let seen = git_seen(&world.fake, "alabsystems/secret");
    let refs = "/alabsystems/secret.git/info/refs";
    let first = seen.first().expect("git asked the fake");
    assert!(first.path_and_query.starts_with(refs), "{first:?}");
    assert!(!first.had_authorization, "the first ask carries nothing");
    assert!(
        seen.iter()
            .skip(1)
            .any(|s| s.path_and_query.starts_with(refs)
                && s.as_login.as_deref() == Some("fixture-release-bot")),
        "{seen:?}"
    );
}

/// The negative control of the live start: the same read with the client's clock at
/// `SIM_START` writes its grant to expire at `SIM_START + 600`, long past by the helper's
/// clock, so rule 6 quits.
#[test]
fn a_grant_at_the_simulated_start_is_expired_to_the_helper() {
    let world = World::release("git-release-expired");
    let err = ls_remote_secret(&world, common::SIM_START)
        .expect_err("the helper quits on an expired grant")
        .into_data();
    assert_eq!(err.kind, Kind::Unauthenticated, "{}", err.why);
    assert_eq!(err.kind.exit(), 4);
    let seen = git_seen(&world.fake, "alabsystems/secret");
    assert!(!seen.is_empty(), "git asked the fake");
    assert!(
        seen.iter()
            .all(|s| s.as_login.as_deref() != Some("fixture-release-bot")),
        "{seen:?}"
    );
}

#[test]
fn a_dev_repository_is_read_as_dev() {
    let world = World::canonical("git-dev-read");
    common::canonical_repos(&world.fake);
    let gh = world.client(git_env()).build();
    let out = gh
        .run_git(&GitRun::ls_remote(&common::slug("fixture-dev/app"), None))
        .unwrap_or_else(|e| panic!("the read: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert_eq!(out.grant, None);
    assert!(out.stdout.contains(common::MAIN_SHA), "{}", out.stdout);
    let seen = git_seen(&world.fake, "fixture-dev/app");
    assert!(
        seen.iter()
            .any(|s| s.as_login.as_deref() == Some("fixture-dev-bot")),
        "{seen:?}"
    );
    assert!(!world.dirs.grants().exists(), "a dev read wrote a grant");
}

#[test]
fn a_leased_delete_pushes_from_an_empty_repository() {
    let world = World::canonical("git-loopback-lease");
    common::canonical_repos(&world.fake);
    world
        .fake
        .set_tag("fixture-dev/app", "v1", common::MAIN_SHA);
    world
        .fake
        .set_tag("fixture-dev/app", "v2", common::MAIN_SHA);
    let gh = world.client(git_env()).build();
    let slug = common::slug("fixture-dev/app");
    let has = |reference: &str| {
        world
            .fake
            .git_refs("fixture-dev/app")
            .iter()
            .any(|(name, _)| name == reference)
    };
    let out = gh
        .run_git(&GitRun::push_delete(
            &slug,
            "refs/tags/v1",
            common::MAIN_SHA,
        ))
        .unwrap_or_else(|e| panic!("the leased delete: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    assert!(!has("refs/tags/v1"), "the tag was deleted");
    let other = "89abcdef0123456789abcdef0123456789abcdef";
    let err = gh
        .run_git(&GitRun::push_delete(&slug, "refs/tags/v2", other))
        .expect_err("another sha is refused")
        .into_data();
    assert_eq!(err.kind, Kind::Conflict, "{}", err.why);
    assert_eq!(err.kind.exit(), 9);
    assert!(
        matches!(&err.sub, Sub::GuardFailed { what } if what.contains("stale info")),
        "{:?}",
        err.sub
    );
    assert!(has("refs/tags/v2"), "the guarded tag stayed");
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn a_leased_delete_pushes_as_the_writer() {
    let world = reader_world("git-writer-push");
    let gh = world.client(git_env()).build();
    let slug = common::slug("fixture-dev/app");
    let read = gh
        .run_git(&GitRun::ls_remote(&slug, None))
        .unwrap_or_else(|e| panic!("the read: {}", e.why));
    assert_eq!(read.as_.as_deref(), Some("reader"));
    assert_eq!(read.grant, None);
    assert!(
        git_seen(&world.fake, "fixture-dev/app")
            .iter()
            .any(|s| s.as_login.as_deref() == Some("fixture-reader")),
        "the read went as the read chain's head"
    );
    assert!(!world.dirs.grants().exists(), "a read wrote a grant");
    world.fake.clear_seen();
    let out = gh
        .run_git(&GitRun::push_delete(
            &slug,
            "refs/tags/v1",
            common::MAIN_SHA,
        ))
        .unwrap_or_else(|e| panic!("the leased delete: {}", e.why));
    assert_eq!(out.as_.as_deref(), Some("dev"));
    let seen = git_seen(&world.fake, "fixture-dev/app");
    let receive: Vec<&agh_fake::Seen> = seen
        .iter()
        .filter(|s| s.path_and_query.contains("git-receive-pack") && s.had_authorization)
        .collect();
    assert!(!receive.is_empty(), "{seen:?}");
    assert!(
        receive
            .iter()
            .all(|s| s.as_login.as_deref() == Some("fixture-dev-bot")),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .all(|s| s.as_login.as_deref() != Some("fixture-reader")),
        "{seen:?}"
    );
    assert!(
        !world
            .fake
            .git_refs("fixture-dev/app")
            .iter()
            .any(|(name, _)| name == "refs/tags/v1")
    );
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn a_netrc_in_home_is_never_read() {
    let world = World::canonical("git-netrc");
    world.fake.add_repo(
        "fixture-anon/app",
        RepoSpec::private().pull("fixture-dev-bot"),
    );
    world
        .fake
        .set_branch("fixture-anon/app", "main", common::MAIN_SHA);
    std::fs::remove_file(world.path().join("tokens").join("dev.token"))
        .expect("the dev token file");
    let home = world.path().join("parent-home");
    std::fs::create_dir_all(&home).expect("the parent's home");
    let netrc = home.join(".netrc");
    std::fs::write(
        &netrc,
        format!(
            "machine 127.0.0.1 login x password {}\n",
            agh_fake::token("dev")
        ),
    )
    .expect("the parent's .netrc");
    common::chmod(&netrc, 0o600);
    let env = git_env().with("HOME", &home.to_string_lossy());
    let gh = world.client(env).build();
    let err = gh
        .run_git(&GitRun::ls_remote(&common::slug("fixture-anon/app"), None))
        .expect_err("the helper quits, and git stops")
        .into_data();
    assert_eq!(err.kind, Kind::Unauthenticated, "{}", err.why);
    assert_eq!(err.kind.exit(), 4);
    let seen = git_seen(&world.fake, "fixture-anon/app");
    assert!(!seen.is_empty(), "git asked the fake");
    assert!(
        seen.iter()
            .all(|s| s.as_login.as_deref() != Some("fixture-dev-bot")),
        "{seen:?}"
    );
}

#[test]
fn a_refused_credential_is_unauthenticated_and_redacted() {
    let world = World::canonical("git-refused");
    world.fake.add_repo("fixture-dev/app", RepoSpec::private());
    world
        .fake
        .set_branch("fixture-dev/app", "main", common::MAIN_SHA);
    let token = agh_fake::token("dev");
    let basic = agh_fake::basic(&token);
    let gh = world.client(git_env()).build();
    let err = gh
        .run_git(&GitRun::ls_remote(&common::slug("fixture-dev/app"), None))
        .expect_err("the dev token has no pull");
    let printed = format!("{err:?}");
    let err = err.into_data();
    assert_eq!(err.kind, Kind::Unauthenticated, "{}", err.why);
    assert_eq!(err.kind.exit(), 4);
    for text in [&err.why, &err.next, &printed] {
        assert!(!text.contains(&token), "a token in {text}");
        assert!(!text.contains(&basic), "a credential in {text}");
    }
    assert!(
        git_seen(&world.fake, "fixture-dev/app")
            .iter()
            .any(|s| s.as_login.as_deref() == Some("fixture-dev-bot")),
        "the helper answered the dev token"
    );
    let dir = world.path().join("refused");
    let (exit, stderr) = cli_clone(&world, "fixture-dev/app", &dir, live_start());
    assert_eq!(exit, 4, "{stderr}");
    assert!(!stderr.contains(&token), "a token on stderr");
    assert!(!stderr.contains(&basic), "a credential on stderr");
}

/// A release-tier read with the argv literal writes its grant, finds `git` and spawns it
/// against a web port the test binds and never answers, so the run meets its budget.
#[test]
fn a_run_past_its_budget_is_could_not_run() {
    let world = World::release("git-budget");
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a silent port");
    let web = silent.local_addr().expect("its address").port();
    let ports = world.fake.ports();
    let gh = git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, web, ports.cdn),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .build();
    let budget = Duration::from_secs(1);
    let named = common::granted(&gh, "alabsystems/aterm");
    let run = GitRun::ls_remote(&common::slug("alabsystems/aterm"), None)
        .with_named(named)
        .with_budget(budget);
    let (tx, rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let ended = gh.run_git(&run).map(|_| ()).map_err(|e| e.into_data());
        let _ = tx.send(ended);
    });
    let ended = rx
        .recv_timeout(common::HANG)
        .expect("the run ended within the hang detector");
    runner.join().expect("the runner thread");
    let err = ended.expect_err("git waits on the silent port");
    assert_eq!(err.kind, Kind::CouldNotRun, "{}", err.why);
    assert_eq!(err.sub, Sub::Local, "{}", err.why);
    let git = host_git_path().display().to_string();
    assert!(err.why.contains(&git), "{}", err.why);
    assert!(err.why.contains(&format!("{budget:?}")), "{}", err.why);
    assert!(files(&world.dirs.grants()).is_empty());
    drop(silent);
}

/// Vault adoption routes the run and its helper alike (design §6.10). With no agh.toml, the
/// vault's dev token, whose login `fixture-dev` names its owner, is learned and recorded by
/// the client (`Ctx::github`), and the helper adopts it from that record: a private
/// repository is read as the token, and a leased delete is pushed as the owner's writer under
/// its grant, `GitOut.as_` `dev` both times.
#[test]
fn a_vault_routed_run_reads_and_pushes_as_the_vault_token() {
    let root = common::scratch("git-vault");
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    let token = agh_fake::token("vault-dev");
    fake.add_token(&token, "fixture-dev");
    let path = agh::identity::vault::Slot::Dev.path(&dirs.home);
    std::fs::create_dir_all(&dirs.home).expect("the home");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the vault file's name");
    common::write_token(path.parent().expect("the vault directory"), name, &token);
    fake.add_repo("fixture-dev/app", RepoSpec::private().push("fixture-dev"));
    fake.set_branch("fixture-dev/app", "main", common::MAIN_SHA);
    fake.set_tag("fixture-dev/app", "v1", common::MAIN_SHA);
    let (ctx, _captured) = common::ctx(root.path(), &fake, git_env());
    let mut ctx = ctx.with_git_helper(common::helper());
    ctx.fx = Arc::new(Fx::simulated(live_start()));
    let gh = ctx
        .github()
        .unwrap_or_else(|e| panic!("the client: {}", e.why));
    let slug = common::slug("fixture-dev/app");
    let read = gh
        .run_git(&GitRun::ls_remote(&slug, None))
        .unwrap_or_else(|e| panic!("the read: {}", e.why));
    assert_eq!(read.as_.as_deref(), Some("dev"));
    assert!(read.stdout.contains(common::MAIN_SHA), "{}", read.stdout);
    let pushed = gh
        .run_git(&GitRun::push_delete(
            &slug,
            "refs/tags/v1",
            common::MAIN_SHA,
        ))
        .unwrap_or_else(|e| panic!("the leased delete: {}", e.why));
    assert_eq!(pushed.as_.as_deref(), Some("dev"));
    assert!(
        fake.git_refs("fixture-dev/app")
            .iter()
            .all(|(name, _)| name != "refs/tags/v1"),
        "the tag is still there"
    );
    let seen = git_seen(&fake, "fixture-dev/app");
    assert!(
        seen.iter()
            .any(|s| s.as_login.as_deref() == Some("fixture-dev")),
        "{seen:?}"
    );
    assert!(files(&dirs.grants()).is_empty(), "a grant outlived the run");
}

/// A vault token whose login the client learned while another call held the record's lock
/// routes the client's call, but its login is not recorded, so git's helper, which reads the
/// record and asks GitHub nothing, would not answer as it: the run is Refused before any
/// grant or child, never sent to end Unauthenticated.
#[test]
fn a_vault_login_the_helper_cannot_read_refuses_the_run() {
    let root = common::scratch("git-vault-unrecorded");
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    let token = agh_fake::token("vault-dev");
    fake.add_token(&token, "fixture-dev");
    let path = agh::identity::vault::Slot::Dev.path(&dirs.home);
    std::fs::create_dir_all(&dirs.home).expect("the home");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the vault file's name");
    common::write_token(path.parent().expect("the vault directory"), name, &token);
    fake.add_repo("fixture-dev/app", RepoSpec::private().push("fixture-dev"));
    fake.set_tag("fixture-dev/app", "v1", common::MAIN_SHA);
    let _held = agh::identity::vault::hold_record_lock_for_tests(&dirs).expect("the record lock");
    let (ctx, _captured) = common::ctx(root.path(), &fake, git_env());
    let mut ctx = ctx.with_git_helper(common::helper());
    ctx.fx = Arc::new(Fx::simulated(live_start()));
    let gh = ctx
        .github()
        .unwrap_or_else(|e| panic!("the client: {}", e.why));
    let slug = common::slug("fixture-dev/app");
    for run in [
        GitRun::ls_remote(&slug, None),
        GitRun::push_delete(&slug, "refs/tags/v1", common::MAIN_SHA),
    ] {
        let err = gh
            .run_git(&run)
            .expect_err("the helper would not answer")
            .into_data();
        assert_eq!(err.kind, Kind::Refused, "{:?}: {}", run.op, err.why);
        assert!(err.why.contains("vault adoption record"), "{}", err.why);
        assert!(!err.why.contains(&token), "{}", err.why);
    }
    assert!(gh.fx().children_run().is_empty());
    assert!(git_seen(&fake, "fixture-dev/app").is_empty());
    assert!(!dirs.grants().exists(), "a refused run wrote a grant");
}

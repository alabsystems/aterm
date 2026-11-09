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
use agh::fx::child::{ChildOut, ChildSpec};
use agh::git::{FetchBatch, FetchRow, GitRun, HelperCommand};
use agh::identity::MapEnv;
use agh::policy::{Named, RefusalCode, Router};
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh::{Github, GithubBuilder, Kind, SideEffect, Sub};
use agh_fake::{FakeGithub, FakeOrigin, RepoSpec};

use crate::common;

mod commit_ancestry_tests;

/// Exercise the real CLI entry and its signal-to-collector handoff. The fake Git waits for
/// a release file, so even the old orphaning CLI can be cleaned without signalling a guessed
/// descendant PID. The production collector still must retire it before returning 130.
#[test]
fn native_cli_stop_retires_its_git_before_returning() {
    native_cli_stop(false);
}

#[test]
fn native_cli_ancestry_stop_retires_its_git_before_returning() {
    native_cli_stop(true);
}

fn native_cli_stop(ancestry: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Instant;

    struct Fixture {
        child: aterm_uds::spawnfd::Child,
        release: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.release, b"release");
            // This is the actual owning handle, never an observed numeric PID.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    {
        let root = common::scratch("native-cli-stop");
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let git = bin.join("git");
        std::fs::write(
            &git,
            b"#!/bin/sh\ndir=${0%/*}\nprintf '%s\\n' \"$$\" > \"$dir/pid\"\nwhile test ! -f \"$dir/release\"; do /bin/sleep 0.01; done\n",
        )
        .expect("fake Git");
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).expect("executable");
        let path = std::env::join_paths([
            bin.clone(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .expect("PATH");
        let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("agh"));
        command.args(["--anonymous", "repo"]);
        if ancestry {
            let repo = root.path().join("repo.git");
            host_git(
                root.path(),
                &[
                    "init",
                    "--quiet",
                    "--bare",
                    "--template=",
                    repo.to_str().unwrap(),
                ],
            );
            let input = root.path().join("commits.json");
            let oid = "1".repeat(40);
            std::fs::write(
                &input,
                format!(
                    "{{\"commits\":[\"{oid}\"],\"queries\":[{{\"ancestor\":\"{oid}\",\"descendant\":\"{oid}\"}}]}}"
                ),
            )
            .expect("commit query");
            command
                .args(["commit-ancestry", "alabsystems/aterm"])
                .arg(&repo)
                .arg("--input")
                .arg(&input);
        } else {
            command.args(["public-refs", "alabsystems/aterm"]);
        }
        command
            .env_clear()
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("PATH", path)
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut fixture = Fixture {
            child: command.spawn().expect("actual CLI"),
            release: bin.join("release"),
        };
        let marker = bin.join("pid");
        let until = Instant::now() + Duration::from_secs(60);
        while !std::fs::read_to_string(&marker).is_ok_and(|text| text.ends_with('\n'))
            && Instant::now() < until
        {
            assert!(fixture.child.try_wait().expect("CLI wait").is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(&marker)
            .expect("actual Git started")
            .trim()
            .parse::<i32>()
            .expect("Git pid");
        let git_identity = aterm_uds::process::read_identity(pid).expect("running Git identity");
        fixture.child.terminate().expect("TERM only the actual CLI");
        let until = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = fixture.child.try_wait().expect("CLI wait") {
                break status;
            }
            assert!(Instant::now() < until, "CLI stop hang detector");
            std::thread::sleep(Duration::from_millis(10));
        };
        let git_identity_at_return = aterm_uds::process::read_identity(pid);
        let git_presence_at_return = aterm_uds::process::process_presence(pid);
        let git_reaped_at_return = git_identity_at_return
            .is_some_and(|current| current != git_identity)
            || git_presence_at_return == aterm_uds::process::ProcessPresence::Absent;
        std::fs::write(&fixture.release, b"release").expect("cooperative fixture cleanup");
        let git_no_longer_runs = || {
            let identity = aterm_uds::process::read_identity(pid);
            let presence = aterm_uds::process::process_presence(pid);
            identity.is_some_and(|current| current != git_identity)
                || matches!(
                    presence,
                    aterm_uds::process::ProcessPresence::Exited
                        | aterm_uds::process::ProcessPresence::Absent
                )
        };
        let until = Instant::now() + Duration::from_secs(60);
        while !git_no_longer_runs() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(git_no_longer_runs(), "fixture ended");
        assert_eq!(status.code(), Some(130));
        assert!(
            git_reaped_at_return,
            "Git was positively reaped before CLI return: {git_presence_at_return:?} {git_identity_at_return:?}"
        );
    }
}

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

fn push_batch(rows: &[(&str, &str, &str)]) -> agh::github::refs::AtomicRefBatch {
    agh::github::refs::AtomicRefBatch::new(
        rows.iter()
            .map(|(reference, before, after)| agh::github::refs::RefUpdate {
                reference: (*reference).into(),
                before: (*before).into(),
                after: (*after).into(),
            })
            .collect(),
    )
    .expect("batch")
}

const ABSENT: &str = "0000000000000000000000000000000000000000";

#[test]
fn native_push_preserves_exact_objects_to_an_empty_remote_and_changes_no_local_state() {
    use agh::journal::IntentStore as _;
    use agh::policy::{RouteRequest, RouteTarget};
    let world = World::canonical("native-push-empty");
    common::canonical_repos(&world.fake);
    let source = bare_repo(world.path(), "source");
    host_git(
        &source,
        &["tag", "-a", "v1", "-m", "an annotated release", "main"],
    );
    let sha = host_git(&source, &["rev-parse", "main"]);
    let tag = host_git(&source, &["rev-parse", "refs/tags/v1"]);
    let tag_bytes = host_git(&source, &["cat-file", "tag", &tag]);
    let remote = world.path().join("empty.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", &remote.to_string_lossy()],
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let batch = push_batch(&[
        ("refs/heads/main", ABSENT, &sha),
        ("refs/tags/v1", ABSENT, &tag),
        ("refs/attest/release", ABSENT, &sha),
        ("refs/tags/版本", ABSENT, &tag),
    ]);
    let refs_before = host_git(&source, &["show-ref"]);
    let config_before = std::fs::read(source.join("config")).expect("source config");
    gh.refs(&slug)
        .check_push(&permit, &source, &batch)
        .expect("dry run");
    assert!(host_git(&remote, &["for-each-ref", "--format=%(refname)"]).is_empty());
    assert!(!world.dirs.journal().exists(), "dry run made a journal");
    let journal = gh.journal(&slug).expect("journal");
    gh.refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .expect("push")
        .into_result()
        .expect("acknowledged");
    assert_eq!(host_git(&remote, &["rev-parse", "refs/tags/v1"]), tag);
    assert_eq!(host_git(&remote, &["cat-file", "tag", &tag]), tag_bytes);
    assert_eq!(
        host_git(&remote, &["rev-parse", "refs/attest/release"]),
        sha
    );
    assert_eq!(host_git(&source, &["show-ref"]), refs_before);
    assert_eq!(
        std::fs::read(source.join("config")).expect("config"),
        config_before
    );
    assert!(!source.join("FETCH_HEAD").exists());
    assert!(journal.open().expect("open intents").is_empty());
    assert!(files(&world.dirs.grants()).is_empty());
    let pushes: Vec<_> = gh
        .fx()
        .children_run()
        .into_iter()
        .filter(|s| s.args.iter().any(|a| a == "push"))
        .collect();
    assert_eq!(pushes.len(), 2);
    for p in pushes {
        assert!(p.args.iter().any(|a| a == "--atomic"));
        assert!(!p.cwd.as_ref().expect("cwd").starts_with(&source));
        assert_eq!(
            env_of(&p, "GIT_NO_REPLACE_OBJECTS").map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("1"))
        );
    }
}

#[test]
fn native_push_checks_fast_forward_immutable_tags_stale_leases_and_missing_objects() {
    let world = World::canonical("native-push-guards");
    let remote = bare_repo(world.path(), "remote");
    let source = world.path().join("source");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            &remote.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    let before = host_git(&source, &["rev-parse", "main"]);
    host_git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "next"],
    );
    let after = host_git(&source, &["rev-parse", "main"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    gh.run_git(&GitRun::push_refs(
        &slug,
        &source,
        push_batch(&[("refs/heads/main", &before, &after)]),
        false,
    ))
    .expect("fast-forward");
    host_git(&remote, &["tag", "v1", &before]);
    host_git(
        &source,
        &[
            "tag",
            "-a",
            "source-tag",
            "-m",
            "an immutable custom tag",
            "main",
        ],
    );
    let custom_tag = host_git(&source, &["rev-parse", "refs/tags/source-tag"]);
    host_git(
        &remote,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            &source.to_string_lossy(),
            &format!("{custom_tag}:refs/attest/tag"),
        ],
    );
    for (rows, kind) in [
        (vec![("refs/heads/main", &*after, &*before)], Kind::Refused),
        (vec![("refs/tags/v1", &*before, &*after)], Kind::Refused),
        (
            vec![("refs/attest/tag", &*custom_tag, &*after)],
            Kind::Refused,
        ),
        (
            vec![
                ("refs/heads/main", &*before, &*after),
                ("refs/tags/new", ABSENT, &*after),
            ],
            Kind::Conflict,
        ),
        (
            vec![("refs/heads/missing", ABSENT, common::MAIN_SHA)],
            Kind::CouldNotRun,
        ),
        (vec![("refs/heads/main", &*after, &*after)], Kind::Usage),
    ] {
        let error = gh
            .run_git(&GitRun::push_refs(&slug, &source, push_batch(&rows), false))
            .expect_err("refused");
        assert_eq!(error.kind, kind, "{}", error.why);
        assert_eq!(error.side_effect, SideEffect::None);
        assert_eq!(host_git(&remote, &["rev-parse", "main"]), after);
        assert_eq!(host_git(&remote, &["tag", "--list"]), "v1");
    }
    host_git(
        &source,
        &["config", "hook.pre-push.command", "touch must-not-run"],
    );
    let error = gh
        .run_git(&GitRun::push_refs(
            &slug,
            &source,
            push_batch(&[("refs/tags/new", ABSENT, &after)]),
            false,
        ))
        .expect_err("config hook");
    assert_eq!(error.kind, Kind::Refused);
    assert!(matches!(error.sub, Sub::GitConfig { .. }));
}

/// A real remote base with an old blob, followed by one local new-blob commit.
fn incremental_source(world: &World) -> (PathBuf, PathBuf, String, String) {
    let remote = bare_repo(world.path(), "remote");
    let source = world.path().join("source");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            &remote.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    std::fs::write(source.join("old"), b"the authenticated old blob\n").expect("old");
    host_git(&source, &["add", "old"]);
    host_git(&source, &["commit", "--quiet", "-m", "base"]);
    host_git(&source, &["push", "--quiet", "origin", "main"]);
    let before = host_git(&source, &["rev-parse", "main"]);
    std::fs::write(source.join("new"), b"new object that must be checked\n").expect("new");
    host_git(&source, &["add", "new"]);
    host_git(&source, &["commit", "--quiet", "-m", "after"]);
    let after = host_git(&source, &["rev-parse", "main"]);
    (remote, source, before, after)
}

/// The cut's actual tag, main claim and paired unlock, against a local bare
/// receive-pack. Run explicitly for before/after measurements, single-threaded.
#[test]
#[ignore = "local git CPU measurement; run with --ignored --nocapture --test-threads=1"]
fn native_cut_push_measurement() {
    use agh::git::{
        ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF,
    };
    let world = World::canonical("cut-push-measure");
    common::canonical_repos(&world.fake);
    let (remote, source, _, _) = incremental_source(&world);
    // A deterministic history with wide trees and changing binary blobs makes
    // local import/compression work visible, without any network or credential.
    let mut input = Vec::new();
    let mut random = 7_u64;
    let base = host_git(&source, &["rev-parse", "main"]);
    for n in 0..6000 {
        let mut blob = vec![0_u8; 8192];
        for byte in &mut blob {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            *byte = random as u8;
        }
        let from = if n == 0 {
            format!("from {base}\n")
        } else {
            String::new()
        };
        input.extend_from_slice(format!("commit refs/heads/history\ncommitter fixture <fixture> {} +0000\ndata 7\nhistory\n{from}M 100644 inline file-{}\ndata {}\n", 1700000000 + n, n % 512, blob.len()).as_bytes());
        input.extend_from_slice(&blob);
        input.extend_from_slice(b"\n\n");
    }
    input.extend_from_slice(b"done\n");
    let mut cmd = aterm_uds::spawnfd::command(host_git_path());
    cmd.args(["fast-import", "--quiet", "--done"])
        .current_dir(&source);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut cmd,
        aterm_uds::secret::Secret::new(&input),
        std::time::Instant::now() + common::HANG,
        1 << 20,
    )
    .unwrap();
    assert!(out.status.success(), "{:?}", out);
    host_git(&source, &["repack", "-adf", "--window=0"]);
    host_git(&source, &["push", "--quiet", "origin", "history:main"]);
    let before = host_git(&source, &["rev-parse", "history"]);
    let tree = host_git(&source, &["rev-parse", "history^{tree}"]);
    let after = host_git(
        &source,
        &["commit-tree", &tree, "-p", &before, "-m", "claim"],
    );
    let metadata = host_git(
        &source,
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            "--filter=blob:none",
            &after,
        ],
    );
    let commits = host_git(&source, &["rev-list", "--count", &after]);
    let new = host_git(
        &source,
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            &after,
            &format!("^{before}"),
        ],
    );
    assert_eq!(commits, "6004");
    assert_eq!(metadata.lines().count(), 12007);
    assert_eq!(new, after);
    println!(
        "cut inventory: commits {commits}, trees {}, metadata {}, new objects 1; ledger records 0 (native-writer fixture)",
        metadata.lines().count() - 6004,
        metadata.lines().count()
    );
    host_git(&source, &["tag", "-a", "cut-tag", "-m", "cut", &before]);
    let tag = host_git(&source, &["rev-parse", "cut-tag"]);
    host_git(&source, &["tag", "-a", "cut-fence", "-m", "fence", &before]);
    let fence = host_git(&source, &["rev-parse", "cut-fence"]);
    host_git(
        &source,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{before}:{RELEASE_LEASE_REF}"),
            &format!("{fence}:{PUBLISHER_FENCE_REF}"),
        ],
    );
    host_git(
        &source,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture-dev/app.git",
        ],
    );
    let child_spans = Arc::new(std::sync::Mutex::new(Vec::new()));
    let previous_cpu = Arc::new(std::sync::Mutex::new(None::<u64>));
    let prior = Arc::clone(&previous_cpu);
    let observed = Arc::clone(&child_spans);
    let fx = Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
        let end =
            aterm_sysprobe::jobs::counters_of(std::process::id()).and_then(|cpu| cpu.child_cpu_ns);
        let mut previous = prior.lock().unwrap();
        let delta = previous.zip(end).and_then(|(a, b)| b.checked_sub(a));
        *previous = end;
        observed.lock().unwrap().push((span, delta));
    });
    let gh = world
        .client(git_env())
        .fx(Arc::new(fx))
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(
        &gh,
        &agh::policy::RouteRequest::write(agh::policy::RouteTarget::Repo(slug.clone())),
    );
    let unlock = ControlRefTransaction::new(
        &before,
        vec![
            ControlRefChange {
                reference: RELEASE_LEASE_REF.into(),
                before: Some(before.clone()),
                after: None,
            },
            ControlRefChange {
                reference: PUBLISHER_FENCE_REF.into(),
                before: Some(fence),
                after: None,
            },
        ],
    )
    .unwrap();
    for (name, run) in [
        (
            "tag",
            Some(GitRun::push_refs(
                &slug,
                &source,
                push_batch(&[("refs/tags/cut-tag", ABSENT, &tag)]),
                false,
            )),
        ),
        (
            "claim",
            Some(GitRun::push_refs(
                &slug,
                &source,
                push_batch(&[("refs/heads/main", &before, &after)]),
                false,
            )),
        ),
        ("unlock", None),
    ] {
        let first = gh.fx().children_run().len();
        let cpu = aterm_sysprobe::jobs::counters_of(std::process::id()).unwrap();
        *previous_cpu.lock().unwrap() = cpu.child_cpu_ns;
        let start = std::time::Instant::now();
        if let Some(run) = run {
            gh.run_git(&run).unwrap();
        } else {
            gh.refs(&slug)
                .check_control(&permit, &source, &unlock, agh::git::GIT_BUDGET)
                .unwrap();
        }
        let wall = start.elapsed().as_secs_f64();
        let end = aterm_sysprobe::jobs::counters_of(std::process::id()).unwrap();
        println!(
            "cut fixture {name}: wall {wall:.3}s child {:.3}s self {:.3}s",
            (end.child_cpu_ns.unwrap() - cpu.child_cpu_ns.unwrap()) as f64 / 1e9,
            (end.cpu_ns - cpu.cpu_ns) as f64 / 1e9
        );
        let specs = gh.fx().children_run();
        for (span, child_cpu) in child_spans
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s.ordinal >= first)
        {
            let spec = &specs[span.ordinal];
            // This fixture contains no real remotes or credentials. Retain the
            // exact arguments so repeated walks can be identified unambiguously.
            println!(
                "cut child {name} {}: wall {:.6}s child_cpu_ns {:?} args {:?}",
                span.ordinal - first,
                span.finished.duration_since(span.started).as_secs_f64(),
                child_cpu,
                spec.args
            );
        }
    }
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), after);
    assert_eq!(host_git(&remote, &["rev-parse", "refs/tags/cut-tag"]), tag);
}

/// NEGATIVE CONTROL: the old create path fetches the entire source closure.
/// The received pack must contain exactly one annotated tag when main already
/// owns its commit, and a tag of new content must take the full admission.
#[test]
fn native_push_tags_pack_only_what_the_observed_remote_lacks() {
    for published in [true, false] {
        let world = World::canonical("tag-pack-count");
        let (remote, source, before, after) = incremental_source(&world);
        host_git(&remote, &["config", "receive.unpackLimit", "0"]);
        let commit = if published { &before } else { &after };
        host_git(&source, &["tag", "-a", "release", "-m", "release", commit]);
        let tag = host_git(&source, &["rev-parse", "release"]);
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        gh.run_git(&GitRun::push_refs(
            &common::slug("fixture-dev/app"),
            &source,
            push_batch(&[("refs/tags/release", ABSENT, &tag)]),
            false,
        ))
        .unwrap();
        let packs: Vec<_> = std::fs::read_dir(remote.join("objects/pack"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "pack"))
            .collect();
        assert_eq!(packs.len(), 1);
        let bytes = std::fs::read(&packs[0]).unwrap();
        let objects = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(objects, if published { 1 } else { 4 });
        assert_eq!(
            host_git(&remote, &["rev-parse", "release^{commit}"]),
            *commit
        );
        let children = gh.fx().children_run();
        assert_eq!(
            children.iter().any(|s| s.args.iter().any(|a| a == "fetch")),
            !published
        );
        if published {
            let pack = children
                .iter()
                .find(|s| s.args.iter().any(|a| a == "pack-objects"))
                .unwrap();
            assert!(pack.args.iter().any(|a| a == "--no-revs"));
        }
        if !published {
            assert_eq!(
                host_git(&remote, &["show", "release:new"]),
                "new object that must be checked"
            );
        }
    }
}

/// A local ref is no authority for remote ownership. A corrupt parent witness
/// or raw tag must fail before the writer, even if Git can peel its contents.
#[test]
fn native_push_remote_owned_tags_refuse_corrupt_witnesses_and_tags() {
    for broken in ["witness", "tag"] {
        let world = World::canonical("tag-witness-corrupt");
        let (remote, source, before, after) = incremental_source(&world);
        host_git(&source, &["push", "--quiet", "origin", "main"]);
        host_git(&source, &["tag", "-a", "release", "-m", "release", &before]);
        let tag = host_git(&source, &["rev-parse", "release"]);
        if broken == "witness" {
            replace_loose(&source, &after, &before);
        } else {
            host_git(&source, &["tag", "-a", "other", "-m", "other", &before]);
            let other = host_git(&source, &["rev-parse", "other"]);
            replace_loose(&source, &tag, &other);
        }
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let error = gh
            .run_git(&GitRun::push_refs(
                &common::slug("fixture-dev/app"),
                &source,
                push_batch(&[("refs/tags/release", ABSENT, &tag)]),
                false,
            ))
            .unwrap_err();
        assert_eq!(error.side_effect, SideEffect::None);
        assert!(
            !gh.fx()
                .children_run()
                .iter()
                .any(|s| s.args.iter().any(|a| a == "push"))
        );
    }
}

/// NEGATIVE CONTROL: using the incremental ID inventory for a cold publication
/// refuses a valid graph once its IDs exceed the complete-output admission cap.
#[test]
fn native_push_cold_history_is_not_limited_by_the_incremental_inventory() {
    let world = World::canonical("cold-history");
    let (remote, source, _, _) = incremental_source(&world);
    let mut input = Vec::new();
    for n in 0..10_000 {
        let blob = format!("blob {n}\n");
        input.extend_from_slice(format!("commit refs/heads/cold\ncommitter fixture <fixture> {} +0000\ndata 4\nseed\nM 100644 inline file\ndata {}\n{blob}\n", 1700000000 + n, blob.len()).as_bytes());
    }
    input.extend_from_slice(b"done\n");
    let mut cmd = aterm_uds::spawnfd::command(host_git_path());
    cmd.args(["fast-import", "--quiet", "--done"])
        .current_dir(&source);
    let out = aterm_uds::deadline::output_with_input_until(
        &mut cmd,
        aterm_uds::secret::Secret::new(&input),
        std::time::Instant::now() + common::HANG,
        1 << 20,
    )
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    let head = host_git(&source, &["rev-parse", "cold"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    gh.run_git(&GitRun::push_refs(
        &common::slug("fixture-dev/app"),
        &source,
        push_batch(&[("refs/heads/cold", ABSENT, &head)]),
        false,
    ))
    .unwrap();
    assert_eq!(host_git(&remote, &["rev-parse", "cold"]), head);
    assert_eq!(host_git(&remote, &["show", "cold:file"]), "blob 9999");
    assert_eq!(host_git(&remote, &["rev-list", "--count", "cold"]), "10000");
    let inventory = host_git(&remote, &["count-objects", "-v"]);
    let objects: usize = inventory
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(": ")?;
            matches!(key, "count" | "in-pack").then(|| value.parse::<usize>().unwrap())
        })
        .sum();
    assert!(objects * 41 > 1 << 20);
    assert!(
        gh.fx()
            .children_run()
            .iter()
            .any(|s| s.args.iter().any(|a| a == "fetch"))
    );
}

/// NEGATIVE CONTROL: importing or walking the owner's complete file graph on
/// create/unlock restores the cut's historical pack work.
#[test]
fn native_push_controls_prove_remote_ownership_without_importing_history() {
    use agh::git::{
        ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF,
    };
    let world = World::canonical("control-owned-closure");
    common::canonical_repos(&world.fake);
    let (remote, source, owner, _) = incremental_source(&world);
    host_git(
        &source,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture-dev/app.git",
        ],
    );
    host_git(&source, &["tag", "-a", "fence", "-m", "fence", &owner]);
    let token = host_git(&source, &["rev-parse", "fence"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(
        &gh,
        &agh::policy::RouteRequest::write(agh::policy::RouteTarget::Repo(slug.clone())),
    );
    let create = ControlRefTransaction::new(
        &owner,
        vec![ControlRefChange {
            reference: RELEASE_LEASE_REF.into(),
            before: None,
            after: Some(owner.clone()),
        }],
    )
    .unwrap();
    gh.refs(&slug)
        .check_control(&permit, &source, &create, agh::git::GIT_BUDGET)
        .unwrap();
    host_git(
        &source,
        &[
            "push",
            "--quiet",
            &remote.to_string_lossy(),
            &format!("{owner}:{RELEASE_LEASE_REF}"),
            &format!("{token}:{PUBLISHER_FENCE_REF}"),
        ],
    );
    let delete = ControlRefTransaction::new(
        &owner,
        vec![
            ControlRefChange {
                reference: RELEASE_LEASE_REF.into(),
                before: Some(owner.clone()),
                after: None,
            },
            ControlRefChange {
                reference: PUBLISHER_FENCE_REF.into(),
                before: Some(token),
                after: None,
            },
        ],
    )
    .unwrap();
    gh.refs(&slug)
        .check_control(&permit, &source, &delete, agh::git::GIT_BUDGET)
        .unwrap();
    let children = gh.fx().children_run();
    let packs: Vec<_> = children
        .iter()
        .filter(|s| s.args.iter().any(|a| a == "pack-objects"))
        .collect();
    assert_eq!(packs.len(), 2);
    assert!(
        packs
            .iter()
            .all(|s| s.args.iter().any(|a| a == "--no-revs"))
    );
    assert!(!children.iter().any(|s| {
        s.args
            .iter()
            .any(|a| a == "fetch" || a == "--connectivity-only")
    }));
    assert_eq!(host_git(&remote, &["rev-parse", RELEASE_LEASE_REF]), owner);
}

fn native_object_fact_queries(specs: &[ChildSpec]) -> (Vec<String>, Vec<(String, String)>) {
    let kinds = specs
        .iter()
        .flat_map(|spec| spec.args.windows(3))
        .filter(|args| args[0] == "cat-file" && args[1] == "-t")
        .map(|args| args[2].to_str().unwrap().to_owned())
        .collect();
    let ancestry = specs
        .iter()
        .flat_map(|spec| spec.args.windows(3))
        .filter(|args| args[0] == "--is-ancestor")
        .map(|args| {
            (
                args[1].to_str().unwrap().to_owned(),
                args[2].to_str().unwrap().to_owned(),
            )
        })
        .collect();
    (kinds, ancestry)
}

/// Shared immutable facts avoid duplicate Git calls, but each namespace still
/// decides its own policy. Repeating a call must query its private view again.
#[test]
fn native_push_reuses_object_facts_without_reusing_reference_policies() {
    use agh::policy::{RouteRequest, RouteTarget};

    let world = World::canonical("native-object-facts");
    common::canonical_repos(&world.fake);
    let (remote, source, before, after) = incremental_source(&world);
    host_git(&remote, &["update-ref", "refs/aterm/judged", &before]);
    host_git(&remote, &["update-ref", "refs/tags/locked", &before]);
    host_git(&source, &["tag", "-a", "payload", "-m", "payload", &after]);
    let tag = host_git(&source, &["rev-parse", "refs/tags/payload"]);
    let source_refs = host_git(&source, &["show-ref"]);
    let source_config = std::fs::read(source.join(".git/config")).unwrap();
    let remote_refs = host_git(&remote, &["show-ref"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));

    for full in [false, true] {
        let batch = if full {
            push_batch(&[
                ("refs/heads/new", ABSENT, &after),
                ("refs/aterm/new", ABSENT, &after),
            ])
        } else {
            push_batch(&[
                ("refs/heads/main", &before, &after),
                ("refs/aterm/judged", &before, &after),
            ])
        };
        for _ in 0..2 {
            let first = gh.fx().children_run().len();
            gh.refs(&slug).check_push(&permit, &source, &batch).unwrap();
            let calls = gh.fx().children_run();
            let expected = if full {
                (vec![after.clone()], Vec::new())
            } else {
                (
                    vec![after.clone(), before.clone()],
                    vec![(before.clone(), after.clone())],
                )
            };
            assert_eq!(native_object_fact_queries(&calls[first..]), expected);
            assert_eq!(host_git(&remote, &["show-ref"]), remote_refs);
        }
    }
    let first = gh.fx().children_run().len();
    gh.refs(&slug)
        .check_push(
            &permit,
            &source,
            &push_batch(&[("refs/aterm/a-tag", ABSENT, &tag)]),
        )
        .unwrap();
    assert_eq!(
        native_object_fact_queries(&gh.fx().children_run()[first..]),
        (vec![tag.clone()], Vec::new()),
    );
    assert_eq!(host_git(&remote, &["show-ref"]), remote_refs);
    for (batch, expected) in [
        (
            push_batch(&[
                ("refs/heads/main", &before, &after),
                ("refs/tags/locked", &before, &after),
            ]),
            (
                vec![after.clone(), before.clone()],
                vec![(before.clone(), after.clone())],
            ),
        ),
        (
            push_batch(&[
                ("refs/aterm/a-tag", ABSENT, &tag),
                ("refs/heads/z-tag", ABSENT, &tag),
            ]),
            (vec![tag.clone()], Vec::new()),
        ),
    ] {
        let first = gh.fx().children_run().len();
        let error = gh
            .refs(&slug)
            .check_push(&permit, &source, &batch)
            .unwrap_err();
        assert_eq!(error.kind, Kind::Refused);
        assert_eq!(error.side_effect, SideEffect::None);
        let calls = gh.fx().children_run();
        assert_eq!(native_object_fact_queries(&calls[first..]), expected);
        assert!(
            !calls[first..]
                .iter()
                .any(|spec| spec.args.iter().any(|arg| arg == "push"))
        );
        assert_eq!(host_git(&remote, &["show-ref"]), remote_refs);
    }
    assert_eq!(host_git(&source, &["show-ref"]), source_refs);
    assert_eq!(
        std::fs::read(source.join(".git/config")).unwrap(),
        source_config
    );
}

/// Distinct successful pairs stay valid. Sharing either endpoint, or reversing
/// the ordered pair, does not reuse a fact about a different ancestry relation.
#[test]
fn native_push_ancestry_facts_require_both_ordered_endpoints() {
    use agh::policy::{RouteRequest, RouteTarget};

    let world = World::canonical("native-ancestry-facts");
    common::canonical_repos(&world.fake);
    let (remote, source, before, after) = incremental_source(&world);
    let root = host_git(&source, &["rev-parse", &format!("{before}^")]);
    let old_tree = host_git(&source, &["rev-parse", &format!("{before}^{{tree}}")]);
    let new_tree = host_git(&source, &["rev-parse", &format!("{after}^{{tree}}")]);
    let later = host_git(
        &source,
        &["commit-tree", &new_tree, "-p", &after, "-m", "later"],
    );
    let fork = host_git(
        &source,
        &["commit-tree", &old_tree, "-p", &before, "-m", "fork"],
    );
    let sibling = host_git(
        &source,
        &["commit-tree", &old_tree, "-p", &root, "-m", "sibling"],
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let cases = [
        (
            "shared before, both advance",
            &before,
            &after,
            &before,
            &later,
            true,
            vec![after.clone(), before.clone(), later.clone()],
        ),
        (
            "shared after, both advance",
            &before,
            &later,
            &after,
            &later,
            true,
            vec![later.clone(), before.clone(), after.clone()],
        ),
        (
            "different before, second diverges",
            &before,
            &after,
            &fork,
            &after,
            false,
            vec![after.clone(), before.clone(), fork.clone()],
        ),
        (
            "different after, second diverges",
            &before,
            &after,
            &before,
            &sibling,
            false,
            vec![after.clone(), before.clone(), sibling.clone()],
        ),
        (
            "reversed pair",
            &before,
            &after,
            &after,
            &before,
            false,
            vec![after.clone(), before.clone()],
        ),
    ];
    for (name, first_before, first_after, second_before, second_after, allowed, kinds) in cases {
        // Seed the owned remote's exact BEFORE values before the caller takes authority.
        host_git(&remote, &["update-ref", "refs/heads/a", first_before]);
        host_git(
            &remote,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                &source.to_string_lossy(),
                &format!("+{second_before}:refs/heads/z"),
            ],
        );
        let remote_refs = host_git(&remote, &["show-ref"]);
        let batch = push_batch(&[
            ("refs/heads/a", first_before, first_after),
            ("refs/heads/z", second_before, second_after),
        ]);
        let first = gh.fx().children_run().len();
        let result = gh.refs(&slug).check_push(&permit, &source, &batch);
        assert_eq!(result.is_ok(), allowed, "{name}");
        if let Err(error) = result {
            assert_eq!(error.kind, Kind::Refused, "{name}");
            assert_eq!(error.side_effect, SideEffect::None, "{name}");
        }
        let calls = gh.fx().children_run();
        assert_eq!(
            native_object_fact_queries(&calls[first..]),
            (
                kinds,
                vec![
                    (first_before.clone(), first_after.clone()),
                    (second_before.clone(), second_after.clone()),
                ]
            ),
            "{name}"
        );
        assert_eq!(
            calls[first..]
                .iter()
                .filter(|spec| spec.args.iter().any(|arg| arg == "push"))
                .count(),
            usize::from(allowed),
            "{name}"
        );
        assert_eq!(host_git(&remote, &["show-ref"]), remote_refs, "{name}");
    }
}

#[test]
fn native_push_phase_observation_preserves_success_refusal_and_failure() {
    use agh::git::NativePushPhase::{Connectivity, MetadataPack, MetadataVerify};
    use agh::policy::{RouteRequest, RouteTarget};
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;

    let world = World::canonical("native-phase-observation");
    common::canonical_repos(&world.fake);
    let (remote, source, before, after) = incremental_source(&world);
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[("refs/heads/main", &before, &after)]);
    let source_config = std::fs::read(source.join(".git/config")).unwrap();
    let spans = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&spans);
    let git_spans = Arc::new(Mutex::new(Vec::new()));
    let git_observed = Arc::clone(&git_spans);
    let fx = Fx::simulated(live_start())
        .with_git_child_observer_for_tests(move |span| {
            git_observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        })
        .with_native_push_observer_for_tests(move |span| {
            observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        });
    let gh = world
        .client(git_env())
        .fx(Arc::new(fx))
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    // The observer persists across calls, and each call has three distinct real spans.
    for call in 0..2 {
        let first_ordinal = gh.fx().children_run().len();
        let began = Instant::now();
        gh.refs(&slug).check_push(&permit, &source, &batch).unwrap();
        let finished = Instant::now();
        let spans = spans.lock().unwrap().clone();
        assert_eq!(spans.len(), (call + 1) * 3);
        let current = &spans[call * 3..];
        assert_eq!(
            current.iter().map(|s| s.phase).collect::<Vec<_>>(),
            [MetadataPack, MetadataVerify, Connectivity]
        );
        assert!(current.iter().all(|s| s.success
            && began <= s.started
            && s.started <= s.finished
            && s.finished <= finished));
        assert!(current.windows(2).all(|p| p[0].finished <= p[1].started));
        let logged = gh.fx().children_run();
        let children = git_spans.lock().unwrap().clone();
        let children: Vec<_> = children
            .iter()
            .filter(|s| s.ordinal >= first_ordinal)
            .collect();
        assert_eq!(children.len(), logged.len() - first_ordinal);
        assert!(
            children
                .iter()
                .enumerate()
                .all(|(i, s)| s.ordinal == first_ordinal + i
                    && s.error.is_none()
                    && began <= s.started
                    && s.started <= s.finished
                    && s.finished <= finished)
        );
        assert!(children.windows(2).all(|p| p[0].finished <= p[1].started));
        for phase in current {
            let nested: Vec<_> = children
                .iter()
                .filter(|s| s.started < phase.finished && phase.started < s.finished)
                .collect();
            assert!(!nested.is_empty());
            assert!(
                nested
                    .iter()
                    .all(|s| phase.started <= s.started && s.finished <= phase.finished)
            );
        }
    }
    let wrong_before = host_git(&source, &["rev-parse", &format!("{before}^")]);
    let error = gh
        .refs(&slug)
        .check_push(
            &permit,
            &source,
            &push_batch(&[("refs/heads/main", &wrong_before, &after)]),
        )
        .unwrap_err();
    assert_eq!(error.kind, Kind::Conflict);
    assert_eq!(
        spans.lock().unwrap().len(),
        6,
        "refusal precedes metadata work"
    );

    let command = gh
        .fx()
        .children_run()
        .into_iter()
        .find(|child| child.args.iter().any(|a| a == "--connectivity-only"))
        .unwrap();
    let failed_spans = Arc::new(Mutex::new(Vec::new()));
    let failed_observed = Arc::clone(&failed_spans);
    let failed_children = Arc::new(Mutex::new(Vec::new()));
    let failed_child_observed = Arc::clone(&failed_children);
    let failed = Fx::simulated(live_start())
        .with_git_child_observer_for_tests(move |span| {
            failed_child_observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        })
        .with_native_push_observer_for_tests(move |span| {
            failed_observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        })
        .with_git_intercept_once_for_tests(
            command.program,
            command.args,
            || {},
            |result| {
                result.as_mut().unwrap().code = Some(17);
            },
        );
    let failed = world
        .client(git_env())
        .fx(Arc::new(failed))
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let error = failed
        .refs(&slug)
        .check_push(&permit, &source, &batch)
        .unwrap_err();
    assert_eq!(error.side_effect, SideEffect::None);
    let failed_spans = failed_spans.lock().unwrap().clone();
    assert_eq!(
        failed_spans
            .iter()
            .map(|s| (s.phase, s.success))
            .collect::<Vec<_>>(),
        [
            (MetadataPack, true),
            (MetadataVerify, true),
            (Connectivity, false)
        ]
    );
    let failed_children = failed_children.lock().unwrap().clone();
    assert_eq!(failed_children.len(), failed.fx().children_run().len());
    assert_eq!(
        failed_children
            .iter()
            .filter(|s| s.code == Some(17) && s.error.is_none())
            .count(),
        1
    );
    assert_eq!(host_git(&source, &["rev-parse", "main"]), after);
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), before);
    assert_eq!(
        std::fs::read(source.join(".git/config")).unwrap(),
        source_config
    );
    assert!(!world.dirs.journal().exists());
}

#[test]
#[should_panic]
fn native_push_phase_observation_cannot_be_installed_on_a_live_client() {
    let _ = Fx::live().with_native_push_observer_for_tests(|_| {});
}

/// Unlink before writing: a local clone can share the old loose inode with its
/// fixture remote, whose authenticated bytes must stay independent.
fn replace_loose(source: &Path, wanted: &str, substitute: &str) {
    let objects = source.join(".git/objects");
    let path = |oid: &str| objects.join(&oid[..2]).join(&oid[2..]);
    let replacement = std::fs::read(path(substitute)).expect("substitute object");
    std::fs::remove_file(path(wanted)).expect("unlink owned loose object");
    std::fs::write(path(wanted), replacement).expect("substituted bytes");
}

#[test]
fn native_push_incremental_refuses_corrupt_metadata_new_blobs_and_missing_closure() {
    for broken in [
        "commit",
        "tree",
        "old-tree",
        "new-blob",
        "missing-blob",
        "missing-parent",
    ] {
        let world = World::canonical(&format!("incremental-{broken}"));
        let (remote, source, before, after) = incremental_source(&world);
        let old_blob = host_git(&source, &["rev-parse", &format!("{before}:old")]);
        let new_blob = host_git(&source, &["rev-parse", &format!("{after}:new")]);
        match broken {
            "commit" => replace_loose(&source, &after, &before),
            "tree" | "old-tree" => {
                let old = host_git(&source, &["rev-parse", &format!("{before}^{{tree}}")]);
                let new = host_git(&source, &["rev-parse", &format!("{after}^{{tree}}")]);
                if broken == "old-tree" {
                    replace_loose(&source, &old, &new);
                } else {
                    replace_loose(&source, &new, &old);
                }
            }
            "new-blob" => replace_loose(&source, &new_blob, &old_blob),
            _ => {
                let oid = if broken == "missing-blob" {
                    &new_blob
                } else {
                    &before
                };
                std::fs::remove_file(source.join(".git/objects").join(&oid[..2]).join(&oid[2..]))
                    .expect("remove closure member");
            }
        }
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let error = gh
            .run_git(&GitRun::push_refs(
                &common::slug("fixture-dev/app"),
                &source,
                push_batch(&[("refs/heads/main", &before, &after)]),
                false,
            ))
            .expect_err("broken source cannot write");
        assert_eq!(
            error.side_effect,
            SideEffect::None,
            "{broken}: {}",
            error.why
        );
        assert!(
            !gh.fx()
                .children_run()
                .iter()
                .any(|s| s.args.iter().any(|a| a == "push")),
            "{broken} reached the writer"
        );
        assert_eq!(host_git(&remote, &["rev-parse", "main"]), before);
        assert_eq!(
            host_git(&remote, &["show", "main:old"]),
            "the authenticated old blob"
        );
    }
}

#[test]
fn native_push_incremental_borrows_only_authenticated_old_blob_authority() {
    let world = World::canonical("incremental-old-blob");
    let (remote, source, before, after) = incremental_source(&world);
    let old = host_git(&source, &["rev-parse", &format!("{before}:old")]);
    let new = host_git(&source, &["rev-parse", &format!("{after}:new")]);
    replace_loose(&source, &old, &new);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    gh.run_git(&GitRun::push_refs(
        &common::slug("fixture-dev/app"),
        &source,
        push_batch(&[("refs/heads/main", &before, &after)]),
        false,
    ))
    .expect("remote already owns the exact BEFORE blob");
    assert_eq!(
        host_git(&remote, &["show", "main:old"]),
        "the authenticated old blob"
    );
    assert_eq!(
        host_git(&remote, &["show", "main:new"]),
        "new object that must be checked"
    );
    let children = gh.fx().children_run();
    assert!(
        !children.iter().any(|s| s.args.iter().any(|a| a == "fetch")),
        "incremental route downloaded an old graph"
    );
    let pack = children
        .iter()
        .find(|s| s.args.iter().any(|a| a == "pack-objects"))
        .expect("new-object pack");
    assert!(pack.args.iter().any(|a| a == "--threads=8"));
    assert!(!pack.args.iter().any(|a| a == "--thin"));
    // NEGATIVE CONTROL: the old second revision walk has --revs here.
    // The inventoried set must be the pack input, including a real new blob.
    let new_pack = children
        .iter()
        .filter(|s| s.args.iter().any(|a| a == "pack-objects"))
        .nth(1)
        .unwrap();
    assert!(new_pack.args.iter().any(|a| a == "--no-revs"));
    assert_eq!(
        children
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "ls-remote"))
            .count(),
        2
    );
}

#[test]
fn native_push_incremental_preserves_raw_signed_objects_and_ignores_source_grafts() {
    let world = World::canonical("incremental-raw");
    let (remote, source, before, after) = incremental_source(&world);
    let tree = host_git(&source, &["rev-parse", &format!("{after}^{{tree}}")]);
    let raw = format!(
        "tree {tree}\nparent {before}\nauthor fixture <fixture> 1700000000 +0000\ncommitter fixture <fixture> 1700000000 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n fixture bytes\n -----END PGP SIGNATURE-----\n\nsigned commit bytes\n"
    );
    let input = world.path().join("commit-input");
    std::fs::write(&input, &raw).expect("raw commit");
    let signed = host_git(
        &source,
        &[
            "hash-object",
            "-w",
            "-t",
            "commit",
            &input.to_string_lossy(),
        ],
    );
    let tag_raw = format!(
        "object {signed}\ntype commit\ntag v1\ntagger fixture <fixture> 1700000000 +0000\n\nrelease\n-----BEGIN PGP SIGNATURE-----\nfixture tag bytes\n-----END PGP SIGNATURE-----\n"
    );
    std::fs::write(&input, &tag_raw).expect("raw tag");
    let tag = host_git(
        &source,
        &["hash-object", "-w", "-t", "tag", &input.to_string_lossy()],
    );
    let config = std::fs::read(source.join(".git/config")).expect("config");
    // The original source claims this commit has no parents. The clean graph
    // view must ignore that graft and preserve the actual raw parent/FF edge.
    std::fs::create_dir_all(source.join(".git/info")).expect("info");
    std::fs::write(source.join(".git/info/grafts"), format!("{signed}\n")).expect("graft");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    gh.run_git(&GitRun::push_refs(
        &common::slug("fixture-dev/app"),
        &source,
        push_batch(&[
            ("refs/heads/main", &before, &signed),
            ("refs/tags/v1", ABSENT, &tag),
        ]),
        false,
    ))
    .expect("raw graph ignores graft");
    assert_eq!(
        host_git(&remote, &["cat-file", "commit", &signed]),
        raw.trim_end()
    );
    assert_eq!(
        host_git(&remote, &["cat-file", "tag", &tag]),
        tag_raw.trim_end()
    );
    assert_eq!(
        std::fs::read(source.join(".git/config")).expect("config"),
        config
    );
    assert!(!source.join(".git/FETCH_HEAD").exists());
}

/// A real fixture push supplies the objects; its receive reply is replaced once.
/// Restoring BEFORE models receive failure before commit. Keeping AFTER models
/// a committed transaction whose reply was lost. The protected reason is the
/// negative control: it must not read or retry.
#[test]
fn native_push_receive_failures_reread_before_retrying() {
    use agh::policy::{RouteRequest, RouteTarget};
    for (reason, landed, expected_pushes, stalled, conflict) in [
        ("failed", false, 2, false, false),
        ("failed", true, 1, false, false),
        ("failed", false, 1, true, false),
        ("failed", false, 1, false, true),
        ("protected branch hook declined", false, 1, false, false),
    ] {
        let world = World::canonical("native-receive-failure");
        common::canonical_repos(&world.fake);
        let source = bare_repo(world.path(), "source");
        let sha = host_git(&source, &["rev-parse", "main"]);
        let remote = world.path().join("origin.git");
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", &remote.to_string_lossy()],
        );
        let slug = common::slug("fixture-dev/app");
        let batch = push_batch(&[("refs/heads/main", ABSENT, &sha)]);
        let probe = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        probe
            .run_git(&GitRun::push_refs(&slug, &source, batch.clone(), true))
            .unwrap();
        let spec = probe
            .fx()
            .children_run()
            .into_iter()
            .find(|s| s.args.iter().any(|a| a == "push"))
            .unwrap();
        let args = spec.args.into_iter().filter(|a| a != "--dry-run").collect();
        let origin = remote.clone();
        let clock = Arc::new(std::sync::OnceLock::<Arc<Fx>>::new());
        let advance = clock.clone();
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program,
            args,
            || {},
            move |result| {
                let out = result.as_mut().expect("real fixture push");
                assert_eq!(out.code, Some(0));
                if !landed {
                    host_git(&origin, &["update-ref", "-d", "refs/heads/main"]);
                }
                if conflict {
                    let tree = host_git(&origin, &["rev-parse", &format!("{sha}^{{tree}}")]);
                    let other = host_git(
                        &origin,
                        &["commit-tree", &tree, "-p", &sha, "-m", "another writer"],
                    );
                    host_git(&origin, &["update-ref", "refs/heads/main", &other]);
                }
                if stalled {
                    advance
                        .get()
                        .unwrap()
                        .sleep(Duration::from_secs(3600))
                        .unwrap();
                }
                out.code = Some(1);
                out.stdout = format!("!\t{sha}:refs/heads/main\t[remote rejected] ({reason})\n")
                    .into_bytes();
                out.stderr = b"error: failed to push some refs\n".to_vec();
            },
        );
        let fx = Arc::new(fx);
        assert!(clock.set(fx.clone()).is_ok());
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .fx(fx)
            .build();
        let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
        let journal = gh.journal(&slug).unwrap();
        let result = gh
            .refs(&slug)
            .push_atomic(&permit, &source, &batch, &journal)
            .unwrap()
            .into_result();
        if stalled {
            let error = result.unwrap_err();
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::CouldNotRun, SideEffect::None)
            );
            assert!(error.why.contains("no remote ref progress"));
        } else if conflict {
            assert_eq!(result.unwrap_err().kind, Kind::Conflict);
        } else if reason == "failed" {
            result.expect("receive failure converges from freshly observed refs");
            assert_eq!(
                host_git(&remote, &["rev-parse", "main"]),
                batch.updates()[0].after
            );
        } else {
            assert_eq!(result.unwrap_err().kind, Kind::Refused);
        }
        let calls = gh.fx().children_run();
        assert_eq!(
            calls
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "push"))
                .count(),
            expected_pushes
        );
        // The two preparation reads precede the push; only transient failures
        // earn the third, authoritative read.
        assert_eq!(
            calls
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "ls-remote"))
                .count(),
            if reason == "failed" { 3 } else { 2 }
        );
    }
}

#[test]
fn leased_delete_receive_failures_reread_before_retrying() {
    for (reason, landed, pushes) in [
        ("failed", false, 2),
        ("failed", true, 1),
        ("protected branch hook declined", false, 1),
    ] {
        let world = World::canonical("delete-receive-failure");
        common::canonical_repos(&world.fake);
        let remote = bare_repo(world.path(), "origin");
        let sha = host_git(&remote, &["rev-parse", "main"]);
        host_git(&remote, &["update-ref", "refs/tags/v1", &sha]);
        let run = GitRun::push_delete(&common::slug("fixture-dev/app"), "refs/tags/v1", &sha);
        let probe = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        probe.run_git(&run).unwrap();
        host_git(&remote, &["update-ref", "refs/tags/v1", &sha]);
        let spec = probe
            .fx()
            .children_run()
            .into_iter()
            .find(|s| s.args.iter().any(|a| a == "push"))
            .unwrap();
        let origin = remote.clone();
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program,
            spec.args,
            || {},
            move |result| {
                let out = result.as_mut().unwrap();
                assert_eq!(out.code, Some(0));
                if !landed {
                    host_git(&origin, &["update-ref", "refs/tags/v1", &sha]);
                }
                out.code = Some(1);
                out.stdout = format!("!\t(delete):refs/tags/v1\t[remote rejected] ({reason})\n")
                    .into_bytes();
            },
        );
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .fx(Arc::new(fx))
            .build();
        let result = gh.run_git(&run);
        if reason == "failed" {
            result.unwrap();
            assert_eq!(host_git(&remote, &["for-each-ref", "refs/tags/v1"]), "");
        } else {
            assert_eq!(result.unwrap_err().kind, Kind::Refused);
        }
        let calls = gh.fx().children_run();
        assert_eq!(
            calls
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "push"))
                .count(),
            pushes
        );
        assert_eq!(
            calls
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "ls-remote"))
                .count(),
            usize::from(reason == "failed")
        );
    }
}

/// Both controls execute the actual Git atomic push. One changes a fixture ref
/// just before advertisement; the other loses only the successful result.
fn native_push_unknown_case(race_to_after: bool, incremental: bool) {
    use agh::journal::IntentStore as _;
    use agh::policy::{RouteRequest, RouteTarget};
    let world = World::canonical("native-push-unknown");
    common::canonical_repos(&world.fake);
    let source = bare_repo(world.path(), "source");
    let old = host_git(&source, &["rev-parse", "main"]);
    let sha = if incremental {
        let tree = host_git(&source, &["rev-parse", "main^{tree}"]);
        host_git(&source, &["commit-tree", &tree, "-p", &old, "-m", "next"])
    } else {
        old.clone()
    };
    let remote = world.path().join("empty.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", &remote.to_string_lossy()],
    );
    host_git(
        &remote,
        &[
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            &source.to_string_lossy(),
            &if incremental {
                format!("{old}:refs/heads/main")
            } else {
                sha.clone()
            },
        ],
    );
    if incremental {
        // Another writer can only race the ref to AFTER after supplying its
        // objects. Preload those fixture bytes without changing the old ref.
        host_git(
            &remote,
            &[
                "fetch",
                "--quiet",
                "--no-write-fetch-head",
                &source.to_string_lossy(),
                &sha,
            ],
        );
    }
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[
        (
            "refs/heads/main",
            if incremental { &old } else { ABSENT },
            &sha,
        ),
        ("refs/tags/v1", ABSENT, &sha),
    ]);
    let probe = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    probe
        .run_git(&GitRun::push_refs(&slug, &source, batch.clone(), true))
        .expect("preflight");
    let spec = probe
        .fx()
        .children_run()
        .into_iter()
        .find(|s| s.args.iter().any(|a| a == "push"))
        .expect("push child");
    let args = spec.args.into_iter().filter(|a| a != "--dry-run").collect();
    let race_remote = remote.clone();
    let race_sha = sha.clone();
    let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let before_seen = seen.clone();
    let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
        spec.program,
        args,
        move || {
            before_seen.store(true, std::sync::atomic::Ordering::SeqCst);
            if race_to_after {
                host_git(&race_remote, &["update-ref", "refs/heads/main", &race_sha]);
            }
        },
        move |result| {
            assert_eq!(result.as_ref().expect("actual Git child").code, Some(0));
            if !race_to_after {
                *result = Err(
                    agh::Error::could_not_run("fixture lost the push acknowledgement")
                        .with_side_effect(SideEffect::Unknown),
                );
            }
        },
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .fx(Arc::new(fx))
        .build();
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let journal = gh.journal(&slug).expect("journal");
    let outcome = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .expect("settled outcome");
    let agh::write::Outcome::Unknown { intent, error } = outcome else {
        panic!("the ambiguous write must stay open");
    };
    assert_eq!(error.side_effect, SideEffect::Unknown);
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(journal.open().expect("open").len(), 1);
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), sha);
    assert_eq!(host_git(&remote, &["rev-parse", "refs/tags/v1"]), sha);
    let pushes = gh
        .fx()
        .children_run()
        .iter()
        .filter(|s| s.args.iter().any(|a| a == "push"))
        .count();
    let error = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .expect_err("open intent blocks retry");
    assert_eq!(
        error.kind,
        Kind::OutcomeUnknown,
        "{}: {}",
        intent,
        error.why
    );
    assert_eq!(
        gh.fx()
            .children_run()
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "push"))
            .count(),
        pushes
    );
    assert!(files(&world.dirs.grants()).is_empty());
}

#[test]
fn native_push_racing_to_after_is_unknown_instead_of_a_false_cas_success() {
    native_push_unknown_case(true, false);
}

#[test]
fn native_push_lost_acknowledgement_keeps_a_nonretryable_intent() {
    native_push_unknown_case(false, false);
}

#[test]
fn native_push_incremental_racing_to_after_keeps_a_nonretryable_intent() {
    native_push_unknown_case(true, true);
}

#[test]
fn native_push_incremental_lost_acknowledgement_keeps_a_nonretryable_intent() {
    native_push_unknown_case(false, true);
}

#[test]
fn native_push_a_stale_lease_at_advertisement_rejects_the_whole_real_batch() {
    use agh::journal::IntentStore as _;
    use agh::policy::{RouteRequest, RouteTarget};
    let world = World::canonical("native-push-stale");
    common::canonical_repos(&world.fake);
    let source = bare_repo(world.path(), "source");
    let before = host_git(&source, &["rev-parse", "main"]);
    let work = world.path().join("source-work");
    host_git(&work, &["commit", "--quiet", "--allow-empty", "-m", "next"]);
    let after = host_git(&work, &["rev-parse", "main"]);
    host_git(
        &source,
        &[
            "fetch",
            "--quiet",
            &work.to_string_lossy(),
            "main:refs/heads/main",
        ],
    );
    let remote = world.path().join("empty.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", &remote.to_string_lossy()],
    );
    host_git(
        &remote,
        &[
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            &source.to_string_lossy(),
            &after,
        ],
    );
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[
        ("refs/heads/main", ABSENT, &after),
        ("refs/tags/v1", ABSENT, &after),
    ]);
    let probe = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    probe
        .run_git(&GitRun::push_refs(&slug, &source, batch.clone(), true))
        .expect("dry preflight");
    let spec = probe
        .fx()
        .children_run()
        .into_iter()
        .find(|s| s.args.iter().any(|a| a == "push"))
        .expect("push");
    let args = spec.args.into_iter().filter(|a| a != "--dry-run").collect();
    let race_remote = remote.clone();
    let race_before = before.clone();
    let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
        spec.program,
        args,
        move || {
            host_git(
                &race_remote,
                &["update-ref", "refs/heads/main", &race_before],
            );
        },
        |result| assert_ne!(result.as_ref().expect("real Git").code, Some(0)),
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .fx(Arc::new(fx))
        .build();
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let journal = gh.journal(&slug).expect("journal");
    let result = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .expect("known refusal")
        .into_result();
    let error = result.expect_err("stale lease");
    assert_eq!(error.kind, Kind::Conflict, "{}", error.why);
    assert_eq!(error.side_effect, SideEffect::None);
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), before);
    assert_eq!(host_git(&remote, &["tag", "--list"]), "");
    assert!(journal.open().expect("open").is_empty());
}

#[test]
fn native_push_cli_reports_dry_run_and_journaled_success_and_validates_before_routing() {
    let world = World::canonical("native-push-cli");
    common::canonical_repos(&world.fake);
    let source = bare_repo(world.path(), "source");
    let sha = host_git(&source, &["rev-parse", "main"]);
    let remote = world.path().join("empty.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", &remote.to_string_lossy()],
    );
    let input = world.path().join("push.json");
    let write_input = |before: &str, after: &str| {
        std::fs::write(
            &input,
            aterm_json::json!([
                { "ref": "refs/heads/main", "before": before, "after": after }
            ])
            .to_string(),
        )
        .expect("input")
    };
    let run = |dry: bool| {
        let (ctx, captured) = common::ctx(world.path(), &world.fake, git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("fixture-dev/app", &remote);
        ctx.fx = Arc::new(Fx::simulated(live_start()));
        let mut args = vec![
            "repo",
            "push",
            "fixture-dev/app",
            source.to_str().expect("source"),
            "--input",
            input.to_str().expect("input"),
        ];
        if dry {
            args.push("--dry-run");
        }
        let exit = common::run_cli(&mut ctx, &args);
        (exit, captured)
    };
    write_input(ABSENT, &sha);
    let (exit, captured) = run(true);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        common::stdout_json(&captured)["outcome"],
        aterm_json::Value::from("would_send")
    );
    assert!(!world.dirs.journal().exists());
    let (exit, captured) = run(false);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let report = common::stdout_json(&captured);
    assert_eq!(report["outcome"], aterm_json::Value::from("created"));
    assert!(report["intent"].as_str().is_some());
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), sha);
    for (before, after) in [(&*sha, &*sha), (&*sha, ABSENT)] {
        write_input(before, after);
        world.fake.clear_seen();
        let (exit, captured) = run(false);
        assert_eq!(exit, 2, "{}", captured.stderr_text());
        assert!(
            world.fake.seen().is_empty(),
            "invalid batch routed or read GitHub"
        );
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
fn read_only_fetches_preserve_pushurl_refusals_and_writes_still_reject_them() {
    let world = World::canonical("fetch-write-only-config");
    let remote = bare_repo(world.path(), "remote");
    let elsewhere = bare_repo(world.path(), "elsewhere");
    let source = world.path().join("source");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            &remote.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    host_git(
        &source,
        &[
            "config",
            "remote.upstream.pushurl",
            "NEVER-PUSH-TO-UPSTREAM",
        ],
    );
    host_git(
        &source,
        &[
            "config",
            "remote.origin.pushurl",
            &elsewhere.to_string_lossy(),
        ],
    );
    let config = std::fs::read(source.join(".git/config")).unwrap();
    let head = host_git(&remote, &["rev-parse", "main"]);
    let other = host_git(&elsewhere, &["for-each-ref"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    for run in [
        GitRun::fetch(&slug, &source),
        GitRun::fetch_main(&slug, &source, "refs/agh/main"),
        GitRun::fetch_ref(&slug, &source, "refs/heads/main", "refs/agh/exact", true),
        GitRun::fetch_batch(
            &slug,
            &source,
            fetch_rows(&[("refs/heads/main", "refs/agh/batch", &head)]),
            true,
        ),
    ] {
        gh.run_git(&run).unwrap();
    }
    assert_eq!(host_git(&source, &["rev-parse", "refs/agh/batch"]), head);
    assert_eq!(host_git(&source, &["rev-parse", "HEAD"]), head);
    assert_eq!(std::fs::read(source.join(".git/config")).unwrap(), config);
    assert_eq!(host_git(&elsewhere, &["for-each-ref"]), other);
    host_git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "next"],
    );
    let after = host_git(&source, &["rev-parse", "HEAD"]);
    let error = gh
        .run_git(&GitRun::push_refs(
            &slug,
            &source,
            push_batch(&[("refs/heads/main", &head, &after)]),
            true,
        ))
        .expect_err("write screening remains strict even for a dry run")
        .into_data();
    assert_eq!(error.kind, Kind::Refused);
    assert!(matches!(error.sub, Sub::GitConfig { key } if key == "remote.*"));
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), head);
    assert_eq!(host_git(&elsewhere, &["for-each-ref"]), other);
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
    .chain([
        (
            format!("remote.{url}.url"),
            other_url.clone(),
            format!("remote.{url}.url"),
        ),
        (
            format!("remote.{url}.pushurl"),
            other_url,
            format!("remote.{url}.pushurl"),
        ),
    ])
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
                key: format!("{}.*", listed.split_once('.').unwrap().0)
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
            key: "hook.*".to_string()
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

fn configured_worktrees(world: &World) -> (Github, PathBuf, PathBuf, PathBuf) {
    let bare = bare_repo(world.path(), "app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    let main = world.path().join("clone");
    gh.run_git(&GitRun::new_clone(&slug, &main))
        .unwrap_or_else(|e| panic!("the clone: {}", e.why));
    let linked = world.path().join("linked");
    host_git(
        &main,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    host_git(
        &main,
        &["config", "--local", "extensions.worktreeConfig", "true"],
    );
    host_git(
        &main,
        &["config", "--worktree", "user.name", "main fixture"],
    );
    host_git(
        &linked,
        &["config", "--worktree", "user.name", "linked fixture"],
    );
    (gh, bare, main, linked)
}

#[test]
fn a_fetch_accepts_benign_main_and_linked_worktree_config() {
    let world = World::canonical("git-fetch-worktree-config");
    let (gh, bare, main, linked) = configured_worktrees(&world);
    let slug = common::slug("fixture-dev/app");
    // Ordinary named-remote URLs do not route this operation: agh hands git
    // the admitted URL explicitly. They remain harmless in either scope.
    // Shared config carries the reported regression; each worktree also exercises every
    // allowed variable, both plain and URL-scoped, through Git's real parser.
    host_git(
        &main,
        &["config", "--local", "http.postBuffer", "524288000"],
    );
    for dir in [&main, &linked] {
        for (var, value) in [
            ("postBuffer", "524288000"),
            ("lowSpeedLimit", "1"),
            ("lowSpeedTime", "60"),
            ("maxRequests", "2"),
            ("minSessions", "1"),
            ("keepAliveIdle", "60"),
            ("keepAliveInterval", "30"),
            ("keepAliveCount", "3"),
        ] {
            for prefix in ["http.", "http.https://github.com/fixture-dev/app.git."] {
                host_git(
                    dir,
                    &["config", "--worktree", &format!("{prefix}{var}"), value],
                );
            }
        }
        host_git(
            dir,
            &[
                "config",
                "--worktree",
                "remote.origin.url",
                "file:///not-the-admitted-remote",
            ],
        );
        host_git(
            dir,
            &["config", "--worktree", "core.sparseCheckout", "false"],
        );
    }
    let configs = [
        main.join(".git/config"),
        main.join(".git/config.worktree"),
        PathBuf::from(host_git(&linked, &["rev-parse", "--absolute-git-dir"]))
            .join("config.worktree"),
    ];
    let before: Vec<_> = configs
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let first = host_git(&bare, &["rev-parse", "main"]);
    let second = host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    host_git(&bare, &["update-ref", "refs/heads/main", &second]);
    for dir in [&main, &linked] {
        gh.run_git(&GitRun::check_local(&slug, dir))
            .unwrap_or_else(|e| panic!("local {}: {}", dir.display(), e.why));
        gh.run_git(&GitRun::fetch(&slug, dir))
            .unwrap_or_else(|e| panic!("fetch {}: {}", dir.display(), e.why));
        assert_eq!(
            host_git(dir, &["rev-parse", "refs/remotes/origin/main"]),
            second
        );
    }
    for (path, before) in configs.iter().zip(before) {
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "{} changed",
            path.display()
        );
    }
}

#[test]
fn a_fetch_refuses_http_routing_and_auth_in_shared_worktree_config() {
    let world = World::canonical("git-fetch-shared-http-screen");
    let (gh, _bare, main, linked) = configured_worktrees(&world);
    let slug = common::slug("fixture-dev/app");
    let before = [&main, &linked].map(|dir| host_git(dir, &["for-each-ref"]));
    for (key, value) in [
        ("http.proxy", "http://127.0.0.1:1"),
        ("http.extraHeader", "Authorization: Fixture shared"),
        ("http.sslCAInfo", "/nowhere"),
        ("http.curloptResolve", "github.com:443:127.0.0.1"),
    ] {
        host_git(&main, &["config", "--local", "--no-includes", key, value]);
        for (index, dir) in [&main, &linked].into_iter().enumerate() {
            for operation in [GitRun::check_local(&slug, dir), GitRun::fetch(&slug, dir)] {
                let error = gh
                    .run_git(&operation)
                    .expect_err("shared HTTP routing or authentication config")
                    .into_data();
                assert_eq!(error.kind, Kind::Refused, "{key}: {}", error.why);
                assert_eq!(
                    error.sub,
                    Sub::GitConfig {
                        key: "http.*".to_string()
                    },
                    "{key}"
                );
            }
            assert_eq!(
                host_git(dir, &["for-each-ref"]),
                before[index],
                "{key} moved {}'s refs",
                dir.display()
            );
        }
        host_git(&main, &["config", "--local", "--unset-all", key]);
    }
}

#[test]
fn a_fetch_refuses_rewrites_in_each_worktree_config_without_following_includes() {
    let world = World::canonical("git-fetch-worktree-screen");
    let (gh, _bare, main, linked) = configured_worktrees(&world);
    let slug = common::slug("fixture-dev/app");
    // If the screen followed includes rather than listing their keys, this
    // malformed file would fail parsing instead of naming include.path.
    let include = world.path().join("not-a-config");
    std::fs::write(&include, "[unterminated\n").unwrap();
    let include_name = include.to_str().unwrap();
    let rows = [
        (
            "http.extraHeader",
            "Authorization: fixture",
            "http.extraheader",
        ),
        (
            "http.https://github.com/.sslVerify",
            "false",
            "http.sslverify",
        ),
        (
            "http.https://github.com/.followRedirects",
            "true",
            "http.followredirects",
        ),
        ("http.futureOption", "true", "http.futureoption"),
        (
            "url.https://elsewhere.invalid/.insteadOf",
            "https://github.com/",
            "url.https://elsewhere.invalid/.insteadof",
        ),
        ("credential.helper", "false", "credential.helper"),
        ("include.path", include_name, "include.path"),
        (
            "includeIf.gitdir:/nowhere/.path",
            include_name,
            "includeif.gitdir:/nowhere/.path",
        ),
        (
            "remote.origin.proxy",
            "http://127.0.0.1:1",
            "remote.origin.proxy",
        ),
        (
            "remote.https://github.com/elsewhere/app.git.url",
            "file:///elsewhere",
            "remote.https://github.com/elsewhere/app.git.url",
        ),
        ("core.hooksPath", "/nowhere", "core.hookspath"),
        ("hook.x.command", "false", "hook.x.command"),
        (
            "extensions.partialClone",
            "origin",
            "extensions.partialclone",
        ),
    ];
    for dir in [&main, &linked] {
        let before = host_git(dir, &["for-each-ref"]);
        let config = PathBuf::from(host_git(dir, &["rev-parse", "--absolute-git-dir"]))
            .join("config.worktree");
        let original = std::fs::read(&config).unwrap();
        for (key, value, expected) in rows {
            host_git(dir, &["config", "--worktree", "--no-includes", key, value]);
            for operation in [GitRun::check_local(&slug, dir), GitRun::fetch(&slug, dir)] {
                let error = gh
                    .run_git(&operation)
                    .expect_err("worktree rewrite")
                    .into_data();
                assert_eq!(error.kind, Kind::Refused, "{key}: {}", error.why);
                assert_eq!(
                    error.sub,
                    Sub::GitConfig {
                        key: format!("{}.*", expected.split_once('.').unwrap().0)
                    },
                    "{key}"
                );
            }
            // Repository setup would follow a malicious include even when
            // a cleanup `git config --no-includes --unset` follows it.
            std::fs::write(&config, &original).unwrap();
            assert_eq!(host_git(dir, &["for-each-ref"]), before, "{key} moved refs");
        }
    }
}

#[test]
fn worktree_config_screen_does_not_follow_common_includes_or_inactive_worktree_files() {
    let world = World::canonical("git-common-config-screen");
    let (gh, _bare, main, _linked) = configured_worktrees(&world);
    let slug = common::slug("fixture-dev/app");
    let before = host_git(&main, &["for-each-ref"]);
    let config = main.join(".git/config");
    let original = std::fs::read(&config).unwrap();
    let include = world.path().join("bad-common-include");
    std::fs::write(&include, "[unterminated\n").unwrap();
    let mut included = original.clone();
    included.extend_from_slice(format!("\n[include]\npath = {}\n", include.display()).as_bytes());
    std::fs::write(&config, included).unwrap();
    let error = gh
        .run_git(&GitRun::check_local(&slug, &main))
        .unwrap_err()
        .into_data();
    assert_eq!(error.kind, Kind::Refused);
    assert_eq!(
        error.sub,
        Sub::GitConfig {
            key: "include.*".into()
        }
    );
    std::fs::write(&config, original).unwrap();
    host_git(
        &main,
        &["config", "--local", "extensions.worktreeConfig", "false"],
    );
    std::fs::write(main.join(".git/config.worktree"), "[unterminated\n").unwrap();
    gh.run_git(&GitRun::check_local(&slug, &main)).unwrap();
    gh.run_git(&GitRun::fetch(&slug, &main)).unwrap();
    assert_eq!(host_git(&main, &["for-each-ref"]), before);
}

#[test]
fn worktree_config_parser_recording_never_exposes_url_subsections_or_values() {
    use agh::fx::tape::{TapeHeader, TapeMode};

    let world = World::canonical("git-config-recording");
    let (_gh, _bare, main, _linked) = configured_worktrees(&world);
    let slug = common::slug("fixture-dev/app");
    let config = main.join(".git/config");
    let original = std::fs::read(&config).unwrap();
    let sentinel = "CONFIG_PRIVACY_SENTINEL_729cc1";
    for (index, extra) in [
        format!("\n[url \"https://user:{sentinel}@elsewhere.invalid/\"]\ninsteadOf = https://github.com/\n"),
        format!("\n[extensions]\nworktreeConfig = {sentinel}\n"),
        format!("\n[user]\nname = \"{sentinel}\n"),
    ].into_iter().enumerate() {
        let mut bytes = original.clone();
        bytes.extend_from_slice(extra.as_bytes());
        std::fs::write(&config, bytes).unwrap();
        let tape = world.path().join(format!("config-{index}.tape"));
        let mut header = TapeHeader::for_run(&["repo".into(), "check-local".into()], &common::endpoints(&world.fake));
        header.mode = TapeMode::Credentialed;
        let fx = Arc::new(Fx::record_for_tests(&tape, &header).unwrap());
        let gh = world.client(git_env()).fx(fx.clone()).build();
        let error = gh.run_git(&GitRun::check_local(&slug, &main)).unwrap_err();
        assert_eq!(error.kind, Kind::Refused, "{}", error.why);
        assert!(!error.why.contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
        fx.finish(error.kind.exit()).unwrap();
        let recorded = std::fs::read_to_string(&tape).unwrap();
        assert!(!recorded.contains(sentinel), "config input escaped to tape");
        assert!(recorded.contains("config-screen-v2"), "no parser exchange was recorded");
    }
    std::fs::write(config, original).unwrap();
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

fn fetch_rows(rows: &[(&str, &str, &str)]) -> FetchBatch {
    FetchBatch::new(
        rows.iter()
            .map(|(source, into, expected)| FetchRow {
                source: (*source).into(),
                into: (*into).into(),
                expected_oid: (*expected).into(),
            })
            .collect(),
    )
    .expect("admitted fixture rows")
}

#[test]
fn batch_fetch_checks_raw_tag_identities_in_one_transfer_and_preserves_the_checkout() {
    let world = World::canonical("git-batch-fetch");
    let bare = bare_repo(world.path(), "app");
    let checkout = world.path().join("checkout with spaces");
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
    host_git(&bare, &["update-ref", "refs/heads/main", &second]);
    host_git(&bare, &["tag", "-a", "v2", &second, "-m", "two"]);
    let tag = host_git(&bare, &["rev-parse", "refs/tags/v2"]);
    let rows = fetch_rows(&[
        ("refs/heads/main", "refs/agh/main", &second),
        ("refs/tags/v2", "refs/agh/tag", &tag),
    ]);
    let config = std::fs::read(checkout.join(".git/config")).unwrap();
    std::fs::write(checkout.join(".git/FETCH_HEAD"), b"kept fetch head\n").unwrap();
    std::fs::write(checkout.join("local-work"), b"kept local work\n").unwrap();
    let marker = world.path().join("hook-ran");
    let hook = checkout.join(".git/hooks/reference-transaction");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho called > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let out = gh
        .run_git(&GitRun::fetch_batch(
            &common::slug("fixture-dev/app"),
            &checkout,
            rows,
            true,
        ))
        .unwrap();
    assert_eq!((out.as_, out.grant), (None, None));
    assert_eq!(host_git(&checkout, &["rev-parse", "refs/agh/main"]), second);
    assert_eq!(host_git(&checkout, &["rev-parse", "refs/agh/tag"]), tag);
    assert_eq!(host_git(&checkout, &["rev-parse", "HEAD"]), first);
    assert_eq!(std::fs::read(checkout.join(".git/config")).unwrap(), config);
    assert_eq!(
        std::fs::read(checkout.join(".git/FETCH_HEAD")).unwrap(),
        b"kept fetch head\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("local-work")).unwrap(),
        b"kept local work\n"
    );
    assert!(
        !marker.exists(),
        "reference transaction hooks must stay disabled"
    );
    let children = gh.fx().children_run();
    let fetches = children
        .iter()
        .filter(|spec| spec.args.iter().any(|arg| arg == "fetch"))
        .collect::<Vec<_>>();
    assert_eq!(
        fetches.len(),
        2,
        "one network fetch and one local object import"
    );
    assert!(
        fetches
            .iter()
            .all(|spec| spec.args.iter().any(|arg| arg == "--stdin"))
    );
    assert!(children.iter().all(|spec| !runs_agh_s_helper(spec)));
    assert!(files(&world.dirs.grants()).is_empty());
    let third = host_git(&bare, &["commit-tree", &tree, "-p", &second, "-m", "three"]);
    host_git(&bare, &["update-ref", "refs/heads/main", &third]);
    let rows = fetch_rows(&[("refs/heads/main", "refs/agh/main", &third)]);
    gh.run_git(&GitRun::fetch_batch(
        &common::slug("fixture-dev/app"),
        &checkout,
        rows,
        true,
    ))
    .unwrap();
    assert_eq!(
        host_git(
            &checkout,
            &["reflog", "show", "--format=%H", "refs/agh/main"]
        )
        .lines()
        .collect::<Vec<_>>(),
        vec![third.as_str(), second.as_str()],
        "cache history retains the previous exact snapshot"
    );
}

#[test]
fn a_missing_or_moved_batch_source_leaves_every_cache_ref_unchanged() {
    let world = World::canonical("git-batch-source-change");
    let bare = bare_repo(world.path(), "app");
    let receiver = world.path().join("receiver.git");
    std::fs::create_dir(&receiver).unwrap();
    host_git(&receiver, &["init", "--quiet", "--bare"]);
    let first = host_git(&bare, &["rev-parse", "main"]);
    host_git(
        &receiver,
        &[
            "fetch",
            "--quiet",
            &bare.to_string_lossy(),
            "refs/heads/main:refs/agh/kept",
        ],
    );
    host_git(&bare, &["tag", "-a", "v1", &first, "-m", "raw tag"]);
    let before = host_git(&receiver, &["for-each-ref"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    for source in ["refs/tags/v1", "refs/tags/missing"] {
        let fetches_before = gh
            .fx()
            .children_run()
            .iter()
            .filter(|spec| spec.args.iter().any(|arg| arg == "fetch"))
            .count();
        let rows = fetch_rows(&[
            ("refs/heads/main", "refs/agh/new-main", &first),
            (source, "refs/agh/new-tag", &first),
        ]);
        let error = gh
            .run_git(&GitRun::fetch_batch(
                &common::slug("fixture-dev/app"),
                &receiver,
                rows,
                true,
            ))
            .expect_err("moved or missing source");
        assert_eq!(error.side_effect, SideEffect::None);
        assert_eq!(
            gh.fx()
                .children_run()
                .iter()
                .filter(|spec| spec.args.iter().any(|arg| arg == "fetch"))
                .count(),
            fetches_before + 1,
            "the actual scratch fetch must expose the source mismatch, before object import"
        );
        if source == "refs/tags/v1" {
            assert!(error.why.contains("expected raw object ID"));
        }
        assert_eq!(host_git(&receiver, &["for-each-ref"]), before);
    }
}

#[test]
fn a_cold_catalog_batch_larger_than_argv_transfers_all_1490_refs_once() {
    let world = World::canonical("git-batch-cold-catalog");
    let bare = bare_repo(world.path(), "app");
    let first = host_git(&bare, &["rev-parse", "main"]);
    std::fs::create_dir_all(bare.join("refs/tags")).unwrap();
    let mut rows = Vec::new();
    for index in 0..1490 {
        let source = format!("refs/tags/{}-{index:04}", "v".repeat(230));
        std::fs::write(bare.join(&source), format!("{first}\n")).unwrap();
        rows.push(FetchRow {
            source,
            into: format!("refs/agh/catalog-{index:04}"),
            expected_oid: first.clone(),
        });
    }
    assert!(rows.iter().map(|row| row.source.len() + 24).sum::<usize>() > 256 * 1024);
    let receiver = world.path().join("receiver.git");
    std::fs::create_dir(&receiver).unwrap();
    host_git(&receiver, &["init", "--quiet", "--bare"]);
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    gh.run_git(&GitRun::fetch_batch(
        &common::slug("fixture-dev/app"),
        &receiver,
        FetchBatch::new(rows).unwrap(),
        true,
    ))
    .unwrap();
    assert_eq!(
        host_git(
            &receiver,
            &["for-each-ref", "--format=%(objectname)", "refs/agh/"]
        )
        .lines()
        .count(),
        1490
    );
    assert!(
        host_git(
            &receiver,
            &["for-each-ref", "--format=%(objectname)", "refs/agh/"]
        )
        .lines()
        .all(|oid| oid == first)
    );
    assert_eq!(
        gh.fx()
            .children_run()
            .iter()
            .filter(|spec| spec.args.iter().any(|arg| arg == "fetch"))
            .count(),
        2
    );
}

#[test]
fn batch_fetch_cas_preserves_a_concurrent_cache_change_without_partial_updates() {
    let world = World::canonical("git-batch-cache-race");
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
    host_git(&bare, &["tag", "v1", &first]);
    let rows = fetch_rows(&[
        ("refs/heads/main", "refs/agh/main", &first),
        ("refs/tags/v1", "refs/agh/tag", &first),
    ]);
    let slug = common::slug("fixture-dev/app");
    let probe = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    probe
        .run_git(&GitRun::fetch_batch(&slug, &checkout, rows.clone(), true))
        .unwrap();
    let update = probe
        .fx()
        .children_run()
        .into_iter()
        .find(|spec| spec.args.iter().any(|arg| arg == "update-ref"))
        .unwrap();
    host_git(&checkout, &["update-ref", "-d", "refs/agh/main"]);
    host_git(&checkout, &["update-ref", "-d", "refs/agh/tag"]);
    let concurrent = checkout.clone();
    let concurrent_oid = first.clone();
    let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
        update.program,
        update.args,
        move || {
            host_git(
                &concurrent,
                &["update-ref", "refs/agh/main", &concurrent_oid],
            );
        },
        |result| {
            assert_ne!(
                result.as_ref().expect("actual ref transaction").code,
                Some(0)
            )
        },
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .fx(Arc::new(fx))
        .build();
    let error = gh
        .run_git(&GitRun::fetch_batch(&slug, &checkout, rows, true))
        .expect_err("concurrent cache write");
    assert_eq!(error.side_effect, SideEffect::None);
    assert_eq!(host_git(&checkout, &["rev-parse", "refs/agh/main"]), first);
    assert_eq!(host_git(&checkout, &["for-each-ref", "refs/agh/tag"]), "");
}

#[test]
fn batch_fetch_refuses_unsafe_cache_paths_before_any_fetch() {
    use std::os::unix::fs::symlink;
    for hard_link in [false, true] {
        let world = World::canonical("git-batch-cache-path");
        let bare = bare_repo(world.path(), "app");
        let receiver = world.path().join("receiver.git");
        std::fs::create_dir(&receiver).unwrap();
        host_git(&receiver, &["init", "--quiet", "--bare"]);
        let first = host_git(&bare, &["rev-parse", "main"]);
        let outside = world.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let kept = outside.join("kept");
        std::fs::write(&kept, b"kept external bytes\n").unwrap();
        if hard_link {
            std::fs::create_dir_all(receiver.join("logs/refs/agh")).unwrap();
            std::fs::hard_link(&kept, receiver.join("logs/refs/agh/main")).unwrap();
        } else {
            symlink(&outside, receiver.join("refs/agh")).unwrap();
        }
        let gh = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &bare)
            .build();
        let rows = fetch_rows(&[("refs/heads/main", "refs/agh/main", &first)]);
        let error = gh
            .run_git(&GitRun::fetch_batch(
                &common::slug("fixture-dev/app"),
                &receiver,
                rows,
                true,
            ))
            .expect_err("unsafe cache path");
        assert_eq!(error.kind, Kind::Refused);
        assert_eq!(std::fs::read(kept).unwrap(), b"kept external bytes\n");
        assert!(
            gh.fx()
                .children_run()
                .iter()
                .all(|spec| !spec.args.iter().any(|arg| arg == "fetch"))
        );
    }
}

/// What a batch fetch's scratch held right after its network transfer returned.
struct Received {
    /// The scratch's own objects, loose and packed; borrowed ones are not counted.
    objects: usize,
    /// Its `objects/info/alternates`, when it has one.
    alternates: Option<String>,
    /// The scratch repository, which must not outlive its run.
    scratch: PathBuf,
}

/// The objects `repo` holds itself: `count-objects`' loose `count` plus its `in-pack`.
fn own_objects(repo: &Path) -> usize {
    host_git(repo, &["count-objects", "-v"])
        .lines()
        .filter_map(|line| {
            line.strip_prefix("count: ")
                .or_else(|| line.strip_prefix("in-pack: "))
        })
        .map(|count| count.parse::<usize>().expect("an object count"))
        .sum()
}

/// A client whose every network fetch is measured in its scratch as soon as it returns. With
/// `cold`, that fetch's spec, the scratch's alternates line is removed just before the fetch
/// runs: the negative control.
fn measured_client(
    world: &World,
    bare: &Path,
    cold: Option<&ChildSpec>,
) -> (Github, Arc<std::sync::Mutex<Vec<Received>>>) {
    use std::sync::{Mutex, OnceLock, Weak};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let effects: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
    let (record, observed) = (Arc::clone(&seen), Arc::clone(&effects));
    let mut fx = Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
        let fx = observed.get().and_then(Weak::upgrade).expect("the effects");
        let spec = &fx.children_run()[span.ordinal];
        if spec.args.first().is_some_and(|arg| arg == "fetch") {
            let scratch = spec
                .cwd
                .clone()
                .expect("the network fetch runs in its scratch");
            record.lock().unwrap().push(Received {
                objects: own_objects(&scratch),
                alternates: std::fs::read_to_string(scratch.join("objects/info/alternates")).ok(),
                scratch,
            });
        }
    });
    if let Some(spec) = cold {
        let observed = Arc::clone(&effects);
        fx = fx.with_git_intercept_once_for_tests(
            spec.program.clone(),
            spec.args.clone(),
            move || {
                let fx = observed.get().and_then(Weak::upgrade).expect("the effects");
                let scratch = fx.children_run().last().and_then(|spec| spec.cwd.clone());
                let alternates = scratch
                    .expect("the scratch")
                    .join("objects/info/alternates");
                std::fs::remove_file(alternates).expect("an alternates line to remove");
            },
            |_| {},
        );
    }
    let fx = Arc::new(fx);
    assert!(effects.set(Arc::downgrade(&fx)).is_ok());
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", bare)
        .fx(fx)
        .build();
    (gh, seen)
}

/// The commits of [`ahead`]'s origin before upstream moves on.
const AHEAD_HISTORY: usize = 40;

/// A local origin [`AHEAD_HISTORY`] commits deep and tagged v1 to v3, three callers cloned
/// from it, then one upstream commit tagged v4 that the callers lack.
struct Ahead {
    world: World,
    bare: PathBuf,
    /// Two complete clones, the first under a path with spaces, and a shallow one.
    callers: [PathBuf; 3],
    /// The callers' main.
    tip: String,
    old_tags: [String; 3],
    new: String,
    new_tag: String,
}

fn ahead(label: &str) -> Ahead {
    let world = World::canonical(label);
    let bare = bare_repo(world.path(), "app");
    let tree = host_git(&bare, &["rev-parse", "main^{tree}"]);
    let mut tip = host_git(&bare, &["rev-parse", "main"]);
    for index in 1..AHEAD_HISTORY {
        let message = format!("commit {index}");
        tip = host_git(&bare, &["commit-tree", &tree, "-p", &tip, "-m", &message]);
        if index % 10 == 0 {
            let tag = format!("v{}", index / 10);
            host_git(&bare, &["tag", "-a", &tag, &tip, "-m", &tag]);
        }
    }
    host_git(&bare, &["update-ref", "refs/heads/main", &tip]);
    let url = format!("file://{}", bare.display());
    let callers = [
        ("caller with spaces", None),
        ("cold caller", None),
        ("shallow caller", Some("--depth=1")),
    ]
    .map(|(name, depth)| {
        let dest = world.path().join(name);
        let dest_text = dest.to_string_lossy().into_owned();
        let mut args = vec!["clone", "--quiet"];
        args.extend(depth);
        args.extend([url.as_str(), dest_text.as_str()]);
        host_git(world.path(), &args);
        dest
    });
    let old_tags = ["v1", "v2", "v3"].map(|tag| host_git(&bare, &["rev-parse", tag]));
    let new = host_git(&bare, &["commit-tree", &tree, "-p", &tip, "-m", "upstream"]);
    host_git(&bare, &["update-ref", "refs/heads/main", &new]);
    host_git(&bare, &["tag", "-a", "v4", &new, "-m", "v4"]);
    let new_tag = host_git(&bare, &["rev-parse", "v4"]);
    Ahead {
        world,
        bare,
        callers,
        tip,
        old_tags,
        new,
        new_tag,
    }
}

impl Ahead {
    /// The objects `revs` reach in the origin.
    fn objects(&self, revs: &[&str]) -> usize {
        let mut args = vec!["rev-list", "--objects"];
        args.extend(revs);
        host_git(&self.bare, &args).lines().count()
    }
}

/// The one network fetch a measured client's run made.
fn only_fetch(seen: &std::sync::Mutex<Vec<Received>>) -> Received {
    let mut seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one network fetch per run");
    seen.pop().unwrap()
}

/// The alternates line that lends `caller`'s objects.
fn lends(caller: &Path) -> String {
    let common = host_git(
        caller,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    format!(
        "{}/objects\n",
        std::fs::canonicalize(common).unwrap().display()
    )
}

/// `caller`'s cache `refs` name their objects, whose closures it holds, and it gained no
/// alternates file.
fn settled(caller: &Path, refs: &[(&str, &str)]) {
    for (reference, oid) in refs {
        assert_eq!(&host_git(caller, &["rev-parse", reference]), oid);
    }
    host_git(caller, &["rev-list", "--objects", "--glob=refs/agh/*"]);
    assert!(
        !caller.join(".git/objects/info/alternates").exists(),
        "the caller gained an alternates file"
    );
}

/// The network fetch a client ran, to replay with its alternates line removed.
fn network_fetch(gh: &Github) -> ChildSpec {
    gh.fx()
        .children_run()
        .into_iter()
        .find(|spec| spec.args.first().is_some_and(|arg| arg == "fetch"))
        .expect("the network fetch")
}

/// A batch fetch into a complete caller borrows the caller's objects, so its scratch receives
/// only what the caller lacks. Negative control: the same fetch with the scratch's alternates
/// line removed receives the whole history, about `AHEAD_HISTORY` times as many objects. A
/// shallow caller's scratch borrows nothing. No caller gains an alternates file, and no
/// scratch outlives its run.
#[test]
fn batch_fetch_borrows_a_complete_caller_s_objects_and_receives_only_what_it_lacks() {
    let ahead = ahead("git-batch-haves");
    let [warm, cold, shallow] = ahead.callers.each_ref().map(PathBuf::as_path);
    let (new, new_tag, old) = (&ahead.new, &ahead.new_tag, &ahead.old_tags);
    let rows = fetch_rows(&[
        ("refs/heads/main", "refs/agh/main", new),
        ("refs/tags/v1", "refs/agh/v1", &old[0]),
        ("refs/tags/v2", "refs/agh/v2", &old[1]),
        ("refs/tags/v3", "refs/agh/v3", &old[2]),
        ("refs/tags/v4", "refs/agh/v4", new_tag),
    ]);
    let history = ahead.objects(&["--all"]);
    let lacked = ahead.objects(&["--all", "--not", &ahead.tip, &old[0], &old[1], &old[2]]);
    assert_eq!(lacked, 2, "the upstream commit and its tag");
    assert!(history > AHEAD_HISTORY);
    let slug = common::slug("fixture-dev/app");
    let every_row = [
        ("refs/agh/main", new.as_str()),
        ("refs/agh/v1", old[0].as_str()),
        ("refs/agh/v4", new_tag.as_str()),
    ];

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, None);
    gh.run_git(&GitRun::fetch_batch(&slug, warm, rows.clone(), true))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(received.objects, lacked, "only what the caller lacked");
    assert_eq!(received.alternates, Some(lends(warm)));
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(warm, &every_row);

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, Some(&network_fetch(&gh)));
    gh.run_git(&GitRun::fetch_batch(&slug, cold, rows, true))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(
        received.objects, history,
        "the whole history, about AHEAD_HISTORY times as much"
    );
    assert_eq!(received.alternates, None);
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(cold, &every_row);

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, None);
    let main = fetch_rows(&[("refs/heads/main", "refs/agh/main", new)]);
    gh.run_git(&GitRun::fetch_batch(&slug, shallow, main, true))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(received.alternates, None, "a shallow caller lends nothing");
    assert_eq!(received.objects, ahead.objects(&["main"]));
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(shallow, &[("refs/agh/main", new)]);
}

/// An exact read borrows a complete caller's objects the same way: its scratch receives the
/// upstream commit and its tag alone. Negative control: with the scratch's alternates line
/// removed, the same read receives the tag's whole history. A shallow caller lends nothing.
#[test]
fn exact_fetch_borrows_a_complete_caller_s_objects_and_receives_only_what_it_lacks() {
    let ahead = ahead("git-exact-haves");
    let [warm, cold, shallow] = ahead.callers.each_ref().map(PathBuf::as_path);
    let slug = common::slug("fixture-dev/app");
    let read =
        |caller: &Path| GitRun::fetch_ref(&slug, caller, "refs/tags/v4", "refs/agh/exact", true);
    let closure = ahead.objects(&["refs/tags/v4"]);
    let lacked = ahead.objects(&["refs/tags/v4", "--not", &ahead.tip]);
    assert_eq!(lacked, 2, "the upstream commit and its tag");
    assert!(closure > AHEAD_HISTORY);
    let exact = [("refs/agh/exact", ahead.new_tag.as_str())];

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, None);
    gh.run_git(&read(warm))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(received.objects, lacked, "only what the caller lacked");
    assert_eq!(received.alternates, Some(lends(warm)));
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(warm, &exact);

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, Some(&network_fetch(&gh)));
    gh.run_git(&read(cold))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(received.objects, closure, "the tag's whole history");
    assert_eq!(received.alternates, None);
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(cold, &exact);

    let (gh, seen) = measured_client(&ahead.world, &ahead.bare, None);
    gh.run_git(&read(shallow))
        .unwrap_or_else(|error| panic!("{}", error.why));
    let received = only_fetch(&seen);
    assert_eq!(received.alternates, None, "a shallow caller lends nothing");
    assert_eq!(received.objects, closure);
    assert!(!received.scratch.exists(), "the scratch outlived its run");
    settled(shallow, &exact);
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

fn git_answer(code: i32, stdout: &[u8], stderr: &str) -> ChildOut {
    ChildOut {
        code: Some(code),
        stdout: stdout.to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn scripted_anonymous_git(world: &World, answers: Vec<ChildOut>) -> (Github, Arc<Fx>) {
    let fx = Arc::new(Fx::simulated(live_start()).with_git_answers_for_tests(answers));
    let gh = git_builder(
        Router::empty(),
        common::endpoints(&world.fake),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .retry(RetryPolicy::STANDARD)
    .fx(fx.clone())
    .build();
    (gh, fx)
}

#[test]
fn anonymous_git_two_504s_then_success_logs_each_retry() {
    let world = World::canonical("git-retry-504");
    let transient = "error: RPC failed; HTTP 504 curl 22 The requested URL returned error: 504\n";
    let (gh, fx) = scripted_anonymous_git(
        &world,
        vec![
            git_answer(128, b"", transient),
            git_answer(128, b"", transient),
            git_answer(0, b"abc\trefs/heads/main\n", ""),
        ],
    );
    let out = gh
        .run_git(&GitRun::public_refs(&common::slug("fixture-anon/app")))
        .expect("the third smart-HTTP read succeeds");
    assert!(out.stdout.contains("refs/heads/main"));
    assert_eq!(fx.children_run().len(), 3);
    assert_eq!(fx.retry_notes().len(), 2);
    assert!(
        fx.retry_notes()
            .iter()
            .all(|note| note.contains("HTTP 504"))
    );
    for spec in fx.children_run() {
        let progress = spec
            .progress
            .as_ref()
            .expect("anonymous network reads use an inactivity bound");
        assert_eq!(progress.window, Duration::from_secs(60));
        assert!(
            progress
                .waiting_on
                .starts_with("git ls-remote fixture-anon/app"),
            "{}",
            progress.waiting_on
        );
        assert_eq!(
            env_of(&spec, "GIT_TRACE_CURL_NO_DATA").map(OsString::as_os_str),
            Some(std::ffi::OsStr::new("1"))
        );
        assert_eq!(
            env_of(&spec, "GIT_TRACE_REDACT").map(OsString::as_os_str),
            Some(std::ffi::OsStr::new("1"))
        );
        assert!(env_of(&spec, "GIT_TRACE_CURL").is_some());
    }
}

/// A clone attempt owns a private sibling stage. Retrying clears only that stage and success
/// publishes it once. NEGATIVE CONTROL: handing Git the caller destination makes both captured
/// targets equal `destination` and fails this test.
#[test]
fn anonymous_clone_retries_in_a_task_owned_stage_then_publishes() {
    let world = World::canonical("git-clone-retry-stage");
    let transient = "error: RPC failed; HTTP 504 curl 22 The requested URL returned error: 504\n";
    let (gh, fx) = scripted_anonymous_git(
        &world,
        vec![git_answer(128, b"", transient), git_answer(0, b"", "")],
    );
    let destination = world.path().join("destination");

    gh.run_git(&GitRun::new_clone(
        &common::slug("fixture-anon/app"),
        &destination,
    ))
    .expect("the retried clone publishes its stage");

    let children = fx.children_run();
    assert_eq!(children.len(), 2);
    let targets: Vec<PathBuf> = children
        .iter()
        .map(|spec| PathBuf::from(spec.args.last().expect("clone destination").as_os_str()))
        .collect();
    assert_eq!(targets[0], targets[1], "one owned stage spans the retries");
    assert_ne!(targets[0], destination, "Git never writes the caller path");
    assert_eq!(targets[0].parent(), destination.parent());
    assert!(
        destination.is_dir(),
        "success atomically published the stage"
    );
    assert!(!targets[0].exists(), "the staging name moved into place");
    assert_eq!(fx.retry_notes().len(), 1);
}

/// The automatic progress window is confined to credential-free network reads. A routed read
/// keeps the caller's wall bound, which also remains the credential grant lifetime.
#[test]
fn authenticated_git_keeps_its_wall_bound() {
    let world = World::canonical("git-authenticated-wall-bound");
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_answers_for_tests(vec![git_answer(
            0,
            b"abc\trefs/heads/main\n",
            "",
        )]),
    );
    let gh = git_builder(
        world.router.clone(),
        common::endpoints(&world.fake),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .fx(fx.clone())
    .build();
    let wall = Duration::from_secs(4 * 60 * 60);
    gh.run_git(&GitRun::ls_remote(&common::slug("fixture-dev/app"), None).with_budget(wall))
        .expect("the routed read succeeds");

    let children = fx.children_run();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].budget, wall);
    assert_eq!(children[0].progress, None);
}

#[test]
fn anonymous_git_404_is_final_and_does_not_consume_the_next_answer() {
    let world = World::canonical("git-retry-404");
    let (gh, fx) = scripted_anonymous_git(
        &world,
        vec![
            git_answer(
                128,
                b"",
                "fatal: unable to access 'https://github.com/x/y.git/': The requested URL returned error: 404\n",
            ),
            git_answer(0, b"abc\trefs/heads/main\n", ""),
        ],
    );
    let error = gh
        .run_git(&GitRun::public_refs(&common::slug("fixture-anon/app")))
        .expect_err("404 is final");
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(fx.children_run().len(), 1);
    assert_eq!(fx.git_answers_remaining_for_tests(), Some(1));
    assert!(fx.retry_notes().is_empty());
}

#[test]
fn anonymous_git_endless_5xx_stops_at_and_names_the_shared_bound() {
    let world = World::canonical("git-retry-bound");
    let transient = "error: RPC failed; HTTP 503 curl 22 The requested URL returned error: 503\n";
    let mut answers = vec![git_answer(128, b"", transient); 19];
    answers.push(git_answer(0, b"abc\trefs/heads/main\n", ""));
    let (gh, fx) = scripted_anonymous_git(&world, answers);
    let error = gh
        .run_git(&GitRun::public_refs(&common::slug("fixture-anon/app")))
        .expect_err("the bound stops an endless outage");
    assert!(
        error
            .why
            .contains("19 attempts / 200s total retry-pause bound"),
        "{}",
        error.why
    );
    assert_eq!(fx.children_run().len(), 19);
    assert_eq!(fx.git_answers_remaining_for_tests(), Some(1));
    assert_eq!(fx.retry_notes().len(), 18);
}

#[test]
fn anonymous_git_connection_reset_is_transient() {
    let world = World::canonical("git-retry-reset");
    let (gh, fx) = scripted_anonymous_git(
        &world,
        vec![
            git_answer(
                128,
                b"",
                "error: RPC failed; curl 56 Recv failure: Connection reset by peer\n",
            ),
            git_answer(0, b"abc\trefs/heads/main\n", ""),
        ],
    );
    gh.run_git(&GitRun::public_refs(&common::slug("fixture-anon/app")))
        .expect("the reset is retried");
    assert_eq!(fx.children_run().len(), 2);
    assert_eq!(fx.retry_notes().len(), 1);
    assert!(fx.retry_notes()[0].contains("connection reset"));
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

/// An ambient token is not a routing input and cannot reach routed Git. The reader token in
/// agh.toml answers the native ref refresh, while both planted ambient variables are absent
/// from the Git child and terminal prompting is disabled. NEGATIVE CONTROL: routing the HTTP
/// exchange with either planted token cannot authenticate as `fixture-reader`.
#[test]
fn dead_ambient_tokens_do_not_override_a_routed_refresh() {
    use agh_fake::git_backend::{GitHttpBackend, Identity, Service};

    let world = reader_world("git-dead-ambient");
    let seed = bare_repo(world.path(), "ambient-seed");
    let main = host_git(&seed, &["rev-parse", "main"]);
    let backend = GitHttpBackend::start(
        "fixture-dev/app",
        &seed,
        &agh_fake::token("reader"),
        &agh_fake::token("dev"),
    )
    .expect("the private Git fixture");
    let target = world.path().join("target.git");
    std::fs::create_dir_all(&target).expect("the target repository");
    host_git(&target, &["init", "--quiet", "--bare"]);
    let dead = agh_fake::token("dead-ambient");
    let env = git_env()
        .with("GH_TOKEN", &dead)
        .with("GITHUB_TOKEN", &dead);
    let ports = world.fake.ports();
    let gh = git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, backend.port(), ports.cdn),
        &world.dirs,
        env,
        live_start(),
    )
    .build();
    let window = Duration::from_secs(60);
    let out = gh
        .run_git(
            &GitRun::fetch_main(&common::slug("fixture-dev/app"), &target, "refs/agh/main")
                .with_progress_window(window),
        )
        .unwrap_or_else(|error| panic!("the routed refresh: {}", error.why));
    assert_eq!(out.as_.as_deref(), Some("reader"));
    assert_eq!(host_git(&target, &["rev-parse", "refs/agh/main"]), main);
    let seen = backend.seen();
    assert!(
        seen.iter()
            .any(|request| request.identity == Identity::Reader
                && request.service == Some(Service::Upload)
                && request.backend_attempted),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .all(|request| request.identity != Identity::Unknown),
        "an ambient credential reached the fixture: {seen:?}"
    );
    let children = gh.fx().children_run();
    assert!(!children.is_empty(), "the routed refresh ran no Git child");
    for child in &children {
        assert!(env_of(child, "GH_TOKEN").is_none(), "{child:?}");
        assert!(env_of(child, "GITHUB_TOKEN").is_none(), "{child:?}");
        assert_eq!(
            env_of(child, "GIT_TERMINAL_PROMPT").map(OsString::as_os_str),
            Some(std::ffi::OsStr::new("0")),
            "{child:?}"
        );
        assert_eq!(
            child.progress.as_ref().map(|progress| progress.window),
            Some(window),
            "{child:?}"
        );
    }
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
fn native_http_objects_atomic_refusal_and_lost_reply_use_real_git_and_helper() {
    use agh::journal::IntentStore as _;
    use agh::policy::{RouteRequest, RouteTarget};
    use agh_fake::git_backend::{GitHttpBackend, Identity, Service};

    let world = reader_world("native-http-objects");
    let seed = bare_repo(world.path(), "seed");
    let before = host_git(&seed, &["rev-parse", "main"]);
    let source = world.path().join("source");
    host_git(
        world.path(),
        &[
            "clone",
            "--quiet",
            &seed.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    std::fs::write(source.join("record"), b"synthetic object body\n").unwrap();
    host_git(&source, &["add", "record"]);
    host_git(&source, &["commit", "--quiet", "-m", "new object"]);
    host_git(&source, &["tag", "-a", "v1", "-m", "exact annotated tag"]);
    let after = host_git(&source, &["rev-parse", "main"]);
    let tag = host_git(&source, &["rev-parse", "v1"]);
    let tag_bytes = host_git(&source, &["cat-file", "tag", &tag]);
    let config = std::fs::read(source.join(".git/config")).unwrap();
    let source_refs = host_git(&source, &["show-ref"]);
    let backend = GitHttpBackend::start(
        "fixture-dev/app",
        &seed,
        &agh_fake::token("reader"),
        &agh_fake::token("dev"),
    )
    .unwrap();
    let ports = world.fake.ports();
    let gh = git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, backend.port(), ports.cdn),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .build();
    let slug = common::slug("fixture-dev/app");
    let read = gh.run_git(&GitRun::ls_remote(&slug, None)).unwrap();
    assert_eq!(read.as_.as_deref(), Some("reader"));
    assert!(read.stdout.contains(&before));
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let batch = push_batch(&[
        ("refs/heads/main", &before, &after),
        ("refs/tags/v1", ABSENT, &tag),
    ]);
    gh.refs(&slug)
        .check_push(&permit, &source, &batch)
        .expect("real atomic capability");
    let journal = gh.journal(&slug).unwrap();
    assert!(journal.open().unwrap().is_empty());
    assert_eq!(
        host_git(backend.repository(), &["show-ref"]),
        host_git(&seed, &["show-ref"])
    );

    // Force an actual receive-pack transaction failure after object transfer. A
    // client-side stale lease alone cannot prove server-side atomic rollback.
    let lock = backend.repository().join("refs/heads/main.lock");
    std::fs::write(&lock, b"owned fixture lock\n").unwrap();
    let refused = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .unwrap()
        .into_result()
        .expect_err("receive transaction made no progress while its ref stayed locked");
    assert_eq!(refused.kind, Kind::CouldNotRun);
    assert!(refused.why.contains("no remote ref progress"));
    assert_eq!(
        host_git(backend.repository(), &["show-ref"]),
        host_git(&seed, &["show-ref"])
    );
    assert!(journal.open().unwrap().is_empty());
    assert!(
        backend
            .seen()
            .iter()
            .any(|r| r.post && r.service == Some(Service::Receive) && r.backend_attempted)
    );
    std::fs::remove_file(&lock).unwrap();

    gh.refs(&slug)
        .push_atomic(&permit, &source, &batch, &journal)
        .unwrap()
        .into_result()
        .expect("acknowledged real atomic push");
    assert_eq!(
        host_git(backend.repository(), &["rev-parse", "main"]),
        after
    );
    assert_eq!(
        host_git(backend.repository(), &["cat-file", "tag", &tag]),
        tag_bytes
    );
    assert!(journal.open().unwrap().is_empty());
    let received = world.path().join("received");
    let clone = gh
        .run_git(&GitRun::new_clone(&slug, &received))
        .expect("real upload-pack objects");
    assert_eq!(clone.as_.as_deref(), Some("reader"));
    assert_eq!(
        std::fs::read(received.join("record")).unwrap(),
        b"synthetic object body\n"
    );
    assert_eq!(host_git(&received, &["cat-file", "tag", &tag]), tag_bytes);
    let refs_before_stale = host_git(backend.repository(), &["show-ref"]);
    let posts = || {
        backend
            .seen()
            .iter()
            .filter(|r| r.post && r.service == Some(Service::Receive))
            .count()
    };
    let posts_before = posts();
    let stale = gh
        .run_git(&GitRun::push_refs(
            &slug,
            &source,
            push_batch(&[
                ("refs/heads/main", &before, &after),
                ("refs/attest/unwanted", ABSENT, &after),
            ]),
            false,
        ))
        .expect_err("stale lease");
    assert_eq!(stale.kind, Kind::Conflict);
    assert_eq!(posts(), posts_before);
    assert_eq!(
        host_git(backend.repository(), &["show-ref"]),
        refs_before_stale
    );
    assert_eq!(host_git(&source, &["show-ref"]), source_refs);
    assert_eq!(std::fs::read(source.join(".git/config")).unwrap(), config);
    assert!(!source.join(".git/FETCH_HEAD").exists());

    std::fs::write(source.join("record"), b"second synthetic body\n").unwrap();
    host_git(&source, &["commit", "--quiet", "-am", "next object"]);
    let next = host_git(&source, &["rev-parse", "main"]);
    let next_batch = push_batch(&[
        ("refs/heads/main", &after, &next),
        ("refs/attest/next", ABSENT, &next),
    ]);
    let before_loss_posts = posts();
    backend.lose_next_receive_response();
    let outcome = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &next_batch, &journal)
        .unwrap();
    outcome
        .into_result()
        .expect("fresh remote AFTER resolves the lost response");
    assert!(journal.open().unwrap().is_empty());
    let after_loss_posts = posts();
    assert_eq!(after_loss_posts, before_loss_posts + 1);

    // Independently confirm the runner's recovery using authenticated HTTP
    // discovery. No receive request may be repeated after the complete AFTER.
    let discovered = gh.run_git(&GitRun::ls_remote(&slug, None)).unwrap();
    assert_eq!(discovered.as_.as_deref(), Some("reader"));
    let refs: std::collections::BTreeMap<_, _> = discovered
        .stdout
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(oid, name)| (name, oid)))
        .collect();
    for update in next_batch.updates() {
        assert_eq!(
            refs.get(update.reference.as_str()).copied(),
            Some(update.after.as_str())
        );
    }
    assert!(journal.open().unwrap().is_empty());
    assert_eq!(posts(), after_loss_posts);
    assert_eq!(host_git(&seed, &["rev-parse", "main"]), before);
    assert!(files(&world.dirs.grants()).is_empty());
    let seen = backend.seen();
    assert!(
        seen.iter().any(|r| r.post
            && r.service == Some(Service::Upload)
            && r.identity == Identity::Reader)
    );
    assert!(
        seen.iter()
            .filter(|r| r.backend_attempted && r.service == Some(Service::Receive))
            .all(|r| r.identity == Identity::Writer)
    );
    assert_eq!(seen.iter().filter(|r| r.response_dropped).count(), 1);
    assert!(
        seen.iter()
            .filter(|r| r.identity == Identity::Anonymous)
            .all(|r| !r.backend_attempted && r.status == 401)
    );
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
fn a_dead_routed_credential_is_unauthenticated_named_and_redacted() {
    let world = World::canonical("git-dead-routed");
    world.fake.add_repo("fixture-dev/app", RepoSpec::private());
    world
        .fake
        .set_branch("fixture-dev/app", "main", common::MAIN_SHA);
    let token = agh_fake::token("dead-routed");
    common::write_token(&world.path().join("tokens"), "dev.token", &token);
    let basic = agh_fake::basic(&token);
    let gh = world.client(git_env()).build();
    let err = gh
        .run_git(&GitRun::ls_remote(&common::slug("fixture-dev/app"), None))
        .expect_err("the dev token has no pull");
    let printed = format!("{err:?}");
    let err = err.into_data();
    assert_eq!(err.kind, Kind::Unauthenticated, "{}", err.why);
    assert_eq!(err.kind.exit(), 4);
    assert!(err.why.contains("routed identity dev"), "{}", err.why);
    for text in [&err.why, &err.next, &printed] {
        assert!(!text.contains(&token), "a token in {text}");
        assert!(!text.contains(&basic), "a credential in {text}");
    }
    assert!(
        git_seen(&world.fake, "fixture-dev/app")
            .iter()
            .any(|request| request.had_authorization && request.as_login.is_none()),
        "the helper did not answer the dead routed token"
    );
    let dir = world.path().join("refused");
    let (exit, stderr) = cli_clone(&world, "fixture-dev/app", &dir, live_start());
    assert_eq!(exit, 4, "{stderr}");
    assert!(stderr.contains("routed identity dev"), "{stderr}");
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

/// A release caller can replace the ordinary wall budget with one inactivity window. Every
/// native child of the operation receives it, including the source-admission reads before the
/// main Git call, and its diagnostic label contains only the canonical operation vocabulary.
#[test]
fn a_progress_window_reaches_every_native_git_child() {
    let world = World::canonical("git-progress-window");
    let source = bare_repo(world.path(), "source");
    let slug = common::slug("fixture-dev/app");
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &source)
        .build();
    let window = Duration::from_secs(60);
    gh.run_git(&GitRun::check_local(&slug, &source).with_progress_window(window))
        .expect("the admitted local source");

    let children = gh.fx().children_run();
    assert!(!children.is_empty());
    let mut saw_operation = false;
    for child in children {
        let progress = child.progress.expect("progress-bound native child");
        assert_eq!(progress.window, window);
        saw_operation |= progress
            .waiting_on
            .starts_with("git check-local fixture-dev/app");
        assert!(
            progress.waiting_on.starts_with("git "),
            "{}",
            progress.waiting_on
        );
        assert!(!progress.waiting_on.contains(source.to_str().unwrap()));
    }
    assert!(saw_operation, "the main canonical operation was dispatched");
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

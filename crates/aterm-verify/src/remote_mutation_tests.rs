// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The admitted transport retains a real native owner through journal settlement.

use super::*;
use agh::config::{self, Dirs};
use agh::fx::Fx;
use agh::git::HelperCommand;
use agh::github::refs::{AtomicRefBatch, RefUpdate};
use agh::identity::MapEnv;
use agh::journal::{
    Intent, IntentDraft, IntentId, IntentStore, IntentView, Journal, JournalEnd, JournalRecord,
    Settled,
};
use agh::policy::{RouteRequest, RouteTarget, Router};
use agh::retry::RetryPolicy;
use agh::wire::Endpoints;
use agh::wire::http::HttpTransport;
use agh::write::{Outcome, WriteOp};
use agh::{GithubBuilder, Kind, SideEffect, Slug};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ROOT: &str = "ATERM_TEST_MUTATION_ROOT";
const MODE: &str = "ATERM_TEST_MUTATION_MODE";
const PARENT: &str = "ATERM_TEST_MUTATION_PARENT";
const CASE: &str = "remote::mutation::tests::an_admitted_native_writer_settles_before_transport_cancellation_returns";
const APP: &str = "fixture-dev/app";
const HANG: Duration = Duration::from_secs(60);
const LIMIT: usize = 16 * 1024;

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn host_git(program: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .current_dir(cwd)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture");
    let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
        &mut command,
        HANG,
        "native mutation fixture Git",
        1024 * 1024,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct PauseAfterBegin<'a> {
    journal: &'a Journal,
    stop: Arc<AtomicBool>,
    root: &'a Path,
    overflow: bool,
    parent: u32,
}

impl IntentStore for PauseAfterBegin<'_> {
    fn begin(&self, draft: &IntentDraft) -> agh::Result<IntentId> {
        let id = self.journal.begin(draft)?;
        assert_eq!(self.journal.open()?.len(), 1);
        std::fs::write(self.root.join("begun"), id.as_str()).unwrap();
        // The capture-error control closes output before cancellation. Those failures must
        // not unwind the native owner before it can return through intent settlement.
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(b"native-intent-begun\n");
        let _ = stdout.flush();
        if self.overflow {
            let _ = stdout.write_all(&vec![b'x'; LIMIT * 2]);
            let _ = stdout.flush();
        }
        drop(stdout);
        aterm_spec::wait::until_while_working(self.parent, "native owner cancellation", || {
            self.stop.load(Ordering::Acquire).then_some(())
        });
        Ok(id)
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> agh::Result<()> {
        self.journal.settle(id, outcome)
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> agh::Result<()> {
        self.journal.observe(id, found)
    }

    fn open(&self) -> agh::Result<Vec<Intent>> {
        self.journal.open()
    }

    fn latest(&self, op: WriteOp, key: &str) -> agh::Result<Option<IntentView>> {
        self.journal.latest(op, key)
    }
}

fn native_owner(root: &Path, mode: &str, stop: Arc<AtomicBool>) {
    let path = std::env::var_os("PATH").unwrap();
    let git = std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("git"))
        .find(|program| program.is_file())
        .expect("host Git");
    let source = root.join("source.git");
    let remote = root.join("remote.git");
    for repository in [&source, &remote] {
        host_git(
            &git,
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--template=",
                repository.to_str().unwrap(),
            ],
        );
    }
    let tree = host_git(&git, &source, &["mktree"]);
    let after = host_git(&git, &source, &["commit-tree", &tree, "-m", "fixture"]);
    host_git(&git, &source, &["update-ref", "refs/heads/main", &after]);
    let dirs = Dirs::under(root);
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    let token_dir = root.join("tokens");
    aterm_uds::private_file::create_private_dir(&token_dir).unwrap();
    let token = agh_fake::token("native-cancellation");
    let token_file = token_dir.join("dev.token");
    private_file(&token_file, format!("{token}\n").as_bytes());
    let token_path = token_file
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let text = format!(
        "[profiles.dev]\ntoken_file = \"{token_path}\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n\n\
         [default]\nread = [\"dev\", \"anonymous\"]\n"
    );
    private_file(&dirs.config_file(), text.as_bytes());
    let router = Router::from_validated(config::load(&dirs).unwrap().unwrap());
    let fake = agh_fake::FakeGithub::start();
    fake.add_token(&token, "fixture-dev-bot");
    fake.add_repo(APP, agh_fake::RepoSpec::private().push("fixture-dev-bot"));
    let ports = fake.ports();
    let env = MapEnv::new().with("PATH", path.to_str().unwrap());
    let fx =
        Arc::new(Fx::simulated(Fx::live().wall_secs().unwrap()).with_git_stop(Some(stop.clone())));
    let gh = GithubBuilder::new(router)
        .transport(Arc::new(HttpTransport::direct(Arc::new(env.clone()))))
        .endpoints(Endpoints::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        ))
        .env(Arc::new(env))
        .fx(fx.clone())
        .retry(RetryPolicy::NONE)
        .journal(dirs)
        // The local bare remote needs no credential callback, and cancellation comes first.
        .git_helper_for_tests(HelperCommand {
            exe: git.clone(),
            args: Vec::new(),
        })
        .git_remote_for_tests(APP, &remote)
        .build();
    let slug = Slug::parse(APP).unwrap();
    let permit = gh
        .permit(&RouteRequest::write(RouteTarget::Repo(slug.clone())))
        .unwrap();
    let journal = gh.journal(&slug).unwrap();
    let intents = PauseAfterBegin {
        journal: &journal,
        stop,
        root,
        overflow: mode == "overflow",
        parent: std::env::var(PARENT).unwrap().parse().unwrap(),
    };
    let batch = AtomicRefBatch::new(vec![RefUpdate {
        reference: "refs/heads/main".into(),
        before: "0".repeat(40),
        after,
    }])
    .unwrap();
    let result = gh
        .refs(&slug)
        .push_atomic(&permit, &source, &batch, &intents)
        .unwrap();
    let Outcome::NeverSent(error) = result else {
        panic!("cancelled native owner must settle its unsent write")
    };
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Interrupted, SideEffect::None)
    );
    assert!(journal.open().unwrap().is_empty());
    let children = fx.children_run();
    assert!(!children.is_empty());
    assert!(
        children
            .iter()
            .all(|child| !child.args.iter().any(|arg| arg == "push"))
    );
    assert_eq!(host_git(&git, &remote, &["for-each-ref"]), "");
    std::fs::write(root.join("settled"), b"never-sent").unwrap();
}

#[test]
fn an_admitted_native_writer_settles_before_transport_cancellation_returns() {
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let mode = std::env::var(MODE).unwrap();
        assert!(matches!(
            mode.as_str(),
            "interrupt" | "deadline" | "overflow" | "capture-error"
        ));
        // SAFETY: this isolated fixture exclusively owns the inherited cancellation socket
        // at stdin; no thread or callback reads, closes or replaces that descriptor.
        let socket = unsafe { aterm_uds::spawnfd::adopt_inherited_socket(0) }.unwrap();
        crate::remote_cli::cancellation(aterm_uds::CtlStream::from(socket), |stop| {
            native_owner(&root, &mode, stop)
        })
        .unwrap();
        return;
    }
    for mode in ["interrupt", "deadline", "overflow", "capture-error"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let begun = root.path().join("begun");
        let triggered = Cell::new(false);
        let raw_interrupt = AtomicBool::new(false);
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", CASE, "--nocapture", "--test-threads=1"])
            .env(ROOT, root.path())
            .env(MODE, mode)
            .env(PARENT, std::process::id().to_string());
        let start = Instant::now();
        let until = start + HANG;
        let mut stopped = || {
            assert!(crate::exec::group::held());
            if mode == "interrupt" && begun.exists() {
                triggered.set(true);
                raw_interrupt.store(true, Ordering::Release);
            }
            raw_interrupt.load(Ordering::Acquire)
        };
        let mut now = || {
            if mode == "deadline" && begun.exists() {
                triggered.set(true);
                until
            } else {
                start
            }
        };
        let mut observe = |_: ChildOutput, _: &[u8]| {
            if begun.exists() && matches!(mode, "overflow" | "capture-error") {
                triggered.set(true);
                if mode == "capture-error" {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            }
            Ok(())
        };
        let result = capture_with(
            &mut command,
            until,
            &mut stopped,
            LIMIT,
            &mut now,
            &mut observe,
        );
        assert!(result.is_err(), "{mode}: {result:?}");
        assert!(
            triggered.get(),
            "{mode}: cancellation followed durable begin"
        );
        assert_eq!(
            std::fs::read(root.path().join("settled")).unwrap(),
            b"never-sent",
            "{mode}"
        );
        let fx = Arc::new(Fx::live());
        let journal =
            Journal::open(&Dirs::under(root.path()), &Slug::parse(APP).unwrap(), &fx).unwrap();
        let (records, end) = journal.read().unwrap();
        assert_eq!(end, JournalEnd::Clean);
        assert!(
            matches!(
                records.as_slice(),
                [
                    (_, JournalRecord::Intent(_)),
                    (
                        _,
                        JournalRecord::Settled {
                            outcome: Settled::NeverSent,
                            ..
                        }
                    )
                ]
            ),
            "{mode}: {records:?}"
        );
        assert!(journal.open().unwrap().is_empty());
    }
}

#[test]
fn an_expired_mutation_budget_refuses_before_spawn_while_a_live_budget_runs_the_command() {
    let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    for expired in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let marker = root.path().join("ran");
        let mut command = aterm_uds::spawnfd::command(&shell);
        command
            .args(["-c", r#"printf admitted > "$1""#, "mutation-admission"])
            .arg(&marker);
        let start = Instant::now();
        let until = start + HANG;
        let result = capture_with(
            &mut command,
            until,
            &mut || {
                assert!(crate::exec::group::held());
                false
            },
            LIMIT,
            &mut || if expired { until } else { start },
            &mut |_, _| Ok(()),
        );
        if expired {
            assert!(result.is_err());
            assert!(!marker.exists());
        } else {
            assert!(result.unwrap().status.success());
            assert_eq!(std::fs::read(marker).unwrap(), b"admitted");
        }
    }
}

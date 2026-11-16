// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Cooperative cancellation after the real native-write journal's durable begin.

use super::*;
use crate::git::HelperCommand;
use crate::github::refs::{AtomicRefBatch, RefUpdate};
use crate::journal::{Journal, JournalEnd, JournalRecord};
use std::sync::atomic::{AtomicBool, Ordering};

struct StopAfterBegin<'a> {
    journal: &'a Journal,
    stop: Arc<AtomicBool>,
    begun: AtomicBool,
}

impl IntentStore for StopAfterBegin<'_> {
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId> {
        let id = self.journal.begin(draft)?;
        assert_eq!(self.journal.open()?.len(), 1);
        self.begun.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        Ok(id)
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()> {
        self.journal.settle(id, outcome)
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()> {
        self.journal.observe(id, found)
    }

    fn open(&self) -> Result<Vec<Intent>> {
        self.journal.open()
    }

    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>> {
        self.journal.latest(op, key)
    }
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
        Duration::from_secs(60),
        "native stop fixture Git",
        1024 * 1024,
    )
    .expect("owned fixture Git completes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output")
        .trim()
        .into()
}

#[test]
fn native_stop_after_durable_begin_settles_the_real_journal_without_dispatching_a_push() {
    let root = scratch();
    let path = std::env::var_os("PATH").expect("host PATH");
    let git = std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("git"))
        .find(|program| program.is_file())
        .expect("host Git");
    let source = root.path().join("source.git");
    let remote = root.path().join("remote.git");
    for repository in [&source, &remote] {
        host_git(
            &git,
            root.path(),
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

    drop(canonical(root.path()));
    let dirs = Dirs::under(root.path());
    crate::test_fixtures::write_config(&dirs, &crate::test_fixtures::canonical_text(root.path()));
    let router = Router::from_validated(config::load(&dirs).unwrap().unwrap());
    let fake = fake();
    let env = MapEnv::new().with("PATH", path.to_str().expect("UTF-8 host PATH"));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Fx::live().wall_secs().unwrap();
    let fx = Arc::new(Fx::simulated(start).with_git_stop(Some(stop.clone())));
    let gh = builder(
        router,
        Arc::new(HttpTransport::direct(Arc::new(env.clone()))),
        endpoints(&fake),
    )
    .env(Arc::new(env))
    .fx(fx.clone())
    .journal(dirs)
    // A local bare remote needs no credential callback; cancellation precedes all Git dispatch.
    .git_helper_for_tests(HelperCommand {
        exe: git.clone(),
        args: Vec::new(),
    })
    .git_remote_for_tests(APP, &remote)
    .build();
    let slug = slug(APP);
    let permit = permit_for(&gh, &app_write());
    let journal = gh.journal(&slug).unwrap();
    let intents = StopAfterBegin {
        journal: &journal,
        stop,
        begun: AtomicBool::new(false),
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
        panic!("a stopped native owner must settle its unsent write");
    };
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Interrupted, SideEffect::None)
    );
    assert!(intents.begun.load(Ordering::Acquire));
    let (records, end) = journal.read().unwrap();
    assert_eq!(end, JournalEnd::Clean);
    assert!(matches!(
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
    ));
    assert!(journal.open().unwrap().is_empty());
    let children = fx.children_run();
    assert!(
        !children.is_empty(),
        "the real native runner reached its Git door"
    );
    assert!(
        children
            .iter()
            .all(|child| !child.args.iter().any(|arg| arg == "push"))
    );
    assert_eq!(host_git(&git, &remote, &["for-each-ref"]), "");
}

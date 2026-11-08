// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publisher policy fixtures use the real native local-bare origin, while
//! ordinary Git remains a separate local object/worktree seam.
#![cfg(unix)]

use crate::github_remote::Remote;
use crate::ledger::{ControlRefTransaction, GitRunner, Origin, RefWrite, Result, RunOut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

pub struct Git {
    local: crate::ledger::testing::Git,
    remote: Option<Remote>,
    barrier: Option<(&'static str, Arc<Barrier>, AtomicBool)>,
}

/// Independent fixture actors may deliberately replace remote refs. Their
/// setup writes go to this explicit scratch bare path, never to GitHub.
pub fn bare_remote(repo: &Path) -> PathBuf {
    let recorded = repo.join(".git/publisher-fixture-origin");
    if let Ok(path) = std::fs::read_to_string(&recorded) {
        return PathBuf::from(path);
    }
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .output()
        .unwrap();
    assert!(out.status.success(), "fixture has no origin");
    let path = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    assert!(
        path.is_absolute() && path.is_dir(),
        "fixture origin is not a bare path"
    );
    path
}

impl Git {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        let repo = repo.into();
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(&repo)
            .args(["remote", "get-url", "origin"])
            .output()
            .unwrap();
        let remote = out.status.success().then(|| {
            let bare = bare_remote(&repo);
            std::fs::write(
                repo.join(".git/publisher-fixture-origin"),
                bare.to_str().unwrap(),
            )
            .unwrap();
            let changed = crate::ledger::testing::git()
                .arg("-C")
                .arg(&repo)
                .args([
                    "config",
                    "remote.origin.url",
                    "https://github.com/fixture/app.git",
                ])
                .status()
                .unwrap();
            assert!(changed.success());
            crate::github_remote::claim_fixture(&repo, &bare)
        });
        Self {
            local: crate::ledger::testing::Git::new(repo),
            remote,
            barrier: None,
        }
    }

    pub fn with_first_get_barrier(
        mut self,
        reference: &'static str,
        barrier: Arc<Barrier>,
    ) -> Self {
        self.barrier = Some((reference, barrier, AtomicBool::new(true)));
        self
    }

    fn origin(&self) -> &Remote {
        self.remote.as_ref().expect("this fixture has no origin")
    }
}

impl GitRunner for Git {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        assert!(
            !args
                .iter()
                .any(|word| matches!(*word, "fetch" | "push" | "ls-remote")),
            "publisher tests must use typed Origin for transport: {args:?}"
        );
        self.local.git(args)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        self.local.git_input(args, input)
    }

    fn git_env(&self, args: &[&str], env: &[(&str, &str)]) -> Result<RunOut> {
        self.local.git_env(args, env)
    }
}

impl Origin for Git {
    fn refresh_main(&self) -> std::result::Result<String, String> {
        self.origin().refresh_main()
    }

    fn get(&self, reference: &str) -> std::result::Result<Option<String>, String> {
        let answer = self.origin().get(reference)?;
        if let Some((selected, barrier, first)) = &self.barrier
            && reference == *selected
            && first.swap(false, Ordering::SeqCst)
        {
            barrier.wait();
        }
        Ok(answer)
    }

    fn import(&self, oid: &str) -> std::result::Result<(), String> {
        Origin::import(self.origin(), oid)
    }
    fn peel(&self, oid: &str) -> std::result::Result<Option<String>, String> {
        Origin::peel(self.origin(), oid)
    }
    fn tag_object(&self, oid: &str) -> std::result::Result<Vec<u8>, String> {
        self.origin().tag_object(oid)
    }
    fn write(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> std::result::Result<RefWrite, String> {
        self.origin().write_ref(reference, before, after)
    }
    fn delete(&self, reference: &str, expected_oid: &str) -> std::result::Result<RefWrite, String> {
        self.origin().delete_ref(reference, expected_oid)
    }
    fn control_transaction(
        &self,
        transaction: ControlRefTransaction,
    ) -> std::result::Result<RefWrite, String> {
        Origin::control_transaction(self.origin(), transaction)
    }
}

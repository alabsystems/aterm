// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) use aterm_uds::fixture_git::git;

/// Set by a test that starts its own test binary again with a Git environment it chose (a
/// global config, a `GIT_DIR`, a global attributes file), so the restarted binary's load-time
/// hook (the integration binary's `hermetic_git`) keeps that environment.
pub(crate) const CHOSEN_GIT_ENV: &str = "ATERM_FIXTURE_GIT_CHOSEN";

/// The [`GitRunner`](super::GitRunner) a test hands the code under test:
/// [`GitCli`](super::GitCli)'s `git -C <repo> <args…>`, run by [`git`] (the direct git,
/// isolated from this machine) rather than the trampolined `git` on PATH.
pub(crate) struct Git {
    repo: PathBuf,
}

impl Git {
    pub(crate) fn new(repo: impl Into<PathBuf>) -> Self {
        Git { repo: repo.into() }
    }
}

impl super::GitRunner for Git {
    fn git(&self, args: &[&str]) -> super::Result<super::RunOut> {
        self.git_env(args, &[])
    }

    fn git_env(&self, args: &[&str], env: &[(&str, &str)]) -> super::Result<super::RunOut> {
        let out = git()
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .envs(env.iter().copied())
            .output()
            .map_err(|e| {
                super::Error::new(format!("failed to spawn git {}: {e}", args.join(" ")))
            })?;
        Ok(super::RunOut {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> super::Result<super::RunOut> {
        let mut cmd = git();
        cmd.arg("-C").arg(&self.repo);
        super::run_with_input(cmd, args, input)
    }
}

/// `git init -q -b main <dir>`, then [`config`] there.
/// # Panics
/// When git fails: a fixture without its repository decides nothing.
pub(crate) fn init(dir: &Path, pairs: &[(&str, &str)]) {
    let out = git()
        .args(["init", "-q", "-b", "main"])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    config(dir, pairs);
}

/// Append to the config of the fresh repository at `repo`, in one write, what one
/// `git config <key> <value>` per pair would leave (each section once, in first-key
/// order; checked byte for byte against git 2.50).
///
/// # Panics
/// On a key that is not `section.name`, a section the file already holds, a value
/// git would quote, or a config that cannot be written.
pub(crate) fn config(repo: &Path, pairs: &[(&str, &str)]) {
    let file = if repo.join(".git").is_dir() {
        repo.join(".git/config")
    } else {
        repo.join("config")
    };
    let mut text = fs::read_to_string(&file).unwrap();
    let mut sections: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for &(key, value) in pairs.iter().chain([&("maintenance.auto", "false")]) {
        let (section, name) = key
            .split_once('.')
            .filter(|(_, name)| !name.contains('.'))
            .expect("a section.name key, no subsection");
        assert!(
            !value.is_empty()
                && value.trim() == value
                && !value.contains(['"', '\\', '#', ';', '\n', '\t']),
            "{key} = {value:?} is a value git would quote"
        );
        match sections.iter_mut().find(|(s, _)| *s == section) {
            Some((_, keys)) => keys.push((name, value)),
            None => sections.push((section, vec![(name, value)])),
        }
    }
    for (section, keys) in sections {
        assert!(
            !text.contains(&format!("[{section}]")),
            "[{section}] already in {}",
            file.display()
        );
        text.push_str(&format!("[{section}]\n"));
        for (name, value) in keys {
            text.push_str(&format!("\t{name} = {value}\n"));
        }
    }
    fs::write(&file, text).unwrap();
}

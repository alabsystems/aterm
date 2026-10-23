// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test stand-ins for the AI layer.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::facts::{Job, Ran, RunFailed, Runner};

/// One child the code asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The program's file name.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its environment.
    pub env: Vec<(String, String)>,
    /// Its working directory.
    pub cwd: Option<PathBuf>,
    /// The request it was fed, if any.
    pub stdin: Option<String>,
    /// Whether it ran on the terminal.
    pub terminal: bool,
}

/// What a scripted run answers.
pub type Answer = Box<dyn Fn(&Call) -> Result<Ran, RunFailed> + Send + Sync>;

/// A runner that records and answers from `answer`; a terminal run answers `exit`.
pub struct FakeRunner {
    /// Every call, in order.
    pub calls: Mutex<Vec<Call>>,
    /// The scripted answer to a captured run.
    pub answer: Answer,
    /// What a terminal run exits with, by call.
    pub exit: Box<dyn Fn(&Call) -> i32 + Send + Sync>,
}

impl FakeRunner {
    /// One that answers every run with exit 0 and no output.
    pub fn quiet() -> FakeRunner {
        FakeRunner::with(|_| Ok(ran(0, "", "")))
    }

    /// One that answers with `f`.
    pub fn with(f: impl Fn(&Call) -> Result<Ran, RunFailed> + Send + Sync + 'static) -> FakeRunner {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            answer: Box::new(f),
            exit: Box::new(|_| 0),
        }
    }

    /// The same, with terminal runs exiting as `exit` says.
    pub fn exiting(mut self, exit: impl Fn(&Call) -> i32 + Send + Sync + 'static) -> FakeRunner {
        self.exit = Box::new(exit);
        self
    }

    /// The calls so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls").clone()
    }
}

/// A run's result.
pub fn ran(exit: i32, stdout: &str, stderr: &str) -> Ran {
    Ran {
        exit: Some(exit),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn name(p: &Path) -> String {
    p.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

impl Runner for FakeRunner {
    fn run(&self, job: &Job<'_>) -> Result<Ran, RunFailed> {
        let call = Call {
            program: name(job.program),
            args: job.args.to_vec(),
            env: job.env.to_vec(),
            cwd: job.cwd.map(Path::to_path_buf),
            stdin: job.stdin.and_then(|p| std::fs::read_to_string(p).ok()),
            terminal: false,
        };
        let answer = (self.answer)(&call);
        self.calls.lock().expect("calls").push(call);
        answer
    }

    fn terminal(
        &self,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> Result<i32, String> {
        let call = Call {
            program: name(program),
            args: args.to_vec(),
            env: env.to_vec(),
            cwd: cwd.map(Path::to_path_buf),
            stdin: None,
            terminal: true,
        };
        let exit = (self.exit)(&call);
        self.calls.lock().expect("calls").push(call);
        Ok(exit)
    }
}

/// Temporary machine layout with `$HOME`, an atpkg prefix, and aterm's state root.
pub struct Box3 {
    /// Keeps the directory.
    pub dir: aterm_tempfile::TempDir,
    /// `$HOME`.
    pub home: PathBuf,
    /// atpkg's prefix.
    pub pkg: atpkg::store::Layout,
    /// aterm's state root.
    pub state: PathBuf,
}

/// Lays out a machine with shims for `installed`.
pub fn machine(installed: &[&str]) -> Box3 {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let prefix = dir.path().join("pkg");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(prefix.join("agents")).expect("agents");
    std::fs::create_dir_all(&state).expect("state");
    for id in installed {
        std::fs::write(prefix.join("agents").join(id), "#!/bin/sh\n").expect("shim");
    }
    Box3 {
        dir,
        home,
        pkg: atpkg::store::Layout { prefix },
        state,
    }
}

/// A [`super::fleet::Fetcher`] that serves fixed bodies by URL and records requests.
#[derive(Default)]
pub struct FakeFetcher {
    bodies: Mutex<Vec<(String, Vec<u8>)>>,
    asked: Mutex<Vec<String>>,
}

impl FakeFetcher {
    /// Serves `body` at `url` (replacing what it served there).
    pub fn serve(&self, url: &str, body: &[u8]) {
        let mut bodies = self.bodies.lock().expect("bodies");
        bodies.retain(|(u, _)| u != url);
        bodies.push((url.to_string(), body.to_vec()));
    }

    /// The URLs asked, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
}

impl super::fleet::Fetcher for FakeFetcher {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        self.asked.lock().expect("asked").push(url.to_string());
        let bodies = self.bodies.lock().expect("bodies");
        match bodies.iter().find(|(u, _)| u == url) {
            Some((_, b)) if b.len() as u64 > max => Err("over the cap".to_string()),
            Some((_, b)) => Ok(b.clone()),
            None => Err("unreachable".to_string()),
        }
    }
}

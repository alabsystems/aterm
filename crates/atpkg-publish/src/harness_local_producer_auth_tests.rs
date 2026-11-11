// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;

use super::*;
use crate::artifact::tools::{Ran, RunError};
use crate::host::rest::GIT_TOKEN_ENV;

struct Recording {
    calls: RefCell<Vec<Run>>,
    answer: Ran,
}

impl Recording {
    fn new(code: i32, output: &str) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            answer: Ran {
                code: Some(code),
                stdout: Vec::new(),
                stderr: output.as_bytes().to_vec(),
            },
        }
    }
}

impl Tools for Recording {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        self.calls.borrow_mut().push(run.clone());
        Ok(self.answer.clone())
    }
    fn strip(&self, _: &Path, _: bool) -> std::result::Result<(), String> {
        panic!("fetch never strips")
    }
    fn resign(&self, _: &Path) -> std::result::Result<(), String> {
        panic!("fetch never signs")
    }
    fn same_unsigned(&self, _: &Path, _: &Path) -> std::result::Result<bool, String> {
        panic!("fetch never compares images")
    }
    fn thin(&self, _: &Path, _: &str, _: &Path) -> std::result::Result<(), String> {
        panic!("fetch never thins")
    }
    fn relocate(
        &self,
        _: &Path,
        _: &[PathBuf],
    ) -> std::result::Result<crate::artifact::relocate::Report, String> {
        panic!("fetch never relocates")
    }
    fn compressor(&self) -> &dyn crate::artifact::tarwrite::Compressor {
        panic!("fetch never compresses")
    }
}

fn config(home: &Path) -> Config {
    Config::parse(&[
        "claude-code-alab".into(),
        "--checkout".into(),
        home.join("source").display().to_string(),
        "--out".into(),
        home.join("out").display().to_string(),
        "--prefix".into(),
        home.join("pkg").display().to_string(),
        "--native".into(),
        "--unverified".into(),
    ])
    .expect("explicit fixture config")
}

#[test]
fn https_fetch_uses_the_configured_credential_only_in_the_known_helper_environment() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let token = dir.path().join("credential");
    let secret = "github_pat_harness_fixture";
    std::fs::write(&token, secret).expect("fixture credential");
    let tools = Recording::new(0, "");
    fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &token,
    )
    .expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let args = run.argv();
    assert_eq!(args[..2], ["-c", "credential.helper="]);
    let helper = args
        .iter()
        .position(|arg| arg.starts_with("credential.https://github.com.helper=!"))
        .expect("known scoped helper");
    let fetch = args
        .iter()
        .position(|arg| arg == "fetch")
        .expect("actual fetch");
    assert!(helper < fetch);
    assert!(!args.iter().any(|arg| arg.contains(secret)));
    assert_eq!(run.env_of(GIT_TOKEN_ENV), Some(&secret.into()));
    assert_eq!(run.env_of("GIT_TERMINAL_PROMPT"), Some(&"0".into()));
    for name in ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"] {
        assert_eq!(run.env_of(name), Some(&"0".into()));
    }
    assert_eq!(run.env_of("GIT_TRACE_REDACT"), Some(&"1".into()));
    assert_eq!(run.env_of("GIT_ASKPASS"), Some(&"/usr/bin/false".into()));
    for name in [
        GIT_TOKEN_ENV,
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GIT_CURL_VERBOSE",
        "SHELLOPTS",
    ] {
        assert!(run.env_remove.iter().any(|entry| entry == name));
    }
    assert_eq!(run.cwd.as_ref(), Some(&config.checkout));
    assert!(run.deadline.is_some());
}

#[test]
fn absent_https_credential_starts_no_git_and_fetch_failures_redact_credentials() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let token = dir.path().join("credential");
    let tools = Recording::new(0, "");
    assert!(
        fetch_source(
            &tools,
            &config,
            "https://github.com/fixture/claude-code-alab.git",
            &token
        )
        .is_err()
    );
    assert!(tools.calls.borrow().is_empty());
    let secret = ["github_", "pat_failed_fetch_fixture"].concat();
    std::fs::write(&token, &secret).expect("fixture credential");
    let failed = Recording::new(128, &format!("remote diagnostic {secret}"));
    let error = fetch_source(
        &failed,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &token,
    )
    .expect_err("refused fetch")
    .to_string();
    assert!(!error.contains(&secret));
    assert!(error.contains("[redacted]"));
    assert_eq!(failed.calls.borrow().len(), 1);
}

#[test]
fn ssh_fetch_keeps_batch_mode_and_never_reads_the_https_credential() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    fetch_source(
        &tools,
        &config,
        "git@github.com:fixture/claude-code-alab.git",
        &dir.path().join("absent-credential"),
    )
    .expect("recorded SSH fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    assert!(run.env_of(GIT_TOKEN_ENV).is_none());
    assert_eq!(
        run.env_of("GIT_SSH_COMMAND"),
        Some(&"ssh -oBatchMode=yes -oNumberOfPasswordPrompts=0".into())
    );
    assert!(!run.argv().iter().any(|arg| arg.contains("helper=!")));
    assert_eq!(run.argv()[..2], ["-c", "credential.helper="]);
}

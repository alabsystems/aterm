// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::path::PathBuf;

const OBJECT: &str = "4444444444444444444444444444444444444444";
const RUN_ERROR: &str = "fixture runner refused the process: permission denied";

struct Probe {
    fail_at: usize,
    calls: Cell<usize>,
}

impl Probe {
    fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            calls: Cell::new(0),
        }
    }
}

impl Git for Probe {
    fn run(&self, _args: &[&OsStr], _env: &[(&str, &str)]) -> Result<Ran, String> {
        let call = self.calls.get();
        self.calls.set(call + 1);
        if call == self.fail_at {
            return Err(RUN_ERROR.to_owned());
        }
        let (status, stdout) = match call {
            0 => (129, String::new()),
            1 | 2 => (0, String::new()),
            3 => (0, format!("{OBJECT}\n")),
            4 => (128, String::new()),
            5 => (0, "blob\n".to_owned()),
            _ => panic!("unexpected probe run {call}"),
        };
        Ok(Ran {
            status: Some(status),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }

    fn scratch(&self) -> Result<Box<dyn AsRef<Path> + '_>, String> {
        Ok(Box::new(PathBuf::from("probe")))
    }

    fn trace_file(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn read_trace(&self, path: &Path, _limit: usize) -> Result<Vec<u8>, String> {
        let control = path.ends_with("control.trace");
        let code = if control { 0 } else { 128 };
        let mut events = vec![
            aterm_json::json!({"event":"version","sid":"root","evt":"4"}),
            aterm_json::json!({"event":"start","sid":"root","argv":["git","cat-file","-t",OBJECT]}),
            aterm_json::json!({"event":"cmd_name","sid":"root","name":"cat-file"}),
        ];
        if control {
            events.push(
                aterm_json::json!({"event":"child_start","sid":"root","argv":["git","fetch"]}),
            );
        }
        events.extend([
            aterm_json::json!({"event":"exit","sid":"root","code":code}),
            aterm_json::json!({"event":"atexit","sid":"root","code":code}),
        ]);
        Ok(format!(
            "{}\n",
            events
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .into_bytes())
    }
}

#[test]
fn an_option_probe_failure_keeps_the_runner_error() {
    let git = Probe::new(0);
    let why = NoLazyFetch::of(&git).expect_err("an unanswered probe admits no capability");
    assert!(why.contains("--no-lazy-fetch"), "{why}");
    assert!(why.contains(RUN_ERROR), "{why}");
    assert_eq!(git.calls.get(), 1);
}

#[test]
fn fallback_probe_failures_keep_the_step_and_runner_error() {
    for (call, step) in [
        (1, "git init"),
        (2, "git init"),
        (3, "git hash-object"),
        (4, "cat-file (protected read)"),
        (5, "cat-file (control read)"),
    ] {
        let git = Probe::new(call);
        let why = NoLazyFetch::of(&git).expect_err("an unanswered probe admits no capability");
        assert!(why.contains(step), "{step}: {why}");
        assert!(why.contains(RUN_ERROR), "{why}");
        assert_eq!(git.calls.get(), call + 1, "nothing runs after the failure");
    }
}

#[test]
fn a_completed_fallback_probe_still_admits_the_variable() {
    let git = Probe::new(usize::MAX);
    assert_eq!(NoLazyFetch::of(&git), Ok(Some(NoLazyFetch::Variable)));
    assert_eq!(git.calls.get(), 6);
}

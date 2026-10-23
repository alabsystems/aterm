// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Subprocess tests for `aterm pkg lane`.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    checkout: PathBuf,
}

const STAGE0: &str = "build/aarch64-apple-darwin/stage0/bin";

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-lane-verb-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        let checkout = root.path().join("trust");
        let touch = |dir: &Path, names: &[&str]| {
            std::fs::create_dir_all(dir).expect("fixture dir");
            for name in names {
                std::fs::write(dir.join(name), b"").expect("fixture file");
            }
        };
        touch(
            &checkout.join(STAGE0),
            &["cargo", "rustc", "targo", "trustc", "trustdoc", "trustfmt"],
        );
        touch(
            &home.join("toolchains/trust-x/bin"),
            &["cargo", "rustc", "rustdoc", "targo", "trustc", "trustdoc"],
        );
        touch(
            &home.join(".rustup/toolchains/1.97.1-aarch64-apple-darwin/bin"),
            &["cargo", "rustc", "rustdoc", "rustfmt"],
        );
        Self {
            root,
            home,
            checkout,
        }
    }

    fn lane(&self, payload: &[u8], extra: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(["lane", "--json", "tool_input.command"])
            .args(extra)
            .current_dir(&self.checkout)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("RUSTUP_TOOLCHAIN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run atpkg");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload)
            .expect("write payload");
        child.wait_with_output().expect("wait")
    }

    fn bash(&self, command: &str) -> Output {
        self.lane(&hook_payload(command), &[])
    }
}

fn hook_payload(command: &str) -> Vec<u8> {
    let mut quoted = String::from('"');
    for c in command.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    format!(
        "{{\"session_id\":\"s\",\"transcript_path\":\"/t.jsonl\",\"cwd\":\"/c\",\
         \"permission_mode\":\"default\",\"hook_event_name\":\"PreToolUse\",\
         \"tool_name\":\"Bash\",\"tool_input\":{{\"command\":{quoted},\"description\":\"d\"}}}}"
    )
    .into_bytes()
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\n--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_trust_toolchains_own_rustc_by_path_goes_through_the_wired_verb() {
    let fx = Fixture::new("bypath");
    let absolute = format!("{}/{STAGE0}/rustc -Vv", fx.checkout.display());
    for cmd in [
        absolute.as_str(),
        "build/aarch64-apple-darwin/stage0/bin/rustc -Vv",
        "./build/aarch64-apple-darwin/stage0/bin/rustc -vV 2>&1 | head -3",
        "~/toolchains/trust-x/bin/rustc -vV",
        "$HOME/toolchains/trust-x/bin/rustdoc --version",
        "rustup run trust rustc -vV",
    ] {
        let out = fx.bash(cmd);
        assert_eq!(out.status.code(), Some(0), "{cmd:?}\n{}", text(&out));
        assert!(out.stderr.is_empty(), "{cmd:?}\n{}", text(&out));
    }
    for cmd in [
        "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc -V",
        "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustdoc --version",
        "rustc -vV",
    ] {
        let out = fx.bash(cmd);
        assert_eq!(out.status.code(), Some(2), "{cmd:?}\n{}", text(&out));
    }
    let root = fx.root.path().display().to_string();
    let out = fx.lane(
        &hook_payload("build/aarch64-apple-darwin/stage0/bin/rustc -Vv"),
        &["--cwd", &root],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn the_wired_verb_answers_with_its_exit_code_and_stderr() {
    let fx = Fixture::new("answers");
    let out = fx.bash("build/aarch64-apple-darwin/stage0/bin/cargo --version");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        err.contains("build/aarch64-apple-darwin/stage0/bin/targo --version"),
        "{err}"
    );

    let out = fx.bash("cargo +1.97.1 test -p x");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(err.contains("targo trust test -p x"), "{err}");
    assert!(err.contains("targo --unverified test -p x"), "{err}");

    let out = fx.bash("ATERM_STOCK_REASON='wasm32 cell' cargo +stable build");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    let mut deep = String::from("cargo build");
    for _ in 0..9 {
        deep = format!("eval {deep}");
    }
    let out = fx.bash(&deep);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));

    let out = fx.lane(
        br#"{"tool_name":"Read","tool_input":{"file_path":"/a/cargo"}}"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(out.stderr.is_empty(), "{}", text(&out));

    let out = fx.lane(b"cargo build", &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));

    let out = fx.lane(b"", &["--verbose"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm pkg lane`, run as a PROCESS the way a guard the owner wires calls it
//! (review of 2026-09-27): a hook payload on stdin, `lane --json
//! tool_input.command`, the answer in the exit code — 0 let through, 2 refused, 1
//! unreadable — and on stderr.
//!
//! The reader's unit tests judge path-form commands against a SIMULATED directory
//! probe. The fix they pin — a Trust toolchain's own `rustc`/`rustdoc` by path is
//! not stock, its `cargo` is refused with its own `targo` — also needs the probe
//! to find the directory the way the shell would: absolute, relative to the
//! hook's working directory, or under `~/`. So these drive the dev `atpkg`
//! binary against a REAL temp tree, with a temp `HOME` and the tree's checkout as
//! the working directory, as Claude Code runs a `PreToolUse` hook.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The directories of the guard's own cases, as files on disk: a trust checkout's
/// `stage0/bin` (the Trust names of the one measured 2026-09-28, and no `rustdoc`),
/// a Trust toolchain under `~/`, and rustup's stock 1.97.1 under `~/`.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    checkout: PathBuf,
}

const STAGE0: &str = "build/aarch64-apple-darwin/stage0/bin";

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-lane-verb-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let checkout = root.join("trust");
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

    /// `atpkg lane --json tool_input.command [extra…]`, `payload` on stdin, run in
    /// the checkout.
    fn lane(&self, payload: &[u8], extra: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(["lane", "--json", "tool_input.command"])
            .args(extra)
            .current_dir(&self.checkout)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The shape Claude Code hands a `PreToolUse` hook for a Bash call; the reader
/// looks at `tool_input.command` alone.
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

/// A Trust toolchain's own `rustc`/`rustdoc` BY PATH exits 0 and says nothing,
/// wherever the directory is written from; the same names in a stock directory,
/// or read under a working directory where no Trust toolchain sits, exit 2.
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
        // No `rustdoc` twin beside rustup's stock toolchain, and no `trustc`.
        "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustdoc --version",
        "rustc -vV",
    ] {
        let out = fx.bash(cmd);
        assert_eq!(out.status.code(), Some(2), "{cmd:?}\n{}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("is stock Rust"),
            "{cmd:?}\n{}",
            text(&out)
        );
    }
    // The relative directory is read under `--cwd` when one is given: there is no
    // Trust toolchain under the fixture's root, so the same words are stock.
    let root = fx.root.display().to_string();
    let out = fx.lane(
        &hook_payload("build/aarch64-apple-darwin/stage0/bin/rustc -Vv"),
        &["--cwd", &root],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

/// Its `cargo` by path is refused — with the `targo` beside it, never as "stock
/// Rust" — and the owner's other answers hold through the process: a stock
/// command is refused with its Trust spelling, the escape lets it through with
/// one line, text nested past the reader's cap is refused, a payload with no
/// command is nothing to read, and input that is not JSON claims nothing.
#[test]
fn the_wired_verb_answers_with_its_exit_code_and_stderr() {
    let fx = Fixture::new("answers");
    let out = fx.bash("build/aarch64-apple-darwin/stage0/bin/cargo --version");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        err.contains("\n  build/aarch64-apple-darwin/stage0/bin/targo --version\n"),
        "{err}"
    );
    assert!(!err.contains("is stock Rust"), "{err}");

    let out = fx.bash("cargo +1.97.1 test -p x");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        err.starts_with("`cargo +1.97.1 test` is stock Rust."),
        "{err}"
    );
    assert!(err.contains("targo trust test -p x"), "{err}");
    assert!(err.contains("targo --unverified test -p x"), "{err}");

    let out = fx.bash("ATERM_STOCK_REASON='wasm32 cell' cargo +stable build");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("aterm: stock `cargo +stable build`"),
        "{}",
        text(&out)
    );

    let mut deep = String::from("cargo build");
    for _ in 0..9 {
        deep = format!("eval {deep}");
    }
    let out = fx.bash(&deep);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nested more than 8 levels deep"),
        "{}",
        text(&out)
    );

    let out = fx.lane(
        br#"{"tool_name":"Read","tool_input":{"file_path":"/a/cargo"}}"#,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(out.stderr.is_empty(), "{}", text(&out));

    let out = fx.lane(b"cargo build", &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
}

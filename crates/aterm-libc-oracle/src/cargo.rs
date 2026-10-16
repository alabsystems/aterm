// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Driving one oracle workspace (`libc-oracle/`, or a control's copy of it)
//! with the Trust toolchain, and reading what cargo and libtest report as data:
//! compiler errors from `--message-format json`, test verdicts from libtest's
//! JSON events.
//!
//! Every command starts from the workspace directory, so its own
//! `.cargo/config.toml` and the checkout's above it apply, and never names
//! `--target`: an explicit target withholds `[target.*]` flags from host units
//! (build scripts), so a plain host build is the one shape every unit agrees on.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::Value;
use serde::Serialize;

use crate::trust::Trust;

/// The conformance crate every build selects.
const CONFORMANCE: &str = "libc-conformance";

/// Ambient settings that would redirect the cell or replace the config's flags.
pub const AMBIENT: [&str; 5] = [
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
];

/// One compiler error, as data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// The error code (`E0308`), when the error has one.
    pub code: Option<String>,
    pub message: String,
    /// Every span's label.
    pub labels: Vec<String>,
    /// Every span's source lines.
    pub snippets: Vec<String>,
    /// Where each span points, for a reader that needs the line.
    #[serde(skip)]
    pub spans: Vec<Span>,
    /// The compiler's own rendering, for a person.
    #[serde(skip)]
    pub rendered: String,
}

/// Where one span of an error points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: String,
    /// 1-based.
    pub line: usize,
    /// The span the error is about, not one that explains it.
    pub primary: bool,
    pub label: Option<String>,
}

impl Span {
    fn from_json(span: &Value) -> Option<Self> {
        Some(Self {
            file: span.get("file_name")?.as_str()?.to_string(),
            line: usize::try_from(span.get("line_start")?.as_u64()?).ok()?,
            primary: span.get("is_primary").and_then(Value::as_bool) == Some(true),
            label: span
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

impl Diagnostic {
    fn from_message(message: &Value) -> Option<Self> {
        let level = message.get("level")?.as_str()?;
        if !level.starts_with("error") {
            return None;
        }
        let spans = message
            .get("spans")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let labels = spans
            .iter()
            .filter_map(|s| s.get("label")?.as_str().map(str::to_string))
            .collect();
        let snippets = spans
            .iter()
            .filter_map(|s| s.get("text")?.as_array())
            .flatten()
            .filter_map(|line| line.get("text")?.as_str().map(str::to_string))
            .collect();
        Some(Self {
            code: message
                .get("code")
                .and_then(|c| c.get("code")?.as_str())
                .map(str::to_string),
            message: message.get("message")?.as_str()?.to_string(),
            labels,
            snippets,
            spans: spans.iter().filter_map(Span::from_json).collect(),
            rendered: message
                .get("rendered")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Whether `ident` appears as a whole identifier in the message, a label or
    /// the source the error points at.
    #[must_use]
    pub fn names(&self, ident: &str) -> bool {
        std::iter::once(&self.message)
            .chain(&self.labels)
            .chain(&self.snippets)
            .any(|text| {
                text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|word| word == ident)
            })
    }
}

/// A test libtest reported failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedTest {
    /// The full test path (`cell_mac::runtime::behaviour_fd_set`).
    pub name: String,
    /// What it printed, its panic message included.
    pub stdout: String,
}

impl FailedTest {
    /// The test function's own name, the path's last segment.
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.name.rsplit("::").next().unwrap_or(&self.name)
    }
}

/// Why a `--locked --offline` command refused, read off cargo's stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockedFailure {
    /// `Cargo.lock` disagrees with the manifests: cargo would have to rewrite it.
    StaleLock,
    /// Resolution needs an index entry or a crate this machine has not
    /// downloaded. Also what a stale lock whose new requirement was never
    /// downloaded looks like offline; an online `fetch --locked` tells them apart.
    NotDownloaded,
    /// Anything else: cargo refused the manifests or the config.
    Refused,
}

/// What one cargo command did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub success: bool,
    /// The exit code, `None` when a signal ended it.
    pub code: Option<i32>,
    pub errors: Vec<Diagnostic>,
    pub passed: usize,
    pub failed: Vec<FailedTest>,
    pub stderr: String,
}

impl Outcome {
    /// Read cargo's and libtest's JSON lines off `stdout`.
    #[must_use]
    pub fn parse(success: bool, code: Option<i32>, stdout: &str, stderr: &str) -> Self {
        let mut outcome = Self {
            success,
            code,
            stderr: stderr.to_string(),
            ..Self::default()
        };
        for line in stdout.lines().filter(|l| l.trim_start().starts_with('{')) {
            let Ok(event) = aterm_json::from_str::<Value>(line) else {
                continue;
            };
            if event.get("reason").and_then(Value::as_str) == Some("compiler-message") {
                if let Some(d) = event.get("message").and_then(Diagnostic::from_message) {
                    outcome.errors.push(d);
                }
                continue;
            }
            if event.get("type").and_then(Value::as_str) != Some("test") {
                continue;
            }
            match event.get("event").and_then(Value::as_str) {
                Some("ok") => outcome.passed += 1,
                Some("failed") => outcome.failed.push(FailedTest {
                    name: event
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    stdout: event
                        .get("stdout")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                _ => {}
            }
        }
        outcome
    }

    /// Whether a failure was the disk filling, which says nothing about the
    /// tree. ENOSPC's sentence, so a linker's `write failed: No space left on
    /// device` counts as well as an `(os error 28)` I/O error.
    #[must_use]
    pub fn out_of_space(&self) -> bool {
        const ENOSPC: &str = "No space left on device";
        !self.success
            && (self.stderr.contains(ENOSPC)
                || self.errors.iter().any(|d| d.message.contains(ENOSPC)))
    }

    /// How a `--locked --offline` command failed; `None` when it succeeded. A
    /// command without `--locked` is never [`LockedFailure::StaleLock`].
    #[must_use]
    pub fn locked_failure(&self) -> Option<LockedFailure> {
        if self.success {
            return None;
        }
        // The stale-lock help line also names `--offline`, so it goes first.
        Some(if self.stderr.contains("because --locked was passed") {
            LockedFailure::StaleLock
        } else if self.stderr.contains("offline mode")
            || self.stderr.contains("--offline was specified")
        {
            LockedFailure::NotDownloaded
        } else {
            LockedFailure::Refused
        })
    }

    /// Whether the build failed with error `code` naming `ident`.
    #[must_use]
    pub fn has_error(&self, code: &str, ident: &str) -> bool {
        self.errors
            .iter()
            .any(|d| d.code.as_deref() == Some(code) && d.names(ident))
    }

    /// Whether the test whose function is `leaf` failed.
    #[must_use]
    pub fn test_failed(&self, leaf: &str) -> bool {
        self.failed.iter().any(|t| t.leaf() == leaf)
    }

    /// What a person needs to see of a failure: the compiler's errors, else the
    /// failed tests' output, else the end of cargo's stderr.
    #[must_use]
    pub fn detail(&self) -> String {
        const SHOWN: usize = 5;
        if !self.errors.is_empty() {
            let mut out: Vec<String> = self
                .errors
                .iter()
                .take(SHOWN)
                .map(|d| d.rendered.trim_end().to_string())
                .collect();
            if self.errors.len() > SHOWN {
                out.push(format!("… and {} more errors", self.errors.len() - SHOWN));
            }
            return out.join("\n");
        }
        if !self.failed.is_empty() {
            return self
                .failed
                .iter()
                .map(|t| format!("test {} failed:\n{}", t.name, t.stdout.trim_end()))
                .collect::<Vec<_>>()
                .join("\n");
        }
        tail(&self.stderr, 30)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "exit {code}"),
            None => f.write_str("killed by a signal"),
        }
    }
}

/// The last `lines` lines of `text`.
#[must_use]
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Which crate to document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocTarget {
    /// The pinned registry libc.
    Reference,
    /// The shim, under its second package name.
    Shim,
}

impl DocTarget {
    const fn package(self) -> &'static str {
        match self {
            Self::Reference => "libc",
            Self::Shim => "aterm-libc-under-test",
        }
    }

    const fn lib_name(self) -> &'static str {
        match self {
            Self::Reference => "libc",
            Self::Shim => "libc_shim",
        }
    }
}

/// A build of the conformance crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// Type-check: every compile-time assertion.
    Check,
    /// Compile and run: the runtime behaviour tests too.
    Test,
}

/// One oracle workspace and the target directory its builds use.
pub struct Workspace<'t> {
    trust: &'t Trust,
    dir: PathBuf,
    target: PathBuf,
}

impl<'t> Workspace<'t> {
    #[must_use]
    pub fn new(trust: &'t Trust, dir: &Path, target: &Path) -> Self {
        Self {
            trust,
            dir: dir.to_path_buf(),
            target: target.to_path_buf(),
        }
    }

    fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> io::Result<Outcome> {
        let mut command = aterm_uds::spawnfd::command(&self.trust.targo);
        command
            .args(args)
            .current_dir(&self.dir)
            .env("RUSTC", &self.trust.trustc)
            .env("RUSTDOC", &self.trust.trustdoc);
        for name in AMBIENT {
            command.env_remove(name);
        }
        let out = command.output()?;
        let outcome = Outcome::parse(
            out.status.success(),
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        );
        if outcome.out_of_space() {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!(
                    "the disk filled during the build:\n{}",
                    tail(&outcome.stderr, 5)
                ),
            ));
        }
        Ok(outcome)
    }

    /// `words`, then `--target-dir <target> --message-format json`.
    fn compiling(&self, words: &[&str]) -> Vec<OsString> {
        let mut args: Vec<OsString> = words.iter().map(OsString::from).collect();
        args.push("--target-dir".into());
        args.push(self.target.clone().into_os_string());
        args.extend(["--message-format", "json"].map(OsString::from));
        args
    }

    /// `metadata --locked --offline`: the manifests are valid and resolve to
    /// exactly `Cargo.lock`. Full resolution, not `--no-deps`, which never
    /// consults the lock.
    ///
    /// # Errors
    /// When targo cannot be started.
    pub fn metadata_locked(&self) -> io::Result<Outcome> {
        self.run(&["metadata", "--locked", "--offline", "--format-version", "1"])
    }

    /// `fetch --locked --offline`: the locked dependencies are on this machine.
    ///
    /// # Errors
    /// When targo cannot be started.
    pub fn fetch_offline(&self) -> io::Result<Outcome> {
        self.run(&["fetch", "--locked", "--offline"])
    }

    /// Build the conformance crate.
    ///
    /// # Errors
    /// When targo cannot be started.
    pub fn build(&self, build: Build) -> io::Result<Outcome> {
        let verb = match build {
            Build::Check => "check",
            Build::Test => "test",
        };
        let mut args = self.compiling(&[
            "--unverified",
            verb,
            "--locked",
            "--offline",
            "-p",
            CONFORMANCE,
            "--lib",
        ]);
        if build == Build::Test {
            args.extend(["--", "-Zunstable-options", "--format", "json"].map(OsString::from));
        }
        self.run(&args)
    }

    /// Document one crate as rustdoc JSON; the path is where it landed when the
    /// outcome succeeded.
    ///
    /// # Errors
    /// When targo cannot be started.
    pub fn rustdoc(&self, target: DocTarget) -> io::Result<(Outcome, PathBuf)> {
        let mut args = self.compiling(&[
            "--unverified",
            "rustdoc",
            "--locked",
            "--offline",
            "-p",
            target.package(),
        ]);
        if target == DocTarget::Shim {
            // The reference gets `extra_traits` through the conformance crate's
            // edge; a selected workspace package needs the flag itself.
            args.extend(["--features", "extra_traits"].map(OsString::from));
        }
        args.extend(["--", "-Zunstable-options", "--output-format", "json"].map(OsString::from));
        let outcome = self.run(&args)?;
        let path = self
            .target
            .join("doc")
            .join(format!("{}.json", target.lib_name()));
        Ok((outcome, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERROR: &str = r#"{"reason":"compiler-message","package_id":"x","message":{"rendered":"error[E0624]: method `si_status` is private\n","message":"method `si_status` is private","code":{"code":"E0624","explanation":"…"},"level":"error","spans":[{"file_name":"conformance/src/cell_mac.rs","label":"private method","text":[{"text":"const _: unsafe fn(&libc_shim::siginfo_t) -> libc_shim::c_int = libc_shim::siginfo_t::si_status;","highlight_start":1,"highlight_end":2}]}],"children":[]}}"#;

    #[test]
    fn errors_and_test_verdicts_are_read_off_the_json_lines() {
        let stdout = [
            ERROR,
            r#"{"reason":"compiler-message","message":{"message":"unused","level":"warning","spans":[]}}"#,
            r#"{"reason":"build-finished","success":false}"#,
            r#"{ "type": "test", "name": "cell_mac::runtime::behaviour_fd_zero", "event": "ok" }"#,
            r#"{ "type": "test", "name": "cell_mac::runtime::behaviour_fd_set", "event": "failed", "stdout": "FD_SET individual bit pattern differs at fd 0" }"#,
            "   Compiling libc v0.2.186",
            r#"{ "type": "suite", "event": "failed", "passed": 1, "failed": 1 }"#,
        ]
        .join("\n");
        let outcome = Outcome::parse(false, Some(101), &stdout, "");
        assert_eq!(outcome.errors.len(), 1, "warnings are not errors");
        assert!(outcome.has_error("E0624", "si_status"));
        assert!(!outcome.has_error("E0308", "si_status"));
        assert!(!outcome.has_error("E0624", "si_pid"));
        assert_eq!(outcome.passed, 1);
        assert!(outcome.test_failed("behaviour_fd_set"));
        assert!(!outcome.test_failed("behaviour_fd_zero"));
        assert_eq!(outcome.failed[0].leaf(), "behaviour_fd_set");
    }

    #[test]
    fn a_name_matches_whole_identifiers_in_the_source_it_points_at() {
        let outcome = Outcome::parse(false, Some(101), ERROR, "");
        let error = &outcome.errors[0];
        assert!(error.names("si_status"), "in the message");
        assert!(error.names("siginfo_t"), "in the snippet");
        assert!(error.names("private"), "in the label");
        assert!(!error.names("si_stat"), "a prefix is not a name");
    }

    #[test]
    fn a_full_disk_is_told_apart_from_a_failed_build() {
        let full = |stderr| Outcome::parse(false, Some(101), "", stderr).out_of_space();
        assert!(full(
            "error: failed to write `x.rlib`: No space left on device (os error 28)"
        ));
        assert!(full("ld: write failed: No space left on device"));
        assert!(!Outcome::parse(false, Some(101), ERROR, "").out_of_space());
    }

    /// cargo's own stderr for each case, captured from a copy of the oracle.
    #[test]
    fn a_locked_offline_refusal_is_classified_by_its_cause() {
        let failed = |stderr: &str| Outcome::parse(false, Some(101), "", stderr).locked_failure();
        assert_eq!(
            failed(
                "error: cannot update the lock file /x/libc-oracle/Cargo.lock because --locked \
                 was passed to prevent this\nhelp: to generate the lock file without accessing \
                 the network, remove the --locked flag and use --offline instead."
            ),
            Some(LockedFailure::StaleLock)
        );
        assert_eq!(
            failed(
                "error: no matching package named `libc` found\nlocation searched: crates.io \
                 index\nnote: offline mode (via `--offline`) can sometimes cause surprising \
                 resolution failures"
            ),
            Some(LockedFailure::NotDownloaded)
        );
        assert_eq!(
            failed(
                "error: failed to download `libc v0.2.186`\n\nCaused by:\n  attempting to make \
                 an HTTP request, but --offline was specified"
            ),
            Some(LockedFailure::NotDownloaded)
        );
        assert_eq!(
            failed(
                "error: unclosed array, expected `]`\n  --> conformance/Cargo.toml:18:12\nerror: \
                 failed to load manifest for workspace member `/x/libc-oracle/conformance`"
            ),
            Some(LockedFailure::Refused)
        );
        assert_eq!(Outcome::parse(true, Some(0), "", "").locked_failure(), None);
    }

    #[test]
    fn the_detail_prefers_errors_then_tests_then_stderr() {
        let errors = Outcome::parse(false, Some(101), ERROR, "noise");
        assert!(errors.detail().starts_with("error[E0624]"));
        let stderr = Outcome::parse(false, Some(101), "", "a\nb\nerror: linker failed");
        assert_eq!(stderr.detail(), "a\nb\nerror: linker failed");
        assert_eq!(tail("1\n2\n3", 2), "2\n3");
    }
}

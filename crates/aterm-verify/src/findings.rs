// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a failure is: one [`Finding`] per failed test when a `targo test` log
//! accounts for every one, else the failing row itself — each with the exact
//! command that reruns just it, and the few lines that say what went wrong.

use std::ffi::OsString;

use crate::exec::Cmd;
use crate::ladder::Finding;

/// What the run prints when it kills a child at its ceiling ([`crate::exec`]),
/// and for a test binary the run's ceiling left no time to start
/// ([`crate::testrun`]).
pub const CEILING_KILL: &str = "aterm-verify: TIMEOUT";

/// How many lines of a failure's own output a finding carries.
pub const MESSAGE_LINES: usize = 12;

/// The failed tests of a `targo test` child, one finding each, with cargo's
/// rerun spec and libtest's name (`-p atpkg --test atpkg_suite -- tar::ustar`).
/// Each rerun line is prefixed with the variables the child ran under
/// ([`rerun_env`]). A log that cannot account for every failure (a crashed
/// binary, a torn log, a ceiling kill) is one finding for the whole row: a list
/// that silently misses a failure is worse than no list.
#[must_use]
pub fn test_findings(label: &str, output: &str, env: &[(OsString, OsString)]) -> Vec<Finding> {
    let plain = strip(output);
    let tests = if plain.contains(CEILING_KILL) {
        None
    } else {
        crate::libtest::failed_tests(&plain)
    };
    let Some(tests) = tests else {
        return vec![row_finding(label, output)];
    };
    let prefix: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={} ",
                k.to_string_lossy(),
                shell_word(&v.to_string_lossy())
            )
        })
        .collect();
    tests
        .into_iter()
        .map(|t| Finding {
            id: one_line(&format!("{} -- {}", t.spec, t.name)),
            rerun: Some(format!(
                "{prefix}targo --unverified test {} -- {} --exact",
                t.spec,
                shell_word(&t.name)
            )),
            message: test_message(&t.block),
        })
        .collect()
}

/// The variables a rerun of a test child's failures must set: the child's own
/// (`TRAIL_LAWS_FULL`, the measuring suites' bundle) and `--test-threads`.
/// Where the build lands and which doc driver compiles change no test's
/// verdict, so those stay off the line.
#[must_use]
pub fn rerun_env(cmd: &Cmd, test_threads: Option<u32>) -> Vec<(OsString, OsString)> {
    const PLACEMENT: [&str; 3] = ["CARGO_TARGET_DIR", "CARGO_BUILD_JOBS", "RUSTDOC"];
    let mut env: Vec<(OsString, OsString)> = cmd
        .envs
        .iter()
        .filter(|(k, _)| !PLACEMENT.iter().any(|p| k == p) && k != "RUST_TEST_THREADS")
        .cloned()
        .collect();
    if let Some(n) = test_threads {
        env.push(("RUST_TEST_THREADS".into(), n.to_string().into()));
    }
    env
}

/// The row as one finding: its label, and the lines of its output that say
/// what went wrong ([`excerpt`]).
#[must_use]
pub fn row_finding(label: &str, output: &str) -> Finding {
    Finding {
        id: one_line(label),
        rerun: None,
        message: excerpt(output, MESSAGE_LINES),
    }
}

/// A row decided without a child: its label is the whole of what it said.
#[must_use]
pub fn label_finding(label: &str) -> Finding {
    Finding {
        id: one_line(label),
        rerun: None,
        message: Vec::new(),
    }
}

/// The lines of a failing child's output worth reading first: its errors (an
/// `error`, a panic, a `FAIL` row, a could-not-run) with the two lines either
/// side of each, else its warnings the same way, at most `max`; with neither,
/// its last `max` lines. An error repeated word for word (cargo prints one for
/// each target that compiles the file) is shown once. cargo's progress lines
/// and targo's lane banners never count.
#[must_use]
pub fn excerpt(output: &str, max: usize) -> Vec<String> {
    let plain = strip(output);
    let lines: Vec<&str> = plain
        .lines()
        .filter(|l| !l.trim().is_empty() && !is_progress(l) && !is_banner(l))
        .collect();
    let around = |hit: &dyn Fn(&str) -> bool| -> Vec<usize> {
        let mut picked: Vec<usize> = Vec::new();
        let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if picked.len() >= max {
                break;
            }
            // An error is the line and the location under it.
            let key = (l.trim(), lines.get(i + 1).map(|n| n.trim()));
            if hit(l) && !seen.contains(&key) {
                seen.push(key);
                picked.extend(i.saturating_sub(2)..lines.len().min(i + 3));
            }
        }
        picked.sort_unstable();
        picked.dedup();
        picked.truncate(max);
        picked
    };
    let mut picked = around(&is_error);
    if picked.is_empty() {
        picked = around(&|l: &str| l.trim_start().starts_with("warning:"));
    }
    if picked.is_empty() {
        let from = lines.len().saturating_sub(max);
        return lines[from..].iter().map(|l| (*l).to_string()).collect();
    }
    picked.into_iter().map(|i| lines[i].to_string()).collect()
}

/// How a failed test failed: its block's lines, without the backtrace notes
/// and frames `RUST_BACKTRACE` adds, at most [`MESSAGE_LINES`].
fn test_message(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_trace = false;
    for l in block.lines() {
        let t = l.trim_start();
        if t.eq_ignore_ascii_case("stack backtrace:") {
            in_trace = true;
            continue;
        }
        if in_trace && is_frame(l) {
            continue;
        }
        in_trace = false;
        if t.is_empty() || t.starts_with("note: run with `RUST_BACKTRACE") {
            continue;
        }
        out.push(l.to_string());
        if out.len() == MESSAGE_LINES {
            break;
        }
    }
    out
}

/// A backtrace line: an indented numbered frame or the `at <file>` under one.
fn is_frame(line: &str) -> bool {
    let t = line.trim_start();
    let indented = t.len() < line.len();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    indented && ((digits > 0 && t[digits..].starts_with(": ")) || t.starts_with("at "))
}

/// cargo's progress verbs: a line that opens with one says nothing about a failure.
const PROGRESS_VERBS: [&str; 22] = [
    "Adding",
    "Blocking",
    "Building",
    "Checking",
    "Compiling",
    "Doc-tests",
    "Documenting",
    "Downloaded",
    "Downloading",
    "Finished",
    "Fresh",
    "Installed",
    "Installing",
    "Locking",
    "Packaging",
    "Removing",
    "Replacing",
    "Running",
    "Scraping",
    "Unpacking",
    "Updating",
    "Waiting",
];

fn is_progress(line: &str) -> bool {
    let t = line.trim_start();
    PROGRESS_VERBS
        .iter()
        .any(|v| t.strip_prefix(v).is_some_and(|rest| rest.starts_with(' ')))
}

/// What targo prints on every invocation in the unverified lane, whatever the
/// outcome.
const LANE_BANNERS: [&str; 3] = [
    "warning: nested explicit-unverified Targo propagation",
    "warning: UNVERIFIED: ",
    "note: compiled WITHOUT Trust verification",
];

fn is_banner(line: &str) -> bool {
    LANE_BANNERS
        .iter()
        .any(|b| line.trim_start().starts_with(b))
}

/// A line that says what went wrong, as compilers, guards and test harnesses
/// write one.
fn is_error(line: &str) -> bool {
    let t = line.trim_start();
    ["error", "ERROR", "fatal", "FAIL", "COULD NOT RUN"]
        .iter()
        .any(|p| t.starts_with(p))
        || [
            "panicked at",
            "didn't exit successfully",
            ": FAIL",
            CEILING_KILL,
        ]
        .iter()
        .any(|p| t.contains(p))
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// `name` quoted for a shell when it holds anything but the characters a test
/// path is made of.
fn shell_word(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-' | '.' | '/'))
    {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', r"'\''"))
    }
}

fn strip(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\x1b') {
        std::borrow::Cow::Owned(crate::libtest::strip_ansi(s))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_log(name: &str, message: &str) -> String {
        format!(
            "     Running tests/x.rs (target/debug/deps/x-1)\n\nrunning 1 test\n\
             test {name} ... FAILED\n\nfailures:\n\n---- {name} stdout ----\n{message}\n\n\
             failures:\n    {name}\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; \
             0 measured; 0 filtered out; finished in 0.01s\n\nerror: test failed, to rerun \
             pass `-p atpkg --test x`\n"
        )
    }

    #[test]
    fn a_failed_test_is_its_own_finding_with_the_command_that_reruns_it() {
        let log = failed_log(
            "tar::ustar_is_pinned",
            "thread 'tar::ustar_is_pinned' panicked at src/tar.rs:9:5:\nassertion failed",
        );
        let found = test_findings("targo test --tests", &log, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "-p atpkg --test x -- tar::ustar_is_pinned");
        assert_eq!(
            found[0].rerun.as_deref(),
            Some("targo --unverified test -p atpkg --test x -- tar::ustar_is_pinned --exact")
        );
        assert_eq!(
            found[0].message,
            [
                "thread 'tar::ustar_is_pinned' panicked at src/tar.rs:9:5:",
                "assertion failed"
            ]
        );
    }

    #[test]
    fn a_log_that_cannot_account_for_its_failures_is_one_row_finding() {
        let log = "     Running tests/x.rs (target/debug/deps/x-1)\nerror: test failed, to rerun \
                   pass `-p atpkg --test x`\n\nCaused by:\n  process didn't exit successfully \
                   (signal: 11, SIGSEGV)\n";
        let found = test_findings("targo test --tests", log, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "targo test --tests");
        assert_eq!(found[0].rerun, None);
        assert!(
            found[0].message.iter().any(|l| l.contains("SIGSEGV")),
            "{found:?}"
        );
    }

    /// The rerun keeps the child's variables and drops the ones that only place the build.
    #[test]
    fn a_rerun_sets_what_the_test_child_ran_under() {
        let cmd = Cmd::new("targo")
            .env("TRAIL_LAWS_FULL", "1")
            .env("CARGO_TARGET_DIR", "/t")
            .env("RUSTDOC", "/s/trustdoc")
            .env("ATERM_PAINT_BIN", "/b/My App/aterm");
        let env = rerun_env(&cmd, Some(3));
        let log = failed_log("laws::full_grid", "boom");
        let found = test_findings("targo test --tests", &log, &env);
        assert_eq!(
            found[0].rerun.as_deref(),
            Some(
                "TRAIL_LAWS_FULL=1 ATERM_PAINT_BIN='/b/My App/aterm' RUST_TEST_THREADS=3 \
                 targo --unverified test -p atpkg --test x -- laws::full_grid --exact"
            )
        );
        assert_eq!(rerun_env(&Cmd::new("targo"), None), Vec::new());
    }

    #[test]
    fn an_excerpt_is_the_errors_first_never_the_progress() {
        assert_eq!(
            excerpt(
                "warning: UNVERIFIED: `targo run` was explicitly authorized\n\
                 note: compiled WITHOUT Trust verification — 0 obligations checked\n\
                 aterm-census: unknown option \"--all\"\n",
                12
            ),
            ["aterm-census: unknown option \"--all\""],
            "targo's lane banners are never the excerpt"
        );
        let out = "   Compiling a v0.1.0\nwarning: a build script said so\nnote: x\nnote: y\n\
                   note: z\n    Checking b v0.1.0\nerror: unused variable: `x`\n  \
                   --> src/lib.rs:3:9\n   |\n    Finished dev\n";
        assert_eq!(
            excerpt(out, 12),
            [
                "note: y",
                "note: z",
                "error: unused variable: `x`",
                "  --> src/lib.rs:3:9",
                "   |"
            ]
        );
        assert_eq!(
            excerpt("warning: only this\nmore\n", 12),
            ["warning: only this", "more"],
            "warnings when there is no error"
        );
        assert_eq!(excerpt("one\ntwo\nthree\n", 2), ["two", "three"]);
        let twice = "error[E0433]: no `x`\n  --> src/a.rs:1:1\n   |\n   = n1\n   = n2\n   = n3\n\
                     error[E0433]: no `x`\n  --> src/a.rs:1:1\n   |\n   = n4\n   = n5\n   = n6\n\
                     error[E0433]: no `y`\n  --> src/b.rs:2:2\n";
        assert_eq!(
            excerpt(twice, 12),
            [
                "error[E0433]: no `x`",
                "  --> src/a.rs:1:1",
                "   |",
                "   = n5",
                "   = n6",
                "error[E0433]: no `y`",
                "  --> src/b.rs:2:2"
            ],
            "an error cargo printed twice is shown once"
        );
        assert_eq!(excerpt("", 4), Vec::<String>::new());
    }
}

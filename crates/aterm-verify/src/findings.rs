// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Structured test and check failure findings with rerun commands and excerpts.

use std::ffi::OsString;

use crate::exec::Cmd;
use crate::ladder::Finding;

/// What the run prints when it kills a child at its ceiling ([`crate::exec`]).
pub const CEILING_KILL: &str = "aterm-verify: TIMEOUT";

/// How many lines of a failure's own output a finding carries.
pub const MESSAGE_LINES: usize = 12;

/// The failed tests of a `targo test` child, one finding each, or one for the whole row where
/// the log cannot account for every failure.
#[must_use]
pub fn test_findings(label: &str, output: &str, env: &[(OsString, OsString)]) -> Vec<Finding> {
    split_test_findings(label, output, env).0
}

/// [`test_findings`], and the names of the failed tests it leaves out.
#[must_use]
pub fn split_test_findings(
    label: &str,
    output: &str,
    env: &[(OsString, OsString)],
) -> (Vec<Finding>, Vec<String>) {
    let plain = strip(output);
    let tests = if plain.contains(CEILING_KILL) {
        None
    } else {
        crate::libtest::failed_tests(&plain)
    };
    let Some(tests) = tests else {
        return (vec![row_finding(label, output)], Vec::new());
    };
    let mixed = tests.iter().any(|t| t.refused) && tests.iter().any(|t| !t.refused);
    let (refused, tests): (Vec<_>, Vec<_>) = tests.into_iter().partition(|t| mixed && t.refused);
    let mut refused: Vec<String> = refused.into_iter().map(|t| t.name).collect();
    refused.sort_unstable();
    refused.dedup();
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
    let found = tests
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
        .collect();
    (found, refused)
}

/// The variables a rerun of a test child's failures must set.
#[must_use]
pub fn rerun_env(cmd: &Cmd, test_threads: Option<u32>) -> Vec<(OsString, OsString)> {
    const PLACEMENT: [&str; 5] = [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_JOBS",
        "RUSTDOC",
        crate::docdriver::DRIVER_ENV,
        crate::docdriver::SKIP_ENV,
    ];
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

/// The one line a stage group's survivors add to the receipt.
#[must_use]
pub fn survivor_line(stage: &str, ended: &[String], named: &[String], partial: bool) -> String {
    let total = ended.len() + named.len();
    let mut parts = Vec::new();
    if !ended.is_empty() {
        parts.push(format!("{}, ended", tally_words(ended)));
    }
    if !named.is_empty() {
        parts.push(format!(
            "{} other named: {}",
            named.len(),
            tally_words(named)
        ));
    }
    if partial {
        parts.push(
            "its group's listing was partial, so it may have left processes not named here"
                .to_owned(),
        );
    }
    let head = if total == 0 {
        format!("stage {stage}")
    } else {
        format!("stage {stage} left {total} processes in its group")
    };
    format!("{head}: {}", parts.join("; "))
}

/// `word ×n` for each distinct word, most frequent first: `aterm ×7, ssh-agent ×1`.
#[must_use]
pub fn tally_words(words: &[String]) -> String {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for w in words {
        *counts.entry(w.as_str()).or_default() += 1;
    }
    let mut pairs: Vec<(&str, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    pairs
        .iter()
        .map(|(w, n)| format!("{w} \u{d7}{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The row as one finding.
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

/// The lines of a failing child's output worth reading first.
#[must_use]
pub fn excerpt(output: &str, max: usize) -> Vec<String> {
    let plain = strip(output);
    let lines: Vec<&str> = plain
        .lines()
        .filter(|l| !l.trim().is_empty() && !is_progress(l) && !is_banner(l))
        .collect();
    let around = |hit: &dyn Fn(&str) -> bool| -> Vec<usize> {
        let mut anchors: Vec<usize> = Vec::new();
        let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if anchors.len() >= max {
                break;
            }
            let context = if l.trim_start().starts_with("ERROR") {
                None
            } else {
                lines.get(i + 1).map(|n| n.trim())
            };
            let key = (l.trim(), context);
            if hit(l) && !seen.contains(&key) {
                seen.push(key);
                anchors.push(i);
            }
        }
        // Reserve the causes before their context: even a one-line excerpt must
        // show a diagnostic, and nearby notes must not crowd out a later cause.
        let mut picked = anchors.clone();
        for i in anchors {
            for (nearby, line) in lines
                .iter()
                .enumerate()
                .take(lines.len().min(i + 3))
                .skip(i.saturating_sub(2))
            {
                if picked.len() < max && !is_error(line) && !picked.contains(&nearby) {
                    picked.push(nearby);
                }
            }
        }
        picked.sort_unstable();
        picked
    };
    let mut picked = around(&is_actionable_error);
    if picked.is_empty() {
        picked = around(&is_error);
    }
    if picked.is_empty() {
        picked = around(&|l: &str| l.trim_start().starts_with("warning:"));
    }
    if picked.is_empty() {
        let from = lines.len().saturating_sub(max);
        return lines[from..].iter().map(|l| (*l).to_string()).collect();
    }
    picked.into_iter().map(|i| lines[i].to_string()).collect()
}

/// How a failed test failed.
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

/// Cargo's progress verbs: a line that opens with one says nothing about a failure.
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

/// What targo prints on every invocation in the unverified lane, whatever the outcome.
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

/// A line that says what went wrong, as compilers, guards and test harnesses write one.
fn is_error(line: &str) -> bool {
    // A census or forge row marks its failure `✗ FAIL …`, the line that names its fix.
    let t = line
        .trim_start()
        .trim_start_matches('\u{2717}')
        .trim_start();
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

/// Compiler diagnostics and explicit failed checks take precedence over logger
/// severity records, which can be noisy without causing the command to fail.
fn is_actionable_error(line: &str) -> bool {
    let t = line
        .trim_start()
        .trim_start_matches('\u{2717}')
        .trim_start();
    [
        "error:",
        "error[",
        "fatal:",
        "fatal error:",
        "FAIL",
        "COULD NOT RUN",
    ]
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

/// `name` quoted for a shell when it holds anything but the characters a test path is made of.
pub(crate) fn shell_word(name: &str) -> String {
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
    fn a_refusal_beside_a_failure_is_no_finding() {
        let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
        let refused = failed_log("fish::live", &format!("{sentinel} — no fish on this host"));
        let real = failed_log("tar::ustar_is_pinned", "assertion failed");
        let (found, left) = split_test_findings("t", &format!("{refused}{real}"), &[]);
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["-p atpkg --test x -- tar::ustar_is_pinned"]);
        assert_eq!(left, ["fish::live"]);
        let (found, left) = split_test_findings("t", &refused, &[]);
        assert_eq!(found.len(), 1);
        assert!(left.is_empty());
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

    #[test]
    fn a_rerun_sets_what_the_test_child_ran_under() {
        let cmd = Cmd::new("targo")
            .env("TRAIL_LAWS_FULL", "1")
            .env("CARGO_TARGET_DIR", "/t")
            .env("RUSTDOC", "/s/trustdoc")
            .env(crate::docdriver::DRIVER_ENV, "/s/trustdoc")
            .env(crate::docdriver::SKIP_ENV, "plain")
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

    #[test]
    fn actionable_diagnostics_survive_noisy_logger_records() {
        let noise: String = (0..32)
            .map(|i| {
                format!(
                    "ERROR rustc_interface::passes checksum unavailable\n\
                     note: dependency {i}\nERROR logger detail {i}\n"
                )
            })
            .collect();
        for diagnostic in [
            "error: method `unused` is never used",
            "error[E0425]: cannot find value `missing`",
            "fatal error: cannot open input",
            "thread 'main' panicked at src/lib.rs:7:9:",
            "✗ FAIL [OB-7] missing obligation",
        ] {
            let output = format!(
                "{noise}\x1b[31m{diagnostic}\x1b[0m\n\
                   --> src/lib.rs:7:9\n   |\n\
                 error: could not compile `example`\n"
            );
            let found = excerpt(&output, MESSAGE_LINES);
            assert!(found.iter().any(|line| line == diagnostic), "{found:?}");
            assert!(found.iter().any(|line| line.contains("src/lib.rs:7:9")));
            assert!(found.len() <= MESSAGE_LINES);
            assert!(found.iter().all(|line| !line.starts_with("ERROR")));
        }
    }

    #[test]
    fn an_excerpt_reserves_causes_before_context_and_keeps_distinct_locations() {
        let first = "error[E0425]: cannot find value `missing`";
        let output = format!(
            "note: preceding context\n{first}\n  --> src/a.rs:1:1\n   |\n\
             {first}\n  --> src/b.rs:2:2\n   |\n"
        );
        assert_eq!(excerpt(&output, 1), [first]);
        let found = excerpt(&output, MESSAGE_LINES);
        assert_eq!(found.iter().filter(|line| *line == first).count(), 2);
        for source in ["src/a.rs:1:1", "src/b.rs:2:2"] {
            assert!(found.iter().any(|line| line.contains(source)));
        }
        assert!(excerpt(&output, 0).is_empty());
    }

    #[test]
    fn logger_only_failures_still_show_each_distinct_error() {
        let repeated = "ERROR transport could not write";
        let mut output: String = (0..32)
            .map(|i| format!("{repeated}\nnote: attempt {i}\n"))
            .collect();
        let distinct = "ERROR transport could not close";
        output.push_str(distinct);
        let found = excerpt(&output, MESSAGE_LINES);
        assert_eq!(found.iter().filter(|line| *line == repeated).count(), 1);
        assert!(found.iter().any(|line| line == distinct));
        assert!(found.len() <= MESSAGE_LINES);
    }

    #[test]
    fn a_rows_marked_failure_is_in_the_excerpt() {
        let forge = "      \u{2713} [OB-6] held\n\
                     \x20     \u{2717} FAIL [OB-7] `a.rs` has no notice. FIX: add one.\n\
                     \x20       [OB-7] scope: forks\n      \u{2713} [OB-8] held\n\
                     \x20   cargo forge attest: FAILED \u{2014} 1 obligation violation(s).\n";
        assert_eq!(
            excerpt(forge, 12),
            [
                "      \u{2713} [OB-6] held",
                "      \u{2717} FAIL [OB-7] `a.rs` has no notice. FIX: add one.",
                "        [OB-7] scope: forks",
                "      \u{2713} [OB-8] held",
                "    cargo forge attest: FAILED \u{2014} 1 obligation violation(s).",
            ],
            "the row that names the fix is shown, not only the tally after it"
        );
    }
}

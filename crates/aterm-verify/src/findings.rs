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
pub fn test_findings(label: &str, output: &str, rerun: &Rerun) -> Vec<Finding> {
    split_test_findings(label, output, rerun).0
}

/// [`test_findings`], and the names of the failed tests it leaves out.
#[must_use]
pub fn split_test_findings(
    label: &str,
    output: &str,
    rerun: &Rerun,
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
    let found = tests
        .into_iter()
        .map(|t| Finding {
            id: one_line(&format!("{} -- {}", t.spec, t.name)),
            rerun: Some(format!(
                "{}targo --unverified test {} -- {} --exact{}",
                rerun.prefix(&t.spec),
                t.spec,
                shell_word(&t.name),
                rerun.seed.map_or_else(String::new, |seed| format!(
                    " -Zunstable-options --shuffle-seed={seed}"
                ))
            )),
            message: test_message(&t.block),
        })
        .collect();
    (found, refused)
}

/// What a rerun of a test child's failures sets: the variables the child ran under, less those
/// that only place its build, and the `RUST_TEST_THREADS` and `ATERM_TEST_CORES` each binary
/// ran at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rerun {
    /// The variables every rerun sets, in the child's order, `RUST_TEST_THREADS` last where the
    /// run pinned one.
    env: Vec<(OsString, OsString)>,
    /// The binaries that ran at a width of their own, by cargo's rerun spec, each with it and the
    /// cores each of its test threads was granted: a rerun of one sets both in place of the
    /// run's, so a failure that only its pool size shows is shown again.
    widths: Vec<(String, u32, u32)>,
    seed: Option<u64>,
}

impl Rerun {
    /// What a rerun of `cmd`'s failures sets: its variables less those that place its build, and
    /// `RUST_TEST_THREADS=test_threads` where the run pinned one, never `cmd`'s own.
    #[must_use]
    pub fn of(cmd: &Cmd, test_threads: Option<u32>) -> Rerun {
        const PLACEMENT: [&str; 10] = [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "RUSTDOC",
            crate::docdriver::DRIVER_ENV,
            crate::docdriver::SKIP_ENV,
            crate::docdriver::RUNTOOL_ENV,
            crate::docdriver::RUNTOOL_SCRATCH_ENV,
            crate::docdriver::RUNTOOL_SAMPLES_ENV,
            crate::docdriver::RUNTOOL_HALT_ENV,
            crate::testrun::SUBPROCESS_SLOTS_ENV,
        ];
        let mut env: Vec<(OsString, OsString)> = cmd
            .envs
            .iter()
            .filter(|(k, _)| {
                !PLACEMENT.iter().any(|p| k == p) && k != THREADS && k != aterm_cores::GRANT_ENV
            })
            .cloned()
            .collect();
        if let Some(n) = test_threads {
            env.push((THREADS.into(), n.to_string().into()));
        }
        Rerun {
            env,
            widths: Vec::new(),
            seed: None,
        }
    }

    /// This rerun, each binary `widths` names (by cargo's rerun spec, its width and the cores it
    /// was charged) rerun at the width and the per-thread grant it ran at
    /// ([`crate::exec::test_thread_cores`]).
    #[must_use]
    pub fn at_widths(self, widths: impl IntoIterator<Item = (String, u32, u32)>) -> Rerun {
        let widths = widths
            .into_iter()
            .map(|(spec, width, charge)| {
                let width = width.max(1);
                (spec, width, crate::exec::test_thread_cores(width, charge))
            })
            .collect();
        Rerun { widths, ..self }
    }

    /// Preserve an explicitly requested libtest order in the failure's replay command.
    #[must_use]
    pub const fn with_seed(mut self, seed: Option<u64>) -> Self {
        self.seed = seed;
        self
    }

    /// The variables a rerun of `spec`'s failures sets, as a shell command's prefix: the run's,
    /// with `spec`'s own width as `RUST_TEST_THREADS` and its grant as `ATERM_TEST_CORES` where it
    /// ran at one.
    fn prefix(&self, spec: &str) -> String {
        let own: Vec<(OsString, OsString)> = self
            .widths
            .iter()
            .find(|(s, _, _)| s == spec)
            .map(|&(_, w, cores)| {
                vec![
                    (OsString::from(THREADS), OsString::from(w.to_string())),
                    (
                        OsString::from(aterm_cores::GRANT_ENV),
                        OsString::from(cores.to_string()),
                    ),
                ]
            })
            .unwrap_or_default();
        self.env
            .iter()
            .filter(|(k, _)| own.is_empty() || k != THREADS)
            .chain(&own)
            .map(|(k, v)| {
                format!(
                    "{}={} ",
                    k.to_string_lossy(),
                    shell_word(&v.to_string_lossy())
                )
            })
            .collect()
    }
}

/// The variable a test binary reads its width from.
const THREADS: &str = "RUST_TEST_THREADS";

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
        // A compiler location identifies the cause; reserve it before optional
        // context from earlier diagnostics can consume the remaining budget.
        for &i in &anchors {
            if picked.len() < max
                && lines
                    .get(i + 1)
                    .is_some_and(|line| line.trim_start().starts_with("--> "))
                && !picked.contains(&(i + 1))
            {
                picked.push(i + 1);
            }
        }
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
#[path = "findings_tests.rs"]
mod tests;

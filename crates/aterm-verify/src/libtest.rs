// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading a `targo test` child's log: which binaries ran and passed what,
//! which failed tests it can account for (each with its binary's re-run spec),
//! whether every failure was the machine refusing ([`COULD_NOT_RUN_SENTINEL`]),
//! and which macOS services a binary reported silent ([`host_skips`]). Pure
//! (`&str` in). A coloured log reads as a plain one.

/// cargo's test-binary header, with its verb dropped.
pub(crate) fn header(line: &str) -> Option<String> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("Running ") {
        (rest.contains(" (") && rest.ends_with(')') && !rest.contains('`'))
            .then(|| rest.to_string())
    } else if t.starts_with("Doc-tests ") {
        Some(t.to_string())
    } else {
        None
    }
}

/// Why a `targo test` child that exited 0 decided nothing ([`ran_no_test`]).
pub const RAN_NO_TEST: &str = "cargo announced test binaries and not one printed a libtest \
     `test result:` line, so no test ran — a runner the caller's cargo config or environment \
     names for this host (`target.<triple>.runner`, `CARGO_TARGET_<TRIPLE>_RUNNER`) outranks the \
     gate's and started them in its place, or they exited before their harness; nothing was \
     decided about them";

/// Did a `targo test` child's log announce test binaries — cargo's `Running`
/// / `Doc-tests` headers — and then print not one libtest `test result:`
/// line? Then whatever ran in the binaries' place ran no test
/// ([`crate::ladder::Report::decide_test_child`]). A log with no header ran
/// nothing to count, and one binary with a result is a run.
#[must_use]
pub fn ran_no_test(log: &str) -> bool {
    if log.contains('\x1b') {
        return ran_no_test(&strip_ansi(log));
    }
    log.lines().any(|l| header(l).is_some()) && !log.lines().any(|l| l.starts_with("test result: "))
}

/// Why a test target of a selective `targo test` child that exited 0 decided
/// nothing ([`passed_by_target`] gives it no passed test).
pub const PASSED_NONE: &str = "not one row passed: the filter selected none (a renamed \
     module, a lost `--ignored`), so it measured nothing";

/// Each test binary of a `targo test` child's log, in log order: its cargo
/// target (`tests/paint.rs`, `unittests src/lib.rs`) and the `N passed` of the
/// last `test result:` line in its section, `None` when it printed none. The
/// last line is the binary's own: a re-exec child's result prints before its
/// parent's. Per binary, never summed, so one target that selected nothing
/// is not hidden by another that passed.
#[must_use]
pub fn passed_by_target(log: &str) -> Vec<(String, Option<usize>)> {
    if log.contains('\x1b') {
        return passed_by_target(&strip_ansi(log));
    }
    let mut out: Vec<(String, Option<usize>)> = Vec::new();
    for l in log.lines() {
        if let Some(h) = header(l) {
            let target = h.split_once(" (").map_or(h.as_str(), |(t, _)| t);
            out.push((target.to_string(), None));
        } else if let Some(rest) = l.strip_prefix("test result: ")
            && let Some((_, counts)) = rest.split_once(". ")
            && let Some(n) = counts
                .split_once(" passed")
                .and_then(|(n, _)| n.trim().parse::<usize>().ok())
            && let Some(last) = out.last_mut()
        {
            last.1 = Some(n);
        }
    }
    out
}

/// The lines a test binary prints, once each, when a macOS service it needs
/// did not answer and its tests of it returned without asserting anything
/// (`aterm_uds::deadline::{SKIPPED, DISK_IMAGE_SKIPPED}`), included from
/// aterm-uds's own file.
pub(crate) mod skipped {
    include!("../../aterm-uds/src/deadline/skipped.rs");
}

/// A macOS service whose silence a test binary names with one line: the line,
/// the head of the test stage's skip row for it, what its tests check, and
/// what clears it.
struct Wedged {
    line: &'static str,
    head: &'static str,
    checks: &'static str,
    remedy: &'static str,
}

/// Every service a test skips for: each gets its own row ([`host_skips`]).
const WEDGED: [Wedged; 2] = [
    Wedged {
        line: skipped::SKIPPED,
        head: "tests: trustd did not answer — ",
        checks: "code-signing and TLS checks",
        remedy: "rebooting the Mac clears a wedged trustd",
    },
    Wedged {
        line: skipped::DISK_IMAGE_SKIPPED,
        head: "tests: hdiutil did not answer — ",
        checks: "disk-image checks",
        remedy: "rebooting the Mac clears a wedged disk-image service",
    },
];

/// The binaries of a `targo test` log that printed `line`: each by its crate
/// and target, in log order, once. Matched anywhere in a line: on one test
/// thread libtest has already written `test x ... ` when the notice lands, and
/// on many another thread's line can follow it before its newline.
#[must_use]
fn printed(log: &str, line: &str) -> Vec<String> {
    let mut at: Option<String> = None;
    let mut found: Vec<String> = Vec::new();
    for l in log.lines() {
        if let Some(h) = header(l) {
            at = Some(short_binary(&h));
        } else if l.contains(line) {
            let name = at.clone().unwrap_or_else(|| "a test binary".to_string());
            if !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// The test stage's skip rows, one per service whose silence any binary named
/// ([`WEDGED`]): what ran stays decided, and each row names what did not and
/// what clears it. Empty when none did.
#[must_use]
pub fn host_skips(log: &str) -> Vec<String> {
    if log.contains('\x1b') {
        return host_skips(&strip_ansi(log));
    }
    WEDGED
        .iter()
        .filter_map(|w| {
            let bins = printed(log, w.line);
            let n = bins.len();
            (n > 0).then(|| {
                format!(
                    "{}{n} test binar{} skipped {} {} ({}); {}",
                    w.head,
                    if n == 1 { "y" } else { "ies" },
                    if n == 1 { "its" } else { "their" },
                    w.checks,
                    bins.join(", "),
                    w.remedy
                )
            })
        })
        .collect()
}

/// Drop every CSI escape (`ESC [` … a final byte in `@`–`~`) and every other
/// two-byte escape, so a coloured log reads exactly like a plain one. Only
/// called when the log has an `ESC` in it.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        rest = if let Some(body) = after.strip_prefix('[') {
            // A CSI: parameters and intermediates, then one final byte.
            match body.find(|c: char| ('\x40'..='\x7e').contains(&c)) {
                Some(end) => &body[end + 1..],
                None => "",
            }
        } else {
            // Any other escape: ESC plus one byte (or a stray ESC at the end).
            after.char_indices().nth(1).map_or("", |(i, _)| &after[i..])
        };
    }
    out.push_str(rest);
    out
}

/// `unittests src/lib.rs (target/debug/deps/aterm_gui-66ca…)` ->
/// `aterm_gui unittests src/lib.rs`: the crate (the binary's file name less its
/// hash) and the target, which is what a reader looks for in a table.
fn short_binary(header: &str) -> String {
    let Some((target, _)) = header.split_once(" (") else {
        return header.to_string();
    };
    let stem = binary_path(header)
        .and_then(|p| p.rsplit('/').next())
        .map(|f| f.rsplit_once('-').map_or(f, |(s, _)| s));
    match stem {
        Some(stem) => format!("{stem} {target}"),
        None => target.to_string(),
    }
}

/// The path in a `Running <target> (<path>)` header — the binary to re-run.
pub(crate) fn binary_path(header: &str) -> Option<&str> {
    header.rsplit_once(" (")?.1.strip_suffix(')')
}

/// The words a test prints when the machine refused it, not the tree: a
/// headless boot the OS refused (`crates/aterm/tests/support/headless_boot.rs`),
/// a host with no GPU adapter (`aterm-gpu`'s test helpers), a paint take its
/// own instrument disowns as starved, or a paint or spin probe that decided
/// nothing, among others. A red row under this sentinel says nothing about the
/// code, so the run records it as COULD NOT RUN, never `FAIL`.
///
/// Producers print it verbatim in their panic message, most through
/// `aterm_spec::host::could_not_run` (its `COULD_NOT_RUN` is this string); a
/// reworded producer turns its refusals into findings, which fails closed.
pub const COULD_NOT_RUN_SENTINEL: &str = "aterm-gate: COULD NOT RUN";

/// The failed tests in a failed `targo test` child's log, when every failure
/// carries [`COULD_NOT_RUN_SENTINEL`]; `None` when any does not, or when the
/// log cannot account for every failure. A finding must never read as the
/// machine's fault, so all of this must hold:
///
/// * at least one `---- <name> stdout ----` block, and every one carries the
///   sentinel;
/// * the names listed under the closing `failures:` are exactly the names that
///   had a block, counted with multiplicity;
/// * every binary cargo reported failed (`error: test failed, to rerun pass`)
///   printed a `test result: FAILED.` line: a binary that died of a signal
///   prints none, and a crash is a finding;
/// * no binary exited other than libtest's 101 ([`abnormal_exit`]): one that
///   printed its result and then died of a signal is a crash too.
///
/// A block is a position in the log, never a name: two binaries of one
/// `--no-fail-fast` log can fail tests of the same bare name, one a refusal and
/// one a real assertion.
#[must_use]
pub fn environment_refusals(log: &str) -> Option<Vec<String>> {
    if log.contains('\x1b') {
        return environment_refusals(&strip_ansi(log));
    }
    // (name, refused) for each block, in log order.
    let mut blocks: Vec<(&str, bool)> = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    let mut current: Option<usize> = None;
    let mut in_list = false;
    let (mut results, mut binaries) = (0usize, 0usize);
    for line in log.lines() {
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            current = Some(blocks.len());
            in_list = false;
            blocks.push((name, false));
            continue;
        }
        if line == "failures:" {
            current = None;
            in_list = true;
            continue;
        }
        if in_list {
            if let Some(name) = line.strip_prefix("    ") {
                listed.push(name.trim_end());
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            in_list = false;
        }
        if current.is_none() && abnormal_exit(line) {
            return None;
        }
        if line.starts_with("test result: FAILED.") {
            results += 1;
            current = None;
        }
        if line.starts_with("error: test failed, to rerun pass")
            || line.starts_with("error: doctest failed, to rerun pass")
        {
            binaries += 1;
        }
        if let Some(at) = current
            && line.contains(COULD_NOT_RUN_SENTINEL)
        {
            blocks[at].1 = true;
        }
    }
    let mut named: Vec<&str> = blocks.iter().map(|(name, _)| *name).collect();
    named.sort_unstable();
    listed.sort_unstable();
    (!blocks.is_empty()
        && blocks.iter().all(|(_, refused)| *refused)
        && listed == named
        && binaries > 0
        && results == binaries)
        .then(|| {
            named.dedup();
            named.into_iter().map(str::to_string).collect()
        })
}

/// One failed test, as a `--no-fail-fast` `targo test` log accounts for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedTest {
    /// cargo's re-run spec for the binary that failed it — `-p atpkg --test
    /// atpkg_suite`, `-p aterm-gui --lib`, `-p x --doc` — the text between the
    /// backticks of its `error: test failed, to rerun pass` line. It names the
    /// package and the target, which a `Running` header does not
    /// (`tests/paint.rs` exists in more than one crate).
    pub spec: String,
    /// The test's name as libtest listed it.
    pub name: String,
    /// Everything its `---- <name> stdout ----` block held.
    pub block: String,
    /// The block carries [`COULD_NOT_RUN_SENTINEL`]: the machine refused this
    /// test, so it decided nothing.
    pub refused: bool,
}

/// cargo's word that a test binary did not exit with libtest's 101: the
/// `Caused by:` under the re-run line, and the `process didn't exit
/// successfully: … (signal: …)` under that. cargo prints neither for a plain
/// failed test, so outside a test's own block either means the binary ended in
/// something its `test result:` line does not account for (a teardown crash, a
/// background thread's abort after the summary).
fn abnormal_exit(line: &str) -> bool {
    line.trim_end() == "Caused by:"
        || line
            .trim_start()
            .starts_with("process didn't exit successfully")
}

/// cargo's `error: test failed, to rerun pass `<spec>`` (or the doctest
/// spelling) — the spec.
fn rerun_spec(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("error: test failed, to rerun pass `")
        .or_else(|| line.strip_prefix("error: doctest failed, to rerun pass `"))?;
    let spec = rest.split('`').next()?;
    (!spec.is_empty()).then_some(spec)
}

/// Every failed test in a failed `targo test` child's log, each with the
/// binary that failed it ([`FailedTest::spec`]); `None` unless the log
/// accounts for every failure, so a failure it cannot name is never dropped
/// from a list a reader compares against another run's.
///
/// The accounting [`environment_refusals`] demands, and more:
///
/// * at least one `---- <name> stdout ----` block, each followed by the
///   `error: test failed, to rerun pass` line of its binary (how a block gets
///   its spec);
/// * the names listed under the closing `failures:` are exactly the blocks'
///   names, counted with multiplicity;
/// * every binary cargo reported failed printed a `test result: FAILED.` line
///   (a binary that died of a signal prints none, and a failed re-exec child
///   prints a result with no re-run line of its own);
/// * per binary, the names listed since the last re-run line are exactly the
///   blocks read since it, and no `test result:` or re-run line arrives inside
///   an open block: a test whose output quotes a nested cargo log (this crate's
///   contract tests panic with fixture ladders) would otherwise have its block
///   cut and credited to the nested spec;
/// * once a `failures:` line closes a binary's blocks, only its listed names
///   (four-space indented), blank lines and its `test result:` follow: a test
///   whose message holds a `failures:` line would otherwise have its block
///   closed there;
/// * every binary exited with libtest's 101 ([`abnormal_exit`]).
#[must_use]
pub fn failed_tests(log: &str) -> Option<Vec<FailedTest>> {
    if log.contains('\x1b') {
        return failed_tests(&strip_ansi(log));
    }
    let mut done: Vec<FailedTest> = Vec::new();
    // Blocks read since the last re-run line: (name, text, refused).
    let mut pending: Vec<(String, String, bool)> = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    // Names listed, and whether a `test result: FAILED.` came, since the last
    // re-run line: one binary's accounting.
    let mut listed_here: Vec<&str> = Vec::new();
    let mut result_here = false;
    let mut current: Option<usize> = None;
    let mut in_list = false;
    // A `failures:` line closed this binary's blocks: its closing list, up to
    // its `test result:`.
    let mut closing = false;
    let (mut results, mut binaries) = (0usize, 0usize);
    for line in log.lines() {
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            // A block after the closing list: one of the two was a message's.
            if closing {
                return None;
            }
            current = Some(pending.len());
            in_list = false;
            pending.push((name.to_string(), String::new(), false));
            continue;
        }
        if line == "failures:" {
            // A second closing list: the first was a line of a message.
            if closing {
                return None;
            }
            closing = !pending.is_empty();
            current = None;
            in_list = true;
            continue;
        }
        if in_list {
            if let Some(name) = line.strip_prefix("    ") {
                listed.push(name.trim_end());
                listed_here.push(name.trim_end());
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            in_list = false;
        }
        // Anything but the result after the closing list: the `failures:`
        // that opened it was a line of a test's message, and the rest of that
        // message fell outside every block.
        if closing && !line.starts_with("test result: ") {
            return None;
        }
        // A result or a re-run line inside a test's own output is a nested
        // log: which binary any block belongs to can no longer be told.
        if current.is_some() && (line.starts_with("test result: ") || rerun_spec(line).is_some()) {
            return None;
        }
        // A binary that did not exit with libtest's 101.
        if current.is_none() && abnormal_exit(line) {
            return None;
        }
        if line.starts_with("test result: ") {
            closing = false;
        }
        if line.starts_with("test result: FAILED.") {
            results += 1;
            result_here = true;
            current = None;
        }
        if let Some(spec) = rerun_spec(line) {
            let mut blocks: Vec<&str> = pending.iter().map(|(name, ..)| name.as_str()).collect();
            blocks.sort_unstable();
            listed_here.sort_unstable();
            if !result_here || blocks != listed_here {
                return None;
            }
            listed_here.clear();
            result_here = false;
            binaries += 1;
            current = None;
            done.extend(pending.drain(..).map(|(name, block, refused)| FailedTest {
                spec: spec.to_string(),
                name,
                block,
                refused,
            }));
            continue;
        }
        if let Some(at) = current {
            let block = &mut pending[at];
            block.2 |= line.contains(COULD_NOT_RUN_SENTINEL);
            block.1.push_str(line);
            block.1.push('\n');
        }
    }
    let mut named: Vec<&str> = done.iter().map(|t| t.name.as_str()).collect();
    named.sort_unstable();
    listed.sort_unstable();
    (pending.is_empty()
        && !done.is_empty()
        && listed == named
        && binaries > 0
        && results == binaries)
        .then_some(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "     Running unittests src/lib.rs (target/debug/deps/x-abc)\n";

    /// Each binary is read on its own, coloured or not: a target whose rows were
    /// all ignored or filtered out passed none even beside one that passed, a
    /// re-exec child's result is not its parent's, and a binary with no result
    /// line is `None`.
    #[test]
    fn passed_is_read_per_target_never_summed() {
        let line = |verdict: &str, passed: usize, ignored: usize| {
            format!(
                "test result: {verdict}. {passed} passed; 0 failed; {ignored} ignored; 0 measured; \
                 0 filtered out; finished in 0.01s\n"
            )
        };
        let paint = "     Running tests/paint.rs (target/debug/deps/paint-1a)\n";
        let spin = "     Running tests/spin.rs (target/debug/deps/spin-2b)\n";
        let t = |name: &str, n: Option<usize>| (name.to_string(), n);
        let one_empty = format!("{paint}{}{spin}{}", line("ok", 0, 23), line("ok", 2, 0));
        assert_eq!(
            passed_by_target(&one_empty),
            [t("tests/paint.rs", Some(0)), t("tests/spin.rs", Some(2))]
        );
        let child = format!(
            "{paint}running 1 test\n{}{}",
            line("ok", 1, 0),
            line("FAILED", 0, 0)
        );
        assert_eq!(passed_by_target(&child), [t("tests/paint.rs", Some(0))]);
        assert_eq!(
            passed_by_target(&format!("{HEADER}running 2 tests\n")),
            [t("unittests src/lib.rs", None)]
        );
        assert_eq!(
            passed_by_target(&format!(
                "{spin}test result: \x1b[32mok\x1b[0m. 3 passed; 0 failed; 0 ignored\n"
            )),
            [t("tests/spin.rs", Some(3))]
        );
        assert!(passed_by_target("").is_empty());
    }

    /// One failing test binary as cargo and libtest print it under
    /// `--no-fail-fast`: each `(name, output)` fails with that captured output.
    fn failed_binary(target: &str, failures: &[(&str, &str)]) -> String {
        let mut s =
            format!("     Running {target} (target/debug/deps/{target}-abc)\n\nrunning 3 tests\n");
        for (name, _) in failures {
            s.push_str(&format!("test {name} ... FAILED\n"));
        }
        s.push_str("test passes ... ok\n\nfailures:\n\n");
        for (name, out) in failures {
            s.push_str(&format!("---- {name} stdout ----\n{out}\n\n"));
        }
        s.push_str("failures:\n");
        for (name, _) in failures {
            s.push_str(&format!("    {name}\n"));
        }
        s.push_str(&format!(
            "\ntest result: FAILED. 1 passed; {} failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 3.83s\n\nerror: test failed, to rerun pass `-p x --test {target}`\n",
            failures.len()
        ));
        s
    }

    fn refused(why: &str) -> String {
        format!(
            "\nthread 't' panicked at crates/x/tests/harness/mod.rs:177:5:\n\
             {COULD_NOT_RUN_SENTINEL} — {why}\nnote: run with `RUST_BACKTRACE=1`"
        )
    }

    /// Every failure, across two binaries, carries the sentinel: the child is an
    /// environment refusal, named by the tests that refused.
    #[test]
    fn a_log_whose_every_failure_carries_the_sentinel_is_the_machines() {
        let log = format!(
            "   Compiling x v0.1.0\n{}{}     Running tests/ok.rs (target/debug/deps/ok-1)\n\n\
             running 1 test\ntest fine ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; \
             0 measured; 0 filtered out; finished in 0.01s\n\nerror: 2 targets failed:\n    \
             `-p x --test service_e2e`\n    `-p x --test paint`\n",
            failed_binary(
                "service_e2e",
                &[("starts_the_service", &refused("a stale peer is alive"))]
            ),
            failed_binary(
                "paint",
                &[
                    ("measuring::row_one", &refused("take unproved")),
                    ("measuring::row_two", &refused("take unproved")),
                ]
            ),
        );
        assert_eq!(
            environment_refusals(&log),
            Some(vec![
                "measuring::row_one".to_string(),
                "measuring::row_two".to_string(),
                "starts_the_service".to_string(),
            ])
        );
    }

    /// A refusal never launders a finding: whatever the sentinel cannot vouch
    /// for keeps the whole child a finding. The control is the shared name
    /// refused in both binaries, which is the machine's, named once.
    #[test]
    fn a_refusal_never_launders_a_finding() {
        let finding = "thread 't' panicked at x.rs:1:1:\nassertion failed: service.ready()";
        let refusal = failed_binary(
            "service_e2e",
            &[("starts_the_service", &refused("a stale peer is alive"))],
        );
        let crash = "     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 test\n\
                     error: test failed, to rerun pass `-p x --test crash`\n\nCaused by:\n  \
                     process didn't exit successfully: `crash-1` (signal: 11, SIGSEGV: invalid \
                     memory reference)\n";
        let cases = [
            (
                "one plain failure beside a refusal",
                failed_binary(
                    "paint",
                    &[
                        ("measuring::row_one", &refused("take unproved")),
                        (
                            "measuring::row_two",
                            "thread 't' panicked at x.rs:1:1:\nPAINT CONFORMANCE FAILED [prompt]",
                        ),
                    ],
                ),
            ),
            (
                "a plain failure alone",
                failed_binary("x", &[("t", "thread 't' panicked at x.rs:1:1:\nassert")]),
            ),
            // Keyed by name, these read as the machine's (measured on the
            // pre-fix function): a block is a position in the log, not a name.
            (
                "a finding sharing a refusal's name in a later binary",
                format!(
                    "{refusal}{}",
                    failed_binary("service_on", &[("starts_the_service", finding)])
                ),
            ),
            (
                "a finding sharing a refusal's name in an earlier binary",
                format!(
                    "{}{refusal}",
                    failed_binary("service_on", &[("starts_the_service", finding)])
                ),
            ),
            (
                "a binary that died without a result",
                format!("{refusal}{crash}"),
            ),
            (
                "a listed failure with no block",
                refusal.replace(
                    "failures:\n    starts_the_service\n",
                    "failures:\n    starts_the_service\n    silent_one\n",
                ),
            ),
            ("no failure at all", "running 0 tests\n".to_string()),
            ("an empty log", String::new()),
        ];
        for (case, log) in &cases {
            assert_eq!(environment_refusals(log), None, "{case}:\n{log}");
        }
        let both = format!(
            "{refusal}{}",
            failed_binary(
                "service_on",
                &[("starts_the_service", &refused("a stale peer is alive"))]
            )
        );
        assert_eq!(
            environment_refusals(&both),
            Some(vec!["starts_the_service".to_string()])
        );
    }

    /// Each failure is named with its binary's re-run spec and its own block, the
    /// refusal is marked, and two binaries failing a test of the same bare name
    /// stay two failures with two specs.
    #[test]
    fn a_failed_test_log_names_each_failure_with_its_binarys_rerun_spec() {
        let log = format!(
            "   Compiling x v0.1.0\n{}{}\x1b[1merror\x1b[0m: 2 targets failed:\n    \
             `-p x --test probe`\n    `-p x --test world`\n",
            failed_binary(
                "probe",
                &[
                    (
                        "index_probe_two",
                        "\nthread 'index_probe_two' panicked at crates/x/tests/probe.rs:9:5:\nlock held",
                    ),
                    ("shared_name", "\nplain failure"),
                ]
            ),
            failed_binary(
                "service",
                &[("shared_name", &refused("a stale peer is alive"))]
            ),
        );
        let got = failed_tests(&log).expect("every failure is accounted for");
        let seen: Vec<(&str, &str, bool)> = got
            .iter()
            .map(|t| (t.spec.as_str(), t.name.as_str(), t.refused))
            .collect();
        assert_eq!(
            seen,
            [
                ("-p x --test probe", "index_probe_two", false),
                ("-p x --test probe", "shared_name", false),
                ("-p x --test service", "shared_name", true),
            ]
        );
        assert!(got[0].block.contains("lock held"), "{:?}", got[0]);
        assert!(!got[0].block.contains("plain failure"), "{:?}", got[0]);
        assert!(got[1].block.contains("plain failure"), "{:?}", got[1]);
    }

    /// A log that cannot account for every failure itemizes nothing, so a
    /// failure the list would miss is never dropped from a comparison. Each
    /// case names the defect; the controls, the same blocks without it, are
    /// itemized.
    #[test]
    fn a_log_that_cannot_account_for_every_failure_itemizes_nothing() {
        let one = failed_binary("probe", &[("a", "\nboom")]);
        assert!(failed_tests(&one).is_some(), "the control: {one}");
        let nested = failed_binary("inner", &[("x", "\ninner boom")]);
        let quotes = |msg: &str| failed_binary("outer", &[("quotes", msg)]);
        let mut cases: Vec<(&str, String)> = vec![
            (
                "a binary that died of a signal",
                format!(
                    "{one}     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 \
                     test\nerror: test failed, to rerun pass `-p x --test crash`\n\nCaused \
                     by:\n  process didn't exit successfully: `crash-1` (signal: 11, SIGSEGV)\n"
                ),
            ),
            (
                "a listed failure with no block",
                one.replace("failures:\n    a\n", "failures:\n    a\n    b\n"),
            ),
            (
                "a block no binary claims",
                one.replace(
                    "error: test failed, to rerun pass `-p x --test probe`\n",
                    "",
                ),
            ),
            ("no failure", "running 0 tests\n".to_string()),
            ("an empty log", String::new()),
            // A test quoting a nested cargo log: its result and re-run lines
            // would cut the block short and credit it to the nested spec.
            (
                "a quoted nested binary",
                quotes(&format!(
                    "\nthread 'quotes' panicked at src/a.rs:1:1:\nladder:\n{nested}tail of the message"
                )),
            ),
            (
                "a quoted nested tail",
                quotes(
                    "\nthread 'quotes' panicked at src/a.rs:1:1:\ntest result: FAILED. 0 passed; \
                     1 failed\nerror: test failed, to rerun pass `-p inner --lib`\nrest",
                ),
            ),
        ];
        // A `failures:` line in a test's own message (an oracle listing the
        // cases that disagreed) must not close its block, whichever block
        // holds it and however its list is indented.
        for list in ["  case_07: got 3 want 4", "    case_07"] {
            let msg = format!(
                "\nthread 'corpus' panicked at src/a.rs:1:1:\nthe corpus disagreed\nfailures:\n{list}"
            );
            cases.push((
                "a failures: line in the last block",
                failed_binary("oracle", &[("corpus", &msg)]),
            ));
            cases.push((
                "a failures: line in the first block",
                failed_binary("oracle", &[("corpus", &msg), ("other", "\nboom")]),
            ));
        }
        // A binary that printed its result and then died (a teardown crash):
        // cargo's `Caused by:` under the re-run line leaves the log unaccounted,
        // and no refusal either.
        let refusing = failed_binary("render", &[("t", &refused("a stale peer is alive"))]);
        assert!(environment_refusals(&refusing).is_some(), "{refusing}");
        for why in [
            "(signal: 11, SIGSEGV: invalid memory reference)",
            "(killed by a signal)",
            "(exit status: 134)",
        ] {
            let tail = format!(
                "\nCaused by:\n  process didn't exit successfully: `target/debug/deps/render-abc` \
                 {why}\n"
            );
            assert_eq!(
                environment_refusals(&format!("{refusing}{tail}")),
                None,
                "{why}"
            );
            cases.push((
                "a binary that died after its result",
                format!("{one}{tail}"),
            ));
        }
        for (case, log) in &cases {
            assert_eq!(failed_tests(log), None, "{case}:\n{log}");
        }
        let plain = quotes("\nthread 'quotes' panicked\nrest");
        assert_eq!(failed_tests(&plain).map(|t| t.len()), Some(1));
        let oracle = failed_binary(
            "oracle",
            &[("corpus", "\nthe corpus disagreed\n  case_07: got 3 want 4")],
        );
        let got = failed_tests(&oracle).expect("accounted for");
        assert!(got[0].block.contains("case_07"), "{:?}", got[0].block);
    }
}

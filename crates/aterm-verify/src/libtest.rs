// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading a `targo test` child's log.

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
/// line?
#[must_use]
pub fn ran_no_test(log: &str) -> bool {
    if log.contains('\x1b') {
        return ran_no_test(&strip_ansi(log));
    }
    log.lines().any(|l| header(l).is_some()) && !log.lines().any(|l| l.starts_with("test result: "))
}

/// Why a test target of a selective `targo test` child that exited 0 decided nothing ([`passed_by_target`] gives it no passed test).
pub const PASSED_NONE: &str = "not one row passed: the filter selected none (a renamed \
     module, a lost `--ignored`), so it measured nothing";

/// Each test binary of a `targo test` child's log, in log order.
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

/// Drop every CSI escape (`ESC [` … a final byte in `@`–`~`) and every other two-byte escape, so a coloured log reads exactly like a plain one.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        rest = if let Some(body) = after.strip_prefix('[') {
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

/// The path in a `Running <target> (<path>)` header.
pub(crate) fn binary_path(header: &str) -> Option<&str> {
    header.rsplit_once(" (")?.1.strip_suffix(')')
}

/// The words a test prints when the machine refused it, not the tree.
pub use aterm_uds::host::COULD_NOT_RUN as COULD_NOT_RUN_SENTINEL;

/// Whether `log` carries [`crate::findings::CEILING_KILL`], plain or ANSI-coloured.
#[must_use]
pub(crate) fn has_ceiling_kill(log: &str) -> bool {
    if log.contains('\x1b') {
        return strip_ansi(log).contains(crate::findings::CEILING_KILL);
    }
    log.contains(crate::findings::CEILING_KILL)
}

/// libtest's opening count line (`running <n> test` or `running <n> tests`).
fn libtest_banner(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("running ") else {
        return false;
    };
    let Some(n) = rest
        .strip_suffix(" test")
        .or_else(|| rest.strip_suffix(" tests"))
    else {
        return false;
    };
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
}

/// cargo's `--no-fail-fast` summary header (`error: <n> target(s) failed:`).
fn cargo_summary_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("error: ") else {
        return false;
    };
    let Some(n) = rest
        .strip_suffix(" target failed:")
        .or_else(|| rest.strip_suffix(" targets failed:"))
    else {
        return false;
    };
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
}

/// The failed tests in a failed `targo test` child's log, when every failure carries [`COULD_NOT_RUN_SENTINEL`].
#[must_use]
pub fn environment_refusals(log: &str) -> Option<Vec<String>> {
    if log.contains('\x1b') {
        return environment_refusals(&strip_ansi(log));
    }
    let mut blocks: Vec<(&str, bool)> = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    let mut current: Option<usize> = None;
    let mut in_list = false;
    let mut open_libtest = false;
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
        if current.is_none()
            && (abnormal_exit(line)
                || compile_failure_line(line)
                || line.contains(crate::findings::CEILING_KILL))
        {
            return None;
        }
        if current.is_none() && header(line).is_some() && open_libtest {
            return None;
        }
        if current.is_none() && libtest_banner(line) {
            open_libtest = true;
        }
        if current.is_none() && line.starts_with("test result: ") {
            open_libtest = false;
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
    (!open_libtest
        && !blocks.is_empty()
        && blocks.iter().all(|(_, refused)| *refused)
        && listed == named
        && binaries > 0
        && results == binaries)
        .then(|| {
            named.dedup();
            named.into_iter().map(str::to_string).collect()
        })
}

/// Why the tests in `names` decided nothing, as a COULD NOT RUN row says it.
#[must_use]
pub fn refusal_reason(who: &str, names: &[String], log: &str) -> String {
    let shown: Vec<&str> = names.iter().take(4).map(String::as_str).collect();
    let more = names.len().saturating_sub(shown.len());
    let sentinel = COULD_NOT_RUN_SENTINEL;
    let why = match refusal_reasons(log).as_slice() {
        [] => format!("see `{sentinel}` in their output"),
        [one] => one.clone(),
        [first, rest @ ..] => format!(
            "{first} (the first of {} reasons; each follows `{sentinel}` in their output)",
            rest.len() + 1
        ),
    };
    format!(
        "{who} refused the machine, not the tree ({}{}) — {why}",
        shown.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        },
    )
}

/// What the refusals in a log said.
#[must_use]
pub fn refusal_reasons(log: &str) -> Vec<String> {
    if log.contains('\x1b') {
        return refusal_reasons(&strip_ansi(log));
    }
    let mut reasons: Vec<String> = Vec::new();
    for line in log.lines() {
        let Some((_, rest)) = line.split_once(COULD_NOT_RUN_SENTINEL) else {
            continue;
        };
        let why = rest
            .trim_start_matches(|c: char| c == ':' || c == '—' || c.is_whitespace())
            .trim_end();
        if !why.is_empty() && !reasons.iter().any(|r| r == why) {
            reasons.push(why.to_string());
        }
    }
    reasons
}

/// One failed test, as a `--no-fail-fast` `targo test` log accounts for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedTest {
    /// cargo's re-run spec for the binary that failed it.
    pub spec: String,
    /// The test's name as libtest listed it.
    pub name: String,
    /// Everything its `---- <name> stdout ----` block held.
    pub block: String,
    /// The block carries [`COULD_NOT_RUN_SENTINEL`].
    pub refused: bool,
}

/// cargo's word that a test binary did not exit with libtest's 101: the
/// `Caused by:` under the re-run line, and the `process didn't exit
/// successfully: … (signal: …)` under that.
fn abnormal_exit(line: &str) -> bool {
    line.trim_end() == "Caused by:"
        || line
            .trim_start()
            .starts_with("process didn't exit successfully")
}

/// cargo's `` error: test failed, to rerun pass `<spec>` `` (or the doctest
/// spelling) — the spec.
fn rerun_spec(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("error: test failed, to rerun pass `")
        .or_else(|| line.strip_prefix("error: doctest failed, to rerun pass `"))?;
    let spec = rest.split('`').next()?;
    (!spec.is_empty()).then_some(spec)
}

/// Every failed test in a failed `targo test` child's log, each with the binary that failed it ([`FailedTest::spec`]).
#[must_use]
pub fn failed_tests(log: &str) -> Option<Vec<FailedTest>> {
    if log.contains('\x1b') {
        return failed_tests(&strip_ansi(log));
    }
    let mut done: Vec<FailedTest> = Vec::new();
    let mut pending: Vec<(String, String, bool)> = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    let mut listed_here: Vec<&str> = Vec::new();
    let mut result_here = false;
    let mut current: Option<usize> = None;
    let mut in_list = false;
    let mut closing = false;
    let mut open_libtest = false;
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
        if closing && !line.starts_with("test result: ") {
            return None;
        }
        if current.is_some() && (line.starts_with("test result: ") || rerun_spec(line).is_some()) {
            return None;
        }
        if current.is_none()
            && (abnormal_exit(line)
                || compile_failure_line(line)
                || line.contains(crate::findings::CEILING_KILL))
        {
            return None;
        }
        if current.is_none() && header(line).is_some() && open_libtest {
            return None;
        }
        if current.is_none() && libtest_banner(line) {
            open_libtest = true;
        }
        if line.starts_with("test result: ") {
            closing = false;
            if current.is_none() {
                open_libtest = false;
            }
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
    (!open_libtest
        && pending.is_empty()
        && !done.is_empty()
        && listed == named
        && binaries > 0
        && results == binaries)
        .then_some(done)
}

/// Whether `line` is a top-level compiler/cargo build failure line.
fn compile_failure_line(line: &str) -> bool {
    line.starts_with("error[")
        || line.starts_with("error: could not compile ")
        || line.starts_with("error: failed to compile ")
        || line.starts_with("error: build failed")
}

/// Whether `line` starts a cargo build/check step or top-level compile failure outside a binary's
/// test run.
fn cargo_build_or_compile_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Compiling ")
        || trimmed.starts_with("Checking ")
        || trimmed.starts_with("Documenting ")
        || compile_failure_line(line)
}

/// Whether `chunk` carries a compiler/cargo build failure line.
fn has_compile_failure(chunk: &[&str]) -> bool {
    chunk.iter().copied().any(compile_failure_line)
}

/// Whether a failed child's log attributes every failure to `sentence` (`ENOSPC`): never on a
/// ceiling kill, and in a `targo test` log any binary section that completed cleanly (`test result:
/// ok.`, a skipped doctest pass, or a `harness = false` binary that finished with exit 0) is
/// ignored so simulated full-disk output from a passing test never masks a later crash, timeout,
/// test failure, or compile error.
#[must_use]
pub(crate) fn out_of_disk(log: &str, sentence: &str) -> bool {
    if log.contains('\x1b') {
        return out_of_disk(&strip_ansi(log), sentence);
    }
    if log.contains(crate::findings::CEILING_KILL) {
        return false;
    }
    if let Some(failed) = failed_tests(log)
        && !failed.is_empty()
    {
        return failed.iter().all(|test| test.block.contains(sentence));
    }
    let mut lead: Vec<&str> = Vec::new();
    let mut sections: Vec<Vec<&str>> = Vec::new();
    let mut in_block = false;
    for line in log.lines() {
        if line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
            .is_some()
        {
            in_block = true;
        } else if line == "failures:"
            || line.starts_with("test result: ")
            || rerun_spec(line).is_some()
            || abnormal_exit(line)
        {
            in_block = false;
        }
        if !in_block && header(line).is_some() {
            sections.push(vec![line]);
        } else if let Some(cur) = sections.last_mut() {
            cur.push(line);
        } else {
            lead.push(line);
        }
    }
    if sections.is_empty() {
        return log.contains(sentence);
    }
    let mut outside_chunks: Vec<Vec<&str>> = Vec::new();
    if !lead.is_empty() {
        outside_chunks.push(lead);
    }
    let total_sections = sections.len();
    let mut non_passing = 0usize;
    for (idx, section) in sections.into_iter().enumerate() {
        let pass_end = section
            .iter()
            .rposition(|l| {
                l.starts_with("test result: ok.") || l.starts_with("aterm-verify: no doctest in ")
            })
            .filter(|&end| {
                !section[end + 1..].iter().any(|l| {
                    l.starts_with("test result: FAILED.")
                        || rerun_spec(l).is_some()
                        || abnormal_exit(l)
                        || libtest_banner(l)
                })
            });
        if let Some(end) = pass_end {
            if end + 1 < section.len() {
                outside_chunks.push(section[end + 1..].to_vec());
            }
            continue;
        }
        let no_failure_trailer = !section.iter().any(|l| {
            libtest_banner(l)
                || l.starts_with("test result: ")
                || rerun_spec(l).is_some()
                || abnormal_exit(l)
        });
        let tail_start = section.iter().enumerate().skip(1).find_map(|(i, l)| {
            (cargo_summary_line(l) || cargo_build_or_compile_line(l)).then_some(i)
        });
        if no_failure_trailer && (idx + 1 < total_sections || tail_start.is_some()) {
            if let Some(start) = tail_start {
                outside_chunks.push(section[start..].to_vec());
            }
            continue;
        }
        non_passing += 1;
        let joined = section.join("\n");
        match failed_tests(&joined) {
            Some(failed) if !failed.is_empty() => {
                if !failed.iter().all(|test| test.block.contains(sentence)) {
                    return false;
                }
            }
            _ => {
                if !joined.contains(sentence) {
                    return false;
                }
            }
        }
    }
    for chunk in &outside_chunks {
        if has_compile_failure(chunk) && !chunk.iter().any(|line| line.contains(sentence)) {
            return false;
        }
    }
    if non_passing > 0 {
        true
    } else {
        outside_chunks
            .iter()
            .flatten()
            .any(|line| line.contains(sentence))
    }
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

    #[test]
    fn a_full_disk_a_passing_test_simulated_does_not_hide_a_failed_test() {
        let run = |output: String| crate::exec::Run {
            ok: false,
            output,
            code: Some(101),
            spawn_error: None,
        };
        let simulated = format!(
            "     Running link (target/debug/deps/link-abc)\n\nrunning 1 test\n\
             aterm-link: post 1 reservation refused, left queued: {} (os error 28)\n\
             test sim::full ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; \
             0 filtered out; finished in 0.10s\n\n",
            crate::exec::ENOSPC_SENTENCE
        );
        let red = failed_binary(
            "db",
            &[(
                "round",
                "thread 'round' panicked at src/a.rs:1:1:\nassertion failed",
            )],
        );
        assert_eq!(
            run(format!("{simulated}{red}")).environment_failure(),
            None,
            "a finding"
        );
        let full = failed_binary(
            "db",
            &[(
                "round",
                &format!(
                    "thread 'round' panicked at src/a.rs:1:1:\n{}",
                    crate::exec::ENOSPC_SENTENCE
                ),
            )],
        );
        assert!(
            run(full)
                .environment_failure()
                .is_some_and(|why| why.contains("ran out of disk")),
            "each failed test's own block names it"
        );
        assert!(
            run(format!(
                "error: failed to write target/x: {}",
                crate::exec::ENOSPC_SENTENCE
            ))
            .environment_failure()
            .is_some(),
            "a build that died of it"
        );
        let killed = "     Running unittests src/lib.rs (target/debug/deps/shell-abc)\n\n\
                      running 133 tests\ntest tests::one ... ok\n\
                      error: test failed, to rerun pass `-p aterm-shell-integration --lib`\n\n\
                      Caused by:\n  process didn't exit successfully: `shell-abc` (killed by a signal)\n";
        assert_eq!(
            run(format!("{simulated}{killed}")).environment_failure(),
            None,
            "a passing binary's simulated full disk never hides a later signal death"
        );
        let timed_out = format!(
            "{simulated}     Running unittests src/lib.rs (target/debug/deps/shell-abc)\n\n\
             running 133 tests\n{} — child killed after 10800.0s of work, over the 10800.0s \
             ceiling (--timeout <seconds|off>)\n  child: shell-abc\n",
            crate::findings::CEILING_KILL
        );
        assert_eq!(
            run(timed_out).environment_failure(),
            None,
            "a ceiling kill is always a finding, never out-of-disk"
        );
        let crashed_enospc = format!(
            "     Running tests/io.rs (target/debug/deps/io-abc)\n\nrunning 1 test\n\
             fatal write error: {}\nerror: test failed, to rerun pass `-p x --test io`\n\n\
             Caused by:\n  process didn't exit successfully: `io-abc` (signal: 6, SIGABRT)\n",
            crate::exec::ENOSPC_SENTENCE
        );
        assert!(
            run(format!("{simulated}{crashed_enospc}"))
                .environment_failure()
                .is_some_and(|why| why.contains("ran out of disk")),
            "a binary that genuinely aborted on ENOSPC still reports out-of-disk"
        );
        assert_eq!(
            run(format!("{simulated}{red}{crashed_enospc}")).environment_failure(),
            None,
            "a plain test failure beside an ENOSPC crash stays a finding"
        );
        assert_eq!(
            run(format!(
                "{simulated}   Compiling y v0.1.0\nerror[E0308]: mismatched types\n"
            ))
            .environment_failure(),
            None,
            "a later compile error after a passing simulated-ENOSPC binary stays a finding"
        );
        assert!(
            run(format!(
                "{simulated}   Compiling y v0.1.0\nerror: failed to write target/y: {}\n",
                crate::exec::ENOSPC_SENTENCE
            ))
            .environment_failure()
            .is_some_and(|why| why.contains("ran out of disk")),
            "a later compile error that itself hit ENOSPC reports out-of-disk"
        );
        let custom_ok = "     Running tests/custom.rs (target/debug/deps/custom-abc)\n";
        assert!(
            run(format!("{custom_ok}{crashed_enospc}"))
                .environment_failure()
                .is_some_and(|why| why.contains("ran out of disk")),
            "a passing harness=false binary before an ENOSPC crash still reports out-of-disk"
        );
        assert_eq!(
            run(format!("{simulated}{custom_ok}")).environment_failure(),
            None,
            "an unfinished harness=false binary after a passing simulated-ENOSPC binary stays a finding"
        );
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
        assert_eq!(
            refusal_reasons(&log),
            ["a stale peer is alive", "take unproved"],
            "each distinct why once, in log order"
        );
        let colon = format!("{COULD_NOT_RUN_SENTINEL}: node could not start; install node\n");
        assert_eq!(
            refusal_reasons(&format!("\x1b[31m{colon}\x1b[0m{colon}")),
            ["node could not start; install node"]
        );
    }

    /// A test that re-runs itself alone in a child (aterm-render's
    /// `alone::in_own_process`) re-raises the child's refusal with the child's
    /// why on its own sentinel line and the child's stderr below it: one
    /// refusal, so the row gives that why once, in full, and no count.
    #[test]
    fn a_reraised_refusal_is_one_reason_the_childs() {
        let name = "seal_maps_discovered_faces::the_seal_maps_every_discovered_face";
        let why = "this host has none of the chain/symbol/emoji candidate files";
        let child = format!(
            "\nthread '{name}' (48286027) panicked at tests/seal_maps_discovered_faces.rs:93:9:\n\
             {COULD_NOT_RUN_SENTINEL} — {why}\nnote: run with `RUST_BACKTRACE=1`\n"
        );
        let row = |first_line: &str| {
            let block = format!(
                "\nthread '{name}' (48286024) panicked at tests/common/alone.rs:63:9:\n\
                 {COULD_NOT_RUN_SENTINEL} — {first_line}\n{child}"
            );
            let log = failed_binary("render_suite", &[(name, &block)]);
            let run = crate::exec::Run {
                ok: false,
                output: log.clone(),
                code: Some(101),
                spawn_error: None,
            };
            (refusal_reasons(&log), run.environment_failure())
        };

        let (reasons, failure) = row(&format!("{why}\n({name}, alone in its own process)"));
        assert_eq!(reasons, [why], "the child's why, once");
        let failure = failure.expect("the machine's, not a finding");
        assert!(
            failure.ends_with(&format!("({name}) — {why}")),
            "the row names the child's why in full, with no count: {failure}"
        );

        let (reasons, failure) = row(&format!("{name}, alone in its own process:"));
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(
            failure
                .expect("still the machine's")
                .contains("alone in its own process: (the first of 2 reasons"),
            "a wrapper's own line first hides the why"
        );
    }

    /// A refusal never launders a finding: whatever the sentinel cannot vouch
    /// for keeps the whole child a finding.
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
                "a binary cut off mid-run with no cargo trailer",
                format!(
                    "{refusal}     Running unittests src/lib.rs (target/debug/deps/shell-1)\n\n\
                     running 133 tests\ntest tests::one ... ok\n"
                ),
            ),
            (
                "a ceiling kill after a refusal",
                format!(
                    "{refusal}{} — child killed after 10800.0s of work\n",
                    crate::findings::CEILING_KILL
                ),
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
    /// failure the list would miss is never dropped from a comparison.
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

    #[test]
    fn out_of_disk_never_masks_trailing_compile_errors_after_custom_or_enospc_binaries() {
        let sentence = "No space left on device";
        let custom_then_compile = "\
     Running tests/custom_harness.rs (target/debug/deps/custom-1)\n\
simulated fault injection: No space left on device (os error 28)\n\
   Compiling broken v0.1.0 (/Users//example/aterm/crates/broken)\n\
error[E0425]: cannot find value `missing` in this scope\n\
error: could not compile `broken` (test \"broken\") due to 1 previous error\n";
        assert!(
            !out_of_disk(custom_then_compile, sentence),
            "passing custom-harness simulated ENOSPC must not mask a later compile error"
        );

        let enospc_then_pass_then_compile = format!(
            "{}     Running unittests src/lib.rs (target/debug/deps/link-1)\n\n\
             running 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 failed; \
             0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n\
                Documenting broken v0.1.0 (/Users//example/aterm/crates/broken)\n\
             error[E0425]: cannot find value `missing` in this scope\n\
             error: could not compile `broken` (doc) due to 1 previous error\n",
            failed_binary(
                "writer",
                &[(
                    "disk_full",
                    "thread 'disk_full' panicked:\nNo space left on device"
                )],
            ),
        );
        assert!(
            !out_of_disk(&enospc_then_pass_then_compile, sentence),
            "trailing non-ENOSPC compile failure must not be hidden behind an earlier ENOSPC binary"
        );

        let custom_enospc_fail = "\
     Running tests/custom_harness.rs (target/debug/deps/custom-1)\n\
fatal: No space left on device (os error 28)\n\
error: test failed, to rerun pass `-p x --test custom_harness`\n\n\
Caused by:\n\
  process didn't exit successfully: `custom-1` (exit status: 1)\n";
        assert!(
            out_of_disk(custom_enospc_fail, sentence),
            "genuinely failing custom-harness binary reporting ENOSPC still classifies as out_of_disk"
        );
    }
}

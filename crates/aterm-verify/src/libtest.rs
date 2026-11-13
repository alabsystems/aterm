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

/// What one failed case's captured block says of its machine: the one rule every reader of a
/// block applies ([`failed_tests`], [`environment_refusals`], [`out_of_disk`], and the live view
/// of a running binary's cases, `exec`'s `LiveCases`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Marks {
    /// The block carries [`COULD_NOT_RUN_SENTINEL`]: the case refused its machine.
    pub(crate) refused: bool,
    /// The block names [`crate::exec::ENOSPC_SENTENCE`]: the case hit a full disk.
    pub(crate) disk: bool,
}

/// [`Marks`] of a case's block, plain or ANSI-coloured.
#[must_use]
pub(crate) fn marks(block: &str) -> Marks {
    let plain = if block.contains('\x1b') {
        std::borrow::Cow::Owned(strip_ansi(block))
    } else {
        std::borrow::Cow::Borrowed(block)
    };
    Marks {
        refused: plain.contains(COULD_NOT_RUN_SENTINEL),
        disk: plain.contains(crate::exec::ENOSPC_SENTENCE),
    }
}

/// Whether the failed cases `seen` so far, each by its block's [`Marks`], already make their
/// binary a FAIL whatever its other cases do: a binary is NOT RUN only when every failed case's
/// block refused ([`environment_refusals`]) or every one hit a full disk ([`out_of_disk`]), so
/// once neither can hold of the cases seen, none still to come can restore it.
#[must_use]
pub(crate) fn decisive(seen: &[Marks]) -> bool {
    !seen.is_empty() && !seen.iter().all(|m| m.refused) && !seen.iter().all(|m| m.disk)
}

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
    banner_count(line).is_some()
}

/// The count libtest's opening line ([`libtest_banner`]) says it runs; `None` for any other line
/// (a count past `u64` included).
fn banner_count(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("running ")?;
    let n = rest
        .strip_suffix(" test")
        .or_else(|| rest.strip_suffix(" tests"))?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// How many tests one test binary's log says libtest runs: the count of its first opening line
/// ([`libtest_banner`], whole and at a line's start, read plain when the log is coloured), after
/// its filter; `None` when it printed none (it never reached its harness, or it is not libtest).
#[must_use]
pub fn tests_run(log: &str) -> Option<u64> {
    if log.contains('\x1b') {
        return tests_run(&strip_ansi(log));
    }
    log.lines()
        .find_map(|l| banner_count(l.trim_end_matches('\r')))
}

/// Counts reported by one completed libtest harness, distinct from its opening
/// announced count, which includes ignored tests. These are diagnostic output,
/// not evidence that a provider action or other external effect succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

/// Read the last result line of a single binary only when all outcome counts
/// are present, agree with its announced count and agree with its verdict.
/// A missing, partial or malformed final summary stays unknown; an earlier
/// nested summary cannot fill it in.
pub(crate) fn test_counts(log: &str) -> Option<TestCounts> {
    if log.contains('\x1b') {
        return test_counts(&strip_ansi(log));
    }
    let announced = tests_run(log)?;
    let result = log
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("test result: "))
        .next_back()?;
    let (verdict, fields) = result.split_once(". ")?;
    let mut fields = fields.split("; ");
    let count = |field: &str, suffix: &str| {
        let digits = field.strip_suffix(suffix)?;
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| digits.parse::<u64>().ok())
            .flatten()
    };
    let counts = TestCounts {
        passed: count(fields.next()?, " passed")?,
        failed: count(fields.next()?, " failed")?,
        ignored: count(fields.next()?, " ignored")?,
        measured: count(fields.next()?, " measured")?,
        filtered_out: count(fields.next()?, " filtered out")?,
    };
    let elapsed = fields.next()?.strip_prefix("finished in ")?;
    if elapsed.strip_suffix('s').is_none_or(str::is_empty) || fields.next().is_some() {
        return None;
    }
    let total = counts
        .passed
        .checked_add(counts.failed)?
        .checked_add(counts.ignored)?
        .checked_add(counts.measured)?;
    (total == announced && matches!((verdict, counts.failed), ("ok", 0) | ("FAILED", 1..)))
        .then_some(counts)
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
    let mut blocks: Vec<(&str, String)> = Vec::new();
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
            blocks.push((name, String::new()));
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
        if let Some(at) = current {
            blocks[at].1.push_str(line);
            blocks[at].1.push('\n');
        }
    }
    let mut named: Vec<&str> = blocks.iter().map(|(name, _)| *name).collect();
    named.sort_unstable();
    listed.sort_unstable();
    (!open_libtest
        && !blocks.is_empty()
        && blocks.iter().all(|(_, block)| marks(block).refused)
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
    let mut pending: Vec<(String, String)> = Vec::new();
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
            pending.push((name.to_string(), String::new()));
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
            done.extend(pending.drain(..).map(|(name, block)| FailedTest {
                spec: spec.to_string(),
                refused: marks(&block).refused,
                name,
                block,
            }));
            continue;
        }
        if let Some(at) = current {
            let block = &mut pending[at];
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

/// The `---- <name> stdout ----` blocks of `lines`, each up to the next block, the closing
/// `failures:` list, a `test result:` line, cargo's re-run line or its abnormal-exit words.
fn blocks_in<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for line in lines {
        if line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
            .is_some()
        {
            out.push(String::new());
            open = true;
        } else if line == "failures:"
            || line.starts_with("test result: ")
            || rerun_spec(line).is_some()
            || abnormal_exit(line)
        {
            open = false;
        } else if open && let Some(block) = out.last_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    out
}

/// Whether a failed child's log attributes every failure to a full disk
/// ([`crate::exec::ENOSPC_SENTENCE`], each failed case's block read by [`marks`]): never on a
/// ceiling kill, and in a `targo test` log any binary section that completed cleanly (`test result:
/// ok.`, a skipped doctest pass, or a `harness = false` binary that finished with exit 0) is
/// ignored so simulated full-disk output from a passing test never masks a later crash, timeout,
/// test failure, or compile error. A binary that crashed after a failed case whose block shows no
/// full disk is that case's FAIL, as it is when the binary ends with libtest's report: a halted
/// run that stopped at that case and a bare one that let the binary crash read it alike.
#[must_use]
pub(crate) fn out_of_disk(log: &str) -> bool {
    if log.contains('\x1b') {
        return out_of_disk(&strip_ansi(log));
    }
    let sentence = crate::exec::ENOSPC_SENTENCE;
    if log.contains(crate::findings::CEILING_KILL) {
        return false;
    }
    if let Some(failed) = failed_tests(log)
        && !failed.is_empty()
    {
        return failed.iter().all(|test| marks(&test.block).disk);
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
                if !failed.iter().all(|test| marks(&test.block).disk) {
                    return false;
                }
            }
            _ => {
                // A crash (or any end libtest did not report): a failed case's block that shows
                // no full disk is a FAIL of its own, whatever the crash said.
                if blocks_in(section.iter().copied())
                    .iter()
                    .any(|b| !marks(b).disk)
                    || !joined.contains(sentence)
                {
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
#[path = "libtest_tests.rs"]
mod tests;

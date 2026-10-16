// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading a `targo test` child's log: the test a killed child was still
//! running, the tests that failed and whether the machine refused them, the
//! host services that went silent, and the slowest tests. Pure: `&str` in,
//! text out.
//!
//! When a test hangs until the ceiling kills its binary, the argv alone names
//! thousands of tests. libtest prints `test <name> has been running for over 60
//! seconds` for a slow test and `test <name> ... ok` when it finishes, so a
//! slow test with no verdict after it was still running when the kill landed.
//!
//! What libtest writes (`library/test/src/formatters/pretty.rs`):
//!
//! * `write_plain` flushes every fragment, so a multithreaded binary prints a
//!   verdict as three writes (`test X ... `, `ok`, `\n`) and anything else on
//!   the same stdout can land between them.
//! * A binary told to run one test on one thread prints `test X ... ` before
//!   running it and the verdict word later, as a bare `ok` line.
//! * The slow notice is one write of a whole line, and its name is bare:
//!   `test X - should panic ... ok` carries a mode suffix the notice does not.
//!
//! aterm-gui tests re-exec their own binary with `--exact` and inherited
//! stdio, so the log is the parent's lines torn by its children's. Four rules
//! absorb the tears:
//!
//! * A verdict is `test <name> ... ` immediately followed by the verdict word,
//!   anywhere in a line; a bare `test C ... ` prefix glued onto the next line
//!   is not one.
//! * A slow name is cleared only by a verdict at or after its notice: an
//!   earlier verdict for the same name belongs to an earlier run of that test.
//! * A name is listed once however many notices carry it: a re-exec child not
//!   held to one thread prints its own.
//! * A verdict inside a `running 1 test` section that was running the same
//!   name is the child's, never this run's. The child runs the parent's own
//!   name while the parent blocks on it, so its verdict always follows the
//!   parent's notice, and crediting it would drop the one name the reader
//!   needs; the name stays listed and the note says a child reported it. Depth
//!   cannot decide this: the parent runs several threads, so several child
//!   sections can be open at once.
//!
//! The test stage runs each binary itself ([`crate::testrun`]), its log headed
//! by cargo's `     Running <target> (<path>)` (or `   Doc-tests <crate>`)
//! line. Only the lines after the last such header are read, so the note names
//! the one binary that hung. A backticked `Running \`…\`` line is a nested
//! `cargo run` inside a test and does not move the anchor.

use std::collections::BTreeMap;

/// One slow test that never got a verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hung {
    /// The bare name, as libtest's slow notice prints it.
    pub name: String,
    /// The 1-based line of the first slow notice for it, in the whole log —
    /// the log is spliced into the ladder whole, so this is the reader's line.
    pub line: usize,
    /// How many lines the log wrote after that notice.
    pub followed: usize,
    /// A verdict for this name arrived after the notice, but inside a re-exec
    /// child's `running 1 test` section running the same name, so it is the
    /// child's and the name is still reported; the reader may see that verdict
    /// in the log.
    pub child_verdict: bool,
}

/// What the last test binary in a log had, and had not, done.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unfinished {
    /// cargo's header for the binary, without the `Running ` verb —
    /// `unittests src/lib.rs (target/debug/deps/x-abc)` or `Doc-tests <crate>`.
    /// `None` when the log has libtest lines but no cargo header, as a test
    /// binary run by hand prints.
    pub binary: Option<String>,
    /// The count the binary declared in its `running N tests` line.
    pub declared: Option<usize>,
    /// Distinct names given a verdict after the header, mode suffix stripped.
    pub verdicts: usize,
    /// The binary printed its `test result:` line — one for every `running`
    /// header in its section, the re-exec children's included.
    pub finished: bool,
    /// Slow tests with no verdict at or after their notice, first notice first.
    pub hung: Vec<Hung>,
}

const SLOW: &str = " has been running for over ";
const SEP: &str = " ... ";
const TEST: &str = "test ";
const VERDICT_WORDS: [&str; 4] = ["ok", "FAILED", "ignored", "bench"];
/// The suffixes libtest's `TestMode` can print after a name on a verdict line
/// (`library/test/src/types.rs`). Stripped as a suffix, never split on ` - `,
/// because a doctest's own name is `src/lib.rs - foo (line 12)`.
const MODE_SUFFIXES: [&str; 3] = [" - should panic", " - compile fail", " - compile"];

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

/// `true` for a skip row of [`host_skips`]: it names what clears it (a
/// reboot), so no re-run here lifts it.
#[must_use]
pub fn is_host_skip(row: &str) -> bool {
    WEDGED.iter().any(|w| row.starts_with(w.head))
}

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

/// `running N test(s)` — the count.
fn declared_in(line: &str) -> Option<usize> {
    let (n, rest) = line.strip_prefix("running ")?.split_once(' ')?;
    (rest == "test" || rest == "tests")
        .then(|| n.parse().ok())
        .flatten()
}

/// A verdict line's name without libtest's mode suffix, so it matches the
/// bare name the slow notice prints. Exactly one suffix, exactly once.
fn bare(name: &str) -> &str {
    MODE_SUFFIXES
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .unwrap_or(name)
}

/// The name in a slow notice: the text after the last `test ` before the
/// marker. Doctest names contain spaces and parentheses, so no word split.
fn slow_in(line: &str) -> Option<&str> {
    let before = &line[..line.find(SLOW)?];
    let name = &before[before.rfind(TEST)? + TEST.len()..];
    (!name.is_empty()).then_some(name)
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

/// The name in a bare `test <name> ... ` prefix, the first thing libtest writes
/// when it starts a test under one thread. Names the test a `running 1 test`
/// child was asked to run.
fn prefix_in(line: &str) -> Option<&str> {
    let at = line.find(SEP)?;
    let t = line[..at].rfind(TEST)?;
    let name = bare(&line[t + TEST.len()..at]);
    (!name.is_empty()).then_some(name)
}

/// The `N filtered out` of a `test result:` line — non-zero for a `--exact` child run of
/// a binary with more tests in it, zero for the binary's own whole-suite result.
fn filtered_out(line: &str) -> Option<usize> {
    let before = line.split("filtered out").next()?;
    before
        .rsplit(';')
        .next()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Every name given a verdict on this line — a torn line can carry more than
/// one, and a bare `test C ... ` prefix with no verdict word after it is none.
fn verdicts_in(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = line[from..].find(SEP) {
        let at = from + i;
        let after = &line[at + SEP.len()..];
        if VERDICT_WORDS.iter().any(|w| after.starts_with(w))
            && let Some(t) = line[..at].rfind(TEST)
        {
            let name = bare(&line[t + TEST.len()..at]);
            if !name.is_empty() {
                out.push(name);
            }
        }
        from = at + SEP.len();
    }
    out
}

/// Read the last test binary's section of a stage log. `None` when the log
/// has neither a cargo test-binary header nor a `running N tests` line — a
/// wedged compile, say — so the caller adds nothing.
#[must_use]
pub fn scan(log: &str) -> Option<Unfinished> {
    // Stage children write to a file, so nothing is coloured unless a caller
    // exports `CARGO_TERM_COLOR=always`; then CSI sequences around the verdict
    // words and headers would defeat every match below.
    if log.contains('\x1b') {
        return scan(&strip_ansi(log));
    }
    let lines: Vec<&str> = log.lines().collect();
    let anchor = lines.iter().rposition(|l| header(l).is_some());
    let binary = anchor.and_then(|a| header(lines[a]));
    let start = anchor.map_or(0, |a| a + 1);

    let mut declared = None;
    let (mut runs, mut results) = (0usize, 0usize);
    // name -> (first notice, last notice), in section indices, first seen first
    let mut slow: Vec<(&str, usize, usize)> = Vec::new();
    // name -> every verdict index, in section indices
    let mut verdict: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    // Re-exec child sections, whose verdicts are never this run's (module doc):
    // each `running 1 test` opens one, the first test prefix after it names it,
    // and a `test result:` with a non-zero `filtered out` closes the oldest one
    // still open.
    let mut children: Vec<(usize, Option<usize>, Option<&str>)> = Vec::new();
    for (i, line) in lines[start..].iter().enumerate() {
        if let Some(n) = declared_in(line) {
            runs += 1;
            declared.get_or_insert(n);
            // The binary's own run is the first `running N`, and it declares 1
            // when the caller filtered to one test, so only later runs can be
            // children.
            if n == 1 && runs > 1 {
                children.push((i, None, None));
            }
        }
        if let Some(name) = slow_in(line) {
            match slow.iter_mut().find(|(n, _, _)| *n == name) {
                Some(seen) => seen.2 = i,
                None => slow.push((name, i, i)),
            }
        }
        let names = verdicts_in(line);
        // The child's name is the first test prefix after its `running 1 test`,
        // with or without a verdict word after it (a torn prefix is common).
        if let Some(name) = prefix_in(line)
            && let Some(open) = children
                .iter_mut()
                .rev()
                .find(|(_, close, n)| close.is_none() && n.is_none())
        {
            open.2 = Some(name);
        }
        for name in &names {
            verdict.entry(name).or_default().push(i);
        }
        if line.starts_with("test result:") {
            results += 1;
            if filtered_out(line).is_some_and(|f| f > 0)
                && let Some(open) = children.iter_mut().find(|(_, close, _)| close.is_none())
            {
                open.1 = Some(i);
            }
        }
    }
    if binary.is_none() && runs == 0 {
        return None;
    }
    // A verdict inside a child section that was running the same name is that child's.
    let a_childs = |name: &str, at: usize| {
        children.iter().any(|(open, close, n)| {
            *n == Some(name) && *open <= at && close.is_none_or(|c| at <= c)
        })
    };
    let hung: Vec<Hung> = slow
        .into_iter()
        .filter_map(|(name, first, last)| {
            let after: Vec<usize> = verdict
                .get(name)
                .map(|v| v.iter().copied().filter(|at| *at >= last).collect())
                .unwrap_or_default();
            if after.iter().any(|at| !a_childs(name, *at)) {
                return None; // this run's own verdict arrived: not hung
            }
            let line = start + first + 1;
            Some(Hung {
                name: name.to_string(),
                line,
                followed: lines.len() - line,
                child_verdict: !after.is_empty(),
            })
        })
        .collect();
    Some(Unfinished {
        binary,
        declared,
        verdicts: verdict.len(),
        finished: runs > 0 && results >= runs,
        hung,
    })
}

/// One timed verdict: `test <name> ... <word> <1.234s>`, the shape libtest
/// prints under `-Zunstable-options --report-time` (the test run's arguments,
/// `stages::LIBTEST_REPORT_TIME`).
#[derive(Clone, Debug, PartialEq)]
pub struct Timed {
    /// Wall seconds libtest measured for the test.
    pub secs: f64,
    /// cargo's header for the binary it ran in, without the verb, or `None`
    /// before any header.
    pub binary: Option<String>,
    /// The bare name (mode suffix stripped).
    pub name: String,
}

/// The `<1.234s>` right after a verdict word, if there is one.
fn time_after_word(after_sep: &str) -> Option<f64> {
    let word = VERDICT_WORDS.iter().find(|w| after_sep.starts_with(*w))?;
    let rest = after_sep[word.len()..].strip_prefix(" <")?;
    let secs = rest.split_once("s>")?.0;
    secs.parse()
        .ok()
        .filter(|s: &f64| s.is_finite() && *s >= 0.0)
}

/// Every timed verdict in a log, in log order, each credited to the last
/// cargo `Running` header before it. A torn verdict — the word on a later line
/// than its `test <name> ... ` prefix — carries no name on its own line and is
/// not counted, the same rule [`verdicts_in`] keeps.
#[must_use]
pub fn timed_verdicts(log: &str) -> Vec<Timed> {
    if log.contains('\x1b') {
        return timed_verdicts(&strip_ansi(log));
    }
    let mut out = Vec::new();
    let mut binary: Option<String> = None;
    for line in log.lines() {
        if let Some(h) = header(line) {
            binary = Some(h);
            continue;
        }
        let mut from = 0;
        while let Some(i) = line[from..].find(SEP) {
            let at = from + i;
            if let Some(secs) = time_after_word(&line[at + SEP.len()..])
                && let Some(t) = line[..at].rfind(TEST)
            {
                let name = bare(&line[t + TEST.len()..at]);
                if !name.is_empty() {
                    out.push(Timed {
                        secs,
                        binary: binary.clone(),
                        name: name.to_string(),
                    });
                }
            }
            from = at + SEP.len();
        }
    }
    out
}

/// The `n` slowest tests of a `--report-time` log, as indented ladder lines:
/// the long poles inside each binary, which its `finished in` cannot show. A
/// name timed more than once in one binary (a re-exec child re-running it) is
/// listed once, at its longest. `None` when the log has no timed verdict.
#[must_use]
pub fn slowest(log: &str, n: usize) -> Option<String> {
    let all = timed_verdicts(log);
    if all.is_empty() {
        return None;
    }
    let total = all.len();
    let mut best: BTreeMap<(Option<String>, String), f64> = BTreeMap::new();
    for t in all {
        let e = best.entry((t.binary, t.name)).or_insert(t.secs);
        if t.secs > *e {
            *e = t.secs;
        }
    }
    let mut rows: Vec<((Option<String>, String), f64)> = best.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut s = format!(
        "  slowest tests (libtest --report-time; {} of {total} timed verdicts):\n",
        n.min(rows.len())
    );
    for ((binary, name), secs) in rows.into_iter().take(n) {
        s.push_str(&format!(
            "  {secs:>9.3}s  {name}   [{}]\n",
            binary
                .as_deref()
                .map_or_else(|| "?".to_string(), short_binary)
        ));
    }
    Some(s)
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

/// A shell word: quoted only when it needs to be (doctest names have spaces).
fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":/.-_=+@%,~".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn lines_word(n: usize) -> &'static str {
    if n == 1 { "line" } else { "lines" }
}

/// The note `exec`'s TIMEOUT block splices in under its `child:` line: indented,
/// newline-terminated, or `None` when the log carries no libtest lines at all.
#[must_use]
pub fn note(log: &str) -> Option<String> {
    let u = scan(log)?;
    let hdr = u
        .binary
        .as_deref()
        .unwrap_or("(no cargo `Running` header in the log)");
    let mut s = String::new();
    if u.finished {
        s.push_str(&format!(
            "  test binary: {hdr} finished (its `test result:` line printed): the wedge is in \
             cargo between or after test binaries, not in a test\n"
        ));
        return Some(s);
    }
    if u.hung.is_empty() {
        s.push_str(&format!(
            "  test binary: {hdr} — it never printed its `test result:` line, and libtest \
             reported no test slow without a verdict: the wedge is outside any test libtest \
             timed, or in a test that started under 60 s before the kill\n"
        ));
    } else {
        s.push_str(&format!(
            "  test binary: {hdr} — it never printed its `test result:` line\n\
             \x20 still running when killed (libtest reported it slow and no verdict for it \
             followed):\n"
        ));
        for h in &u.hung {
            s.push_str(&format!(
                "    {}   (slow at child line {}; {} {} followed{})\n",
                h.name,
                h.line,
                h.followed,
                lines_word(h.followed),
                if h.child_verdict {
                    "; a re-exec child of this binary ran the same name and DID report a \
                     verdict — that one is the child's, not this run's"
                } else {
                    ""
                }
            ));
            if let Some(path) = u.binary.as_deref().and_then(binary_path) {
                s.push_str(&format!(
                    "    re-run alone: {path} --exact {} --nocapture\n",
                    shell_word(&h.name)
                ));
            }
        }
    }
    s.push_str(&match u.declared {
        Some(d) => format!(
            "  {} of that binary's {d} tests have a verdict in the log above.\n",
            u.verdicts
        ),
        None => format!("  {} tests have a verdict in the log above.\n", u.verdicts),
    });
    Some(s)
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
    const BINARY: &str = "unittests src/lib.rs (target/debug/deps/x-abc)";

    fn hung_names(u: &Unfinished) -> Vec<&str> {
        u.hung.iter().map(|h| h.name.as_str()).collect()
    }

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

    /// A silent `trustd`: each binary that printed aterm-uds's line is named
    /// once, by crate and target, and a log without it names none.
    #[test]
    fn a_binary_that_printed_the_trustd_line_is_named_once() {
        let log = format!(
            "     Running unittests src/lib.rs (target/debug/deps/aterm_uds-1f2e)\n\
             {s}\nrunning 2 tests\n{s}\ntest result: ok. 2 passed\n\
             \x20    Running unittests src/lib.rs (target/debug/deps/aterm_grid-9a)\n\
             running 1 test\ntest result: ok. 1 passed\n\
             \x20    Running tests/contract.rs (target/debug/deps/contract-77)\n\
             {s}\ntest result: ok. 4 passed\n\
             \x20    Running tests/signs.rs (target/debug/deps/signs-5c)\n\
             running 1 test\ntest t::signs ... {s}\nok\ntest result: ok. 1 passed\n",
            s = skipped::SKIPPED
        );
        assert_eq!(
            host_skips(&log),
            [
                "tests: trustd did not answer — 3 test binaries skipped their code-signing and \
                 TLS checks (aterm_uds unittests src/lib.rs, contract tests/contract.rs, signs \
                 tests/signs.rs); rebooting the Mac clears a wedged trustd"
            ]
        );
        assert!(is_host_skip(&host_skips(&log)[0]));
        let clean = "     Running unittests src/lib.rs (target/debug/deps/x-abc)\n\
                     test result: ok. 1 passed\n";
        assert!(host_skips(clean).is_empty());
    }

    /// A wedged disk-image service (`hdiutil create` timing out, a host fault)
    /// is its own row, beside trustd's, each naming only the binaries that
    /// printed its line.
    #[test]
    fn a_silent_disk_image_service_is_its_own_row() {
        let log = format!(
            "     Running unittests src/lib.rs (target/debug/deps/aterm_uds-1f2e)\n\
             running 2 tests\ntest p::dead ... {d}\nok\n{t}\ntest result: ok. 2 passed\n\
             \x20    Running tests/signs.rs (target/debug/deps/signs-5c)\n\
             {t}\ntest result: ok. 1 passed\n",
            d = skipped::DISK_IMAGE_SKIPPED,
            t = skipped::SKIPPED
        );
        assert_eq!(
            host_skips(&log),
            [
                "tests: trustd did not answer — 2 test binaries skipped their code-signing and \
                 TLS checks (aterm_uds unittests src/lib.rs, signs tests/signs.rs); rebooting \
                 the Mac clears a wedged trustd",
                "tests: hdiutil did not answer — 1 test binary skipped its disk-image checks \
                 (aterm_uds unittests src/lib.rs); rebooting the Mac clears a wedged disk-image \
                 service"
            ]
        );
        assert!(host_skips(&log).iter().all(|r| is_host_skip(r)));
    }

    /// A verdict with `<1.234s>` after its word is still a verdict, and the time
    /// is read off it: credited to the binary whose header came last, mode suffix
    /// stripped, a torn verdict not counted, and a re-exec child's second timing
    /// of the same name kept at the longest.
    #[test]
    fn a_report_time_suffix_is_a_verdict_and_a_duration() {
        let log = "     Running unittests src/lib.rs (target/debug/deps/aterm_gui-66ca)\n\
                   running 3 tests\n\
                   test a::fast ... ok <0.001s>\n\
                   test a::slow - should panic ... ok <12.500s>\n\
                   test a::torn ... \n\
                   ok <9.000s>\n\
                   test a::slow ... ok <3.000s>\n\
                   \x20    Running tests/paint.rs (target/debug/deps/paint-99)\n\
                   test b::take ... FAILED <40.250s>\n\
                   test b::skip ... ignored, flaky <0.000s>\n";
        let t = timed_verdicts(log);
        let got: Vec<(&str, f64)> = t.iter().map(|x| (x.name.as_str(), x.secs)).collect();
        assert_eq!(
            got,
            [
                ("a::fast", 0.001),
                ("a::slow", 12.5),
                ("a::slow", 3.0),
                ("b::take", 40.25)
            ]
        );
        assert_eq!(
            t[3].binary.as_deref(),
            Some("tests/paint.rs (target/debug/deps/paint-99)")
        );
        // The hung-test reader is unmoved by the suffix.
        assert_eq!(verdicts_in("test a::fast ... ok <0.001s>"), ["a::fast"]);
        let table = slowest(log, 2).expect("timed verdicts");
        assert_eq!(
            table,
            "  slowest tests (libtest --report-time; 2 of 4 timed verdicts):\n\
             \x20    40.250s  b::take   [paint tests/paint.rs]\n\
             \x20    12.500s  a::slow   [aterm_gui unittests src/lib.rs]\n"
        );
        assert_eq!(
            slowest("test a ... ok\n", 5),
            None,
            "no --report-time, no table"
        );
        // The failed-test reader never reads a verdict line, so a timed
        // `FAILED <…s>` changes nothing it accounts for.
        let failed = "     Running tests/x.rs (target/debug/deps/x-1)\n\nrunning 1 test\n\
                      test t::broke ... FAILED <0.500s>\n\nfailures:\n\n---- t::broke stdout \
                      ----\nboom\n\nfailures:\n    t::broke\n\ntest result: FAILED. 0 passed; 1 \
                      failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.50s\n\n\
                      error: test failed, to rerun pass `-p x --test x`\n";
        let tests = failed_tests(failed).expect("accounted for");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "t::broke");
    }

    #[test]
    fn a_slow_test_with_no_verdict_is_named_once() {
        let log = format!(
            "{HEADER}running 3 tests\n\
             test a::one ... ok\n\
             test a::hang has been running for over 60 seconds\n\
             test a::two ... ok\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert_eq!(
            u.hung,
            [Hung {
                name: "a::hang".into(),
                line: 4,
                followed: 1,
                child_verdict: false
            }]
        );
        assert_eq!(u.declared, Some(3));
        assert_eq!(u.verdicts, 2);
        assert!(!u.finished);
        assert_eq!(u.binary.as_deref(), Some(BINARY));
        let n = note(&log).expect("a note");
        assert_eq!(
            n,
            "  test binary: unittests src/lib.rs (target/debug/deps/x-abc) — it never printed \
             its `test result:` line\n\
             \x20 still running when killed (libtest reported it slow and no verdict for it \
             followed):\n\
             \x20   a::hang   (slow at child line 4; 1 line followed)\n\
             \x20   re-run alone: target/debug/deps/x-abc --exact a::hang --nocapture\n\
             \x20 2 of that binary's 3 tests have a verdict in the log above.\n"
        );
    }

    #[test]
    fn a_slow_test_whose_verdict_arrives_is_not_named() {
        let log = format!(
            "{HEADER}running 3 tests\n\
             test a::one ... ok\n\
             test a::hang has been running for over 60 seconds\n\
             test a::two ... ok\n\
             test a::hang ... ok\n\
             \n\
             test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 61.02s\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert!(u.hung.is_empty(), "{u:?}");
        assert!(u.finished);
        assert_eq!(u.verdicts, 3);
        assert_eq!(
            note(&log).as_deref(),
            Some(
                "  test binary: unittests src/lib.rs (target/debug/deps/x-abc) finished (its \
                 `test result:` line printed): the wedge is in cargo between or after test \
                 binaries, not in a test\n"
            )
        );
    }

    #[test]
    fn an_unfinished_binary_with_no_slow_test_says_so() {
        let log = format!("{HEADER}running 2 tests\ntest a::b ... ok\n");
        let n = note(&log).expect("a note");
        assert!(n.starts_with("  test binary: unittests src/lib.rs (target/debug/deps/x-abc) — it never printed its `test result:` line, and libtest reported no test slow without a verdict"), "{n}");
        assert!(n.contains("started under 60 s before the kill"), "{n}");
        assert!(
            n.ends_with("  1 of that binary's 2 tests have a verdict in the log above.\n"),
            "{n}"
        );
        assert!(!n.contains("still running when killed"), "{n}");
    }

    /// A one-thread re-exec child's `test C ... ` prefix has no newline until C
    /// finishes, so it is glued onto whatever the parent writes next, here P's
    /// whole verdict. The prefix is not a verdict for C; P's torn verdict still
    /// is one for P.
    #[test]
    fn a_torn_reexec_child_prefix_is_not_a_verdict_and_a_torn_parent_verdict_still_counts() {
        let mut log = format!(
            "{HEADER}running 3 tests\n\
             \n\
             running 1 test\n\
             test C ... test P ... ok\n\
             test Q ... ok\n\
             ok\n\
             \n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4873 filtered out; \
             finished in 6.36s\n\
             \n\
             test C has been running for over 60 seconds\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert_eq!(hung_names(&u), ["C"]);
        assert_eq!(u.verdicts, 2, "P and Q, not C: {u:?}");
        assert!(!u.finished, "two `running` headers, one `test result:`");

        log.push_str("test C ... ok\n");
        let u = scan(&log).expect("a test binary's section");
        assert!(u.hung.is_empty(), "{u:?}");
        assert_eq!(u.verdicts, 3);
    }

    #[test]
    fn a_child_that_finished_before_the_parents_slow_line_does_not_clear_it() {
        // An untorn child verdict for C, then the parent's slow notice for C:
        // the child's run of C is over, the parent's is the one still going.
        let log = format!(
            "{HEADER}running 2 tests\n\
             \n\
             running 1 test\n\
             test C ... ok\n\
             \n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4873 filtered out; \
             finished in 0.40s\n\
             \n\
             test C has been running for over 60 seconds\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert_eq!(hung_names(&u), ["C"]);
    }

    #[test]
    fn a_child_that_reports_its_own_slow_line_does_not_name_the_test_twice() {
        // A re-exec child that is not held to one thread takes libtest's
        // `recv_timeout` branch and prints its own notice for the same name.
        let log = format!(
            "{HEADER}running 2 tests\n\
             test C has been running for over 60 seconds\n\
             test D ... ok\n\
             test C has been running for over 60 seconds\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert_eq!(
            u.hung,
            [Hung {
                name: "C".into(),
                line: 3,
                followed: 2,
                child_verdict: false
            }]
        );
    }

    #[test]
    fn a_verdict_with_a_mode_suffix_clears_the_bare_slow_name() {
        // libtest's verdict line carries the mode, its slow notice does not.
        let log = format!(
            "{HEADER}running 1 test\n\
             test a::boom has been running for over 60 seconds\n\
             test a::boom - should panic ... ok\n"
        );
        let u = scan(&log).expect("a test binary's section");
        assert!(u.hung.is_empty(), "{u:?}");
        assert_eq!(u.verdicts, 1);

        for mode in [" - compile fail", " - compile"] {
            let log = format!(
                "   Doc-tests aterm_core\n\
                 running 1 test\n\
                 test src/x.rs - foo (line 3) has been running for over 60 seconds\n\
                 test src/x.rs - foo (line 3){mode} ... ok\n"
            );
            let u = scan(&log).expect("a test binary's section");
            assert!(u.hung.is_empty(), "{mode}: {u:?}");
            assert_eq!(u.verdicts, 1, "{mode}");
        }
        // A suffix strip, never a split on ` - `: the doctest's own ` - ` stays.
        let log = "   Doc-tests aterm_core\n\
                   running 1 test\n\
                   test src/x.rs - foo (line 3) has been running for over 60 seconds\n\
                   test src/x.rs - bar (line 9) ... ok\n";
        let u = scan(log).expect("a test binary's section");
        assert_eq!(hung_names(&u), ["src/x.rs - foo (line 3)"]);
    }

    #[test]
    fn only_the_last_binary_is_scanned() {
        let log = "     Running unittests src/lib.rs (target/debug/deps/first-111)\n\
                   \n\
                   running 1 test\n\
                   test one::slow has been running for over 60 seconds\n\
                   test one::slow ... ok\n\
                   \n\
                   test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                   finished in 70.00s\n\
                   \n\
                   \x20    Running tests/it.rs (target/debug/deps/it-222)\n\
                   \n\
                   running 2 tests\n\
                   test two::fine ... ok\n\
                   \x20    Running `target/debug/aterm-lab tool`\n\
                   test two::stuck has been running for over 60 seconds\n";
        let u = scan(log).expect("a test binary's section");
        assert_eq!(
            u.binary.as_deref(),
            Some("tests/it.rs (target/debug/deps/it-222)")
        );
        assert_eq!(hung_names(&u), ["two::stuck"]);
        assert_eq!(u.declared, Some(2));
        assert_eq!(
            u.verdicts, 1,
            "the first binary's verdicts are not this binary's"
        );
        assert!(!u.finished);
        let n = note(log).expect("a note");
        assert!(
            n.contains("re-run alone: target/debug/deps/it-222 --exact two::stuck --nocapture"),
            "{n}"
        );
    }

    #[test]
    fn doctest_names_with_spaces_and_the_doc_tests_header_parse() {
        let log = "   Doc-tests aterm_gui\n\
                   \n\
                   running 2 tests\n\
                   test src/lib.rs - foo (line 12) has been running for over 60 seconds\n\
                   test src/lib.rs - bar (line 3) ... ok\n";
        let u = scan(log).expect("a doctest section");
        assert_eq!(hung_names(&u), ["src/lib.rs - foo (line 12)"]);
        assert_eq!(u.binary.as_deref(), Some("Doc-tests aterm_gui"));
        assert_eq!(u.declared, Some(2));
        let n = note(log).expect("a note");
        assert!(
            n.contains("  test binary: Doc-tests aterm_gui — it never printed"),
            "{n}"
        );
        assert!(
            n.contains("    src/lib.rs - foo (line 12)   (slow at child line 4; 1 line followed)"),
            "{n}"
        );
        // No path to re-run a doctest binary by; the name is the diagnosis.
        assert!(!n.contains("re-run alone"), "{n}");
    }

    #[test]
    fn a_coloured_log_reads_like_a_plain_one() {
        // What cargo/libtest emit with colour on: SGR around the verdict word and
        // around cargo's `Running` line. The answer must not change.
        let log = "     \x1b[0m\x1b[1m\x1b[32mRunning\x1b[0m unittests src/lib.rs \
                   (target/debug/deps/x-abc)\n\
                   \n\
                   running 2 tests\n\
                   test a::one ... \x1b[32mok\x1b[0m\n\
                   test a::hang has been running for over 60 seconds\n";
        let u = scan(log).expect("a test binary's section");
        assert_eq!(hung_names(&u), ["a::hang"]);
        assert_eq!(u.verdicts, 1, "{u:?}");
        assert_eq!(
            u.binary.as_deref(),
            Some("unittests src/lib.rs (target/debug/deps/x-abc)"),
            "{u:?}"
        );
        let n = note(log).expect("a note");
        assert!(n.contains("--exact a::hang --nocapture"), "{n}");
    }

    #[test]
    fn a_log_without_libtest_lines_adds_nothing() {
        let log = "   Compiling aterm-gui v0.87.0 (/w/crates/aterm-gui)\n\
                   \x20  Compiling aterm-core v0.87.0 (/w/crates/aterm-core)\n\
                   error: could not compile `aterm-gui`\n";
        assert_eq!(scan(log), None);
        assert_eq!(note(log), None);
        assert_eq!(note(""), None);
    }

    /// tests/fixtures/hung-test-stage.log is an excerpt of a real hung run's log,
    /// lines without a libtest marker cut at 120 columns. Three tests were
    /// reported slow: one's own verdict arrived, one's only verdict was a
    /// child's, and one got none.
    #[test]
    fn the_real_logs_shapes_name_exactly_the_hung_test() {
        let log = include_str!("../tests/fixtures/hung-test-stage.log");
        let u = scan(log).expect("the aterm-gui unittests section");
        assert_eq!(
            hung_names(&u),
            [
                "native_settings::tests::visual_preview_matrix_covers_four_viewports_at_three_text_scales",
                "control::tests::cross_session_paste_reports_a_dead_spill_peer_as_write_failed",
            ],
            "the byline name's own verdict arrived; the other two never did: {u:?}"
        );
        // The one whose only verdict was a child's says so; the other has none.
        let flags: Vec<bool> = u.hung.iter().map(|h| h.child_verdict).collect();
        assert_eq!(flags, [true, false], "{u:?}");
        assert_eq!(
            u.binary.as_deref(),
            Some("unittests src/lib.rs (target/debug/deps/aterm_gui-66cafa00b6862bbd)")
        );
        assert_eq!(u.declared, Some(4874));
        assert!(!u.finished);
        let n = note(log).expect("a note");
        assert!(
            n.contains(
                "    re-run alone: target/debug/deps/aterm_gui-66cafa00b6862bbd --exact \
                 control::tests::cross_session_paste_reports_a_dead_spill_peer_as_write_failed \
                 --nocapture"
            ),
            "{n}"
        );
        assert!(
            !n.contains("native_about_byline"),
            "its verdict arrived:\n{n}"
        );
        assert!(
            n.contains("a re-exec child of this binary ran the same name"),
            "the child-verdict case is named as such:\n{n}"
        );
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
             `-p x --test bridge_e2e`\n    `-p x --test paint`\n",
            failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays alive"))]),
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
                "boots_a_world".to_string(),
                "measuring::row_one".to_string(),
                "measuring::row_two".to_string(),
            ])
        );
    }

    /// One real finding among the refusals makes the whole child a finding: a
    /// refusal never launders a failure.
    #[test]
    fn one_failure_without_the_sentinel_makes_the_whole_child_a_finding() {
        let log = failed_binary(
            "paint",
            &[
                ("measuring::row_one", &refused("take unproved")),
                (
                    "measuring::row_two",
                    "thread 't' panicked at x.rs:1:1:\nPAINT CONFORMANCE FAILED [prompt]",
                ),
            ],
        );
        assert_eq!(environment_refusals(&log), None);
        // …and with only the finding, likewise.
        let log = failed_binary("x", &[("t", "thread 't' panicked at x.rs:1:1:\nassert")]);
        assert_eq!(environment_refusals(&log), None);
    }

    /// A test that refused in one binary and failed a real assertion in another
    /// stays a finding: blocks are keyed by position, never by name. The control,
    /// both blocks refused, is the machine's and names the test once.
    #[test]
    fn a_finding_that_shares_a_refusals_name_in_another_binary_is_still_a_finding() {
        let finding = "thread 't' panicked at x.rs:1:1:\nassertion failed: world.ready()";
        let log = format!(
            "{}{}",
            failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays alive"))]),
            failed_binary("fabric_on", &[("boots_a_world", finding)]),
        );
        assert_eq!(environment_refusals(&log), None, "{log}");
        // …in either order.
        let log = format!(
            "{}{}",
            failed_binary("fabric_on", &[("boots_a_world", finding)]),
            failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays alive"))]),
        );
        assert_eq!(environment_refusals(&log), None, "{log}");

        let log = format!(
            "{}{}",
            failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays alive"))]),
            failed_binary("fabric_on", &[("boots_a_world", &refused("strays alive"))]),
        );
        assert_eq!(
            environment_refusals(&log),
            Some(vec!["boots_a_world".to_string()])
        );
    }

    /// A binary that died prints no `test result:`, and a crash is a finding
    /// however its siblings refused.
    #[test]
    fn a_binary_that_died_without_a_result_is_a_finding() {
        let log = format!(
            "{}     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 test\n\
             error: test failed, to rerun pass `-p x --test crash`\n\nCaused by:\n  process \
             didn't exit successfully: `crash-1` (signal: 11, SIGSEGV: invalid memory \
             reference)\n",
            failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays alive"))]),
        );
        assert_eq!(environment_refusals(&log), None);
    }

    /// A failure libtest listed but printed no block for is unread, so the
    /// sentinel cannot be vouched for.
    #[test]
    fn a_listed_failure_with_no_block_is_not_accounted_for() {
        let mut log = failed_binary("bridge_e2e", &[("boots_a_world", &refused("strays"))]);
        log = log.replace(
            "failures:\n    boots_a_world\n",
            "failures:\n    boots_a_world\n    silent_one\n",
        );
        assert_eq!(environment_refusals(&log), None);
        // …and a log with no failure in it at all is nobody's refusal.
        assert_eq!(environment_refusals("running 0 tests\n"), None);
        assert_eq!(environment_refusals(""), None);
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
            failed_binary("world", &[("shared_name", &refused("strays alive"))]),
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
                ("-p x --test world", "shared_name", true),
            ]
        );
        assert!(got[0].block.contains("lock held"), "{:?}", got[0]);
        assert!(!got[0].block.contains("plain failure"), "{:?}", got[0]);
        assert!(got[1].block.contains("plain failure"), "{:?}", got[1]);
    }

    /// A log that cannot account for a failure itemizes nothing: a binary that
    /// died of a signal (a re-run line with no `test result:`), a block the
    /// closing list does not name, and a block no re-run line claims.
    #[test]
    fn a_log_that_cannot_account_for_every_failure_itemizes_nothing() {
        let one = failed_binary("probe", &[("a", "\nboom")]);
        assert!(failed_tests(&one).is_some(), "the control: {one}");
        let crashed = format!(
            "{one}     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 test\n\
             error: test failed, to rerun pass `-p x --test crash`\n\nCaused by:\n  process \
             didn't exit successfully: `crash-1` (signal: 11, SIGSEGV)\n"
        );
        assert_eq!(failed_tests(&crashed), None, "a crash is not itemizable");
        assert_eq!(
            failed_tests(&one.replace("failures:\n    a\n", "failures:\n    a\n    b\n")),
            None,
            "a listed failure with no block"
        );
        let unclaimed = one.replace(
            "error: test failed, to rerun pass `-p x --test probe`\n",
            "",
        );
        assert_eq!(failed_tests(&unclaimed), None, "a block no binary claims");
        assert_eq!(failed_tests("running 0 tests\n"), None);
        assert_eq!(failed_tests(""), None);
    }

    /// A test that quotes a nested cargo log is not itemized: the nested result
    /// and re-run lines would cut its block short and credit it to the nested
    /// spec, whether the quote is a whole failed binary or only its tail.
    #[test]
    fn a_block_that_quotes_a_nested_cargo_log_itemizes_nothing() {
        let nested = failed_binary("inner", &[("x", "\ninner boom")]);
        let outer = failed_binary(
            "outer",
            &[(
                "quotes",
                &format!(
                    "\nthread 'quotes' panicked at src/a.rs:1:1:\nladder:\n{nested}tail of the message"
                ),
            )],
        );
        assert_eq!(failed_tests(&outer), None, "{outer}");
        let tail_only = failed_binary(
            "outer",
            &[(
                "quotes",
                "\nthread 'quotes' panicked at src/a.rs:1:1:\ntest result: FAILED. 0 passed; 1 \
                 failed\nerror: test failed, to rerun pass `-p inner --lib`\nrest",
            )],
        );
        assert_eq!(failed_tests(&tail_only), None, "{tail_only}");
        // The control: the same block without the nested lines is itemized.
        let plain = failed_binary("outer", &[("quotes", "\nthread 'quotes' panicked\nrest")]);
        assert_eq!(failed_tests(&plain).map(|t| t.len()), Some(1));
    }

    /// A `failures:` line in a test's own message (an oracle listing the cases
    /// that disagreed) leaves the log unaccounted rather than closing the block,
    /// however the message's list is indented and whichever block holds it.
    #[test]
    fn a_failures_line_in_a_message_itemizes_nothing() {
        for cases in ["  case_07: got 3 want 4", "    case_07"] {
            let msg = format!(
                "\nthread 'corpus' panicked at src/a.rs:1:1:\nthe corpus disagreed\nfailures:\n{cases}"
            );
            let last = failed_binary("oracle", &[("corpus", &msg)]);
            assert_eq!(failed_tests(&last), None, "{last}");
            let first = failed_binary("oracle", &[("corpus", &msg), ("other", "\nboom")]);
            assert_eq!(failed_tests(&first), None, "{first}");
        }
        // The control: the same message without its `failures:` line.
        let plain = failed_binary(
            "oracle",
            &[("corpus", "\nthe corpus disagreed\n  case_07: got 3 want 4")],
        );
        let got = failed_tests(&plain).expect("accounted for");
        assert!(got[0].block.contains("case_07"), "{:?}", got[0].block);
    }

    /// A binary that printed its result and then died (a teardown crash, a
    /// background thread's abort after the summary) balances its list against
    /// its blocks, but cargo's `Caused by:` / `process didn't exit successfully`
    /// lines under the re-run line leave the log unaccounted, and no refusal.
    #[test]
    fn a_binary_that_died_after_its_result_is_not_accounted_for() {
        let one = failed_binary("render", &[("t", "\nknown red")]);
        assert!(failed_tests(&one).is_some(), "the control: {one}");
        let refusing = failed_binary("render", &[("t", &refused("strays alive"))]);
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
            let crashed = format!("{one}{tail}");
            assert_eq!(failed_tests(&crashed), None, "{crashed}");
            let crashed = format!("{refusing}{tail}");
            assert_eq!(environment_refusals(&crashed), None, "{crashed}");
        }
    }
}

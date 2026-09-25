// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Naming the test a killed `targo test` child was still running.
//!
//! WHY (2026-09-16). The gate's aterm-gui test binary sat three hours on
//! `control::tests::cross_session_paste_reports_a_dead_spill_peer_as_write_failed`
//! until the wall-clock ceiling killed it, and the TIMEOUT block named only the
//! argv — `targo --unverified test --workspace --no-fail-fast --tests`, which in
//! that run spanned 12,976 tests over 218 binaries, of which the one that hung
//! declared 4,874. The evidence was in the child's own log the whole time:
//! libtest prints `test <name> has been running for over 60 seconds` for a slow
//! test and `test <name> ... ok` when it finishes, so a slow test with no
//! verdict after it is the one still running when the kill landed. This module
//! reads that out of the log. It is pure (`&str` in, text out), so the shapes
//! below are pinned by inline fixtures and by
//! `tests/fixtures/hung-test-stage.log` — a CONSTRUCTED log in the real log's
//! shapes, with its real names, because a verbatim excerpt cannot be
//! self-contained (a child section's opening and closing lines sit thousands of
//! lines apart and four are open at once) and the whole 6,023-line section is
//! 552 KB. That section is checked all the same, by the opt-in
//! `ATERM_VERIFY_HUNG_LOG` case beside the fixture's. It is std-only string
//! scanning because this crate has no dependencies on purpose (see Cargo.toml).
//!
//! WHAT LIBTEST ACTUALLY WRITES, read from an upstream libtest source
//! (nightly-2025-11-13, `library/test/src/formatters/pretty.rs`; the Trust
//! toolchain this gate runs ships no `rust-src`, and the shapes below are the
//! ones the 2026-09-16 log exhibits):
//!
//! * `write_plain` flushes every fragment, so a multithreaded binary prints a
//!   verdict as three writes — `test X ... `, `ok`, `\n` — and anything else
//!   on the same stdout can land between them.
//! * A binary told to run ONE test on one thread prints `test X ... ` BEFORE
//!   running it and the verdict word later, as a bare `ok` line.
//! * The slow notice is one write of a whole line, and the name in it is
//!   bare: `test X - should panic ... ok` carries a mode suffix the notice
//!   does not.
//!
//! That matters here because aterm-gui tests re-exec their own binary with
//! `--exact` and inherited stdio — 37 such child runs from 21 distinct tests in
//! that log — so the log is the parent's lines torn by its children's. Four
//! rules absorb the tears: one the real log forced, three the shapes above make
//! possible and the fixtures pin.
//!
//! * A VERDICT is `test <name> ... ` immediately followed by the verdict word,
//!   anywhere in a line; a bare `test C ... ` prefix glued onto the next line is
//!   not one.
//! * A slow name is cleared only by a verdict at or after its notice, since a
//!   child's verdict for the same name printed earlier belongs to an earlier run
//!   of that test.
//! * A name is listed once however many notices carry it, because a re-exec
//!   child that is not held to one thread prints its own.
//! * WHOSE VERDICT IT IS. A child runs the PARENT'S OWN NAME and the parent
//!   blocks on it, so a verdict for the hung name is guaranteed to arrive after
//!   the notice — and crediting it would drop the one name the reader needs. A
//!   verdict that falls inside a `running 1 test` section which was running that
//!   same name is the child's, never this run's; the name stays listed and the
//!   note says a child reported it. Depth alone cannot decide this: the parent
//!   runs four threads, so several child sections are open at once and a notice
//!   can be printed inside one while the parent's own verdict for it lands after
//!   they close. (Found by the review of this module, 2026-09-17: in that log
//!   8 of the 37 child runs printed their verdict untorn, and one of them —
//!   `visual_preview_matrix_…` — landed 870 lines before the parent's own for the
//!   same name.)
//!
//! ANCHOR. cargo runs test binaries one at a time even under `--no-fail-fast`,
//! so the hung binary is the LAST one cargo announced — the last
//! `     Running <target> (<path>)` or `   Doc-tests <crate>` line — and only
//! the lines after it are read. A backticked `Running \`…\`` line is a nested
//! `cargo run` inside a test, not a binary of this run, and does not move it.

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
    /// A verdict for this name DID arrive after the notice, but inside a nested
    /// `running 1 test` section — a re-exec child of this binary running the same
    /// name. It cannot be this run's verdict (the parent is blocked on that child
    /// while it runs), so the name is still reported; this says the reader may see
    /// a verdict for it in the log and should not read that as a contradiction.
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
fn header(line: &str) -> Option<String> {
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
fn strip_ansi(s: &str) -> String {
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

/// The name in a bare `test <name> ... ` PREFIX — the first thing libtest writes when
/// it starts a test under one thread, with or without a verdict word after it. Used to
/// learn which test a `running 1 test` child was asked to run.
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
    // COLOUR. Nothing in this gate asks cargo or libtest for it — stage children
    // write to a file, so neither is on a tty — but a caller that exported
    // `CARGO_TERM_COLOR=always` (or a future lane that does) would put CSI
    // sequences around every verdict word, and then `test X ... ` would not match,
    // the header would not parse, and the note would degrade to its worst shape:
    // no binary, no re-run line, every slow test named. Cheaper to be right: strip
    // the escapes once, and only when there are any.
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
    // WHOSE VERDICT IS IT. A re-exec child of this binary runs the PARENT'S OWN NAME
    // (`current_exe --exact <that name> --nocapture`: 21 distinct tests and 37 child
    // runs in the 2026-09-16 log) and the parent BLOCKS on it, so a verdict for the
    // hung name is guaranteed to arrive after the parent's slow notice — and crediting
    // it would drop the one name the reader needs. Depth alone cannot tell them apart:
    // the parent runs four threads, so several child sections are open at once and a
    // notice can be printed inside one (the log's byline notice is, at depth 2) while
    // the parent's own verdict for it lands after they close (depth 1). What does tell
    // them apart is the window: a verdict for name X that falls INSIDE a child section
    // running X is that child's, never this run's. Each `running 1 test` opens such a
    // section, its name is the first test-prefix printed after it, and a `test result:`
    // with a non-zero `filtered out` closes the oldest one still open.
    let mut children: Vec<(usize, Option<usize>, Option<&str>)> = Vec::new();
    for (i, line) in lines[start..].iter().enumerate() {
        if let Some(n) = declared_in(line) {
            runs += 1;
            declared.get_or_insert(n);
            // The binary's OWN run is the first `running N` after the header — and it
            // declares 1 when the caller filtered to one test, which is exactly what a
            // child looks like. Only the runs after it can be children.
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
        // The child's own name is the first test-prefix after its `running 1 test`,
        // whether or not a verdict word follows it on that line (a torn prefix is the
        // common shape).
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

/// The path in a `Running <target> (<path>)` header — the binary to re-run.
fn binary_path(header: &str) -> Option<&str> {
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

/// The note the TIMEOUT block splices in under its `child:` line: indented,
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

/// The words a test prints when the MACHINE refused it, not the tree: a
/// previous run's daemons still alive (aterm-link's world harness), or a paint
/// take its own instrument disowns as starved (the probe's exit 3). A red row
/// under this sentinel says nothing about the code, so the gate records it as
/// COULD NOT RUN — the same severity as a full disk — and the receipt says
/// `verdict COULD-NOT-RUN`, never `FAIL`.
///
/// Producers print it VERBATIM in their panic message, and
/// `every_producer_prints_the_sentinel_verbatim` below holds each of them to
/// it, so a reworded producer cannot quietly turn its refusals back into
/// findings.
pub const COULD_NOT_RUN_SENTINEL: &str = "aterm-gate: COULD NOT RUN";

/// The failed tests in a failed `targo test` child's log, when EVERY failure
/// carries [`COULD_NOT_RUN_SENTINEL`]; `None` when any failure does not, or
/// when the log cannot account for every failure.
///
/// Conservative in the one direction that matters — a finding must never
/// read as the machine's fault — so all of this must hold:
///
/// * at least one `---- <name> stdout ----` block, and every one of them
///   carries the sentinel;
/// * the names libtest LISTS under its closing `failures:` are exactly the
///   names that had a block, counted with multiplicity, so no failure went
///   unread;
/// * every binary cargo reported failed (`error: test failed, to rerun pass`)
///   printed a `test result: FAILED.` line — a binary that died of a signal
///   prints no result, and a crash is a finding.
///
/// EVERY BLOCK IS ITS OWN FAILURE (2026-09-23). One `--no-fail-fast` log holds
/// many binaries, and two of them can fail a test of the same bare name — a
/// refusal in one, a real assertion in the other. Keyed by name, the second
/// block inherited the first one's sentinel and the finding read as the
/// machine's; so a block is a position in the log, never a name.
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

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "     Running unittests src/lib.rs (target/debug/deps/x-abc)\n";
    const BINARY: &str = "unittests src/lib.rs (target/debug/deps/x-abc)";

    fn hung_names(u: &Unfinished) -> Vec<&str> {
        u.hung.iter().map(|h| h.name.as_str()).collect()
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

    /// Lifted from the real log (stage27, lines 13505-13535): a one-thread
    /// re-exec child's `test C ... ` prefix has no newline until C finishes,
    /// so it is glued onto whatever the parent writes next — here P's whole
    /// verdict. That is structural, not a one-off: 24 lines of that section
    /// carry two `test ` prefixes (log line 13510 is the first). The prefix is
    /// not a verdict for C; P's torn verdict still is one for P.
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
        // libtest's verdict line carries the mode, its slow notice does not
        // (stage27 lines 11256-11257 are the real `- should panic` shape).
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
                   \x20    Running `target/debug/xtask harness-manifest`\n\
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

    /// tests/fixtures/hung-test-stage.log is a verbatim excerpt of the log of
    /// the run that hung on 2026-09-15/16 (verify-fast-77cd5cb47, stage 27,
    /// 15,683 lines): its lines 9660-9668, 11256-11257, 13505-13552,
    /// 15328-15350 and 15675-15683, with `spec_xref_closure:` and `NOTICE`
    /// lines (no libtest marker in them) cut at 120 columns. Three tests were
    /// reported slow; two of them got their verdicts.
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
        // The one whose only verdict was a child's says so; the incident has none at all.
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

    /// THE REAL LOG, when the operator still has it: 6,023 lines the tree cannot hold
    /// (552 KB). `ATERM_VERIFY_HUNG_LOG=<path>` points this at one, and it asserts what
    /// was measured by hand on 2026-09-17 against the 2026-09-16 stage log — exactly one
    /// name, the incident's — so the claim in that commit message is reproducible rather
    /// than remembered. Unset, it says so and asserts nothing: the fixture beside it
    /// carries the permanent coverage.
    #[test]
    fn the_whole_real_log_names_exactly_the_incident_test_when_one_is_given() {
        let Some(path) = std::env::var_os("ATERM_VERIFY_HUNG_LOG") else {
            use std::io::Write as _;
            let _ = std::io::stderr().write_all(
                b"the_whole_real_log_...: ATERM_VERIFY_HUNG_LOG is unset, so the real stage \
                  log is not checked here (the fixture beside it is)\n",
            );
            return;
        };
        let log = std::fs::read_to_string(&path).expect("the named log is readable");
        let u = scan(&log).expect("a test-binary section");
        assert_eq!(
            hung_names(&u),
            ["control::tests::cross_session_paste_reports_a_dead_spill_peer_as_write_failed"],
            "{u:?}"
        );
        assert_eq!(u.declared, Some(4874), "{u:?}");
        assert_eq!(u.verdicts, 4873, "{u:?}");
        assert!(!u.finished, "{u:?}");
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

    /// THE MACHINE, NOT THE TREE. Every failure in the log carries the
    /// sentinel, across two binaries, so the child is an environment refusal
    /// and the names are the ones that refused.
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

    /// THE NEGATIVE CONTROL: one real finding among the refusals, and the
    /// whole child is a finding. A refusal may never launder a failure.
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

    /// ONE NAME, TWO BINARIES, TWO FAILURES. `boots_a_world` refused in one
    /// binary and failed a real assertion in a later one: the finding must not
    /// inherit the refusal's sentinel through the shared name. Keyed by name,
    /// this log read as the machine's (measured on the pre-fix function). The
    /// control is the same log with both blocks refused, which is the
    /// machine's, and names the test once.
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

    /// A binary that DIED — a signal, an abort — prints no `test result:`, and
    /// a crash is a finding however politely its siblings refused.
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

    /// A failure libtest LISTED but printed no block for is a failure nobody
    /// read, and the sentinel cannot be vouched for.
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

    /// THE PRODUCERS, HELD TO THE WORDS. The gate recognises a refusal only by
    /// [`COULD_NOT_RUN_SENTINEL`], so a producer that rewords its panic turns
    /// every refusal back into a finding without a line of the gate changing.
    /// Each file must still carry the sentinel verbatim, once per refusal arm.
    #[test]
    fn every_producer_prints_the_sentinel_verbatim() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (file, arms) in [
            ("crates/aterm-link/tests/harness/mod.rs", 1),
            ("crates/aterm-conformance/tests/paint/measuring.rs", 1),
        ] {
            let text =
                std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(
                text.matches(COULD_NOT_RUN_SENTINEL).count(),
                arms,
                "{file} must print `{COULD_NOT_RUN_SENTINEL}` in each of its {arms} refusal arm(s)"
            );
        }
    }
}

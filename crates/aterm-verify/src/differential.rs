// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DIFFERENTIAL VERDICT (2026-09-26): a red main already has is NAMED, not
//! blocking; a new red blocks.
//!
//! WHY. The merge contract was absolute — whole tree, nothing red — on a
//! `main` that is often red, and every failure was printed as "a finding about
//! the change". So whenever main was red, every run that integrated it was red
//! for reasons outside the branch, the only way to land was around the gate,
//! and a landing that went around it recorded nothing about which reds it
//! carried in. The audit of 2026-09-25 traced four of one day's reds to a
//! single such landing, and found branch after branch spending a full run to
//! rediscover them.
//!
//! THE BASE. A run on HEAD is judged against the receipt of its BASE: the
//! merge-base of HEAD and [`MAIN_REF`] — for a branch that merged main, the
//! main-side parent of that merge (the main commit it merged); for one that
//! did not, the main commit it forked from. EXACTLY that commit: the receipt
//! is looked up under it, then under its tree ([`crate::receipt::tree_key`]),
//! in the shared store, then in the git note [`NOTES_REF`] a `--baseline` run
//! publishes (fetched from [`MAIN_REMOTE`], bounded, when the store has none).
//! An older main commit's receipt cannot answer: it cannot tell a red main
//! still has from one main fixed since and a conflict resolution put back.
//! So a HEAD holding a commit of the local `main` that [`MAIN_REF`] lacks
//! (a stale `origin/main`, whose merge-base is an older main than the one the
//! branch merged) has no base (2026-09-27, second review), and neither has
//! any run whose `origin/main` cannot be confirmed as new as main on
//! [`MAIN_REMOTE`] — read there, bounded, and an unreadable remote confirms
//! nothing (2026-09-28, [`remote_main_fresh`]). Only a whole-tree
//! receipt that lists its failures serves ([`usable`]) — and only one made by
//! the same tools as the run (2026-09-27, third review: the same trustc
//! commit, the same spec checkers and the same build environment taken from
//! the caller, [`tools_differ`] — and, 2026-09-28, the same cargo config
//! files, [`crate::build_config`]): a red another compiler, another `ty`,
//! other flags or another `config.toml` found is not main's under this run's.
//!
//! THE SAME FAILURE. Each failure is itemized ([`Finding`]): per failed test,
//! with the binary that failed it, for a `targo test` child whose log accounts
//! for every failure ([`test_findings`]); per row otherwise ([`row_finding`]).
//! A finding is INHERITED when the base lists the same id with the same
//! [`fingerprint`]: every character of what the failure printed — a failed
//! test's whole block, whitespace, line numbers and paths included — with
//! only the run-to-run noise [`normalize`] names by where it stands masked:
//! where the run happened (the snapshot, the temp dir, a home), thread ids,
//! pids, measured durations, build hashes, frame addresses and commit ids.
//! Never a count, a size or an assertion's values: the same test failing with
//! other values is failing a different way, and that is NEW (a review of
//! 2026-09-27 found the first fingerprint collapsed every digit and hashed
//! only a row's summary lines, so four new guard hits, a new `assert_eq!`
//! value, a new crash under the same smoke label and a different hung test
//! each read as main's; a second review that day found whitespace, line
//! numbers, directories, durations and hex values masked, and a test's output
//! before its panic unread). So is a test the machine refused (nothing was
//! decided about it), a row that printed nothing to compare, a child the gate
//! killed at its ceiling, a test log that cannot account for every failure in
//! it (a binary that crashed or hung, even after its result), and a failure
//! that reads as a clock running out or printed one it read. And so is every
//! whole-row finding whose check is not KNOWN to have run to its end
//! ([`UNFINISHED`], 2026-09-27, fourth review): a suite, a build, a smoke or
//! a driver that stops at its first failure prints the same whatever it did
//! not reach, so main's red there excused a branch's break of what came
//! after it. Only a failed test in an accounted log and a checker whose
//! output shows it reached its end ([`RanToEnd`]) are ever inherited.
//! Everything this cannot judge counts against the change: the differential
//! only ever REMOVES a red that main demonstrably has.
//!
//! THE AGE CAP ([`INHERITED_CAP_SECS`]). Every listed failure carries when main
//! first went red on it (`since`), carried from receipt to receipt while main
//! stays red on that id — whatever it said each time, so a red whose message
//! alternates cannot restart its clock — and through a run that could not
//! itemize it (its receipt's `hidden` lines, [`hidden_failures`]), so a
//! baseline whose test log was unaccounted does not restart it either. A red
//! main has had for longer than the cap blocks again, so red cannot become
//! normal: past it, the fix is on main. A since further ahead of the judging
//! machine's clock than [`CLOCK_SLACK_SECS`] has no readable age, and is
//! never inherited; a judging clock behind main's receipt — or behind main's
//! newest commit, or HEAD's — reads every age short, so the run is judged by
//! the absolute rule ([`Plan::against`]). A branch judged by the absolute
//! rule carries main's clocks all the same ([`resolve`]): once it lands, its
//! receipt is the next branch's base.
//!
//! NO BASE, NO DIFFERENTIAL. No `origin/main`, no merge-base, a HEAD that is
//! itself on main, or no usable receipt for the base: the run is judged by
//! the absolute rule it always was, and says why before any stage runs.
//!
//! THE NEAREST BASE (2026-09-28) is OPT-IN and OFF by default, pending the
//! owner's decision: `--nearest-base` ([`BaseMode::Nearest`], [`resolve_with`])
//! lets a run whose merge-base has no receipt that serves be judged against
//! its newest ancestor's, for the failed tests whose blast radius did not
//! change between the two ([`crate::nearest`], [`judge`]). [`resolve`] is the
//! exact rule, unchanged.
//!
//! THE BASELINE. `tools/verify.sh --baseline`, run on a commit of main (any
//! idle machine, by hand for now), is an ordinary whole-tree run whose receipt
//! is also PUBLISHED: added as a git note on that commit under [`NOTES_REF`]
//! and pushed to [`MAIN_REMOTE`] — from a private ref, retried on a race, so
//! two publishers cannot drop each other's notes. It fetches the published
//! notes FIRST, and is refused when it cannot ([`baseline_refusal`]): when
//! main went red on each failure is carried from them. Every worktree of the
//! repository already shares the receipt through the store; the note is how
//! other machines see it. A note is a record, not evidence, exactly as a
//! receipt file is ([`crate::receipt`]).

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::ladder::{Finding, Tally};
use crate::receipt::{self, Failure, Receipt};

/// The remote main is published on.
pub const MAIN_REMOTE: &str = "origin";

/// Main, as this checkout last fetched it: the ref the base is the merge-base
/// with.
pub const MAIN_REF: &str = "origin/main";

/// Where `--baseline` publishes main's receipts, one git note per commit.
pub const NOTES_REF: &str = "refs/notes/aterm-verify";

/// How long main may have been red on one failure before it stops being
/// excused as inherited: 24 hours.
pub const INHERITED_CAP_SECS: u64 = 24 * 60 * 60;

/// How many first-parent commits the since chain ([`nearest`]) reads.
pub const CHAIN_LIMIT: usize = 500;

/// How long one network git call (a notes fetch or push) may take.
pub const NETWORK_BOUND: Duration = Duration::from_secs(30);

/// Why a refused test cannot be inherited.
const REFUSED: &str =
    "the machine refused it (`aterm-gate: COULD NOT RUN`), so nothing was decided";

/// Why a failure that printed nothing cannot be inherited.
const SILENT: &str = "it printed nothing to compare";

/// Why a child the gate killed at its wall-clock ceiling cannot be inherited.
const HUNG: &str = "the gate killed it at its wall-clock ceiling, and a hang is not a message \
                    to compare";

/// Why a test log that cannot account for its failures cannot be inherited.
const UNACCOUNTED: &str = "its log does not account for every failure in it (a crash, a hang, \
                           a torn log), so none of them can be compared";

/// Why a timing-shaped failure cannot be inherited.
const TIMING_SHAPED: &str = "it reads as a clock running out or prints one it read, and main's \
                             timeout cannot be told from a new hang, or a slower run, that \
                             prints the same words";

/// Why a failure main dates from the future cannot be inherited.
const FUTURE: &str = "main's receipt dates it from the future (a clock ahead of this one), so \
                      its age under the cap cannot be read";

/// What the gate prints when it kills a child at its ceiling
/// ([`crate::exec`]), and what it prints for a test binary the test run's
/// ceiling left no time to start ([`crate::testrun`]).
pub const CEILING_KILL: &str = "aterm-verify: TIMEOUT";

/// How far ahead of the judging machine's clock a failure's since may be and
/// still be read as an age: 15 minutes. Past it, the finding is never
/// inherited ([`judge`]) and its since is not carried ([`recorded_failures`]).
pub const CLOCK_SLACK_SECS: u64 = 15 * 60;

// ---------------------------------------------------------------------------
// Fingerprints
// ---------------------------------------------------------------------------

/// FNV-1a over `lines`, each followed by a newline, as 16 hex digits. Not a
/// security property — whoever writes a receipt can write any hash — only a
/// stable, std-only name for "this failure message".
#[must_use]
pub fn fingerprint<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.bytes().chain(std::iter::once(b'\n')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// One line as a fingerprint sees it: EVERY CHARACTER KEPT — its whitespace
/// too, leading, inner and trailing (only a CRLF's `\r` goes) — with ONLY
/// the run-to-run noise masked that can be named by where it stands:
///
/// * WHERE THE RUN HAPPENED, as the start of an absolute path ([`mask_roots`]):
///   the gate's snapshot (`…/<checkout>-verify.noindex/` → `<snapshot>/`), the
///   temp dir (`/tmp/`, `/private/tmp/`, macOS's `/var/folders/<a>/<b>/T/` →
///   `<tmp>/`, and the per-run entry right under it when its name carries a
///   digit or is a `.tmp…` → `<tmp>/*/`) and a home (`/Users/<name>/`,
///   `/home/<name>/`, `/root/` → `~/`). The rest of the path is the message;
/// * a thread id (`thread 'x' (41377)`) and a pid (`pid 5521`, `pid=5521`,
///   `pid: 5521`): `#`;
/// * a MEASURED duration — right after `in`, `took`, `after`, `elapsed`,
///   `spent`, `waited` or `within` (`finished in 0.91s`, `took 150ms`): `#s`,
///   `#ms`, … — which makes the failure TIMING-SHAPED ([`measured_duration`]),
///   so it is never inherited ([`judge`]): masked, `render took 900ms` and
///   `render took 20ms` hash the same, and the number may be what failed;
/// * an address on a stack or crash frame (a line that opens with a frame's
///   number; `0x` and nine or more hex digits): `0x<hex>`;
/// * cargo's build hash (sixteen hex digits after a `-`, in a path through
///   `deps/` or `build/`): `<hex>`;
/// * a commit id (seven or more hex digits mixing digits and letters, right
///   after `commit`, `HEAD`, `sha`, `tree`, `base`, `rev`, `parent` or
///   `since`): `<hex>`.
///
/// AN ASSERTION'S VALUES ARE DATA, every character of them: on a `left:` or
/// `right:` line (`assert_eq!`'s) no duration, pid, address or commit mask
/// applies.
///
/// EVERYTHING ELSE IS THE MESSAGE (2026-09-27, second review), where the first
/// masks had been broad enough for a new red to hash as main's:
/// * WHITESPACE. Every run of it was one space and each line was trimmed, so
///   in a terminal emulator's own tests `"ab  c"` and `"ab        c"` were one
///   failure, and so were two formatter diffs that differ only in spacing.
///   Two runs of one tree print the same whitespace;
/// * A SOURCE LOCATION'S LINE AND COLUMN, and rustc's snippet gutter. Two runs
///   of one tree print the same ones, and a branch whose red moved line
///   edited that code; masked, a different assertion in the same file failing
///   with the same words (two `unwrap()`s on `None`) read as main's;
/// * AN ABSOLUTE PATH'S OWN DIRECTORIES. It was cut to its last component, so
///   a config resolved to `/etc/aterm.toml` instead of `~/.config/aterm/…` and
///   a formatter diff that moved to another crate's `lib.rs` read as main's;
/// * A DURATION, a hex run or any number anywhere but the contexts above: a
///   parsed timeout (`left: 90s`), an RGBA colour or a digest (`left:
///   "9e0d4c11"`, `got 0x1a2b3c4d5e`) is the value under test.
///
/// Every digit that is not noise was already the message since the first
/// review (2026-09-27): a count, a size, an `assert_eq!`'s two values, a spec
/// checker's state count.
#[must_use]
pub fn normalize(line: &str) -> String {
    masked(line).0
}

/// [`normalize`]'s line, and whether a measured duration was masked in it.
fn masked(line: &str) -> (String, bool) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let head = line.trim_start();
    let value = head.starts_with("left:") || head.starts_with("right:");
    mask_noise(&mask_roots(line), value, opens_with_frame_number(line))
}

/// Does `line` open, after its indentation, with a stack or crash frame's
/// number — `   3: 0x1045d4b8c - std::…` (std's backtrace), `3   libsystem
/// 0x00000001899a2a60 __pthread_kill + 8` (a macOS crash report)?
fn opens_with_frame_number(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && t[digits..].starts_with([':', ' ', '\t'])
}

/// Did any of `lines` print a MEASURED duration ([`normalize`]): a clock was
/// read, and what it read may be what failed.
#[must_use]
pub fn measured_duration<'a>(lines: impl IntoIterator<Item = &'a str>) -> bool {
    lines.into_iter().any(|l| masked(l).1)
}

/// What may stand right before an absolute path's leading `/`.
const PATH_OPENERS: &str = "`'\"([{<=:,";

/// What ends an absolute path (besides whitespace).
const PATH_CLOSERS: &str = "`'\")]}>,;";

/// Every absolute path in `line` with its run-varying root masked
/// ([`root_masked`]); every other character as it was.
fn mask_roots(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let starts = chars[i] == '/'
            && (i == 0 || chars[i - 1].is_whitespace() || PATH_OPENERS.contains(chars[i - 1]));
        if !starts {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !chars[j].is_whitespace() && !PATH_CLOSERS.contains(chars[j]) {
            j += 1;
        }
        let path: String = chars[i..j].iter().collect();
        out.push_str(&root_masked(&path));
        i = j;
    }
    out
}

/// `path` with where the run happened masked, the rest kept: the snapshot
/// (`<snapshot>/…`), the temp dir (`<tmp>/…`, its per-run entry `*`), a home
/// (`~/…`) — or the path as it was.
fn root_masked(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let rest = |from: usize| parts.get(from..).map_or_else(String::new, |p| p.join("/"));
    let rooted = |root: &str, from: usize| {
        let tail = rest(from);
        if tail.is_empty() {
            root.to_string()
        } else {
            format!("{root}/{tail}")
        }
    };
    // The gate's snapshot: `<checkout>-verify.noindex` (and its siblings).
    if let Some(at) = parts
        .iter()
        .position(|p| p.ends_with(".noindex") && p.contains("-verify"))
    {
        return rooted("<snapshot>", at + 1);
    }
    // The temp dir, and the per-run entry the process that made it named.
    let tmp = match parts.get(1..).unwrap_or_default() {
        ["tmp", ..] => Some(2),
        ["private", "tmp", ..] => Some(3),
        ["var", "folders", _, _, "T", ..] => Some(6),
        ["private", "var", "folders", _, _, "T", ..] => Some(7),
        _ => None,
    };
    if let Some(from) = tmp {
        let per_run = parts.get(from).is_some_and(|entry| {
            entry.starts_with(".tmp") || entry.bytes().any(|b| b.is_ascii_digit())
        });
        return if per_run {
            rooted("<tmp>/*", from + 1)
        } else {
            rooted("<tmp>", from)
        };
    }
    // A home.
    match parts.get(1..).unwrap_or_default() {
        ["Users" | "home", name, ..] if !name.is_empty() => rooted("~", 3),
        ["root", ..] => rooted("~", 2),
        _ => path.to_string(),
    }
}

/// The units a duration is printed in, longest first so `ms` is not read as
/// `m` + `s`.
const DURATION_UNITS: [&str; 5] = ["ms", "us", "\u{b5}s", "ns", "s"];

/// The words a MEASURED duration follows (`finished in 0.91s`, `took 150ms`).
const DURATION_AFTER: [&str; 7] = [
    "in", "took", "after", "elapsed", "spent", "waited", "within",
];

/// The words a commit id follows (`commit 43f8b339f`, `HEAD=1a2b3c4`).
const COMMIT_AFTER: [&str; 9] = [
    "commit", "head", "sha", "tree", "base", "rev", "revision", "parent", "since",
];

/// The word right before `chars[i]`, lowercased and without its punctuation:
/// the run of non-space characters ending at `i` (`elapsed=` in
/// `elapsed=3s`), or else the word before the spaces (`took` in `took 3s`).
fn word_before(chars: &[char], i: usize) -> String {
    let mut end = i;
    if end > 0 && chars[end - 1].is_whitespace() {
        while end > 0 && chars[end - 1].is_whitespace() {
            end -= 1;
        }
    }
    let mut start = end;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    chars[start..end]
        .iter()
        .collect::<String>()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_ascii_lowercase()
}

/// Is `chars[i]` inside a path through cargo's `deps/` or `build/` — the
/// whitespace-delimited word around it?
fn in_build_path(chars: &[char], i: usize) -> bool {
    let mut start = i;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = i;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    word.contains("deps/") || word.contains("build/")
}

/// [`normalize`]'s masks over one line, read token by token: a token is a
/// maximal run of ASCII letters and digits, and every mask is decided by the
/// token and the words around it — never by a digit merely being one. On an
/// assertion's `value` line only the build-hash and thread masks apply; on a
/// `frame` line an address is masked too. Returns the line and whether a
/// measured duration was masked in it.
fn mask_noise(s: &str, value: bool, frame: bool) -> (String, bool) {
    let chars: Vec<char> = s.chars().collect();
    let mut duration = false;
    let text = |from: usize, to: usize| -> String { chars[from..to].iter().collect() };
    let is_token = |c: char| c.is_ascii_alphanumeric();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_token(chars[i]) || (i > 0 && is_token(chars[i - 1])) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && is_token(chars[j]) {
            j += 1;
        }
        let token = text(i, j);
        let before = |k: usize| i.checked_sub(k).map(|at| chars[at]);
        let hex = token.bytes().all(|b| b.is_ascii_hexdigit());
        let digits = token.bytes().all(|b| b.is_ascii_digit());
        // An address on a frame: `0x` and nine or more hex digits — where
        // `0x1f`, a `u32` or a digest anywhere else is a value.
        if let Some(rest) = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            && frame
            && !value
            && rest.len() >= 9
            && rest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            out.push_str("0x<hex>");
            i = j;
            continue;
        }
        // cargo's build hash: sixteen hex digits after its `-`, in a path
        // through `deps/` or `build/`.
        if hex && token.len() == 16 && before(1) == Some('-') && in_build_path(&chars, i) {
            out.push_str("<hex>");
            i = j;
            continue;
        }
        // A commit id, after a word that names one.
        let mixed = token.bytes().any(|b| b.is_ascii_digit())
            && token.bytes().any(|b| b.is_ascii_alphabetic());
        if !value
            && hex
            && mixed
            && token.len() >= 7
            && COMMIT_AFTER.contains(&word_before(&chars, i).as_str())
        {
            out.push_str("<hex>");
            i = j;
            continue;
        }
        // A measured duration: `finished in 0.91s`, `took 150ms`.
        if !value
            && DURATION_AFTER.contains(&word_before(&chars, i).as_str())
            && let Some((mask, end)) = duration_at(&chars, i, j)
        {
            out.push_str(&mask);
            duration = true;
            i = end;
            continue;
        }
        if digits {
            // A thread id: `thread 'name' (41377)`.
            let thread = i >= 3
                && text(i - 3, i) == "' ("
                && chars.get(j) == Some(&')')
                && text(0, i).contains("thread '");
            // A pid: `pid 5521`, `pid=5521`, `pid: 5521`, `(pid 5521)`.
            if thread || (!value && pid_before(&chars, i)) {
                out.push('#');
                i = j;
                continue;
            }
        }
        out.push_str(&token);
        i = j;
    }
    (out, duration)
}

/// A duration starting at the token `chars[i..j]`: its mask and where it
/// ends. `150ms`, `3s`; `3.20s` is the token `3`, a `.`, and the token `20s`;
/// `12µs` is the token `12` and `µs`.
fn duration_at(chars: &[char], i: usize, j: usize) -> Option<(String, usize)> {
    let lead = chars[i..j]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if lead == 0 {
        return None;
    }
    let ends_word = |at: usize| chars.get(at).is_none_or(|c| !c.is_ascii_alphanumeric());
    let unit_at = |at: usize| -> Option<(&'static str, usize)> {
        DURATION_UNITS.iter().find_map(|u| {
            let n = u.chars().count();
            let here: String = chars.get(at..at + n)?.iter().collect();
            (here == *u && ends_word(at + n)).then_some((*u, at + n))
        })
    };
    // `150ms` / `3s` in one token, or `12` then `µs`.
    let mut at = i + lead;
    // `3.20s`: a fraction, then the unit.
    if chars.get(at) == Some(&'.') && chars.get(at + 1).is_some_and(char::is_ascii_digit) {
        at += 1;
        while chars.get(at).is_some_and(char::is_ascii_digit) {
            at += 1;
        }
    }
    let (unit, end) = unit_at(at)?;
    Some((format!("#{unit}"), end))
}

/// Is the digit run at `i` a pid: after `pid`, `pid=`, `pid:` or `pid: `,
/// with `pid` a word of its own (any case)?
fn pid_before(chars: &[char], i: usize) -> bool {
    let mut k = i;
    if k > 0 && chars[k - 1] == ' ' {
        k -= 1;
    }
    if k > 0 && matches!(chars[k - 1], '=' | ':') {
        k -= 1;
    }
    k >= 3
        && chars[k - 3..k]
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case("pid")
        && (k == 3 || !chars[k - 4].is_ascii_alphanumeric())
}

/// cargo's progress lines — `Compiling …`, `Finished …`, `Running …` —
/// which differ with the cache and the schedule, never with the failure.
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

/// Is `line` one of cargo's progress lines?
fn is_progress(line: &str) -> bool {
    let t = line.trim_start();
    PROGRESS_VERBS
        .iter()
        .any(|v| t.strip_prefix(v).is_some_and(|rest| rest.starts_with(' ')))
}

/// A line that says what went wrong, as compilers, lints, guards and test
/// harnesses write one — what a row's TIMING label reads ([`row_finding`]),
/// never what its fingerprint is limited to.
fn is_diagnostic(line: &str) -> bool {
    let t = line.trim_start();
    ["error", "ERROR", "fatal", "-->"]
        .iter()
        .any(|p| t.starts_with(p))
        || ["FAIL", "panicked at", "didn't exit successfully"]
            .iter()
            .any(|p| t.contains(p))
}

/// THE WORDS OF A CLOCK RUNNING OUT (2026-09-26), matched case-blind in a
/// failure's message: std's `RecvTimeoutError::Timeout` and `TimedOut`
/// ("timed out"), this tree's "did not finish within" and "deadline", an
/// `elapsed` bound, and the gate's own ceiling kill (`aterm-verify: TIMEOUT`,
/// which the first entry covers). A failure carrying one is TIMING-SHAPED
/// ([`Finding::timing`]): the verdict names it when its stage ran on a loaded
/// machine, as a label — it still counts, nothing is ever retried, and since
/// 2026-09-27 it is never inherited from main ([`judge`]).
///
/// AND THE WORDS OF A POLL THAT GAVE UP (2026-09-27, third review), which
/// name no clock but are one: the smokes' `control socket never started
/// listening` (a hundred polls, [`crate::smoke_stages::SOCKET_POLLS`], ten
/// seconds and each poll's cost), `the window never presented`
/// and `never produced an initial present` (a hundred and fifty), and a
/// control client that answered NOTHING (`<no reply>`, `no metrics reply`) —
/// a crash, a hang and a slow start all leave the same empty answer. Main's
/// control-socket smoke is timing-flaky, and a startup deadlock on a branch
/// printed the same row over the same (often empty) log tail: INHERITED.
pub const TIMING_MARKERS: [&str; 10] = [
    "timeout",
    "timed out",
    "did not finish within",
    "deadline",
    "elapsed",
    "never started listening",
    "never presented",
    "never produced an initial present",
    "<no reply>",
    "no metrics reply",
];

/// What [`Finding::timing`] names for a failure that printed a MEASURED
/// duration ([`measured_duration`]) and no [`TIMING_MARKERS`] entry.
pub const MEASURED_DURATION: &str = "a measured duration";

/// The first [`TIMING_MARKERS`] entry in any of `lines`, case-blind.
#[must_use]
pub fn timing_marker<'a>(lines: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let text: String = lines
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("\n");
    TIMING_MARKERS.iter().find(|m| text.contains(*m)).copied()
}

/// A ladder row's text on one line, as an id.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// THE ROW AS ONE FINDING: `label`, fingerprinted from everything its child
/// printed ([`row_lines`]).
///
/// EVERYTHING, NOT ITS SUMMARY (2026-09-27). The fingerprint was the row's
/// "diagnostic" lines whenever it had any, and for a guard those are its
/// summaries (`  FAIL <check> <count>`, `GUARD: FAIL`): the lines naming each
/// violation were left out, so a branch that added four hits to a check main
/// already failed hashed as main's red. Now every line but cargo's progress
/// counts, and one more line of it — one more violation, one more instance of
/// an error main already has — is a different failure.
///
/// A child the gate killed at its wall-clock ceiling ([`CEILING_KILL`]) is
/// opaque ([`Finding::opaque`]): what hung is not a message, and a different
/// hang reads the same. So is one that printed nothing at all.
#[must_use]
pub fn row_finding(label: &str, output: &str) -> Finding {
    let plain = strip(output);
    let (lines, duration) = row_lines(&plain);
    // Timing-shaped by what the row SAID was wrong — its diagnostic lines, or
    // its label — never by a path or a name somewhere in a long log; or by a
    // measured duration anywhere in what its fingerprint reads, since the
    // fingerprint cannot tell one reading from another.
    let timing = timing_marker(
        plain
            .lines()
            .filter(|l| is_diagnostic(l))
            .chain(std::iter::once(label)),
    )
    .or_else(|| (duration || measured_duration([label])).then_some(MEASURED_DURATION));
    let opaque = if plain.contains(CEILING_KILL) {
        Some(HUNG)
    } else {
        silent(&lines).then_some(SILENT)
    };
    Finding {
        id: one_line(label),
        hash: fingerprint(lines.iter().map(String::as_str)),
        opaque,
        timing,
        package: None,
    }
}

/// What a row's output says, as its fingerprint reads it: every line but
/// cargo's progress ([`is_progress`]), normalized, in BLOCKS — a line that
/// opens at the left margin with neither a digit nor a `|` (rustc's gutter),
/// and the lines under it — and the blocks SORTED, because a lint run reports
/// crates in whatever order they finished. Never deduplicated. With it,
/// whether a measured duration was masked in any line ([`normalize`]).
fn row_lines(plain: &str) -> (Vec<String>, bool) {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut duration = false;
    for line in plain.lines() {
        if is_progress(line) {
            continue;
        }
        let (normalized, timed) = masked(line);
        if normalized.is_empty() {
            continue;
        }
        duration |= timed;
        let opens = line
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace() && !c.is_ascii_digit() && c != '|');
        match blocks.last_mut() {
            Some(block) if !opens => block.push(normalized),
            _ => blocks.push(vec![normalized]),
        }
    }
    blocks.sort_unstable();
    (blocks.into_iter().flatten().collect(), duration)
}

/// A row decided without a child and with nothing printed under it: its label
/// is the whole of what it said (`gui smoke: metrics reset -> ERR …`), so the
/// label is its message. One that printed a log under its row (a smoke whose
/// child exited early, with the child's log tail) is fingerprinted from that
/// log ([`crate::ladder::tally`], [`row_finding`]).
#[must_use]
pub fn label_finding(label: &str) -> Finding {
    let (normalized, duration) = masked(label);
    Finding {
        id: one_line(label),
        hash: fingerprint([normalized.as_str()]),
        opaque: normalized.trim().is_empty().then_some(SILENT),
        timing: timing_marker([label]).or(duration.then_some(MEASURED_DURATION)),
        package: None,
    }
}

/// THE FAILED TESTS OF A `targo test` CHILD, one finding each: id `<re-run
/// spec> -- <name>` (`-p atpkg --test index_probe -- probe_x`), fingerprinted
/// from its whole block ([`test_message`]). A test the machine refused is
/// opaque. When the log cannot account for every failure
/// ([`crate::libtest::failed_tests`]: a binary that crashed — before its
/// result or after it — a log torn or nested), the row is one finding and it
/// is OPAQUE (2026-09-27): the name of a hung test sat only in the ceiling's
/// note, so a different hang hashed the same, and nothing a list would miss
/// may ever be excused. So is a log the gate's ceiling killed anything in
/// ([`CEILING_KILL`]), wherever its note landed — after a binary's `test
/// result:` too (2026-09-27, second review: a binary that printed main's red
/// and then hung in its teardown accounted for its log, and the hang was in no
/// finding).
#[must_use]
pub fn test_findings(label: &str, output: &str) -> Vec<Finding> {
    let plain = strip(output);
    let tests = if plain.contains(CEILING_KILL) {
        None
    } else {
        crate::libtest::failed_tests(&plain)
    };
    let Some(tests) = tests else {
        let mut row = row_finding(label, output);
        row.opaque = row.opaque.or(Some(UNACCOUNTED));
        return vec![row];
    };
    tests
        .into_iter()
        .map(|t| {
            let (lines, duration) = test_message(&t.block);
            Finding {
                id: one_line(&format!("{} -- {}", t.spec, t.name)),
                hash: fingerprint(lines.iter().map(String::as_str)),
                opaque: if t.refused {
                    Some(REFUSED)
                } else if silent(&lines) {
                    Some(SILENT)
                } else {
                    None
                },
                // The panic, and the test's own name: a test named for a
                // deadline is about one — or a clock it read and printed.
                timing: timing_marker(
                    lines
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(t.name.as_str())),
                )
                .or(duration.then_some(MEASURED_DURATION)),
                package: crate::nearest::package_of(&t.spec),
            }
        })
        .collect()
}

/// How a failed test failed: EVERY line of its block, normalized — minus
/// only what the machine's `RUST_BACKTRACE` adds or removes: std's notes about
/// it, and each backtrace (its `stack backtrace:` line and the numbered frames
/// and `at` lines under it; anyhow's `Stack backtrace:` alike).
///
/// THE WHOLE BLOCK (2026-09-27, second review). The message started at the
/// first `panicked at` and ended at the first backtrace: a test that prints
/// each mismatch and then panics with one fixed sentence hashed the same
/// however many new mismatches a branch added, and with `RUST_BACKTRACE` set
/// (the gate does not scrub it) a helper thread's panic and backtrace hid the
/// test's own assertion after it. What a test printed is what it said.
///
/// With the lines, whether a measured duration was masked in any of them
/// ([`normalize`]). Empty lines are libtest's spacing; a line of spaces is
/// kept (a blank row of a grid is a row).
fn test_message(block: &str) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut duration = false;
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
        if t.starts_with("note: run with `RUST_BACKTRACE")
            || t.starts_with("note: Some details are omitted")
        {
            continue;
        }
        let (n, timed) = masked(l);
        if !n.is_empty() {
            out.push(n);
            duration |= timed;
        }
    }
    (out, duration)
}

/// Did a failure print nothing to compare — no line, or only blank ones?
fn silent(lines: &[String]) -> bool {
    lines.iter().all(|l| l.trim().is_empty())
}

/// A line of a backtrace, as std (and anyhow, through it) prints one: an
/// INDENTED numbered frame (`   0: rust_begin_unwind`, `  12: 0x1045d4b8c -
/// std::…` — the number right-aligned in four columns) or the indented `at
/// <file>:<line>:<col>` under one. A test's own `3: got x` at the margin is
/// not one.
fn is_frame(line: &str) -> bool {
    let t = line.trim_start();
    let indented = t.len() < line.len();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    indented && ((digits > 0 && t[digits..].starts_with(": ")) || t.starts_with("at "))
}

fn strip(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\x1b') {
        std::borrow::Cow::Owned(crate::libtest::strip_ansi(s))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

// ---------------------------------------------------------------------------
// Rows that ran to their end
// ---------------------------------------------------------------------------

/// Why a whole-row finding is never inherited unless its check is known to
/// run to its end ([`never_inherited`]).
///
/// ONLY A CHECK THAT RAN TO ITS END IS INHERITED (2026-09-27, fourth review).
/// A row's finding is everything its child printed, and the same output is
/// the same failure only when the child decided everything it covers
/// whatever failed. Most do not: the `tools/test-*.sh` suites stop at their
/// first failing check (`fail() { echo …; exit 1; }`), a build stops at the
/// first crate that fails and never reaches the crates that need it, a smoke
/// returns at its first bad reply, a driver answers one exit code for all its
/// checks. Their output is the same wherever they stopped, so main's red on
/// check 1 excused a branch's break of check 2, which never ran: a delivery
/// suite red on main claimed the merge contract for a branch that broke
/// a later check (the review's P5). Such a row is never inherited. The rows
/// that are: a failed test in an accounted `targo test` log
/// ([`test_findings`]), and a checker whose output shows it reached its end
/// ([`RanToEnd`]: the guard and the header check, the lint when no unit it
/// could not compile is one another unit needs, the formatter when its verb
/// printed its verdict).
pub const UNFINISHED: &str = "nothing shows it ran to its end, and a check that stops at its \
                              first failure prints the same whatever it did not reach";

/// A finding that can never be matched against main's: `why`, or — for one
/// that reads as a clock running out, which is never inherited either — the
/// timing reason ([`judge`]). One already opaque keeps its reason.
#[must_use]
pub fn never_inherited(mut f: Finding, why: &'static str) -> Finding {
    if f.opaque.is_none() {
        f.opaque = Some(if f.timing.is_some() {
            TIMING_SHAPED
        } else {
            why
        });
    }
    f
}

/// Whether a failed checker's output shows it decided everything it covers
/// — `Ok` — or why it cannot ([`UNFINISHED`]): the test that makes its row
/// inheritable ([`crate::ladder::Report::decide_checker_child`]).
pub type RanToEnd = fn(&str) -> Result<(), &'static str>;

/// Why a checker whose closing verdict line never came cannot be inherited.
const CUT_SHORT: &str = "its closing verdict line never came, so it stopped before its end, \
                         and the same output says nothing about what it did not reach";

/// `tools/grep_guard.sh` ran every check: its verdict `GUARD: FAIL` is
/// printed after the last one and nowhere else, and each `COULD NOT RUN`
/// exits before it.
///
/// # Errors
/// [`CUT_SHORT`] when the verdict line never came.
pub fn guard_ran_to_end(output: &str) -> Result<(), &'static str> {
    closing_line(output, "GUARD: FAIL")
}

/// `tools/license_check.sh` read every file: its `LICENSE: FAIL` is printed
/// after the whole walk, and a file it could not read ends it `COULD NOT RUN`
/// instead.
///
/// # Errors
/// [`CUT_SHORT`] when the verdict line never came.
pub fn license_ran_to_end(output: &str) -> Result<(), &'static str> {
    closing_line(output, "LICENSE: FAIL")
}

/// How `tools/export-content-scan.py` closes a run whose guards found
/// something — its `VERDICT_FAIL`; if either changes, change both. Printed
/// only after all three of the engine's content guards decided.
pub const EXPORT_CONTENT_VERDICT_FAILED: &str = "EXPORT CONTENT: FAIL";

/// `tools/export-content-scan.py` ran every guard: its `EXPORT CONTENT: FAIL`
/// comes after the forbidden-content, private-reference and gitleaks guards
/// have all decided, and a run the engine stopped before them — it refused
/// the export itself — closes `EXPORT CONTENT: REFUSED` instead.
///
/// # Errors
/// [`CUT_SHORT`] when the verdict line never came.
pub fn export_content_ran_to_end(output: &str) -> Result<(), &'static str> {
    closing_line(output, EXPORT_CONTENT_VERDICT_FAILED)
}

/// How `tools/export-content-scan.py` opens the line of a run whose three
/// guards are clean — its `VERDICT_PASS`; if either changes, change both. An
/// exit 0 without it decided nothing ([`crate::stages::export_content_outcome`]).
pub const EXPORT_CONTENT_VERDICT_PASSED: &str = "EXPORT CONTENT: PASS";

/// How `tools/export-content-scan.py` opens the line of a run the engine
/// stopped before any guard — it refused the export itself — its
/// `VERDICT_REFUSED`; if either changes, change both.
pub const EXPORT_CONTENT_VERDICT_REFUSED: &str = "EXPORT CONTENT: REFUSED";

/// THE EXPORT CONTENT SCAN'S HITS, ONE FINDING EACH (2026-09-29, review of the
/// stage). The scan prints one row per hit, `  <location>  <what>`, before
/// its `EXPORT CONTENT: FAIL`; each is a finding keyed by what does not move
/// when unrelated code does — the hit's source path and its pattern classes —
/// and, for the second and later hit with one key, its ordinal (`#2`, …):
///
/// * the line number and the bracketed notes (`[export only: …]`, the
///   astream submodule's rev) are dropped from the location, and each
///   pattern file's `(<file>:<line>)` from the classes, leaving each class
///   its section heading;
/// * the id is `export-content-scan.py -- <path> -- <classes>[ #n]`, and the
///   fingerprint is of that key, so one id always fails the same way.
///
/// WHY. As ONE row finding — every printed line fingerprinted, each carrying
/// its source line — a red main (a hit landed, or the engine's baseline grew
/// a pattern) blocked every branch that edited a file above one of its hits,
/// and every branch that cleared some of them but not all: each printed a
/// different set of lines, so FAILED DIFFERENTLY. Keyed per hit, a branch is
/// judged by what it adds: main's hits it still carries are main's red
/// wherever they moved, the ones it cleared are simply gone, and a hit of a
/// path and class main has fewer of — or none — is new.
///
/// A run that never printed its FAIL verdict (the engine refused the export
/// before any guard ran) is the one row finding, never inherited
/// ([`export_content_ran_to_end`]); so is output with any other line before
/// the verdict, which cannot be itemized, as the row finding (inherited only
/// when it prints exactly what main's did).
#[must_use]
pub fn export_content_findings(label: &str, output: &str) -> Vec<Finding> {
    let row = || {
        let finding = row_finding(label, output);
        match export_content_ran_to_end(output) {
            Ok(()) => finding,
            Err(why) => never_inherited(finding, why),
        }
    };
    let plain = strip(output);
    if export_content_ran_to_end(&plain).is_err() {
        return vec![row()];
    }
    let mut keys = Vec::new();
    for line in plain
        .lines()
        .take_while(|l| l.trim_end() != EXPORT_CONTENT_VERDICT_FAILED)
    {
        if line.trim().is_empty() || line.starts_with(EXPORT_CONTENT_BANNER) {
            continue;
        }
        match export_content_hit_key(line) {
            Some(key) => keys.push(key),
            None => return vec![row()],
        }
    }
    if keys.is_empty() {
        return vec![row()];
    }
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    keys.into_iter()
        .map(|key| {
            let n = seen.entry(key.clone()).or_default();
            *n += 1;
            let id = if *n == 1 {
                format!("{} -- {key}", crate::stages::EXPORT_CONTENT_SCRIPT)
            } else {
                format!("{} -- {key} #{n}", crate::stages::EXPORT_CONTENT_SCRIPT)
            };
            Finding {
                id: one_line(&id),
                hash: fingerprint([key.as_str()]),
                opaque: None,
                // A hit is a string in a file, never a clock.
                timing: None,
                package: None,
            }
        })
        .collect()
}

/// The line `tools/export-content-scan.py` opens every run with.
const EXPORT_CONTENT_BANNER: &str = "export content scan: ";

/// `<path> -- <classes>` of one hit row, `  <location>  <what>`
/// ([`export_content_findings`]); `None` for a line that is not one.
fn export_content_hit_key(line: &str) -> Option<String> {
    let row = line.strip_prefix("  ")?;
    if row.starts_with(char::is_whitespace) {
        return None;
    }
    let (location, what) = row.split_once("  ")?;
    let what = what.trim();
    if location.is_empty() || what.is_empty() {
        return None;
    }
    // The path: up to its first ` [note]`, without its `:<line>`.
    let path = location.split_once(" [").map_or(location, |(p, _)| p);
    let path = match path.rsplit_once(':') {
        Some((p, n)) if !p.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            p
        }
        _ => path,
    };
    Some(format!("{path} -- {}", without_pattern_lines(what)))
}

/// `what` with every ` (<file>:<line>)` — a pattern file's own line after a
/// class's heading — taken out.
fn without_pattern_lines(what: &str) -> String {
    let mut out = String::with_capacity(what.len());
    let mut rest = what;
    while let Some(at) = rest.find(" (") {
        let (before, open) = rest.split_at(at);
        out.push_str(before);
        let inner = &open[2..];
        let located = inner.split_once(')').filter(|(inside, _)| {
            inside.rsplit_once(':').is_some_and(|(file, n)| {
                !file.is_empty()
                    && !file.contains(char::is_whitespace)
                    && !n.is_empty()
                    && n.bytes().all(|b| b.is_ascii_digit())
            })
        });
        match located {
            Some((_, after)) => rest = after,
            None => {
                out.push_str(" (");
                rest = inner;
            }
        }
    }
    out.push_str(rest);
    out
}

fn closing_line(output: &str, verdict: &str) -> Result<(), &'static str> {
    if strip(output).lines().any(|l| l.trim_end() == verdict) {
        Ok(())
    } else {
        Err(CUT_SHORT)
    }
}

/// How `xtask gate lint` opens its verdict line when a lane found something
/// — `LINT_VERDICT_FAILED` in `crates/xtask/src/gate.rs`; if either changes,
/// change both. Printed only after every selected lane ran.
pub const LINT_VERDICT_FAILED: &str = "gate lint: FAILED";

/// Why a formatter row with a pass that reached no verdict cannot be
/// inherited.
const FMT_PASS_NOT_RUN: &str = "one of the formatter's passes reached no verdict (NOT RUN), \
                                and a finding in another outranks it in the verb's verdict, so \
                                the files that pass covers went unread";

/// Why a formatter row that printed an error cannot be inherited.
const FMT_ERROR: &str = "it printed an error (a file the formatter could not parse is a file \
                         it did not check), so drift in what it could not read is not in its \
                         output";

/// `gate lint --fmt-only` checked every file: the verb printed its verdict
/// ([`LINT_VERDICT_FAILED`]), which it does after every pass of the lane —
/// and no pass said NOT RUN (the lane folds its passes worst-first, and a
/// finding outranks a NOT RUN, so the verdict alone cannot say), and nothing
/// printed an error: a file `trustfmt` could not parse was not checked.
///
/// # Errors
/// Why the formatter's output cannot show it checked every file.
pub fn fmt_ran_to_end(output: &str) -> Result<(), &'static str> {
    let plain = strip(output);
    if !plain.lines().any(|l| l.starts_with(LINT_VERDICT_FAILED)) {
        return Err(CUT_SHORT);
    }
    if plain
        .lines()
        .any(|l| l.contains("NOT RUN") && !l.contains("excluded by --fmt-only"))
    {
        return Err(FMT_PASS_NOT_RUN);
    }
    // At the margin: a diff's own lines open with a space, `+` or `-`.
    if plain
        .lines()
        .any(|l| l.starts_with("error:") || l.starts_with("error["))
    {
        return Err(FMT_ERROR);
    }
    Ok(())
}

/// Why a lint row that failed a unit other units need cannot be inherited.
const LINT_BLOCKED: &str = "a unit other units are built on (a library, a build script) failed \
                            to lint or compile, so every unit that needs it went unlinted, and \
                            the same output says nothing about them";

/// Why a lint row with a compile error cannot be inherited.
const LINT_HARD_ERROR: &str = "a compile error (`error[E…]`) stops the compiler before its lint \
                               passes, so the target it names went unlinted";

/// Why a lint row that names no unit it could not compile cannot be
/// inherited.
const LINT_UNNAMED: &str = "it names no unit it could not compile, so what it failed on, and \
                            what that kept from being linted, cannot be read";

/// THE LINT REACHED EVERY UNIT (2026-09-27, fourth review): every unit it
/// could not compile (`error: could not compile `<crate>` (<target>)`) is
/// one no other unit is built on, and none failed with a compile error.
///
/// WHY. Under `-D warnings` a lint IS an error: a library with one emits no
/// metadata, and `--keep-going` cannot lint a crate whose dependency did not
/// compile ([`crate::stages::tippy_args`]). So a lint red in a library on
/// main left every crate that depends on it — and the library's own bins,
/// tests and examples — unlinted, the row's output was byte-identical
/// whatever a branch did to them, and a branch that landed new lints there
/// was INHERITED (measured by the review on a two-crate workspace: both logs
/// hashed `7b550958d6df7582`). Only a unit NOTHING is built on leaves the
/// rest linted: a library's own unit tests (`lib test`), a test, an example,
/// a bench, and a binary — a check-mode build of an integration test builds
/// no binary. A library (`lib`, a proc-macro's too), a build script, a build
/// script that failed to RUN, or a target cargo names in a way this does not
/// read is blocking. And a coded compile error (`error[E…]`) ends the
/// compiler before its late lint passes, so even a leaf with one was not
/// linted (an UNCODED hard error in a leaf — a parse error — still reads as
/// a lint here; the test compile fails on it for every target it builds).
///
/// # Errors
/// Why the lint's output cannot show it reached every unit.
pub fn lint_reached_every_unit(output: &str) -> Result<(), &'static str> {
    let plain = strip(output);
    let mut named = false;
    // At the margin, where rustc and cargo write them: a snippet's lines open
    // with its gutter.
    for line in plain.lines() {
        if line.starts_with("error[E") {
            return Err(LINT_HARD_ERROR);
        }
        if line.starts_with("error: failed to run custom build command") {
            return Err(LINT_BLOCKED);
        }
        let Some(rest) = line.strip_prefix("error: could not compile `") else {
            continue;
        };
        named = true;
        let target = rest
            .split_once("` (")
            .and_then(|(_, r)| r.split_once(')'))
            .map(|(target, _)| target);
        if !target.is_some_and(leaf_unit) {
            return Err(LINT_BLOCKED);
        }
    }
    if named { Ok(()) } else { Err(LINT_UNNAMED) }
}

/// A unit of a check-mode build that no other unit is built on, as cargo
/// names it in `could not compile`: a library's unit tests, and every test,
/// example, bench or binary target.
fn leaf_unit(target: &str) -> bool {
    target == "lib test"
        || ["bin \"", "test \"", "example \"", "bench \""]
            .iter()
            .any(|p| target.starts_with(p))
}

// ---------------------------------------------------------------------------
// Judging
// ---------------------------------------------------------------------------

/// When main first went red on a failure: the run that first recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Since {
    pub commit: String,
    /// Seconds since the epoch.
    pub when: u64,
}

/// Why a finding is NEW.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewWhy {
    /// The base lists no failure with its id.
    NotOnMain,
    /// The base lists its id with another fingerprint: it fails differently.
    FailsDifferently,
    /// It can never be matched ([`Finding::opaque`]).
    Opaque(&'static str),
}

/// What the base's receipt says about one finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    New(NewWhy),
    /// Red on main with the same failure, inside the age cap: named, excused.
    Inherited(Since),
    /// Red on main with the same failure for longer than the cap: it blocks.
    Expired(Since),
}

impl Disposition {
    /// Does this finding leave the merge contract standing?
    #[must_use]
    pub fn excused(&self) -> bool {
        matches!(self, Self::Inherited(_))
    }
}

/// The base's reds, as a run is judged against them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseReds {
    /// The base commit.
    pub commit: String,
    /// Where its receipt was found, for the reader: `receipt <sha>`, `receipt
    /// tree-<sha>` or `note <sha>`.
    pub source: String,
    pub failures: Vec<Failure>,
    /// When this run is judged, in seconds since the epoch; the age cap is
    /// measured from here.
    pub now: u64,
    /// `Some` when `commit` is not the merge-base but its NEAREST ancestor
    /// with a receipt — opt-in, `--nearest-base` ([`crate::nearest`]): which
    /// findings it may excuse. `None` — the default, the exact rule — always.
    pub nearest: Option<crate::nearest::Gate>,
}

/// What a run's verdict is judged against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Against {
    /// No base: every red counts. The reason, when there is one to give, is
    /// said in the verdict.
    Absolute(Option<String>),
    /// The base's reds.
    Base(BaseReds),
}

impl Against {
    /// The absolute rule, with nothing to say about why.
    #[must_use]
    pub const fn absolute() -> Self {
        Self::Absolute(None)
    }
}

/// Judge one finding against the base's reds. A since further ahead of `now`
/// than [`CLOCK_SLACK_SECS`] (a receipt published from a machine whose clock
/// is fast) has no age the cap can read, so it is never inherited — until
/// then it was excused until real time passed the skewed time plus the cap.
///
/// A TIMING-SHAPED FAILURE IS NEVER INHERITED (2026-09-27, second review):
/// one that reads as a clock running out, or printed a clock it read
/// ([`Finding::timing`]). Main's test timing out under load and a branch's
/// real deadlock in the same test give up with the same words — `timed out
/// after 5s` both times — and a render budget blown by 20 ms and one blown
/// by 900 ms hash the same, so the same words say nothing about whether it
/// is the same failure. Main listing it counts for nothing: it is NEW, and
/// the verdict's UNDER LOAD label still says when its stage ran busy.
///
/// THROUGH A NEAREST BASE (opt-in, [`crate::nearest`]) a finding the exact
/// rule would inherit is inherited only when [`crate::nearest::Gate::qualifies`]
/// says its blast radius did not change between that base and the
/// merge-base; otherwise it is NEW, and the reason says why.
#[must_use]
pub fn judge(f: &Finding, base: &BaseReds) -> Disposition {
    let exact = judge_exact(f, base);
    match (&base.nearest, &exact) {
        (Some(gate), Disposition::Inherited(_)) => match gate.qualifies(f) {
            Ok(_) => exact,
            Err(why) => Disposition::New(NewWhy::Opaque(why)),
        },
        _ => exact,
    }
}

/// [`judge`] by the exact rule: the base's reds as they stand.
fn judge_exact(f: &Finding, base: &BaseReds) -> Disposition {
    if let Some(why) = f.opaque {
        return Disposition::New(NewWhy::Opaque(why));
    }
    let mut listed = false;
    for b in base.failures.iter().filter(|b| b.id == f.id) {
        listed = true;
        if b.hash == f.hash {
            if f.timing.is_some() {
                return Disposition::New(NewWhy::Opaque(TIMING_SHAPED));
            }
            if b.since_when > base.now.saturating_add(CLOCK_SLACK_SECS) {
                return Disposition::New(NewWhy::Opaque(FUTURE));
            }
            let since = Since {
                commit: b.since.clone(),
                when: b.since_when,
            };
            return if base.now.saturating_sub(b.since_when) > INHERITED_CAP_SECS {
                Disposition::Expired(since)
            } else {
                Disposition::Inherited(since)
            };
        }
    }
    Disposition::New(if listed {
        NewWhy::FailsDifferently
    } else {
        NewWhy::NotOnMain
    })
}

/// Every finding of `t`, row by row, judged.
#[must_use]
pub fn judge_tally(t: &Tally, base: &BaseReds) -> Vec<Vec<Disposition>> {
    (0..t.gate_failures.len())
        .map(|row| t.findings_of(row).iter().map(|f| judge(f, base)).collect())
        .collect()
}

/// The ids of `t`'s findings judged INHERITED against `base`, in ladder order
/// — what a receipt's `inherited` lines name.
#[must_use]
pub fn inherited_ids(t: &Tally, base: &BaseReds) -> Vec<String> {
    (0..t.gate_failures.len())
        .flat_map(|row| t.findings_of(row))
        .filter(|f| judge(f, base).excused())
        .map(|f| f.id.clone())
        .collect()
}

/// Is the `row`-th finding excused — itemized, and every item inherited? A row
/// with no itemized failure never is.
#[must_use]
pub fn row_excused(judged: &[Vec<Disposition>], row: usize) -> bool {
    judged
        .get(row)
        .is_some_and(|items| !items.is_empty() && items.iter().all(Disposition::excused))
}

/// The failures a receipt lists for this run: each finding with when main
/// first went red on it — carried from `since_from` while main stays red on
/// that ID, the earliest since it lists for the id, else this run (`head`,
/// `now`).
///
/// BY ID, NOT BY ID AND MESSAGE (2026-09-27). Carried only while the failure
/// was the same, a red whose message alternated between two forms got a new
/// since at every baseline and was excused for ever. A since from the future
/// (past [`CLOCK_SLACK_SECS`]) is never carried: this run starts the clock.
#[must_use]
pub fn recorded_failures(
    t: &Tally,
    since_from: Option<&BaseReds>,
    head: &str,
    now: u64,
) -> Vec<Failure> {
    t.all_findings()
        .map(|f| {
            let carried = since_from.and_then(|base| {
                base.failures
                    .iter()
                    .filter(|b| {
                        b.id == f.id && b.since_when <= base.now.saturating_add(CLOCK_SLACK_SECS)
                    })
                    .min_by_key(|b| b.since_when)
                    .map(|b| Since {
                        commit: b.since.clone(),
                        when: b.since_when,
                    })
            });
            let since = carried.unwrap_or_else(|| Since {
                commit: head.to_string(),
                when: now,
            });
            Failure {
                id: f.id.clone(),
                hash: f.hash.clone(),
                since: since.commit,
                since_when: since.when,
            }
        })
        .collect()
}

/// THE REDS A RUN COULD NOT SEE, and when main first went red on each
/// (2026-09-27, second review): every failure `since_from` lists — its reds
/// and the ones it carried hidden itself — whose id this run did not itemize,
/// when this run could not have itemized everything ([`saw_everything`]). A
/// receipt carries them in its `hidden` lines, for the since chain alone:
/// never a red a branch is judged against, since this run did not see it.
///
/// WHY. Carried by id, a since survived only while every baseline itemized
/// the id. One baseline whose test log was unaccounted — any other binary on
/// main crashing or hanging makes the whole row one finding under the stage's
/// label — left the id out, and the next baseline that itemized it again
/// started its clock at NOW: the age cap restarted whenever main also had an
/// intermittent crash. A run that saw everything says what it did not find
/// red, and the clock of a red it no longer has stops there.
#[must_use]
pub fn hidden_failures(t: &Tally, since_from: Option<&BaseReds>) -> Vec<Failure> {
    let Some(base) = since_from else {
        return Vec::new();
    };
    if saw_everything(t) {
        return Vec::new();
    }
    let seen: BTreeSet<&str> = t.all_findings().map(|f| f.id.as_str()).collect();
    let mut hidden: Vec<Failure> = Vec::new();
    for b in &base.failures {
        if seen.contains(b.id.as_str()) || b.since_when > base.now.saturating_add(CLOCK_SLACK_SECS)
        {
            continue;
        }
        match hidden.iter_mut().find(|h| h.id == b.id) {
            Some(h) if b.since_when < h.since_when => *h = b.clone(),
            Some(_) => {}
            None => hidden.push(b.clone()),
        }
    }
    hidden
}

/// Could a red have gone unseen in `t`? Not when nothing was skipped, nothing
/// could not run, and every failing row is itemized into findings that can
/// each be read ([`Finding::opaque`] none).
fn saw_everything(t: &Tally) -> bool {
    t.skips.is_empty()
        && t.could_not_run.is_empty()
        && (0..t.gate_failures.len()).all(|row| {
            let items = t.findings_of(row);
            !items.is_empty() && items.iter().all(|f| f.opaque.is_none())
        })
}

/// `seconds` as whole hours, at least one.
#[must_use]
pub fn hours(seconds: u64) -> u64 {
    (seconds / 3600).max(1)
}

/// The first nine characters of a commit id.
#[must_use]
pub fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

// ---------------------------------------------------------------------------
// Finding the base
// ---------------------------------------------------------------------------

/// A receipt that can serve as a base, and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The commit it answers for.
    pub commit: String,
    /// `receipt <sha>`, `receipt tree-<sha>` or `note <sha>`.
    pub source: String,
    pub receipt: Receipt,
    /// A time main's history records LATER than the receipt's own `when`,
    /// and what recorded it — the committer time of [`MAIN_REF`]'s tip or of
    /// HEAD ([`resolve`]) — which a judging clock must not be behind either
    /// ([`Plan::against`]). `None`: the receipt's `when` is the latest known.
    pub clock_floor: Option<(u64, String)>,
    /// `Some` when this is a NEAREST base, not the merge-base's own receipt
    /// (opt-in, [`crate::nearest::find`]); `None` by the exact rule.
    pub nearest: Option<crate::nearest::Gate>,
}

impl Found {
    /// Its reds, judged at `now`.
    #[must_use]
    pub fn reds(&self, now: u64) -> BaseReds {
        BaseReds {
            commit: self.commit.clone(),
            source: self.source.clone(),
            failures: self.receipt.failures.clone().unwrap_or_default(),
            now,
            nearest: self.nearest.clone(),
        }
    }

    /// What a since is carried from, at `now`: its reds AND the ones it
    /// carried hidden ([`hidden_failures`]) — never what a run is judged
    /// against.
    #[must_use]
    pub fn chain(&self, now: u64) -> BaseReds {
        let mut reds = self.reds(now);
        reds.failures.extend(self.receipt.hidden.iter().cloned());
        reds
    }
}

/// Can `r` serve as a base? Only a whole-tree run's receipt of the merge
/// contract's LAND tier that lists its failures: a narrowed run did not look at
/// most of the tree, a `--measure` run ran none of the contract (it is never
/// filed where a base is looked up; this says so if one ever is), and a
/// receipt with no list says nothing about what was red — one from before the
/// list, or one whose source or compiler moved mid-run, whose findings came
/// partly from other bytes or tools than the commit's (`lib.rs`
/// `write_receipt` writes no list for it, 2026-09-27).
///
/// # Errors
/// Why not, in a few words.
pub fn usable(r: &Receipt) -> Result<(), &'static str> {
    if r.scope != "workspace" {
        Err("its run was narrowed")
    } else if !r.lands() {
        Err("its run was the MEASURE tier, not the merge contract")
    } else if r.failures.is_none() {
        Err("it lists no failures (it predates the list, or its source or compiler moved under it)")
    } else {
        Ok(())
    }
}

/// How a run's base was settled, before any stage runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// A usable receipt for the merge-base: the verdict is differential.
    Judge(Found),
    /// The absolute rule. `chain` is where the receipt's since-values are
    /// carried from when HEAD is itself a commit of main ([`nearest`]).
    Absolute { why: String, chain: Option<Found> },
}

impl Plan {
    /// What the verdict is judged against, at `now`.
    ///
    /// A CLOCK BEHIND MAIN'S RECEIPT IS NO CLOCK (2026-09-27, second review):
    /// a `now` more than [`CLOCK_SLACK_SECS`] before the base receipt was
    /// written reads every red's age short — a red 30 h old judged on a clock
    /// 8 h slow was inside the cap — so the run is judged by the absolute rule
    /// and says why. NOR ONE BEHIND MAIN'S NEWEST COMMIT (2026-09-27, third
    /// review): a clock 8 h slow judging a receipt written 30 h ago reads
    /// later than the receipt, so the receipt alone could not catch it; main's
    /// tip committed an hour ago, or HEAD made on another machine, can
    /// ([`Found::clock_floor`]).
    #[must_use]
    pub fn against(&self, now: u64) -> Against {
        match self {
            Self::Judge(found) => {
                let written = (
                    found.receipt.when,
                    format!(
                        "main's {} for {} was written",
                        found.source,
                        short(&found.commit)
                    ),
                );
                let (floor, what) = match &found.clock_floor {
                    Some((t, what)) if *t > written.0 => (*t, what.clone()),
                    _ => written,
                };
                if now.saturating_add(CLOCK_SLACK_SECS) < floor {
                    Against::Absolute(Some(format!(
                        "this machine's clock reads {} h before {what}, so the age of main's reds \
                         cannot be read on it",
                        hours(floor - now)
                    )))
                } else {
                    Against::Base(found.reds(now))
                }
            }
            Self::Absolute { why, .. } => Against::Absolute(Some(why.clone())),
        }
    }

    /// Where the receipt carries each failure's since from, at `now`: the
    /// base's or the chain's reds and its hidden ones ([`Found::chain`]).
    #[must_use]
    pub fn since_from(&self, now: u64) -> Option<BaseReds> {
        match self {
            Self::Judge(found) => Some(found.chain(now)),
            Self::Absolute { chain, .. } => chain.as_ref().map(|c| c.chain(now)),
        }
    }

    /// The `verify: base …` line the ladder prints before any stage.
    #[must_use]
    pub fn header_line(&self, baseline: bool, head: &str) -> String {
        let cap = INHERITED_CAP_SECS / 3600;
        match self {
            Self::Judge(
                found @ Found {
                    nearest: Some(gate),
                    ..
                },
            ) => {
                let n = found.receipt.failures.as_ref().map_or(0, Vec::len);
                format!(
                    "verify: base {} — NEAREST (--nearest-base, opt-in): the merge-base with \
                     {MAIN_REF}, {}, has no receipt that serves this run, and {} is the newest \
                     main commit before it with one ({} commit(s), {} h earlier) — main's {} \
                     lists {n} red(s); the same red here is inherited only when it is a failed \
                     test of a crate none of whose files, nor any of a crate it builds on, \
                     changed between the two ({} of {} crates qualify) — never a whole-row red — and \
                     until main has been red on it {cap} h\n",
                    short(&found.commit),
                    short(&gate.merge_base),
                    short(&found.commit),
                    gate.steps,
                    gate.span_secs / 3600,
                    found.source,
                    gate.qualifying(),
                    gate.crates.len()
                )
            }
            Self::Judge(found) => {
                let n = found.receipt.failures.as_ref().map_or(0, Vec::len);
                format!(
                    "verify: base {} (the merge-base with {MAIN_REF}) — main's {} lists {n} red(s); \
                     the same red here is inherited, not blocking, until main has been red on it \
                     {cap} h\n",
                    short(&found.commit),
                    found.source
                )
            }
            Self::Absolute { why, chain } if baseline => format!(
                "verify: baseline — main at {}: this run records main's reds for branches to be \
                 judged against and publishes its receipt to {MAIN_REMOTE} {NOTES_REF} ({}; \
                 when main went red on each is carried from {})\n",
                short(head),
                why,
                chain.as_ref().map_or_else(
                    || "nothing: no earlier receipt on main's history".to_string(),
                    |c| c.source.clone()
                )
            ),
            Self::Absolute { why, .. } => {
                format!("verify: base — {why}: every red counts (the absolute rule)\n")
            }
        }
    }
}

/// Settle the base of a run on `head` in `root`'s repository — `baseline`
/// for a `--baseline` run. Reads the store and the local notes, and fetches
/// the notes ref from [`MAIN_REMOTE`] (bounded) when neither answers — and,
/// for a run on main, BEFORE the since chain is read (2026-09-27, second
/// review: a baseline on a machine that had never fetched the notes read no
/// chain, recorded every red main had as red since now, and published that
/// over the notes it had not read; [`baseline_refusal`] refuses a baseline
/// whose fetch fails).
///
/// A STALE [`MAIN_REF`] IS NO BASE (2026-09-27, second review). When HEAD
/// holds a commit of the local [`LOCAL_MAIN`] that `origin/main` does not —
/// the branch merged a main this checkout has not fetched (or not pushed) —
/// the merge-base with `origin/main` is an OLDER main than the one the branch
/// merged, and its receipt cannot tell a red main fixed since from one the
/// merge put back. The run is judged by the absolute rule, and says to fetch.
///
/// AND SO IS ONE THE REMOTE HAS MOVED PAST (2026-09-28). The local-main check
/// sees only a main this checkout HAS: a branch that merged main from another
/// ref, another worktree's fetch or a pull straight from the remote holds a
/// main that neither `origin/main` nor the local `main` has, and its merge-base
/// with `origin/main` was an older main all the same. So main's tip is READ
/// from [`MAIN_REMOTE`] ([`remote_main_fresh`], bounded by
/// [`FRESHNESS_BOUND`]): a tip `origin/main` does not hold is a stale
/// `origin/main`, and a remote that cannot be read — offline, no such remote,
/// no main there, the bound overrun — cannot confirm it fresh. Either way the
/// run is judged by the absolute rule, saying why and `git fetch origin`.
///
/// ONLY A RECEIPT OF THE SAME TOOLS IS A BASE (2026-09-27, third review): a
/// receipt made by another trustc, or other spec checkers, than `tools` —
/// this run's — cannot serve ([`tools_differ`]); the next candidate is tried
/// (the store by commit, by tree, the local note, the fetched note), and with
/// none the run is judged by the absolute rule, naming the difference. The
/// base that serves still takes main's OLDEST clock for each red from every
/// receipt about its commit, whatever tools made it ([`oldest_clocks`]).
///
/// A BRANCH JUDGED BY THE ABSOLUTE RULE STILL CARRIES MAIN'S CLOCKS
/// (2026-09-27, third review). Its receipt lists every red with when main
/// first went red on it, and once the branch lands — fast-forwarded, as main
/// takes slices — that receipt IS the next branch's base. With no chain its
/// every red was red "since" the branch's own run, so a red main had had for
/// days was excused for another 24 h. Such a plan's chain is the newest
/// receipt on the base's history ([`nearest`]) — the base's own included,
/// whatever tools made it: an older since only makes a red block sooner.
#[must_use]
pub fn resolve(root: &Path, head: &str, baseline: bool, tools: &Tools) -> Plan {
    resolve_with(root, head, baseline, tools, BaseMode::Exact)
}

/// Which receipt may serve as a run's base.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BaseMode {
    /// The merge-base's own, and nothing else: the default, and the rule the
    /// owner approved.
    #[default]
    Exact,
    /// `--nearest-base` (opt-in, pending the owner's decision,
    /// [`crate::nearest`]): when the merge-base has none that serves, its
    /// newest ancestor's within [`crate::nearest::Bound::DEFAULT`], for the
    /// failed tests whose blast radius did not change between the two.
    Nearest,
}

/// [`resolve`], with the base `mode` the run asked for. [`BaseMode::Exact`]
/// is [`resolve`] exactly; [`BaseMode::Nearest`] changes only the plan of a
/// run whose merge-base has no receipt that serves it.
#[must_use]
pub fn resolve_with(
    root: &Path,
    head: &str,
    baseline: bool,
    tools: &Tools,
    mode: BaseMode,
) -> Plan {
    let absolute = |why: String| Plan::Absolute { why, chain: None };
    if !is_sha(head) {
        return absolute(format!("HEAD is {head}, not a commit"));
    }
    if rev(root, &format!("{MAIN_REF}^{{commit}}")).is_none() {
        return absolute(format!(
            "this repository has no {MAIN_REF} to be judged against"
        ));
    }
    let Some(base) = git_line(root, &["merge-base", head, MAIN_REF]) else {
        return absolute(format!("{} shares no history with {MAIN_REF}", short(head)));
    };
    if baseline || base == head {
        let mut why = if baseline {
            "a baseline is judged by the absolute rule".to_string()
        } else {
            format!(
                "{} is itself on {MAIN_REF}: a run on main has no base to inherit from",
                short(head)
            )
        };
        if let Err(e) = fetch_notes(root)
            && !e.contains(NO_REMOTE_NOTES)
        {
            why.push_str(&format!(
                "; main's published notes could not be fetched ({e}), so when main went red \
                 is carried from this machine's receipts alone"
            ));
        }
        return Plan::Absolute {
            why,
            chain: nearest(root, head),
        };
    }
    // Main's clocks, carried whatever this run is judged by.
    let chained = |why: String| Plan::Absolute {
        why,
        chain: nearest(root, &base),
    };
    if let Some(held) = unfetched_main(root, head) {
        return chained(format!(
            "{} holds {}, a commit of the local `main` that {MAIN_REF} does not have: {MAIN_REF} \
             is stale here, and the merge-base with it is an older main than the one this \
             branch merged — `git fetch {MAIN_REMOTE}` (or push main) and run again",
            short(head),
            short(&held)
        ));
    }
    if let Err(why) = remote_main_fresh(root) {
        return chained(why);
    }
    let judge = |mut found: Found| {
        found.clock_floor = clock_floor(root, head);
        oldest_clocks(root, &mut found);
        Plan::Judge(found)
    };
    let mut why = match lookup(root, &base, true, Some(tools)) {
        Ok(found) => return judge(found),
        Err(why) => why,
    };
    match fetch_notes(root) {
        Ok(()) => match note_receipt(root, &base, Some(tools)) {
            Some(Ok(found)) => return judge(found),
            // The local note, read before the fetch, may have said it already.
            Some(Err(e)) if why.contains(&e) => {}
            Some(Err(e)) => why.push_str(&format!("; the published {e}")),
            None => why.push_str("; no published note either"),
        },
        Err(e) => why.push_str(&format!("; published notes unavailable ({e})")),
    }
    if mode == BaseMode::Nearest {
        match crate::nearest::find(root, &base, tools, crate::nearest::Bound::DEFAULT) {
            Ok(found) => return judge(found),
            Err(e) => why.push_str(&format!("; --nearest-base: {e}")),
        }
    }
    // Named only when it is NEWER than the base: the walk from main's tip
    // passes the base, which has no usable receipt, and finds an ancestor of
    // it whenever nothing after it was baselined — and merging an ancestor
    // leaves the merge-base where it is.
    let hint = nearest(root, MAIN_REF)
        .filter(|n| git_status(root, &["merge-base", "--is-ancestor", &n.commit, &base]).is_err())
        .map_or_else(String::new, |n| {
            format!(
                "; the newest main commit with one is {} — merge it to be judged against it",
                short(&n.commit)
            )
        });
    chained(format!(
        "no usable receipt for the base {} (the merge-base with {MAIN_REF}): {why}{hint}; \
         `tools/verify.sh --baseline` on {} records one",
        short(&base),
        short(&base)
    ))
}

/// MAIN'S OLDEST CLOCK FOR EACH RED, WHATEVER TOOLS READ IT (2026-09-27,
/// fourth review). A base serves only a run of the same tools, so a store
/// receipt made by other tools is passed over for a note made by this run's —
/// and the note's younger since values judged the age cap, dropping the
/// older clock this machine's receipt recorded for the same red. A toolchain
/// switch restarted the 24 h cap. Each red `found` lists now takes the
/// earliest since any receipt or note about its commit records for the same
/// id — its reds or its hidden ones, whatever made it: an older since only
/// makes a red block sooner, as the since chain already reads it
/// ([`resolve`]).
fn oldest_clocks(root: &Path, found: &mut Found) {
    let mut others: Vec<Receipt> = Vec::new();
    if let Ok(store) = receipt::dir(root) {
        let tree = receipt::tree_of(root, &found.commit);
        let keys =
            std::iter::once(found.commit.clone()).chain(tree.as_deref().map(receipt::tree_key));
        for key in keys {
            let Some(r) = std::fs::read_to_string(store.join(&key))
                .ok()
                .and_then(|text| Receipt::parse(&text))
            else {
                continue;
            };
            if r.head == found.commit || (tree.is_some() && r.tree == tree) {
                others.push(r);
            }
        }
    }
    if let Some(r) = git_stdout(
        root,
        &[
            "notes",
            &format!("--ref={NOTES_REF}"),
            "show",
            &found.commit,
        ],
    )
    .and_then(|text| Receipt::parse(&text))
    .filter(|r| r.head == found.commit)
    {
        others.push(r);
    }
    let Some(failures) = found.receipt.failures.as_mut() else {
        return;
    };
    for f in failures {
        let older = others
            .iter()
            .flat_map(|r| r.failures.iter().flatten().chain(&r.hidden))
            .filter(|o| o.id == f.id && o.since_when < f.since_when)
            .min_by_key(|o| o.since_when);
        if let Some(o) = older {
            f.since = o.since.clone();
            f.since_when = o.since_when;
        }
    }
}

/// The latest time main's history records past any receipt — the committer
/// time of [`MAIN_REF`]'s tip, or of `head` when that is later — and what
/// recorded it ([`Found::clock_floor`]). `None` when git names neither.
fn clock_floor(root: &Path, head: &str) -> Option<(u64, String)> {
    let committed = |what: &str| -> Option<u64> {
        git_line(root, &["log", "-1", "--format=%ct", what])?
            .parse()
            .ok()
    };
    let tip = committed(MAIN_REF).map(|t| {
        (
            t,
            format!("the newest commit on {MAIN_REF} was made (its committer time)"),
        )
    });
    let own = committed(head).map(|t| {
        (
            t,
            format!("HEAD {} was committed (its committer time)", short(head)),
        )
    });
    match (tip, own) {
        (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// The local main branch a stale [`MAIN_REF`] is checked against
/// ([`resolve`]).
pub const LOCAL_MAIN: &str = "refs/heads/main";

/// Main on [`MAIN_REMOTE`], as `git ls-remote` names it: the tip a fresh
/// [`MAIN_REF`] holds ([`remote_main_fresh`]).
pub const REMOTE_MAIN: &str = "refs/heads/main";

/// How long reading main's tip from [`MAIN_REMOTE`] may take: one ref
/// advertisement, so far shorter than a fetch ([`NETWORK_BOUND`]). Past it
/// the remote is unreachable for this run, and freshness unconfirmed. It
/// bounds the whole call, the transport git starts included (`git_bounded`
/// kills git's process group, plus at most a second's drain grace).
pub const FRESHNESS_BOUND: Duration = Duration::from_secs(10);

/// IS [`MAIN_REF`] AS NEW AS MAIN ON [`MAIN_REMOTE`]? (2026-09-28.) Reads the
/// remote's [`REMOTE_MAIN`] with `git ls-remote` (bounded by
/// [`FRESHNESS_BOUND`], never a credential prompt) and answers `Ok` only when
/// that tip is `origin/main` itself or an ancestor of it. `Err` — the reason
/// the run is judged by the absolute rule, naming `git fetch origin` — when
/// the remote cannot be read (offline, no such remote, no main there, the
/// bound overrun: freshness cannot be confirmed, and a base it cannot confirm
/// is no base) and when the remote's main holds a commit `origin/main` does
/// not (the merge-base with `origin/main` may be an older main than the one
/// the branch merged).
///
/// # Errors
/// Why `origin/main` cannot be confirmed fresh.
pub fn remote_main_fresh(root: &Path) -> Result<(), String> {
    let local = rev(root, &format!("{MAIN_REF}^{{commit}}")).unwrap_or_default();
    let unconfirmed = |what: String| {
        format!(
            "{what}, so {MAIN_REF} ({}) cannot be confirmed fresh, and the merge-base with a \
             stale one may be an older main than the one this branch merged — `git fetch \
             {MAIN_REMOTE}` where {MAIN_REMOTE} can be reached, and run again",
            short(&local)
        )
    };
    let listed = git_bounded(
        root,
        &["ls-remote", "--refs", MAIN_REMOTE, REMOTE_MAIN],
        FRESHNESS_BOUND,
    )
    .map_err(|e| {
        unconfirmed(format!(
            "main on {MAIN_REMOTE} could not be read (`git ls-remote {MAIN_REMOTE} \
             {REMOTE_MAIN}`: {e})"
        ))
    })?;
    let Some(tip) = listed.lines().find_map(|l| {
        let (sha, name) = l.split_once('\t')?;
        (name.trim() == REMOTE_MAIN && is_sha(sha.trim())).then(|| sha.trim().to_string())
    }) else {
        return Err(unconfirmed(format!("{MAIN_REMOTE} names no {REMOTE_MAIN}")));
    };
    if tip == local || git_status(root, &["merge-base", "--is-ancestor", &tip, MAIN_REF]).is_ok() {
        return Ok(());
    }
    Err(format!(
        "main on {MAIN_REMOTE} is at {}, which {MAIN_REF} here ({}) does not have: {MAIN_REF} is \
         stale, and the merge-base with it may be an older main than the one this branch \
         merged — `git fetch {MAIN_REMOTE}` and run again",
        short(&tip),
        short(&local)
    ))
}

/// What git says when [`MAIN_REMOTE`] has no [`NOTES_REF`] yet: no baseline
/// was ever published, which is not a failure to fetch one.
const NO_REMOTE_NOTES: &str = "couldn't find remote ref";

/// The newest commit of [`LOCAL_MAIN`] that `head` holds, when [`MAIN_REF`]
/// does not hold it — `None` when there is no local main, or `head` holds no
/// local-main commit past `origin/main` (none at all when the two share no
/// history). An ancestry question git cannot answer counts as stale.
fn unfetched_main(root: &Path, head: &str) -> Option<String> {
    rev(root, &format!("{LOCAL_MAIN}^{{commit}}"))?;
    let shared = git_line(root, &["merge-base", head, LOCAL_MAIN])?;
    let fetched = git(root, &["merge-base", "--is-ancestor", &shared, MAIN_REF])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    (!fetched).then_some(shared)
}

/// A usable receipt for `commit` in the store — under the commit, then its
/// tree — or, with `notes`, in the local notes ref; with `tools`, only one
/// those tools made ([`tools_differ`]) — a base — else any (a since chain).
///
/// # Errors
/// What was found and why it could not serve.
pub fn lookup(
    root: &Path,
    commit: &str,
    notes: bool,
    tools: Option<&Tools>,
) -> Result<Found, String> {
    let mut seen = Vec::new();
    if let Ok(store) = receipt::dir(root) {
        let tree = receipt::tree_of(root, commit);
        let keys = std::iter::once((commit.to_string(), commit.to_string()))
            .chain(tree.map(|t| (receipt::tree_key(&t), t)));
        for (key, expect) in keys {
            let Ok(text) = std::fs::read_to_string(store.join(&key)) else {
                continue;
            };
            let Some(r) = Receipt::parse(&text) else {
                seen.push(format!("`{key}` is not a receipt this gate reads"));
                continue;
            };
            let about = if key == commit {
                r.head == expect
            } else {
                r.tree.as_deref() == Some(expect.as_str())
            };
            if !about {
                seen.push(format!("`{key}` is about another commit"));
                continue;
            }
            match serves(&r, tools) {
                Ok(()) => {
                    return Ok(Found {
                        commit: commit.to_string(),
                        source: format!("receipt {}", short_key(&key)),
                        receipt: r,
                        clock_floor: None,
                        nearest: None,
                    });
                }
                Err(why) => seen.push(format!(
                    "its receipt `{}` cannot serve: {why}",
                    short_key(&key)
                )),
            }
        }
    }
    if notes {
        match note_receipt(root, commit, tools) {
            Some(Ok(found)) => return Ok(found),
            Some(Err(why)) => seen.push(why),
            None => {}
        }
    }
    if seen.is_empty() {
        Err("no receipt".to_string())
    } else {
        Err(seen.join("; "))
    }
}

/// Can `r` serve — [`usable`], and, with `tools`, made by them?
fn serves(r: &Receipt, tools: Option<&Tools>) -> Result<(), String> {
    usable(r).map_err(str::to_string)?;
    match tools.and_then(|t| tools_differ(&Tools::of(r), t)) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// `key` with its sha shortened.
fn short_key(key: &str) -> String {
    match key.strip_prefix(receipt::TREE_KEY_PREFIX) {
        Some(tree) => format!("{}{}", receipt::TREE_KEY_PREFIX, short(tree)),
        None => short(key).to_string(),
    }
}

/// The receipt the local notes ref holds for `commit`: `None` when it holds
/// none about it; else it, when it serves ([`serves`]), or why it cannot.
fn note_receipt(root: &Path, commit: &str, tools: Option<&Tools>) -> Option<Result<Found, String>> {
    let text = git_stdout(
        root,
        &["notes", &format!("--ref={NOTES_REF}"), "show", commit],
    )?;
    let r = Receipt::parse(&text)?;
    if r.head != commit {
        return None;
    }
    Some(match serves(&r, tools) {
        Ok(()) => Ok(Found {
            commit: commit.to_string(),
            source: format!("note {}", short(commit)),
            receipt: r,
            clock_floor: None,
            nearest: None,
        }),
        Err(why) => Err(format!("note {} cannot serve: {why}", short(commit))),
    })
}

// ---------------------------------------------------------------------------
// What ran a run
// ---------------------------------------------------------------------------

/// WHAT RAN A RUN, as a receipt records it: its `toolchain` line (`<stage2
/// bin dir> trustc <commit-hash>`), its `checkers` line
/// ([`crate::checkers::Checkers::summary`]), its `build-env` line
/// ([`build_env`]) and its `build-config` line ([`crate::build_config`]).
///
/// A RED IS MAIN'S ONLY UNDER THE TOOLS THAT FOUND IT (2026-09-27, third
/// review). A base receipt made by another trustc — a re-seal, an older
/// store build on the machine that published main's note — or under another
/// `ty` lists the reds main had UNDER THOSE: a codegen or lint difference, a
/// checker's own bug. Under this run's tools main may be green on one, and a
/// branch that breaks that test the same way read as main's red: INHERITED,
/// merge contract claimed. So a base serves only a run made by the same tools
/// ([`tools_differ`]), and one that cannot say which tools made it serves
/// none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    /// `<stage2 bin dir> trustc <commit-hash>` (`unknown` when trustc named
    /// none).
    pub toolchain: String,
    /// `ty = <path> (<origin>, <version>); trust-ir = absent; …`.
    pub checkers: String,
    /// The compile and test-run configuration the environment gave the run
    /// ([`build_env`]); `None` for a receipt that does not say.
    pub build_env: Option<String>,
    /// The cargo config files the run's builds read
    /// ([`crate::build_config::record`], 2026-09-28); `None` for a receipt
    /// that does not say, and for a run whose config files could not be read.
    pub build_config: Option<String>,
}

/// The `RUST*` and `CARGO*` variables a receipt's `build-env` line does NOT
/// record ([`build_env`]), each because it cannot change what is built or
/// what a test decides: where artifacts go (`CARGO_TARGET_DIR`, which the gate
/// removes from every child, `CARGO_BUILD_TARGET_DIR`, cargo's
/// `CARGO_TARGET_TMPDIR`), how many jobs build them and through which
/// jobserver, whether incrementally (the gate sets `CARGO_INCREMENTAL=0` in
/// every child), how cargo prints and logs itself (`CARGO_TERM_*`), three
/// test-run knobs no finding reads — a backtrace (every one is cut from a
/// failed test's message, [`test_message`]), log filtering and libtest's
/// thread count — and WHAT CARGO TELLS A PROCESS IT RAN ABOUT ITSELF
/// ([`BUILD_ENV_IGNORED_PREFIXES`] too). `targo --unverified verify` is `targo
/// run -p xtask -- verify`, so the gate under it inherits `CARGO` and xtask's
/// `CARGO_MANIFEST_*` and `CARGO_PKG_*` (measured 2026-09-27 through a
/// runner: those and nothing else of either family) — none of it a setting
/// a nested cargo reads, and every child cargo runs is handed its own. Recorded,
/// they would have made every receipt name its worktree, and no run through
/// the verb the base of one through the script.
pub const BUILD_ENV_IGNORED: [&str; 16] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_TARGET_TMPDIR",
    "CARGO_BUILD_JOBS",
    "CARGO_MAKEFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_LOG",
    "RUST_BACKTRACE",
    "RUST_LIB_BACKTRACE",
    "RUST_LOG",
    "RUST_TEST_THREADS",
    "CARGO",
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
    "CARGO_PRIMARY_PACKAGE",
    "CARGO_RUSTC_CURRENT_DIR",
];

/// The prefixes of the ignored `CARGO*` families ([`BUILD_ENV_IGNORED`]):
/// cargo's terminal output, and the package a process cargo ran belongs to.
pub const BUILD_ENV_IGNORED_PREFIXES: [&str; 4] = [
    "CARGO_TERM_",
    "CARGO_MANIFEST_",
    "CARGO_PKG_",
    "CARGO_BIN_EXE_",
];

/// What a C or C++ build script's compiler and archiver read (the `cc`
/// crate's names, each also as `<NAME>_<target>`, `TARGET_<NAME>` and
/// `HOST_<NAME>`) — each recorded by [`build_env`].
pub const BUILD_ENV_NATIVE: [&str; 8] = [
    "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "ARFLAGS",
];

/// Apple's deployment target and SDK, and `cc`'s no-defaults switch:
/// recorded by exact name ([`build_env`]).
pub const BUILD_ENV_NATIVE_EXACT: [&str; 3] = [
    "MACOSX_DEPLOYMENT_TARGET",
    "SDKROOT",
    "CRATE_CC_NO_DEFAULTS",
];

/// Does [`build_env`] record the variable `name`?
///
/// AN UNKNOWN VARIABLE IS RECORDED (2026-09-27, fourth review). The line was
/// an allowlist of fifteen names and two prefixes, so every variable not on
/// it failed OPEN — for the base comparison and for the release cutter's
/// `build-env none` alike: `CARGO_HOME` (another `config.toml`, with its
/// rustflags, profile or runner), `RUSTC_BOOTSTRAP`, `CARGO_UNSTABLE_*`, a
/// build script's `CC`/`CFLAGS`, `MACOSX_DEPLOYMENT_TARGET`. Now every
/// `RUST*` and `CARGO*` variable is recorded but the few named as inert
/// ([`BUILD_ENV_IGNORED`]), and so is every native-toolchain variable
/// ([`BUILD_ENV_NATIVE`], [`BUILD_ENV_NATIVE_EXACT`]).
#[must_use]
pub fn build_env_records(name: &str) -> bool {
    if name.starts_with("RUST") || name.starts_with("CARGO") {
        return !BUILD_ENV_IGNORED.contains(&name)
            && !BUILD_ENV_IGNORED_PREFIXES
                .iter()
                .any(|p| name.starts_with(p));
    }
    if BUILD_ENV_NATIVE_EXACT.contains(&name) {
        return true;
    }
    let bare = name
        .strip_prefix("TARGET_")
        .or_else(|| name.strip_prefix("HOST_"))
        .unwrap_or(name);
    BUILD_ENV_NATIVE.iter().any(|n| {
        bare == *n
            || bare
                .strip_prefix(n)
                .is_some_and(|rest| rest.starts_with('_'))
    })
}

/// THE BUILD ENVIRONMENT A RUN TOOK FROM ITS CALLER (2026-09-27, third
/// review): every variable set in `vars` that [`build_env_records`] — sorted,
/// as `NAME="value"` — or `none`. The same trustc under `RUSTFLAGS` (the
/// documented flag-spelling override REPLACES the config's, `--cfg
/// clean_islands` with it), behind a `RUSTC` or a wrapper, with
/// `CARGO_PROFILE_TEST_DEBUG_ASSERTIONS=false`, starting test binaries
/// through a target runner, reading another `CARGO_HOME`'s config or
/// compiling a build script's C under other flags builds or runs other code
/// than a run without: its reds are not this run's main's.
#[must_use]
pub fn build_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> String {
    let mut set: Vec<String> = vars
        .into_iter()
        .filter_map(|(k, v)| {
            let k = k.to_str()?.to_string();
            build_env_records(&k).then(|| format!("{k}={:?}", v.to_string_lossy()))
        })
        .collect();
    set.sort_unstable();
    if set.is_empty() {
        "none".to_string()
    } else {
        set.join(" ")
    }
}

/// One spec checker as a `checkers` line names it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CheckerSeen {
    Absent,
    Found {
        path: String,
        origin: String,
        /// `None` for `no --version answer`.
        version: Option<String>,
    },
}

/// What [`crate::checkers`] writes for a checker that did not say its version.
const NO_VERSION: &str = "no --version answer";

impl Tools {
    /// The tools receipt `r` records.
    #[must_use]
    pub fn of(r: &Receipt) -> Self {
        Self {
            toolchain: r.toolchain.clone(),
            checkers: r.checkers.clone(),
            build_env: r.build_env.clone(),
            build_config: r.build_config.clone(),
        }
    }

    /// The compiler's commit — what tells two compilers apart on any machine
    /// (the directory is where ONE machine keeps it, and the lanes already
    /// key their artifacts on the commit) — or `None` when it names none.
    fn compiler(&self) -> Option<&str> {
        let (_, commit) = self.toolchain.rsplit_once(" trustc ")?;
        let commit = commit.trim();
        (!commit.is_empty() && commit != "unknown").then_some(commit)
    }

    /// Each checker the line names, in its order — `None` when the line is
    /// empty or does not read as [`crate::checkers::Checkers::summary`]
    /// writes it.
    fn checkers(&self) -> Option<Vec<(String, CheckerSeen)>> {
        if self.checkers.trim().is_empty() {
            return None;
        }
        self.checkers
            .split("; ")
            .map(|part| {
                let (name, rest) = part.split_once(" = ")?;
                if rest == "absent" {
                    return Some((name.to_string(), CheckerSeen::Absent));
                }
                let (path, inner) = rest.rsplit_once(" (")?;
                let (origin, version) = inner.strip_suffix(')')?.split_once(", ")?;
                Some((
                    name.to_string(),
                    CheckerSeen::Found {
                        path: path.to_string(),
                        origin: origin.to_string(),
                        version: (version != NO_VERSION).then(|| version.to_string()),
                    },
                ))
            })
            .collect()
    }
}

/// `seen` in a few words.
fn checker_words(name: &str, seen: &CheckerSeen) -> String {
    match seen {
        CheckerSeen::Absent => format!("no `{name}`"),
        CheckerSeen::Found {
            path,
            origin,
            version,
        } => format!(
            "`{name}` {} ({origin}, {path})",
            version.as_deref().unwrap_or(NO_VERSION)
        ),
    }
}

/// Why a base made by `base`'s tools cannot judge a run made by `run`'s —
/// `None` when they are the same tools.
///
/// THE SAME COMPILER is the same trustc commit, a fact that holds on every
/// machine (a note is published from another one, whose stage2 lives in
/// another directory); a side that names no commit — no `toolchain` line (a
/// receipt an older gate wrote), or a trustc that answered none — cannot be
/// told from any other compiler. THE SAME BUILD: the same `build-env` line
/// and the same `build-config` line (2026-09-28: the cargo config FILES the
/// compiles read, [`crate::build_config`]), and a receipt without either says
/// nothing about it. THE SAME CHECKERS: each
/// of `ty`, `trust-ir` and `ay` absent on both sides, or found by the same
/// discovery tier and saying the same `--version` — and where either side
/// has no version to compare, found at the same path. A side that names no
/// checkers says nothing about them.
///
/// AND THE SAME STORE BUILD (2026-09-27, fourth review): two found in the
/// atpkg store are the same only at the same place IN it — the resolved
/// path after its last `/store/` (`ty/3007/bin/ty`), which names the store
/// build on every machine ([`store_build`]). `ty --version` and `trust-ir
/// --version` print no build id (`ty 0.13.0`), so a rebuilt `ty` at another
/// store build said the same version and was the same checker.
#[must_use]
pub fn tools_differ(base: &Tools, run: &Tools) -> Option<String> {
    let Some(theirs) = base.compiler() else {
        return Some(format!(
            "it names no compiler commit (`toolchain {}`), so the compiler that found its reds \
             cannot be told from this run's",
            base.toolchain
        ));
    };
    let Some(ours) = run.compiler() else {
        return Some(format!(
            "this run's trustc names no commit (`{}`), so no receipt's compiler can be told from \
             it",
            run.toolchain
        ));
    };
    if theirs != ours {
        return Some(format!(
            "its reds were found by trustc {theirs}, and this run's is trustc {ours} — a red \
             under one compiler is not main's under another"
        ));
    }
    match (&base.build_env, &run.build_env) {
        (None, _) => {
            return Some(
                "it does not say what build environment its compiles took (`RUSTFLAGS`, a \
                 wrapper, a profile override, a target runner)"
                    .to_string(),
            );
        }
        (Some(theirs), Some(ours)) if theirs == ours => {}
        (Some(theirs), ours) => {
            return Some(format!(
                "its reds were built under `{theirs}`, and this run's under `{}` — the same \
                 compiler under other flags builds other code",
                ours.as_deref().unwrap_or("an environment it cannot say")
            ));
        }
    }
    match (&base.build_config, &run.build_config) {
        (None, _) => {
            return Some(
                "it does not say what cargo config files its compiles read (`.cargo/config.toml` \
                 in the repository and its ancestors, `$CARGO_HOME`'s)"
                    .to_string(),
            );
        }
        (Some(_), None) => {
            return Some(
                "this run's cargo config files could not be read, so no receipt's can be told \
                 from them"
                    .to_string(),
            );
        }
        (Some(theirs), Some(ours)) if theirs == ours => {}
        (Some(theirs), Some(ours)) => {
            return Some(format!(
                "its reds were built reading the cargo config files `{theirs}`, and this run's \
                 read `{ours}` — a `rustflags`, a profile or a runner in a config file builds \
                 other code exactly as the variable would"
            ));
        }
    }
    let Some(theirs) = base.checkers() else {
        return Some(format!(
            "it names no spec checkers it can be read for (`checkers {}`)",
            base.checkers
        ));
    };
    let Some(ours) = run.checkers() else {
        return Some(format!(
            "this run's spec checkers cannot be read (`{}`)",
            run.checkers
        ));
    };
    let names: BTreeSet<&str> = theirs
        .iter()
        .chain(&ours)
        .map(|(n, _)| n.as_str())
        .collect();
    for name in names {
        let find = |side: &[(String, CheckerSeen)]| {
            side.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone())
        };
        let (a, b) = (find(&theirs), find(&ours));
        let same = match (&a, &b) {
            (Some(CheckerSeen::Absent), Some(CheckerSeen::Absent)) => true,
            (
                Some(CheckerSeen::Found {
                    path: p1,
                    origin: o1,
                    version: v1,
                }),
                Some(CheckerSeen::Found {
                    path: p2,
                    origin: o2,
                    version: v2,
                }),
            ) => {
                o1 == o2
                    && match (v1, v2) {
                        (Some(v1), Some(v2)) => {
                            v1 == v2
                                && (o1 != crate::checkers::Origin::Store.label()
                                    || match (store_build(p1), store_build(p2)) {
                                        (Some(b1), Some(b2)) => b1 == b2,
                                        _ => p1 == p2,
                                    })
                        }
                        _ => v1 == v2 && p1 == p2,
                    }
            }
            _ => false,
        };
        if !same {
            let words = |s: &Option<CheckerSeen>| {
                s.as_ref().map_or_else(
                    || format!("nothing about `{name}`"),
                    |s| checker_words(name, s),
                )
            };
            return Some(format!(
                "its reds were found with {}, and this run's tests run {} — a spec checker's \
                 red under one is not main's under another",
                words(&a),
                words(&b)
            ));
        }
    }
    None
}

/// Where a store checker sits IN the store: its resolved path after the last
/// `/store/` (`ty/3007/bin/ty`) — the tool and its store build, the same on
/// every machine — or `None` for a path with no `/store/` in it.
fn store_build(path: &str) -> Option<&str> {
    const STORE: &str = "/store/";
    path.rfind(STORE).map(|at| &path[at + STORE.len()..])
}

/// THE SINCE CHAIN: the newest usable receipt on `from`'s first-parent
/// history, `from` itself included — store (commit, then tree) or local note —
/// within [`CHAIN_LIMIT`] commits. One git log, one notes listing, and a
/// `stat` or two per commit.
#[must_use]
pub fn nearest(root: &Path, from: &str) -> Option<Found> {
    let limit = CHAIN_LIMIT.to_string();
    let walk = git_stdout(
        root,
        &[
            "log",
            "--first-parent",
            "-n",
            &limit,
            "--format=%H %T",
            from,
        ],
    )?;
    let noted: BTreeSet<String> =
        git_stdout(root, &["notes", &format!("--ref={NOTES_REF}"), "list"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect();
    let store = receipt::dir(root).ok();
    for line in walk.lines() {
        let Some((commit, tree)) = line.split_once(' ') else {
            continue;
        };
        let held = store
            .as_ref()
            .is_some_and(|s| s.join(commit).is_file() || s.join(receipt::tree_key(tree)).is_file());
        if held && let Ok(found) = lookup(root, commit, false, None) {
            return Some(found);
        }
        if noted.contains(commit)
            && let Some(Ok(found)) = note_receipt(root, commit, None)
        {
            return Some(found);
        }
    }
    None
}

/// Why `head` cannot be baselined, if it cannot: a tree with uncommitted work
/// gets no receipt at all, a baseline is main's own reds, so `head` must be a
/// commit of [`MAIN_REF`] — and it must READ main's published notes first
/// (2026-09-27, second review): when main went red on each failure is carried
/// from them, and a baseline that could not fetch them would restart every
/// red's clock under the cap and publish that over them. A remote with no
/// notes yet is a first baseline, not a failure.
#[must_use]
pub fn baseline_refusal(root: &Path, head: &str, dirty: bool) -> Option<String> {
    if dirty {
        return Some(
            "the tree has uncommitted work, and only a clean tree gets a receipt".to_string(),
        );
    }
    if !is_sha(head) {
        return Some(format!("HEAD is {head}, not a commit"));
    }
    if rev(root, &format!("{MAIN_REF}^{{commit}}")).is_none() {
        return Some(format!("this repository has no {MAIN_REF} to baseline"));
    }
    let on_main = Command::new("git")
        .args(["merge-base", "--is-ancestor", head, MAIN_REF])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !on_main {
        return Some(format!(
            "HEAD {} is not a commit of {MAIN_REF}: a baseline records main's own reds — run \
             it where HEAD is main (`git fetch && git switch --detach {MAIN_REF}` in a spare \
             worktree)",
            short(head)
        ));
    }
    match fetch_notes(root) {
        Err(e) if !e.contains(NO_REMOTE_NOTES) => Some(format!(
            "main's published notes ({MAIN_REMOTE} {NOTES_REF}) could not be fetched ({e}): a \
             baseline carries when main went red on each failure from them, and one made \
             without them would restart every red's clock under the {} h cap and publish that \
             over them",
            INHERITED_CAP_SECS / 3600
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The published notes
// ---------------------------------------------------------------------------

/// Mirror [`MAIN_REMOTE`]'s [`NOTES_REF`] into the local one (forced: the local
/// ref is only ever the remote's; a publish builds on a private ref).
///
/// # Errors
/// No such remote or ref, a network failure, or the [`NETWORK_BOUND`].
pub fn fetch_notes(root: &Path) -> Result<(), String> {
    if git_line(root, &["remote", "get-url", MAIN_REMOTE]).is_none() {
        return Err(format!("no remote `{MAIN_REMOTE}`"));
    }
    git_bounded(
        root,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            MAIN_REMOTE,
            &format!("+{NOTES_REF}:{NOTES_REF}"),
        ],
        NETWORK_BOUND,
    )
    .map(drop)
}

/// PUBLISH `text` as the note on `commit` under [`NOTES_REF`] at
/// [`MAIN_REMOTE`]: fetch the remote's notes, add this one on a PRIVATE ref
/// built from them, push that ref to the remote's [`NOTES_REF`] — never
/// forced, so a publisher that raced us is never overwritten: the push is
/// refused, and the whole round is retried on the new remote state (three
/// rounds). `scratch` holds the note's text for `git notes add -F`.
///
/// # Errors
/// The last round's reason.
pub fn publish(root: &Path, commit: &str, text: &str, scratch: &Path) -> Result<(), String> {
    let private = format!("{NOTES_REF}-publish-{}", std::process::id());
    let file = scratch.join("baseline-note");
    std::fs::write(&file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    let file = file.to_string_lossy().into_owned();
    let mut last = String::new();
    for _ in 0..3 {
        // A remote with no notes yet is a first publish, not a failure.
        let fetched = fetch_notes(root);
        if let Err(e) = &fetched
            && !e.contains(NO_REMOTE_NOTES)
        {
            last = e.clone();
            break;
        }
        let from_remote = fetched.is_ok() && rev(root, NOTES_REF).is_some();
        let seeded = if from_remote {
            git_status(root, &["update-ref", &private, NOTES_REF])
        } else {
            git_status(root, &["update-ref", "-d", &private])
        };
        if let Err(e) = seeded {
            last = e;
            break;
        }
        if let Err(e) = git_status(
            root,
            &[
                "notes",
                &format!("--ref={private}"),
                "add",
                "-f",
                "-F",
                &file,
                commit,
            ],
        ) {
            last = e;
            break;
        }
        match git_bounded(
            root,
            &[
                "push",
                "--quiet",
                MAIN_REMOTE,
                &format!("{private}:{NOTES_REF}"),
            ],
            NETWORK_BOUND,
        ) {
            Ok(_) => {
                let _ = git_status(root, &["update-ref", NOTES_REF, &private]);
                let _ = git_status(root, &["update-ref", "-d", &private]);
                return Ok(());
            }
            Err(e) => last = e,
        }
    }
    let _ = git_status(root, &["update-ref", "-d", &private]);
    Err(last)
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

fn is_sha(s: &str) -> bool {
    s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    // Never a credential prompt on a terminal nobody is watching, and git's
    // own words in one language: the publish loop reads one of its messages.
    c.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C");
    c
}

/// A local git call's stdout, when it succeeded.
pub(crate) fn git_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let out = git(root, args).stderr(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Its first line, trimmed, when non-empty.
fn git_line(root: &Path, args: &[&str]) -> Option<String> {
    let line = git_stdout(root, args)?.lines().next()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

fn rev(root: &Path, what: &str) -> Option<String> {
    git_line(root, &["rev-parse", "--verify", "--quiet", what])
}

/// A local git call that must succeed.
fn git_status(root: &Path, args: &[&str]) -> Result<(), String> {
    let out = git(root, args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// How long a bounded git call's pipes may stay open after git itself has
/// exited or been killed ([`git_bounded`]): long enough for the kernel to hand
/// over what git wrote, never long enough to wait on a process git left behind.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// A git call that may touch the network: stdout on success, the reason
/// otherwise — its stderr, or the bound it overran. POLLED, as
/// [`crate::exec`]'s ceiling is; both pipes are drained on threads so a chatty
/// child cannot wedge on a full one.
///
/// THE BOUND IS A BOUND ON THE WHOLE CALL (2026-09-28, review). git reaches a
/// remote through a CHILD of its own — `git-remote-https` for this repo's
/// origin, `ssh` for an ssh one — and that child inherits git's pipes. Killing
/// git alone left it running, and joining the drain threads then waited for it
/// to close them: measured, a remote helper that slept 40 s held a 10 s bound
/// for 40.06 s. So git runs in a PROCESS GROUP OF ITS OWN, and an overrun
/// kills the group ([`crate::exec::group::kill`]) — every helper with it —
/// and the drains are waited on for [`DRAIN_GRACE`] at most, never joined
/// unconditionally: a helper that left the group (a `setsid`, an ssh
/// `ControlPersist` master) and still holds a pipe costs a detached thread,
/// never the gate's time. The group is on the interrupt list while it runs
/// ([`crate::exec::group::Live`]), because a group of its own no longer hears
/// the terminal's Ctrl-C; and in a background group, an `ssh` that opens the
/// terminal for a passphrase is stopped (`SIGTTIN`) rather than prompting, so
/// it too ends at the bound.
fn git_bounded(root: &Path, args: &[&str], bound: Duration) -> Result<String, String> {
    let mut cmd = git(root, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    #[cfg(unix)]
    let _live = crate::exec::group::Live::enter(child.id(), false);
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            let _ = tx.send(s);
        });
        rx
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < bound => std::thread::sleep(Duration::from_millis(20)),
            _ => break None,
        }
    };
    // Whatever git started dies with it — on an overrun, and on an exit that
    // left a helper behind holding a pipe. A group already empty answers an
    // error, which is the case with nothing to do.
    #[cfg(unix)]
    let _ = crate::exec::group::kill(child.id());
    if status.is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let what = args
        .iter()
        .find(|a| !a.starts_with('-') && !a.contains('='))
        .copied()
        .unwrap_or_default();
    let overran = || format!("git {what} did not finish within {} s", bound.as_secs());
    let Some(status) = status else {
        return Err(overran());
    };
    let deadline = Instant::now() + DRAIN_GRACE;
    let collect = |rx: &std::sync::mpsc::Receiver<String>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    };
    let (out, err) = (collect(&out), collect(&err).unwrap_or_default());
    match (status.success(), out) {
        (true, Some(out)) => Ok(out),
        // git exited, but something it started still held its output open:
        // what it printed cannot be known to be whole.
        (true, None) => Err(overran()),
        (false, _) => Err(err
            .trim()
            .lines()
            .last()
            .unwrap_or("git failed")
            .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, hash: &str) -> Finding {
        Finding {
            id: id.into(),
            hash: hash.into(),
            opaque: None,
            timing: None,
            package: None,
        }
    }

    fn failure(id: &str, hash: &str, since_when: u64) -> Failure {
        Failure {
            id: id.into(),
            hash: hash.into(),
            since: "b".repeat(40),
            since_when,
        }
    }

    const NOW: u64 = 2_000_000_000;

    fn base(failures: Vec<Failure>) -> BaseReds {
        BaseReds {
            commit: "a".repeat(40),
            source: "receipt aaaaaaaaa".into(),
            failures,
            now: NOW,
            nearest: None,
        }
    }

    /// THE NOISE, AND ONLY THE NOISE. Two runs of the same failure differ in
    /// thread ids, pids, build hashes and where the snapshot, the temp dir or
    /// the home lives; they normalize the same. What the failure SAYS — the
    /// words, the whitespace, the path under those roots, the line and column
    /// — is kept (2026-09-27, second review: line numbers and whitespace were
    /// masked, and paths cut to their last component).
    #[test]
    fn normalizing_removes_run_to_run_noise_and_keeps_the_message() {
        let a = "thread 'probe' (41377) panicked at crates/atpkg/tests/index_probe.rs:212:9:\n\
                 lock held by pid 5521 in /Users//a/aterm-verify.noindex/x/lock \
                 (deps/index_probe-1a2b3c4d5e6f7a8b)";
        let b = "thread 'probe' (7) panicked at crates/atpkg/tests/index_probe.rs:212:9:\n\
                 lock held by pid 88 in /Users//b/aterm-gate-fix-verify.noindex/x/lock \
                 (deps/index_probe-99f0e1d2c3b4a596)";
        let n = |s: &str| s.lines().map(normalize).collect::<Vec<_>>();
        assert_eq!(n(a), n(b));
        assert_eq!(
            n(a),
            [
                "thread 'probe' (#) panicked at crates/atpkg/tests/index_probe.rs:212:9:",
                "lock held by pid # in <snapshot>/x/lock (deps/index_probe-<hex>)"
            ]
        );
        // The words are the message.
        assert_ne!(
            normalize("expected foo, got bar"),
            normalize("expected foo, got baz")
        );
        assert_ne!(
            normalize("crates/a/src/lib.rs:1:1"),
            normalize("crates/b/src/lib.rs:1:1"),
            "a relative path names which file"
        );
        // Every character of whitespace is kept; only a CRLF's `\r` goes.
        assert_eq!(normalize("  padded \t words  "), "  padded \t words  ");
        assert_eq!(normalize("crlf\r"), "crlf");
        // Each mask, alone — and what is left alone beside it.
        for (line, want) in [
            (
                "--> crates/a/src/lib.rs:12:5",
                "--> crates/a/src/lib.rs:12:5",
            ),
            ("12 |     x.clone()", "12 |     x.clone()"),
            ("(pid=5521) PID: 7 pid 9", "(pid=#) PID: # pid #"),
            (
                "finished in 1.00s; took 150ms, 12\u{b5}s",
                "finished in #s; took #ms, 12\u{b5}s",
            ),
            (
                "   3: 0x7ffee4b2c9a0 - std::rt::lang_start",
                "   3: 0x<hex> - std::rt::lang_start",
            ),
            (
                "3   libsystem_kernel.dylib  0x00000001899a2a60 __pthread_kill + 8",
                "3   libsystem_kernel.dylib  0x<hex> __pthread_kill + 8",
            ),
            ("digest 0x7ffee4b2c9a0", "digest 0x7ffee4b2c9a0"),
            (
                "HEAD 43f8b339f is not a commit",
                "HEAD <hex> is not a commit",
            ),
            ("red on main since 43f8b339f", "red on main since <hex>"),
            ("colour 1a2b3cff", "colour 1a2b3cff"),
            ("deps/probe-0000111122223333", "deps/probe-<hex>"),
            ("probe-0000111122223333", "probe-0000111122223333"),
            ("at /private/tmp/.tmpAbC12/x.sock", "at <tmp>/*/x.sock"),
            ("at /var/folders/ab/cd/T/atv-1/x", "at <tmp>/*/x"),
            ("at /tmp/shared/x", "at <tmp>/shared/x"),
            (
                "read `/Users//u/.config/aterm/aterm.toml`",
                "read `~/.config/aterm/aterm.toml`",
            ),
            ("read `/etc/aterm.toml`", "read `/etc/aterm.toml`"),
            ("  left: 150ms", "  left: 150ms"),
            (
                " right: \"pid 5521 since 43f8b339f\"",
                " right: \"pid 5521 since 43f8b339f\"",
            ),
            ("  left: \"/Users//u/.config/x\"", "  left: \"~/.config/x\""),
        ] {
            assert_eq!(normalize(line), want, "{line}");
        }
        assert!(measured_duration(["render took 900ms"]));
        assert!(!measured_duration(["  left: 900ms", "900ms", "in 3 tests"]));
    }

    /// EVERY OTHER NUMBER IS THE MESSAGE (2026-09-27, review): counts, sizes,
    /// an assertion's values, a spec checker's state counts, a time of day, an
    /// address and port — and a word that only looks like a unit.
    #[test]
    fn normalizing_keeps_every_number_that_is_not_noise() {
        for (a, b) in [
            ("  left: 41\n right: 42", "  left: 41\n right: 97"),
            (
                "interpreter walked 1712 reachable states, `ty` reports 1662",
                "interpreter walked 40 reachable states, `ty` reports 90000",
            ),
            (
                "  FAIL B9d MTLCreateSystemDefaultDevice 1",
                "  FAIL B9d MTLCreateSystemDefaultDevice 5",
            ),
            ("wrote 1048576 bytes", "wrote 1048577 bytes"),
            (
                "error: could not compile `a` due to 2 previous errors",
                "error: could not compile `a` due to 3 previous errors",
            ),
            ("at 12:30:45", "at 12:30:46"),
            ("connect 127.0.0.1:5432", "connect 127.0.0.1:5433"),
            ("rapid 5", "rapid 6"),
            ("10seconds", "11seconds"),
            // The second review's (2026-09-27): whitespace, a line and column,
            // an absolute path's directories, a value's duration, hex or path.
            ("  left: \"ab  c\"", "  left: \"ab        c\""),
            ("   indented", " indented"),
            ("trailing ", "trailing"),
            ("render.rs:21:30", "render.rs:388:14"),
            ("12 | x", "13 | x"),
            (
                "Diff in /Users//u/snap/crates/a/src/lib.rs",
                "Diff in /Users//u/snap/crates/b/src/lib.rs",
            ),
            (
                "  left: \"/Users//u/.config/aterm/aterm.toml\"",
                "  left: \"/etc/aterm.toml\"",
            ),
            ("  left: 4s", "  left: 90s"),
            ("  left: \"1a2b3cff\"", "  left: \"9e0d4c11\""),
            ("got 0x1a2b3c4d5e", "got 0x9e0d4c1100"),
        ] {
            let n = |s: &str| s.lines().map(normalize).collect::<Vec<_>>();
            assert_ne!(n(a), n(b), "{a:?} and {b:?} are two failures");
        }
    }

    /// A ROW'S FINDING is its lines in blocks, sorted and counted: the same
    /// lint set in another order is the same failure, one more instance of
    /// an error main already has is not, the same lint on a moved line is not
    /// (2026-09-27, second review), and progress lines (which differ with the
    /// cache) are no part of it.
    #[test]
    fn a_rows_fingerprint_is_its_diagnostics_in_any_order_counted() {
        let lint = |extra: &str| {
            format!(
                "   Compiling a v0.1.0\n{extra}error: this call to `clone` can be replaced\n   \
                 --> crates/a/src/lib.rs:12:5\nerror: unused import\n   --> crates/b/src/x.rs:3:1\n\
                 error: could not compile `a` (lib) due to 2 previous errors\n"
            )
        };
        let one = row_finding("tippy lint", &lint(""));
        assert_eq!(one.id, "tippy lint");
        assert_eq!(one.opaque, None);
        let reordered = "error: unused import\n   --> crates/b/src/x.rs:3:1\n    Checking b\n\
                         error: this call to `clone` can be replaced\n   --> crates/a/src/lib.rs:12:5\n\
                         error: could not compile `a` (lib) due to 2 previous errors\n";
        assert_eq!(row_finding("tippy lint", reordered).hash, one.hash);
        let moved = reordered.replace("x.rs:3:1", "x.rs:9:1");
        assert_ne!(
            row_finding("tippy lint", &moved).hash,
            one.hash,
            "the same lint on another line is another failure"
        );
        let another = row_finding(
            "tippy lint",
            &lint("error: unused import\n   --> crates/b/src/x.rs:7:1\n"),
        );
        assert_ne!(another.hash, one.hash, "a second instance is a new failure");
        // No diagnostic line: every line counts, in order.
        let script = row_finding(
            "license_check.sh",
            "checked 12 files\nbad header: src/a.rs\n",
        );
        assert_eq!(script.opaque, None);
        assert_ne!(
            script.hash,
            row_finding(
                "license_check.sh",
                "checked 12 files\nbad header: src/b.rs\n"
            )
            .hash
        );
        // Nothing at all: nothing to compare.
        assert_eq!(row_finding("x", "\n  \n").opaque, Some(SILENT));
        // ANSI is not a difference.
        assert_eq!(
            row_finding("tippy lint", &format!("\x1b[1m{}\x1b[0m", lint(""))).hash,
            one.hash
        );
    }

    /// A TEST CHILD'S FINDINGS are its failed tests, each id naming the binary
    /// through cargo's re-run spec, each fingerprinted from its whole block —
    /// what it printed before its panic and its panic's line included
    /// (2026-09-27, second review) — a refused test is opaque; a log that
    /// cannot account for its failures, a binary that crashed after its
    /// result among them, is the row.
    #[test]
    fn a_test_childs_findings_are_its_failed_tests() {
        let log = "     Running tests/probe.rs (target/debug/deps/probe-1)\n\nrunning 2 tests\n\
                   test a ... FAILED\ntest b ... FAILED\n\nfailures:\n\n---- a stdout ----\n\
                   some captured chatter 1234\n\nthread 'a' panicked at crates/x/tests/probe.rs:9:5:\n\
                   lock held\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\
                   ---- b stdout ----\naterm-gate: COULD NOT RUN — strays alive\n\n\
                   failures:\n    a\n    b\n\ntest result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; \
                   0 filtered out; finished in 1.00s\n\nerror: test failed, to rerun pass `-p x --test probe`\n";
        let got = test_findings("targo test --workspace --tests", log);
        assert_eq!(
            got.iter()
                .map(|f| (f.id.as_str(), f.opaque))
                .collect::<Vec<_>>(),
            [
                ("-p x --test probe -- a", None),
                ("-p x --test probe -- b", Some(REFUSED)),
            ]
        );
        // Each names the crate its re-run spec names; a whole row names none.
        assert!(got.iter().all(|f| f.package.as_deref() == Some("x")));
        assert_eq!(row_finding("tippy lint", "error: x\n").package, None);
        // What the test printed before its panic is the message, and so is
        // the panic's line; a thread id is noise.
        for changed in [
            log.replace("chatter 1234", "other chatter"),
            log.replace("probe.rs:9:5", "probe.rs:30:5"),
        ] {
            assert_ne!(
                test_findings("targo test --workspace --tests", &changed)[0].hash,
                got[0].hash,
                "{changed}"
            );
        }
        assert_eq!(
            test_findings(
                "targo test --workspace --tests",
                &log.replace("thread 'a' panicked", "thread 'a' (41377) panicked")
            )[0]
            .hash,
            test_findings(
                "targo test --workspace --tests",
                &log.replace("thread 'a' panicked", "thread 'a' (7) panicked")
            )[0]
            .hash
        );
        let differently = log.replace("lock held", "lock lost");
        assert_ne!(
            test_findings("targo test --workspace --tests", &differently)[0].hash,
            got[0].hash
        );
        // Unaccounted (a crashed binary): the row, one finding.
        let crashed = format!(
            "{log}error: test failed, to rerun pass `-p x --test crash`\n  process didn't exit \
             successfully: `crash-1` (signal: 11, SIGSEGV)\n"
        );
        let row = test_findings("targo test --workspace --tests", &crashed);
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].id, "targo test --workspace --tests");
        assert_eq!(row[0].opaque, Some(UNACCOUNTED), "never inherited");
        // A binary that printed its result and THEN died of a signal: the
        // crash is in no listed failure, so the log is not accounted for.
        let late = format!(
            "{log}\nCaused by:\n  process didn't exit successfully: `probe-1` (signal: 11, \
             SIGSEGV: invalid memory reference)\n"
        );
        let row = test_findings("targo test --workspace --tests", &late);
        assert_eq!(
            (row.len(), row[0].opaque),
            (1, Some(UNACCOUNTED)),
            "{row:?}"
        );
    }

    /// A TIMING-SHAPED FAILURE IS TOLD BY WHAT IT SAID (2026-09-26): a test
    /// whose panic says a clock ran out, or whose name is about one, is tagged
    /// with the words that said so; a plain assertion is not; and a row is
    /// tagged by its diagnostic lines or its label, never by a path somewhere
    /// in its progress output.
    #[test]
    fn a_failure_that_reads_as_a_clock_running_out_is_tagged_by_its_message() {
        let log = |name: &str, msg: &str| {
            format!(
                "     Running unittests src/lib.rs (target/debug/deps/g-1)\n\nrunning 1 test\n\
                 test {name} ... FAILED\n\nfailures:\n\n---- {name} stdout ----\n\n\
                 thread '{name}' panicked at src/x.rs:9:5:\n{msg}\n\nfailures:\n    {name}\n\n\
                 test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered \
                 out; finished in 1.00s\n\nerror: test failed, to rerun pass `-p g --lib`\n"
            )
        };
        let tag = |name: &str, msg: &str| test_findings("t", &log(name, msg))[0].timing;
        assert_eq!(
            tag(
                "app::capture",
                "image worker reply: RecvTimeoutError::Timeout"
            ),
            Some("timeout")
        );
        assert_eq!(
            tag("q::expires", "launchctl submit did not finish within 1s"),
            Some("did not finish within")
        );
        assert_eq!(
            tag("launchd_tests::share_the_real_callers_deadline", "left: 1"),
            Some("deadline"),
            "the test's own name"
        );
        assert_eq!(tag("a::plain", "assertion `left == right` failed"), None);
        // A clock it read and printed (2026-09-27, second review).
        assert_eq!(
            tag("r::budget", "render took 900ms, over the 16ms budget"),
            Some(MEASURED_DURATION)
        );
        assert_eq!(tag("r::value", "  left: 900ms\n right: 16ms"), None);
        assert_eq!(
            row_finding("gate x", "   Compiling timeout-rs v1\nerror: lint").timing,
            None,
            "a crate name in progress output is not what the row said"
        );
        assert_eq!(
            row_finding("gate x", "error: the probe timed out after 30s").timing,
            Some("timed out")
        );
        assert_eq!(
            label_finding("smoke: frame deadline missed").timing,
            Some("deadline")
        );
    }

    /// main's receipt listing exactly `f`, red since an hour ago.
    fn listing(f: &Finding) -> BaseReds {
        base(vec![failure(&f.id, &f.hash, NOW - 3600)])
    }

    /// A NEW RED UNDER AN OLD RED'S NAME IS NEW (2026-09-27, review). Each case
    /// the review reproduced read as main's, and the run claimed the merge
    /// contract with exit 0. First: `tools/grep_guard.sh` as it really prints —
    /// a `  FAIL <check> <count>` summary, each hit on its own line, `GUARD:
    /// FAIL` — where main has one hit and the branch adds four in other files.
    #[test]
    fn new_guard_violations_under_a_check_main_already_fails_are_new() {
        let guard = |hits: &[&str]| {
            let mut s = String::from(
                "B9 metal door:\n  ok   B9a no raw objc2-metal outside metal/            0\n",
            );
            for h in hits {
                s.push_str(&format!("        {h}\n"));
            }
            s.push_str(&format!(
                "  FAIL {:<46} {}\nGUARD: FAIL\n",
                "B9d MTLCreateSystemDefaultDevice only via Device::preferred",
                hits.len()
            ));
            s
        };
        let old = "crates/aterm-gpu/src/old.rs:12:MTLCreateSystemDefaultDevice()";
        let on_main = row_finding("grep_guard.sh", &guard(&[old]));
        let on_branch = row_finding(
            "grep_guard.sh",
            &guard(&[
                old,
                "crates/aterm-gui/src/new_a.rs:40:MTLCreateSystemDefaultDevice()",
                "crates/aterm-gui/src/new_b.rs:77:MTLCreateSystemDefaultDevice()",
                "crates/aterm-render/src/new_c.rs:5:MTLCreateSystemDefaultDevice()",
                "crates/aterm-render/src/new_d.rs:9:MTLCreateSystemDefaultDevice()",
            ]),
        );
        let b = listing(&on_main);
        assert_eq!(
            judge(&on_branch, &b),
            Disposition::New(NewWhy::FailsDifferently)
        );
        let mut t = Tally::default();
        t.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "grep_guard.sh",
            std::slice::from_ref(&on_branch),
        );
        let v = crate::verdict::verdict_against(
            crate::Mode::Fast,
            &crate::Scope::workspace(),
            &t,
            &Against::Base(b.clone()),
        );
        assert!(!v.claims_merge_contract, "{}", v.text);
        assert_eq!(v.exit, crate::exit::FAILED, "{}", v.text);
        // The control: the same hit is still main's red — and the same call on
        // a moved line is another hit (2026-09-27, second review).
        assert!(judge(&row_finding("grep_guard.sh", &guard(&[old])), &b).excused());
        let moved = row_finding(
            "grep_guard.sh",
            &guard(&["crates/aterm-gpu/src/old.rs:31:MTLCreateSystemDefaultDevice()"]),
        );
        assert_eq!(
            judge(&moved, &b),
            Disposition::New(NewWhy::FailsDifferently)
        );
    }

    /// The same test failing with other values, and the spec checker's state
    /// counts, are other failures — and so are the same values at another
    /// assertion (2026-09-27, second review); the same values at the same one
    /// are the same failure.
    #[test]
    fn a_test_failing_with_other_values_is_new() {
        let log = |msg: &str, at: &str| {
            format!(
                "     Running unittests src/lib.rs (target/debug/deps/g-1)\n\nrunning 1 test\n\
                 test census::lock_sites ... FAILED\n\nfailures:\n\n---- census::lock_sites stdout \
                 ----\n\nthread 'census::lock_sites' (7) panicked at src/x.rs:{at}:\n{msg}\n\n\
                 failures:\n    census::lock_sites\n\ntest result: FAILED. 0 passed; 1 failed; \
                 0 ignored; 0 measured; 0 filtered out; finished in 1.00s\n\nerror: test failed, \
                 to rerun pass `-p g --lib`\n"
            )
        };
        let assertion = |right: &str| {
            format!(
                "assertion `left == right` failed: lock-census sites\n  left: 41\n right: {right}"
            )
        };
        let one = |msg: &str, at: &str| test_findings("t", &log(msg, at)).remove(0);
        let main = one(&assertion("42"), "9:5");
        assert_eq!(
            judge(&one(&assertion("97"), "9:5"), &listing(&main)),
            Disposition::New(NewWhy::FailsDifferently)
        );
        assert!(judge(&one(&assertion("42"), "9:5"), &listing(&main)).excused());
        assert_eq!(
            judge(&one(&assertion("42"), "30:9"), &listing(&main)),
            Disposition::New(NewWhy::FailsDifferently)
        );
        let spec = |walked: &str, ty: &str| {
            format!("interpreter walked {walked} reachable states, `ty` reports {ty}")
        };
        assert_ne!(
            one(&spec("1712", "1662"), "9:5").hash,
            one(&spec("40", "90000"), "9:5").hash
        );
    }

    /// A smoke whose child exited early is fingerprinted from the child's log
    /// under its row, not from its label: another crash is another failure.
    /// And since the fourth review (2026-09-27) the same crash is not main's
    /// either: the row ENDED the smoke, so every check behind it went
    /// undecided, and nothing a stage decides by itself ([`Report::fail`]) is
    /// inherited. A row after which the stage goes on
    /// ([`Report::fail_and_continue`]) is its label, and is main's.
    ///
    /// [`Report::fail`]: crate::ladder::Report::fail
    /// [`Report::fail_and_continue`]: crate::ladder::Report::fail_and_continue
    #[test]
    fn a_smoke_launch_crash_is_its_log_not_its_label() {
        let smoke = |crash: &str| {
            let mut r = crate::ladder::Report::new("control-socket smoke");
            r.fail("smoke: aterm-gui exited early");
            r.raw(format!(
                "        control-socket smoke child log (last 80 lines):\n          thread \
                 'main' (12) panicked at crates/aterm-gui/src/{crash}\n"
            ));
            r.pass("an unrelated later row");
            crate::ladder::tally(&[r])
        };
        let main = smoke("a.rs:1:1:\n          crash A");
        let again = smoke("a.rs:1:1:\n          crash A");
        let other = smoke("NEW.rs:9:9:\n          NEW crash B");
        assert_eq!(again.findings_of(0)[0].hash, main.findings_of(0)[0].hash);
        assert_ne!(other.findings_of(0)[0].hash, main.findings_of(0)[0].hash);
        let b = listing(&main.findings_of(0)[0]);
        for t in [&again, &other] {
            assert_eq!(
                judge(&t.findings_of(0)[0], &b),
                Disposition::New(NewWhy::Opaque(UNFINISHED))
            );
        }
        // A row with nothing under it is its label — never inherited unless
        // the stage went on after it.
        let mut bare = crate::ladder::Report::new("gui smoke");
        bare.fail("gui smoke: metrics reset -> ERR busy");
        let t = crate::ladder::tally(&[bare]);
        assert_eq!(
            t.findings_of(0),
            [never_inherited(
                label_finding("gui smoke: metrics reset -> ERR busy"),
                UNFINISHED
            )]
        );
        let mut went_on = crate::ladder::Report::new("gui smoke");
        went_on.fail_and_continue("gui smoke: metrics reset -> ERR busy");
        let t = crate::ladder::tally(&[went_on]);
        let f = &t.findings_of(0)[0];
        assert_eq!(f, &label_finding("gui smoke: metrics reset -> ERR busy"));
        assert!(judge(f, &listing(f)).excused());
    }

    /// ONLY A CHECK THAT RAN TO ITS END IS INHERITED (2026-09-27, fourth
    /// review): each checker's proof that it reached its end, both ways.
    #[test]
    fn a_checker_is_inherited_only_when_its_output_shows_it_ran_to_its_end() {
        // The guard and the header check: their closing verdict line, at the
        // margin — a COULD NOT RUN exits before it.
        assert_eq!(guard_ran_to_end("  FAIL B9d x 1\nGUARD: FAIL\n"), Ok(()));
        assert_eq!(
            guard_ran_to_end(
                "  FAIL B9d x 1\ngrep_guard: COULD NOT RUN — git grep failed over the tree (B16)\n"
            ),
            Err(CUT_SHORT)
        );
        assert_eq!(
            guard_ran_to_end("        crates/a/src/x.rs:3:GUARD: FAIL\n"),
            Err(CUT_SHORT),
            "a hit that quotes the verdict is not it"
        );
        assert_eq!(
            license_ran_to_end(
                "  FAIL crates/a/src/x.rs (no-SPDX )\n  FAIL 1 of 9 .rs files lack a complete \
                 SPDX header\nLICENSE: FAIL\n"
            ),
            Ok(())
        );
        assert_eq!(
            license_ran_to_end("  UNREADABLE crates/a/src/x.rs\nLICENSE: COULD NOT RUN\n"),
            Err(CUT_SHORT)
        );

        // The export content scan: its FAIL verdict comes after all three
        // guards; a refused export never reached them.
        assert_eq!(
            export_content_ran_to_end(
                "  crates/a/src/x.rs:12  forbidden content — Credentials \
                 (baseline/forbidden-content.txt:19)\nEXPORT CONTENT: FAIL\n  1 hit(s)\n"
            ),
            Ok(())
        );
        assert_eq!(
            export_content_ran_to_end(
                "  engine: FAIL: transforms failed\nEXPORT CONTENT: REFUSED — the engine \
                 refused the export (publish/transforms.sh); no content guard ran\n"
            ),
            Err(CUT_SHORT)
        );

        // The formatter: its verb's verdict, no pass NOT RUN, no error.
        let fmt = |extra: &str| {
            format!(
                "=== gate lint (tippy -D warnings + trustfmt) ===\n  tippy: NOT RUN — excluded \
                 by --fmt-only. Nothing was learned about it.\nDiff in /r/crates/a/src/lib.rs:12:\n \
                 fn f() {{\n-\tlet x = 1;\n+    let x = 1;\n error(\"a diff's own line\");\n \
                 }}\n{extra}  trustfmt: FINDING — drift at 1 path(s)\n{LINT_VERDICT_FAILED} — \
                 findings in: trustfmt\n"
            )
        };
        assert_eq!(fmt_ran_to_end(&fmt("")), Ok(()));
        assert_eq!(
            fmt_ran_to_end(&fmt(
                "  trustfmt sweep: NOT RUN — no `trustfmt` in /s. The files `--all` cannot reach\n"
            )),
            Err(FMT_PASS_NOT_RUN),
            "a finding outranks the sweep's NOT RUN in the verdict"
        );
        assert_eq!(
            fmt_ran_to_end(&fmt(
                "error: expected item, found `}`\n --> /r/crates/b/src/x.rs:3:1\n"
            )),
            Err(FMT_ERROR)
        );
        assert_eq!(
            fmt_ran_to_end(
                "error[E0425]: cannot find value `gone` in this scope\nerror: could not compile \
                 `xtask` (bin \"xtask\") due to 1 previous error\n"
            ),
            Err(CUT_SHORT),
            "the verb never built"
        );

        // The lint: every unit it could not compile is one nothing is built
        // on, and no compile error stopped a target before its lint passes.
        let lint = |units: &[&str]| {
            let mut s = String::from(
                "error: this call to `clone` can be replaced\n  --> crates/a/tests/probe.rs:12:5\n",
            );
            for u in units {
                s.push_str(&format!(
                    "error: could not compile `a` ({u}) due to 1 previous error\n"
                ));
            }
            s
        };
        for leaf in [
            "lib test",
            "test \"probe\"",
            "example \"demo\"",
            "bench \"b\"",
            "bin \"aterm\"",
            "bin \"aterm\" test",
        ] {
            assert_eq!(lint_reached_every_unit(&lint(&[leaf])), Ok(()), "{leaf}");
        }
        for blocking in ["lib", "build script", "proc-macro"] {
            assert_eq!(
                lint_reached_every_unit(&lint(&["lib test", blocking])),
                Err(LINT_BLOCKED),
                "{blocking}"
            );
        }
        assert_eq!(
            lint_reached_every_unit("error: could not compile `a` due to previous error\n"),
            Err(LINT_BLOCKED),
            "a unit it does not name"
        );
        assert_eq!(
            lint_reached_every_unit(&format!(
                "error[E0425]: cannot find value `gone` in this scope\n{}",
                lint(&["test \"probe\""])
            )),
            Err(LINT_HARD_ERROR)
        );
        assert_eq!(
            lint_reached_every_unit(
                "error: failed to run custom build command for `ring v0.17.8`\n"
            ),
            Err(LINT_BLOCKED)
        );
        assert_eq!(
            lint_reached_every_unit(
                "error: the lock file needs to be updated but --locked was passed\n"
            ),
            Err(LINT_UNNAMED)
        );
        assert_eq!(
            lint_reached_every_unit(
                "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m: could not compile `a` (lib)\x1b[0m\n"
            ),
            Err(LINT_BLOCKED),
            "colour is not a difference"
        );
    }

    /// THE EXPORT SCAN'S HITS ARE KEYED BY PATH AND CLASS (2026-09-29, review
    /// of the stage). Main carries three hits; a branch that edits above them
    /// (every line moves), clears one, and has the engine's baseline shifted
    /// under it (every pattern line moves) still carries only main's — each
    /// inherited — while one more hit of a path and class main has is new,
    /// and so is one in a file main has none in. A refusal, or a row with a
    /// line that is not a hit, stays the one row finding.
    #[test]
    fn export_content_hits_are_findings_keyed_by_path_and_class() {
        let scan = |hits: &[&str]| {
            let mut s = String::from(
                "export content scan: the publication engine's content guards over this \
                 tree's export (publish/manifest.txt + publish/transforms.sh)\n",
            );
            for h in hits {
                s.push_str(&format!("  {h}\n"));
            }
            s.push_str(
                "EXPORT CONTENT: FAIL\n  3 hit(s) the engine's content guards refuse\n  Fix the \
                 SOURCE — …; the engine: /Users//x/publication (rev 0123456789ab).\n",
            );
            s
        };
        let label = "export-content-scan.py: the publication engine's content guards refuse";
        let main = export_content_findings(
            label,
            &scan(&[
                "crates/a/src/footer.rs:1876  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
                "crates/a/src/footer.rs:9  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
                "crates/b/src/codex_tests.rs:1358  forbidden content — Personal / machine \
                 paths (baseline/forbidden-content.txt:9)",
            ]),
        );
        let ids: Vec<&str> = main.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "export-content-scan.py -- crates/a/src/footer.rs -- forbidden content — \
                 Agent-session artifacts",
                "export-content-scan.py -- crates/a/src/footer.rs -- forbidden content — \
                 Agent-session artifacts #2",
                "export-content-scan.py -- crates/b/src/codex_tests.rs -- forbidden content — \
                 Personal / machine paths",
            ]
        );
        assert!(
            main.iter()
                .all(|f| f.opaque.is_none() && f.timing.is_none())
        );
        let reds = base(
            main.iter()
                .map(|f| failure(&f.id, &f.hash, NOW - 3600))
                .collect(),
        );
        let judged = |hits: &[&str]| -> Vec<bool> {
            export_content_findings(label, &scan(hits))
                .iter()
                .map(|f| judge(f, &reds).excused())
                .collect()
        };
        // Lines moved, the baseline's too, the private-reference hit cleared.
        assert_eq!(
            judged(&[
                "crates/a/src/footer.rs:2015  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:33)",
                "crates/a/src/footer.rs:40  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:33)",
            ]),
            [true, true]
        );
        // One more of main's class in main's file, and one in a new file.
        assert_eq!(
            judged(&[
                "crates/a/src/footer.rs:12  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
                "crates/a/src/footer.rs:1876  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
                "crates/a/src/footer.rs:9  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
                "crates/c/src/new.rs:3  forbidden content — Agent-session artifacts \
                 (baseline/forbidden-content.txt:31)",
            ]),
            [true, true, false, false]
        );
        // A notes-carrying location, a withheld name, gitleaks' rule text.
        let keyed = export_content_findings(
            label,
            &scan(&[
                "src/t.txt:2 [export only: publish/transforms.sh rewrote this line]  forbidden \
                 content — Agent artifacts (baseline/forbidden-content.txt:10)",
                "vendor/astream/x.rs:4 [the astream submodule, rev 0123abcd]  forbidden \
                 content — A (baseline/forbidden-content.txt:1); B (publish/forbidden-extra.txt:5)",
                "src/[REDACTED]  forbidden file name — Agent artifacts \
                 (baseline/forbidden-content.txt:10)",
                "src/token.rs:1  gitleaks github-pat — GitHub Personal Access Token (PAT)",
            ]),
        );
        assert_eq!(
            keyed.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            [
                "export-content-scan.py -- src/t.txt -- forbidden content — Agent artifacts",
                "export-content-scan.py -- vendor/astream/x.rs -- forbidden content — A; B",
                "export-content-scan.py -- src/[REDACTED] -- forbidden file name — Agent \
                 artifacts",
                "export-content-scan.py -- src/token.rs -- gitleaks github-pat — GitHub \
                 Personal Access Token (PAT)",
            ]
        );

        // Never itemized: a refused export, and a line that is not a hit.
        let refused = export_content_findings(
            label,
            "export content scan: …\n  engine: FAIL: transforms failed\nEXPORT CONTENT: \
             REFUSED — the engine refused the export (publish/transforms.sh); no content \
             guard ran\n",
        );
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].id, label);
        assert_eq!(refused[0].opaque, Some(CUT_SHORT));
        let mut stray = scan(&["src/a.rs:1  forbidden content — A (baseline/x.txt:1)"]);
        stray.insert_str(
            stray.find("  src/a.rs").expect("the hit"),
            "Traceback (most recent call last):\n",
        );
        let whole = export_content_findings(label, &stray);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].id, label);
        assert_eq!(
            whole[0].opaque, None,
            "a whole row that ran to its verdict is main's only when it printed the same"
        );
    }

    /// P4 (2026-09-27, fourth review): A LINT RED IN A LIBRARY NEVER EXCUSES
    /// THE CRATES IT LEFT UNLINTED. Main's `l` has a lint under `-D warnings`,
    /// so `d`, which depends on `l`, is never linted; the branch adds a lint
    /// to `d`, which is never printed. The two logs are the same — both
    /// hashed as main's, measured by the review on a real two-crate
    /// workspace — and the row was INHERITED. Now it is never inherited; the
    /// control, a lint red in a test target only, still is.
    #[test]
    fn a_lint_red_in_a_library_never_excuses_the_crates_it_left_unlinted() {
        let row = |log: &str| {
            let mut r = crate::ladder::Report::new("tippy lint");
            r.fail_checker_child(
                &crate::exec::Run {
                    ok: false,
                    output: log.to_string(),
                    code: Some(101),
                    spawn_error: None,
                },
                "tippy --workspace -D warnings",
                lint_reached_every_unit,
            );
            crate::ladder::tally(&[r]).findings_of(0)[0].clone()
        };
        let lint_in = |file: &str, unit: &str| {
            format!(
                "    Checking l v0.1.0 (/w/l)\nerror: using `clone` on type `u8` which implements \
                 the `Copy` trait\n --> {file}:3:5\n  |\n3 |     x.clone()\n  |     ^^^^^^^^^ \
                 help: try removing the `clone` call: `x`\n  |\n  = note: `-D \
                 clippy::clone-on-copy` implied by `-D warnings`\n\nerror: could not compile \
                 `l` ({unit}) due to 1 previous error\n    Checking d v0.1.0 (/w/d)\n"
            )
        };
        let main = row(&lint_in("l/src/lib.rs", "lib"));
        let branch = row(&lint_in("l/src/lib.rs", "lib"));
        assert_eq!(branch.hash, main.hash, "the dependent's lint never printed");
        assert_eq!(
            judge(&branch, &listing(&main)),
            Disposition::New(NewWhy::Opaque(LINT_BLOCKED))
        );
        let main = row(&lint_in("l/tests/t.rs", "test \"t\""));
        assert!(
            judge(
                &row(&lint_in("l/tests/t.rs", "test \"t\"")),
                &listing(&main)
            )
            .excused()
        );
    }

    /// AN UNKNOWN BUILD VARIABLE IS RECORDED (2026-09-27, fourth review):
    /// every `RUST*` and `CARGO*` variable but the few named as inert, and a
    /// build script's native toolchain — so a base, or a MEASURE receipt the
    /// release cutter reads as `build-env none`, cannot pass one over.
    #[test]
    fn an_unknown_build_variable_is_recorded() {
        for recorded in [
            "RUSTFLAGS",
            "RUSTC_BOOTSTRAP",
            "RUSTUP_TOOLCHAIN",
            "RUST_MIN_STACK",
            "CARGO_HOME",
            "CARGO_UNSTABLE_BUILD_STD",
            "CARGO_PROFILE_TEST_DEBUG_ASSERTIONS",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
            "CC",
            "CXX",
            "AR",
            "CFLAGS",
            "LDFLAGS",
            "CC_aarch64_apple_darwin",
            "CFLAGS_aarch64-apple-darwin",
            "TARGET_CC",
            "HOST_CFLAGS",
            "MACOSX_DEPLOYMENT_TARGET",
            "SDKROOT",
            "CRATE_CC_NO_DEFAULTS",
        ] {
            assert!(build_env_records(recorded), "{recorded}");
        }
        for inert in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_TARGET_TMPDIR",
            "CARGO_BUILD_JOBS",
            "CARGO_INCREMENTAL",
            "CARGO_TERM_COLOR",
            "CARGO_TERM_VERBOSE",
            "CARGO_LOG",
            "CARGO_MAKEFLAGS",
            "RUST_BACKTRACE",
            "RUST_LIB_BACKTRACE",
            "RUST_LOG",
            "RUST_TEST_THREADS",
            "CARGO",
            "CARGO_MANIFEST_DIR",
            "CARGO_PKG_NAME",
            "CARGO_BIN_EXE_aterm",
            "PATH",
            "HOME",
            "TRUST_NO_MIGRATE_WARN",
            "CCACHE_DIR",
            "ARCHFLAGS_X",
        ] {
            assert!(!build_env_records(inert), "{inert}");
        }
        assert_eq!(
            build_env([
                ("CARGO_HOME".into(), "/h/.cargo".into()),
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("RUSTC_BOOTSTRAP".into(), "1".into()),
            ]),
            "CARGO_HOME=\"/h/.cargo\" RUSTC_BOOTSTRAP=\"1\""
        );
        // What `targo run` hands the process it runs — the gate, under
        // `targo --unverified verify` (`run -p xtask -- verify`) — measured
        // 2026-09-27 through a runner: none of it is recorded, so a run
        // through the verb and one through the script are one build.
        let run_env = [
            ("CARGO", "/store/trust/9192/bin/targo"),
            ("CARGO_MANIFEST_DIR", "/w/aterm-k-gate/crates/xtask"),
            (
                "CARGO_MANIFEST_PATH",
                "/w/aterm-k-gate/crates/xtask/Cargo.toml",
            ),
            ("CARGO_PKG_AUTHORS", ""),
            ("CARGO_PKG_DESCRIPTION", ""),
            ("CARGO_PKG_HOMEPAGE", ""),
            ("CARGO_PKG_LICENSE", "Apache-2.0"),
            ("CARGO_PKG_LICENSE_FILE", ""),
            ("CARGO_PKG_NAME", "xtask"),
            ("CARGO_PKG_README", ""),
            ("CARGO_PKG_REPOSITORY", ""),
            ("CARGO_PKG_RUST_VERSION", ""),
            ("CARGO_PKG_VERSION", "0.95.0"),
            ("CARGO_PKG_VERSION_MAJOR", "0"),
            ("CARGO_PKG_VERSION_MINOR", "95"),
            ("CARGO_PKG_VERSION_PATCH", "0"),
            ("CARGO_PKG_VERSION_PRE", ""),
            ("TARGO_PKG_NAME", "xtask"),
            ("TRUST_TARGO_FRONTEND", "1"),
        ];
        assert_eq!(
            build_env(run_env.iter().map(|(k, v)| ((*k).into(), (*v).into()))),
            "none"
        );
    }

    /// A test binary the ceiling killed is never inherited, whichever test
    /// hung — the hung test's name sat only in the ceiling's note, so a new
    /// hang hashed as main's old one.
    #[test]
    fn a_hung_test_binary_is_never_inherited() {
        let killed = |ok: &str, hung: &str| {
            format!(
                "     Running tests/probe.rs (target/debug/deps/probe-1a2b3c4d5e6f7a8b)\n\n\
                 running 3 tests\ntest {ok} ... ok\ntest {hung} has been running for over 60 \
                 seconds\n{CEILING_KILL} — child killed after 10800.2s, over the 10800.0s \
                 wall-clock ceiling\n\x20 still running when killed:\n\x20   {hung}\n\
                 error: test failed, to rerun pass `-p x --test probe`\n\nCaused by:\n  process \
                 didn't exit successfully: `probe-1a2b3c4d5e6f7a8b` (killed by a signal)\n"
            )
        };
        let label = "targo test --workspace --tests";
        let main = test_findings(label, &killed("b_ok", "a_hangs_on_main"));
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].opaque, Some(HUNG));
        let branch = test_findings(label, &killed("a_hangs_on_main", "b_new_hang"));
        assert_eq!(
            judge(&branch[0], &listing(&main[0])),
            Disposition::New(NewWhy::Opaque(HUNG))
        );
        // Any row the ceiling killed, not only a test child's.
        assert_eq!(
            row_finding("tools/paint_guard.sh", &killed("x", "y")).opaque,
            Some(HUNG)
        );
    }

    /// EVERY ARM OF THE JUDGEMENT: not on main, on main failing differently,
    /// opaque, inherited inside the cap, and past it — at the boundary, where
    /// exactly the cap is still inherited and one second more is not.
    #[test]
    fn a_finding_is_inherited_only_on_the_same_id_and_hash_inside_the_cap() {
        let recent = NOW - 3600;
        let b = base(vec![
            failure("t1", "1111111111111111", recent),
            failure("t2", "2222222222222222", NOW - INHERITED_CAP_SECS),
            failure("t3", "3333333333333333", NOW - INHERITED_CAP_SECS - 1),
        ]);
        let since = |when| Since {
            commit: "b".repeat(40),
            when,
        };
        assert_eq!(
            judge(&finding("t1", "1111111111111111"), &b),
            Disposition::Inherited(since(recent))
        );
        assert_eq!(
            judge(&finding("t1", "9999999999999999"), &b),
            Disposition::New(NewWhy::FailsDifferently)
        );
        assert_eq!(
            judge(&finding("t9", "1111111111111111"), &b),
            Disposition::New(NewWhy::NotOnMain)
        );
        let refused = Finding {
            opaque: Some(REFUSED),
            ..finding("t1", "1111111111111111")
        };
        assert_eq!(
            judge(&refused, &b),
            Disposition::New(NewWhy::Opaque(REFUSED))
        );
        assert_eq!(
            judge(&finding("t2", "2222222222222222"), &b),
            Disposition::Inherited(since(NOW - INHERITED_CAP_SECS)),
            "exactly the cap is still inside it"
        );
        assert_eq!(
            judge(&finding("t3", "3333333333333333"), &b),
            Disposition::Expired(since(NOW - INHERITED_CAP_SECS - 1))
        );
        // A since a little ahead (another machine's clock) is inside the
        // slack; one further ahead has no age, and is never inherited.
        let ahead = base(vec![failure("t1", "1111111111111111", NOW + 99)]);
        assert!(judge(&finding("t1", "1111111111111111"), &ahead).excused());
        let far = base(vec![failure(
            "t1",
            "1111111111111111",
            NOW + CLOCK_SLACK_SECS + 1,
        )]);
        assert_eq!(
            judge(&finding("t1", "1111111111111111"), &far),
            Disposition::New(NewWhy::Opaque(FUTURE))
        );
    }

    /// A ROW IS EXCUSED only when it has itemized failures and every one is
    /// inherited; one new failure beside inherited ones keeps it red, and a
    /// row a caller named without findings never is.
    #[test]
    fn a_row_is_excused_only_when_every_item_is_inherited() {
        let mut t = Tally::default();
        t.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "targo test",
            &[
                finding("t1", "1111111111111111"),
                finding("t2", "2222222222222222"),
            ],
        );
        t.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "tippy",
            &[finding("tippy", "3333333333333333")],
        );
        t.gate_failures.push("by hand".into());
        let b = base(vec![
            failure("t1", "1111111111111111", NOW),
            failure("tippy", "3333333333333333", NOW),
        ]);
        let judged = judge_tally(&t, &b);
        assert!(!row_excused(&judged, 0), "t2 is new");
        assert!(row_excused(&judged, 1));
        assert!(!row_excused(&judged, 2), "no findings, never excused");
        let both = base(vec![
            failure("t1", "1111111111111111", NOW),
            failure("t2", "2222222222222222", NOW),
        ]);
        assert!(row_excused(&judge_tally(&t, &both), 0));
        // The receipt's `inherited` lines: every excused finding, even in a
        // row that stays red for another one.
        assert_eq!(inherited_ids(&t, &b), ["t1", "tippy"]);
    }

    /// SINCE IS CARRIED while main stays red on the id — whatever it said, so
    /// a red whose message alternates keeps its age (2026-09-27) — and starts
    /// at this run otherwise; expired ones keep their age, so the cap keeps
    /// counting; a since from the future is never carried.
    #[test]
    fn since_is_carried_from_the_base_while_main_stays_red_on_the_id() {
        let mut t = Tally::default();
        for (id, hash) in [
            ("t1", "1111111111111111"),
            ("t2", "2222222222222222"),
            ("t3", "3333333333333333"),
            ("t4", "4444444444444444"),
            ("t5", "5555555555555555"),
        ] {
            t.record_with(
                crate::Outcome::Fail(crate::Severity::GateFailed),
                id,
                &[finding(id, hash)],
            );
        }
        let old = NOW - 10 * INHERITED_CAP_SECS;
        let b = base(vec![
            failure("t1", "1111111111111111", NOW - 60),
            failure("t2", "0000000000000000", NOW - 60),
            failure("t3", "3333333333333333", old),
            failure("t4", "0000000000000000", NOW - 30),
            failure("t4", "4444444444444444", NOW - 90),
            failure("t5", "5555555555555555", NOW + CLOCK_SLACK_SECS + 1),
        ]);
        let head = "c".repeat(40);
        let got = recorded_failures(&t, Some(&b), &head, NOW);
        let since: Vec<(&str, u64)> = got
            .iter()
            .map(|f| (f.since.as_str(), f.since_when))
            .collect();
        let bb = "b".repeat(40);
        assert_eq!(
            since,
            [
                (bb.as_str(), NOW - 60),
                (bb.as_str(), NOW - 60),
                (bb.as_str(), old),
                (bb.as_str(), NOW - 90),
                (head.as_str(), NOW),
            ]
        );
        let alone = recorded_failures(&t, None, &head, NOW);
        assert!(alone.iter().all(|f| f.since == head && f.since_when == NOW));
    }

    /// A TIMING-SHAPED FAILURE IS NEVER INHERITED (2026-09-27, second
    /// review): main listing the same id with the same hash counts for
    /// nothing when the failure reads as a clock running out or printed one
    /// it read — a real deadlock gives up with main's flake's words. Not on
    /// main at all, it is simply not on main.
    #[test]
    fn a_timing_shaped_failure_main_lists_is_still_new() {
        let timed = Finding {
            timing: Some("timed out"),
            ..finding("t1", "1111111111111111")
        };
        let b = base(vec![failure("t1", "1111111111111111", NOW - 60)]);
        assert_eq!(
            judge(&timed, &b),
            Disposition::New(NewWhy::Opaque(TIMING_SHAPED))
        );
        assert_eq!(
            judge(
                &Finding {
                    id: "t9".into(),
                    ..timed.clone()
                },
                &b
            ),
            Disposition::New(NewWhy::NotOnMain)
        );
        assert!(judge(&finding("t1", "1111111111111111"), &b).excused());
    }

    /// THE CLOCK RUNS ON THROUGH A RUN THAT COULD NOT SEE THE RED (2026-09-27,
    /// second review). A run whose test log was unaccounted itemizes none of
    /// that stage's reds, so a red main had for days vanished from its list
    /// and the next run to itemize it again started its clock at NOW. Such a
    /// run now carries every red of its since chain it did not itemize as
    /// HIDDEN, with its since, and the next run carries the since from there.
    /// A run that saw everything carries nothing hidden: a red it did not
    /// find is gone, and its clock stops.
    #[test]
    fn a_red_a_run_could_not_see_keeps_its_since() {
        let x = failure("x", "1111111111111111", NOW - 3 * 86_400);
        let chain = base(vec![x.clone(), failure("y", "2222222222222222", NOW - 60)]);
        // Unaccounted: the test row is one opaque finding under its label.
        let mut blind = Tally::default();
        blind.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "test",
            &[Finding {
                opaque: Some(UNACCOUNTED),
                ..finding("test", "3333333333333333")
            }],
        );
        blind.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "y",
            &[finding("y", "2222222222222222")],
        );
        assert_eq!(
            hidden_failures(&blind, Some(&chain)),
            std::slice::from_ref(&x)
        );
        // A skipped stage could have hidden it too.
        let mut skipping = Tally::default();
        skipping.record(crate::Outcome::Skip, "gui smoke");
        assert_eq!(hidden_failures(&skipping, Some(&chain)).len(), 2);
        // Saw everything, and x was not red: nothing is hidden.
        let mut clear = Tally::default();
        clear.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "y",
            &[finding("y", "2222222222222222")],
        );
        assert!(hidden_failures(&clear, Some(&chain)).is_empty());
        assert!(hidden_failures(&blind, None).is_empty());
        // Carried through a receipt's `hidden` lines into the next run's
        // since chain, never into what a run is judged against.
        let found = Found {
            commit: "m".repeat(40),
            source: "note mmmmmmmmm".into(),
            receipt: Receipt {
                failures: Some(recorded_failures(&blind, Some(&chain), "h", NOW)),
                hidden: hidden_failures(&blind, Some(&chain)),
                ..Receipt::default()
            },
            clock_floor: None,
            nearest: None,
        };
        let parsed = Receipt::parse(&found.receipt.render()).expect("it parses");
        assert_eq!(parsed.hidden, std::slice::from_ref(&x));
        assert!(found.reds(NOW).failures.iter().all(|f| f.id != "x"));
        let mut again = Tally::default();
        again.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            "x",
            &[finding("x", "1111111111111111")],
        );
        let carried = recorded_failures(&again, Some(&found.chain(NOW)), "n", NOW);
        assert_eq!(carried[0].since_when, x.since_when);
        assert!(matches!(
            judge(&finding("x", "1111111111111111"), &base(carried)),
            Disposition::Expired(_)
        ));
    }

    /// ONLY A WHOLE-TREE MERGE-CONTRACT RECEIPT THAT LISTS ITS REDS SERVES AS
    /// A BASE. A `--full` receipt ran the contract and serves; a `--measure`
    /// one (2026-09-26) ran none of it, so its list is not main's reds for the
    /// contract — even whole-tree, listed and green; a narrowed one looked at
    /// too little; one from before the list says nothing about what was red.
    #[test]
    fn only_a_whole_tree_merge_receipt_with_a_failure_list_serves_as_a_base() {
        let base = || Receipt {
            head: "a".repeat(40),
            mode: "fast".into(),
            scope: "workspace".into(),
            verdict: "FAIL".into(),
            failures: Some(Vec::new()),
            ..Receipt::default()
        };
        assert_eq!(usable(&base()), Ok(()));
        assert_eq!(
            usable(&Receipt {
                mode: "full".into(),
                measured: Some(false),
                ..base()
            }),
            Ok(())
        );
        assert_eq!(
            usable(&Receipt {
                mode: "measure".into(),
                verdict: "PASS".into(),
                measured: Some(true),
                ..base()
            }),
            Err("its run was the MEASURE tier, not the merge contract")
        );
        assert_eq!(
            usable(&Receipt {
                scope: "crate:aterm-grid".into(),
                ..base()
            }),
            Err("its run was narrowed")
        );
        assert_eq!(
            usable(&Receipt {
                failures: None,
                ..base()
            }),
            Err(
                "it lists no failures (it predates the list, or its source or compiler moved \
                 under it)"
            )
        );
    }

    /// THE BOUND HOLDS THROUGH A REMOTE HELPER (2026-09-28, review): git
    /// reaches a remote through a child of its own that inherits its pipes —
    /// `git-remote-https` for an https origin; here the `ext::` transport's
    /// `sh`, the same shape without a network. It sleeps far past the bound.
    /// The call must answer at the bound (plus the drain grace), not when the
    /// helper exits, and the helper must be dead: before the fix a 40 s helper
    /// held a 10 s bound for 40.06 s.
    #[cfg(unix)]
    #[test]
    fn a_bounded_git_call_ends_at_its_bound_and_takes_its_helper_with_it() {
        let root = crate::mktemp_dir("atv-bounded-helper").expect("mktemp");
        let marker = format!("47.{}", std::process::id());
        let url = format!("ext::sh -c sleep% {marker}");
        let bound = Duration::from_secs(2);
        let started = Instant::now();
        let got = git_bounded(
            &root,
            &["-c", "protocol.ext.allow=always", "ls-remote", &url],
            bound,
        );
        let took = started.elapsed();
        assert!(
            took < bound + DRAIN_GRACE + Duration::from_secs(3),
            "the bound did not hold: {took:?} ({got:?})"
        );
        let e = got.expect_err("a call that overran is an error");
        assert!(e.contains("git ls-remote did not finish within 2 s"), "{e}");
        // The helper died with git's group. (A killed process can take a
        // moment to leave the table.)
        let alive = || {
            Command::new("pgrep")
                .args(["-f", &format!("sleep {marker}")])
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        let gone_by = Instant::now() + Duration::from_secs(3);
        while alive() && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "the helper outlived the bound");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The fast path is untouched: a call that finishes answers its stdout, and
    /// one that fails answers git's last line of stderr.
    #[test]
    fn a_bounded_git_call_that_finishes_answers_its_output_or_its_reason() {
        let root = crate::mktemp_dir("atv-bounded-fast").expect("mktemp");
        assert_eq!(
            git_bounded(&root, &["--version"], Duration::from_secs(10))
                .map(|s| s.starts_with("git version")),
            Ok(true)
        );
        let e = git_bounded(
            &root,
            &["ls-remote", &root.join("no-such-repo").to_string_lossy()],
            Duration::from_secs(10),
        )
        .expect_err("no such repository");
        assert!(!e.contains("did not finish"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a converge pass cost, for the aterm.log line that follows its ledger line ([`line`]):
//! the readings the pass takes of its own process as it starts and as it ends ([`Spent`]), the
//! fields the line makes of their difference and of the rows the engine timed, and the note of
//! the pass's start. Numbers, row ids and note words only: nothing a row read is written here.

use super::{Due, Trigger};
use aterm_converge::{ItemReport, Outcome};

/// One reading of what the pass's process has spent so far ([`super::Ports::spent`]). A pass
/// reads it once it holds its lock and again as it ends; its line carries the difference
/// ([`Spent::since`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    /// Milliseconds on a monotonic clock.
    pub mono_ms: u64,
    /// The programs this process started through its port (`crate::port::Live::runs`).
    pub spawns: u64,
    /// Those of [`Self::spawns`] named `git`.
    pub gits: u64,
    /// Those of [`Self::gits`] by their subcommand (`crate::port::Live::git_by`).
    pub git_by: crate::port::GitBy,
    /// The CPU, user and system, of every child this process has waited for and of the
    /// descendants those children waited for, in milliseconds. A child still running, or never
    /// waited for, is not in it.
    pub child_cpu_ms: u64,
    /// This process's own CPU, user and system, in milliseconds.
    pub cpu_ms: u64,
}

impl Spent {
    /// What was spent from `began` to this later reading.
    #[must_use]
    pub const fn since(self, began: Self) -> Self {
        Self {
            mono_ms: self.mono_ms.saturating_sub(began.mono_ms),
            spawns: self.spawns.saturating_sub(began.spawns),
            gits: self.gits.saturating_sub(began.gits),
            git_by: self.git_by.since(began.git_by),
            child_cpu_ms: self.child_cpu_ms.saturating_sub(began.child_cpu_ms),
            cpu_ms: self.cpu_ms.saturating_sub(began.cpu_ms),
        }
    }
}

/// This process's own CPU so far and its waited children's ([`Spent::cpu_ms`],
/// [`Spent::child_cpu_ms`]), each in milliseconds and 0 where `getrusage` did not answer or
/// this target's `struct rusage` is not the one declared here (64-bit Darwin and Linux are).
/// `aterm`'s converge runner reads it in its [`super::Ports::spent`], and a law there
/// (`converge::tests::spent`) tells the two apart: a child the runner's port waited for shows
/// in the second and not in the first.
#[must_use]
pub fn cpu_ms() -> (u64, u64) {
    /// `getrusage`'s `who` for the calling process, and for the children it has waited for.
    const RUSAGE_SELF: i32 = 0;
    const RUSAGE_CHILDREN: i32 = -1;
    (usage_ms(RUSAGE_SELF), usage_ms(RUSAGE_CHILDREN))
}

/// `getrusage(who)`'s user and system time, in milliseconds.
#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
fn usage_ms(who: i32) -> u64 {
    /// `struct timeval` on a 64-bit target: 16 bytes on both, Darwin's microseconds an `int`
    /// before four bytes of padding and Linux's a `long`.
    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        sec: i64,
        #[cfg(target_vendor = "apple")]
        usec: i32,
        #[cfg(target_vendor = "apple")]
        pad: i32,
        #[cfg(target_os = "linux")]
        usec: i64,
    }
    /// `struct rusage` on a 64-bit target: `ru_utime`, `ru_stime`, then fourteen `long`s.
    #[repr(C)]
    #[derive(Default)]
    struct Usage {
        user: Timeval,
        system: Timeval,
        rest: [i64; 14],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Usage) -> i32;
    }
    let mut usage = Usage::default();
    // SAFETY: `usage` is writable for one `struct rusage` in this target's layout, and the call
    // keeps no pointer to it.
    if unsafe { getrusage(who, &raw mut usage) } != 0 {
        return 0;
    }
    let ms = |time: &Timeval| {
        u64::try_from(time.sec)
            .unwrap_or(0)
            .saturating_mul(1000)
            .saturating_add(u64::try_from(time.usec).unwrap_or(0) / 1000)
    };
    ms(&usage.user).saturating_add(ms(&usage.system))
}

/// No `struct rusage` is declared for this target: nothing is read.
#[cfg(not(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
)))]
const fn usage_ms(_who: i32) -> u64 {
    0
}

/// A row is named in `slow=` once the engine spent this long on it.
pub(super) const SLOW_MS: u64 = 250;
/// The most rows `slow=` names; the rest are summed.
pub(super) const SLOW_ROWS: usize = 8;

/// `row`'s id as the line writes it: the id where it is made of `a-z`, `0-9`, `.`, `_` and `-`
/// alone (every converge row's is), else `#<index>`, so no id can carry a name or a space into
/// the fields.
fn row_word(index: usize, row: &ItemReport) -> String {
    let plain = !row.id.is_empty()
        && row.id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if plain {
        row.id.clone()
    } else {
        format!("#{index}")
    }
}

/// A pass's cost line, written to aterm.log right after its ledger line ([`super::ledger`]):
/// `converge cost build=<build> trigger=<trigger>/<due> ms=<n> spawns=<n> git=<n>
/// child_cpu_ms=<n> cpu_ms=<n> note=<note> changed_rows=<id>,… slow=<id>:<ms>,…
/// git_by=<subcommand>:<n>,…`. Its head is its own, so a tally of `converge build=` lines counts
/// passes and not this line. `note` is the start's note, `-` where its starter gave none.
/// `changed_rows` is every row that ended [`Outcome::Changed`], in run order. `slow` is each row
/// the engine spent at least [`SLOW_MS`] on, longest first (run order between equals), at most
/// [`SLOW_ROWS`] of them, then `rest:<ms>`, the time of every row not named, where that is not 0.
/// `git_by` is the pass's gits by subcommand ([`crate::port::GitBy`]), the most first, so the
/// `git=` of a pass that started thousands says which reads they were. An empty list is `-`.
///
/// The line is one log record, which the log cuts past [`aterm_log::MAX_RECORD_BYTES`], so the
/// lists are cut to fit it instead, from their ends: `git_by` gives up its least counted
/// subcommands first, their sum a last entry `rest:<n>`; where it names none, `changed_rows`
/// gives up its last ids, saying how many it left out as a last entry `+<n>`; where naming none
/// of them still does not fit, `slow` gives up its shortest rows to `rest`. The head, the numbers
/// and the note are never cut: at their largest (each number 20 digits, the trigger and due words
/// the longest, a note of 240 bytes) they are 438 bytes and the build's name, and where they
/// leave no room for the lists cut to their least, the log cuts the line.
pub(super) fn line(
    build: &str,
    trigger: Trigger,
    due: Due,
    spent: Spent,
    note: Option<&str>,
    rows: &[ItemReport],
) -> String {
    let head = format!(
        "converge cost build={build} trigger={}/{} ms={} spawns={} git={} child_cpu_ms={} \
         cpu_ms={} note={}",
        trigger.word(),
        due.word(),
        spent.mono_ms,
        spent.spawns,
        spent.gits,
        spent.child_cpu_ms,
        spent.cpu_ms,
        note.unwrap_or("-")
    );
    let changed: Vec<String> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.outcome == Outcome::Changed)
        .map(|(index, row)| row_word(index, row))
        .collect();
    let mut timed: Vec<(usize, u64)> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (index, row.elapsed_ms.unwrap_or(0)))
        .collect();
    let total = timed
        .iter()
        .fold(0_u64, |sum, (_, ms)| sum.saturating_add(*ms));
    timed.retain(|(_, ms)| *ms >= SLOW_MS);
    // Stable: rows that took the same time keep their run order.
    timed.sort_by_key(|&(_, ms)| std::cmp::Reverse(ms));
    timed.truncate(SLOW_ROWS);
    let mut slow: Vec<(String, u64)> = timed
        .iter()
        .map(|&(index, ms)| (row_word(index, &rows[index]), ms))
        .collect();
    let gits = spent.git_by.counted();
    let mut gits_named = gits.len();
    let mut named = changed.len();
    loop {
        let rest = total.saturating_sub(
            slow.iter()
                .fold(0_u64, |sum, (_, ms)| sum.saturating_add(*ms)),
        );
        let mut changed_words = changed[..named].to_vec();
        if named < changed.len() {
            changed_words.push(format!("+{}", changed.len() - named));
        }
        let mut slow_words: Vec<String> =
            slow.iter().map(|(id, ms)| format!("{id}:{ms}")).collect();
        if rest > 0 {
            slow_words.push(format!("rest:{rest}"));
        }
        let mut git_words: Vec<String> = gits[..gits_named]
            .iter()
            .map(|(word, count)| format!("{word}:{count}"))
            .collect();
        let git_rest = gits[gits_named..]
            .iter()
            .fold(0_u64, |sum, (_, count)| sum.saturating_add(*count));
        if git_rest > 0 {
            git_words.push(format!("rest:{git_rest}"));
        }
        let line = format!(
            "{head} changed_rows={} slow={} git_by={}",
            list(&changed_words),
            list(&slow_words),
            list(&git_words)
        );
        if line.len() <= aterm_log::MAX_RECORD_BYTES {
            return line;
        }
        if gits_named > 0 {
            gits_named -= 1;
        } else if named > 0 {
            named -= 1;
        } else if slow.pop().is_none() {
            // No room even for the lists cut to their least: the log cuts the line.
            return line;
        }
    }
}

/// `words` joined by `,`, or `-` where there are none.
fn list(words: &[String]) -> String {
    if words.is_empty() {
        "-".to_owned()
    } else {
        words.join(",")
    }
}

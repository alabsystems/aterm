// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm.log line of a memory starvation episode's edge ([`aterm_vitals::starving`]),
//! written by the vitals probe thread as it reads the edge, before it hands the reading over,
//! then ordered to the disk ([`Journal::note`]): a starving machine stalls the process that writes
//! about it (on 2026-11-15 the vitals sweep stalled at 23:12:22 and the GUI process's last
//! aterm.log line was at 23:14:12, nine minutes before the kernel panicked). The order waits at
//! most [`SYNC_WAIT`] for the log file, which the log's writer thread may hold mid-append; one
//! that did not get it is owed, and the thread tries again at each reading after until it gets
//! it. The Messages record of the same edge (`message_reporters::starving_record`) follows on the
//! main thread; the next run's recovery census reads this line (`recovery_census`), so its
//! `prev_pressure=` names the episode even where the record never reached the Messages log.
//!
//! The line carries words and numbers only (aterm_log::words): a holder's name is its word or
//! `?`, its tab its key.

use std::time::Duration;

use aterm_vitals::starving::{Edge, Episode, Holder, Look};
use aterm_vitals::{RowScope, Snapshot};

/// The head of the line an opening writes; the census reads it as the episode's start.
pub(crate) const OPENED_LINE: &str = "memory starving: opened";
/// The head of the line an end writes; the census reads it as the episode's end.
pub(crate) const ENDED_LINE: &str = "memory starving: ended";

/// How long the probe thread waits for the log file when another thread is mid-append: the
/// log's exit bound. The flush that follows has no bound of its own; it is the kernel's.
pub(crate) const SYNC_WAIT: Duration = crate::logging::LOG_FLUSH_EXIT_WAIT;

/// The target an edge's line is logged under.
pub(crate) const TARGET: &str = module_path!();

const MIB: f64 = 1_048_576.0;

/// Where an edge's line goes: aterm.log's file logger ([`crate::logging::starving_sink`]), or a
/// test's stand-in.
pub(crate) trait Sink {
    /// Writes `line` at `level` (warn for an opening, info for an end).
    fn write(&self, level: aterm_log::Level, line: &str);
    /// Appends what was written and orders the file to its device, waiting at most
    /// [`SYNC_WAIT`] for the file; whether it was ordered.
    fn sync(&self) -> bool;
}

/// The probe thread's journal of starvation edges: what it still owes the disk.
#[derive(Debug, Default)]
pub(crate) struct Journal {
    /// An edge's line was written and not yet ordered to the device.
    owed: bool,
}

impl Journal {
    /// Writes the line of each starvation edge `snap` raised to `sink`, then orders the file to
    /// its device where it wrote one or still owes an earlier one, keeping the debt where the
    /// file was not to be had. Returns how many edges it wrote.
    pub(crate) fn note(&mut self, snap: &Snapshot, sink: &dyn Sink) -> usize {
        let mut n = 0;
        for e in &snap.events {
            if let aterm_vitals::events::EventKind::Starving(edge) = &e.kind {
                let level = match edge.as_ref() {
                    Edge::Opened(_) => aterm_log::Level::Warn,
                    Edge::Ended(_) => aterm_log::Level::Info,
                };
                sink.write(level, &line(edge));
                n += 1;
            }
        }
        if n > 0 || self.owed {
            self.owed = !sink.sync();
        }
        n
    }

    /// Whether a line is written and not yet ordered to the device.
    #[cfg(test)]
    pub(crate) const fn owed(&self) -> bool {
        self.owed
    }
}

/// The edge's line: its head ([`OPENED_LINE`], [`ENDED_LINE`]), the rule, what the opening
/// reading read (or how long the episode lasted and the worst it read), and its largest holders
/// as the newest sweep named them, `pid:name:MiB:where`.
pub(crate) fn line(edge: &Edge) -> String {
    let mut t = match edge {
        Edge::Opened(e) => format!(
            "{OPENED_LINE} why={}{}",
            e.why.word(),
            look_words(&e.at_open)
        ),
        Edge::Ended(e) => format!(
            "{ENDED_LINE} why={} for_ms={} worst_push_mibps={} worst_refault_mibps={}{}{}",
            e.why.word(),
            e.for_ms().unwrap_or(0),
            mib(e.worst.push_bps),
            mib(e.worst.refault_bps),
            opt("least_headroom_mib", e.worst.headroom.map(|b| b >> 20)),
            opt("worst_wall_permille", e.worst.wall_permille.map(u64::from)),
        ),
    };
    let episode = match edge {
        Edge::Opened(e) | Edge::Ended(e) => e,
    };
    t.push_str(&holder_words(episode));
    t
}

fn look_words(l: &Look) -> String {
    format!(
        " push_mibps={} refault_mibps={}{}{} ram_mib={}{}{} pressure={}",
        l.push_bps
            .map_or_else(|| "-".to_owned(), |b| mib(b).to_string()),
        l.refault_bps
            .map_or_else(|| "-".to_owned(), |b| mib(b).to_string()),
        opt("anon_back_mibps", l.anon_back_bps.map(mib)),
        opt("headroom_mib", l.headroom.map(|b| b >> 20)),
        l.ram >> 20,
        opt("wall_permille", l.wall_permille.map(u64::from)),
        opt("kept_permille", l.kept_permille.map(u64::from)),
        l.pressure.word(),
    )
}

fn holder_words(e: &Episode) -> String {
    if e.holders.is_empty() {
        return " largest=none".to_owned();
    }
    let held: Vec<String> = e.holders.iter().map(holder_word).collect();
    let mut t = format!(" largest={}", held.join(","));
    if let Some(age) = e.holders_age_ms {
        t.push_str(&format!(" largest_age_ms={age}"));
    }
    t
}

fn holder_word(h: &Holder) -> String {
    let at = match h.scope {
        RowScope::Tab(k) => format!("tab{k}"),
        scope => scope.word().to_owned(),
    };
    format!(
        "{}:{}:{}:{at}",
        h.pid,
        aterm_log::words::word(&h.name),
        h.bytes >> 20
    )
}

fn mib(bps: f64) -> u64 {
    (bps.max(0.0) / MIB).round() as u64
}

fn opt(key: &str, v: Option<u64>) -> String {
    v.map_or_else(String::new, |v| format!(" {key}={v}"))
}

#[cfg(test)]
#[path = "memory_starving_tests.rs"]
mod tests;

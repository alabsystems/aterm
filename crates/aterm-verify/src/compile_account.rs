// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMPILE ACCOUNT (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): what one
//! compile call did, as one plain-text line and one `compile-finished` event with the same
//! numbers. Its units come from Cargo's `compiler-artifact` messages (`fresh`), its output bytes
//! from one metadata read of each file an artifact it did not report fresh names (never a walk),
//! and its CPU, wait for a core and peak from its tree's usage ([`crate::exec::TreeUsage`]). It
//! reads no verdict: a call is decided as before, and this is what it cost.
//!
//! A release's slices count their Cargo messages with the same reading ([`Artifacts::book`],
//! [`output_bytes`]), so a unit and a written byte count the same in a release's compile line
//! as in a run's.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::Value;

use crate::compiler_message::UnitMessage;

/// Cargo's compile units of one call, by what its `compiler-artifact` messages say of their
/// freshness: `fresh: true`, `fresh: false`, and neither (no `fresh`, a `fresh` that is not a
/// boolean, or an artifact message with duplicated fields), which is never counted as either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Units {
    pub fresh: u64,
    pub compiled: u64,
    pub unknown: u64,
}

/// What one call's compiler stdout reports: its [`Units`], and the files each artifact it did not
/// report fresh names (`filenames`), in order, repeats kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Artifacts {
    pub units: Units,
    pub written: Vec<PathBuf>,
}

impl Artifacts {
    /// Read one line of a `--message-format=json…` stdout into these artifacts. A line that is
    /// not a compiler message is not read; an artifact message whose fields repeat is an unknown
    /// unit naming no file, and a repeated dispatch message (`compilation-unit-dispatched`) is no
    /// unit.
    pub fn book(&mut self, line: &str) {
        if !line.starts_with('{') {
            return;
        }
        match aterm_json::from_str::<UnitMessage>(line) {
            Ok(UnitMessage::Unit(value)) => {
                if value.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
                    return;
                }
                match value.get("fresh").and_then(Value::as_bool) {
                    Some(true) => {
                        self.units.fresh += 1;
                        return;
                    }
                    Some(false) => self.units.compiled += 1,
                    None => self.units.unknown += 1,
                }
                self.written.extend(
                    value
                        .get("filenames")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from),
                );
            }
            Ok(UnitMessage::Ambiguous {
                artifact_reason_seen: true,
                ..
            }) => {
                self.units.unknown += 1;
            }
            Ok(_) | Err(_) => {}
        }
    }
}

/// Read a `--message-format=json…` stdout ([`Artifacts`]), each of its lines as
/// [`Artifacts::book`] reads it.
#[must_use]
pub fn read(stdout: &str) -> Artifacts {
    let mut out = Artifacts::default();
    for line in stdout.lines() {
        out.book(line);
    }
    out
}

/// The bytes the regular files at `files` hold: one [`std::fs::symlink_metadata`] of each, a
/// path read once and, on Unix, a file (device and inode) counted once, so an artifact hard-linked
/// beside its `deps/` copy is not counted twice. A directory (a `.dSYM`), a symlink and a path
/// that is gone add nothing, and nothing is walked.
#[must_use]
pub fn output_bytes(files: &[PathBuf]) -> u64 {
    let mut paths = std::collections::BTreeSet::<&Path>::new();
    #[cfg(unix)]
    let mut inodes = std::collections::BTreeSet::<(u64, u64)>::new();
    let mut total = 0_u64;
    for file in files {
        if !paths.insert(file) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(file) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if !inodes.insert((meta.dev(), meta.ino())) {
                continue;
            }
        }
        total = total.saturating_add(meta.len());
    }
    total
}

/// `stdout` without Cargo's machine messages: every line but one that begins `{"reason":` or reads
/// as a unit message, so what a build script or a tool printed for a person stays.
#[must_use]
pub fn human(stdout: &str) -> String {
    stdout
        .split_inclusive('\n')
        .filter(|line| {
            let t = line.trim_end_matches(['\n', '\r']);
            !(t.starts_with("{\"reason\":")
                || (t.starts_with('{')
                    && aterm_json::from_str::<UnitMessage>(t)
                        .is_ok_and(|m| !matches!(m, UnitMessage::Other))))
        })
        .collect()
}

/// The flag that puts Cargo's machine messages on a compile call's stdout, its diagnostics
/// rendered for a person on its stderr.
pub const MESSAGE_FORMAT: &str = "--message-format=json-render-diagnostics";

/// `cmd` with [`MESSAGE_FORMAT`] before its first `--` (the arguments Cargo hands on), or last;
/// unchanged where a `--message-format` is already among Cargo's own arguments.
#[must_use]
pub fn with_message_format(mut cmd: crate::exec::Cmd) -> crate::exec::Cmd {
    let end = cmd
        .args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(cmd.args.len());
    if !cmd.args[..end]
        .iter()
        .any(|arg| arg.as_encoded_bytes().starts_with(b"--message-format"))
    {
        cmd.args.insert(end, MESSAGE_FORMAT.into());
    }
    cmd
}

/// ONE COMPILE CALL'S ACCOUNT: the step that made it and the call, by the labels its report gives
/// them; its [`Units`] (`None`: its stdout was not read, or the call does not report them); its
/// tree's usage (`None`: unread, [`crate::exec::Run::usage`]); its wall; and the bytes of the
/// files it wrote ([`output_bytes`] of [`Artifacts::written`]; `None` where its units are).
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub step: String,
    pub call: String,
    pub units: Option<Units>,
    pub usage: Option<crate::exec::TreeUsage>,
    pub wall: Duration,
    pub output_bytes: Option<u64>,
    /// How many of its compilers ran at once, where its stage's meter read them
    /// ([`Account::overlapping`]).
    pub overlap: Option<Overlap>,
}

/// HOW MANY OF A CALL'S COMPILERS RAN AT ONCE, from the spans its stage's reads gave them
/// ([`crate::memory::UnitSpan`]), over the call's wall: the average (their spans' lengths summed
/// over the wall), the least count it ran at or under for 95% of the wall, and the most at one
/// read; and of the units waiting for memory in their admission wrapper, the average and the
/// most at once, with their waits summed. Each date is a read, half a second apart at most: a
/// compiler read once or between two reads is missing, and each counted span is short by up to
/// a read on either end, so the compilers' figures bound the truth from below; a wait runs from
/// its wrapper's first read to its compiler's, each late by up to a read, so it is within one
/// read of the truth either way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlap {
    /// The compilers whose spans were read.
    pub timed: usize,
    pub avg: f64,
    pub p95: u32,
    pub max: u32,
    pub waiting_avg: f64,
    pub waiting_max: u32,
    /// The units' waits for memory summed, seconds.
    pub waited_s: f64,
}

/// One count's share of a window: how long the window ran at each count of open spans, the most
/// open at one instant, and the open spans' lengths summed, ms ([`Overlap::of`]).
fn levels(
    spans: impl Iterator<Item = (u64, u64)>,
    from: u64,
    to: u64,
) -> (Vec<(u32, u64)>, u32, u64) {
    let mut edges: Vec<(u64, i32)> = Vec::new();
    let mut summed = 0_u64;
    for (start, end) in spans {
        if end < start || end < from || start > to {
            continue;
        }
        let (start, end) = (start.max(from), end.min(to));
        summed += end - start;
        edges.push((start, 1));
        edges.push((end, -1));
    }
    // An opening sorts before a closing at the same instant, so a span one read saw counts.
    edges.sort_unstable_by_key(|&(at, step)| (at, -step));
    let (mut open, mut most, mut at) = (0_i32, 0_i32, from);
    let mut time = std::collections::BTreeMap::<u32, u64>::new();
    for (edge, step) in edges {
        *time.entry(open.unsigned_abs()).or_default() += edge - at;
        at = edge;
        open += step;
        most = most.max(open);
    }
    *time.entry(open.unsigned_abs()).or_default() += to - at;
    (time.into_iter().collect(), most.unsigned_abs(), summed)
}

impl Overlap {
    /// The overlap of `spans` over the window `from_ms..to_ms` (ms since the Unix epoch, on the
    /// clock that dated them: [`crate::memory::Meter::unix_ms`]), each span cut to it and one
    /// wholly outside it left out; `None` for a window of no length or no span.
    #[must_use]
    pub fn of(spans: &[crate::memory::UnitSpan], from_ms: u64, to_ms: u64) -> Option<Overlap> {
        if to_ms <= from_ms || spans.is_empty() {
            return None;
        }
        let wall = (to_ms - from_ms) as f64;
        let (time, max, summed) = levels(
            spans
                .iter()
                .map(|s| (s.started_at_unix_ms, s.ended_at_unix_ms)),
            from_ms,
            to_ms,
        );
        let mut covered = 0_u64;
        let p95 = time
            .iter()
            .find(|&&(_, ms)| {
                covered += ms;
                covered as f64 >= 0.95 * wall
            })
            .map_or(max, |&(count, _)| count);
        let waits = || {
            spans
                .iter()
                .filter_map(|s| s.queued_at_unix_ms.map(|q| (q, s.started_at_unix_ms)))
        };
        let (_, waiting_max, waited) = levels(waits(), from_ms, to_ms);
        let unclamped: u64 = waits().map(|(q, st)| st.saturating_sub(q)).sum();
        Some(Overlap {
            timed: spans.len(),
            avg: summed as f64 / wall,
            p95,
            max,
            waiting_avg: waited as f64 / wall,
            waiting_max,
            waited_s: unclamped as f64 / 1000.0,
        })
    }
}

/// `verify-event: {"schema":1,"event":"compile-unit-timed",…}` for each span of call `call`:
/// its unit by the name the memory book keys it by, its explicit backend output request
/// (`codegen_requested`, null when unread), its wait and its compile dated, its CPU and
/// its peak, and the reads' period, so a reader knows the dates' grain.
#[must_use]
pub fn span_events(call: &str, spans: &[crate::memory::UnitSpan]) -> Vec<String> {
    spans
        .iter()
        .map(|s| {
            format!(
                "verify-event: {}",
                crate::event_log::event_value(
                    crate::event_log::EventKind::CompileUnitTimed,
                    aterm_json::json!({
                          "call": call,
                        "package": s.unit.package, "unit": s.unit.kind,
                        "codegen_requested": s.codegen_requested,
                        "queued_at_unix_ms": s.queued_at_unix_ms,
                        "started_at_unix_ms": s.started_at_unix_ms,
                        "ended_at_unix_ms": s.ended_at_unix_ms,
                        "cpu_s": s.cpu_s, "peak_bytes": s.peak_bytes,
                        "read_every_ms": crate::exec::READ_USAGE_EVERY.as_millis(),
                    })
                )
            )
        })
        .collect()
}

impl Account {
    /// The account of a call whose compiler stdout was `stdout` (`None`: not read).
    #[must_use]
    pub fn of(
        step: &str,
        call: &str,
        stdout: Option<&str>,
        usage: Option<crate::exec::TreeUsage>,
        wall: Duration,
    ) -> Account {
        let read = stdout.map(read);
        Account {
            step: step.to_owned(),
            call: call.to_owned(),
            units: read.as_ref().map(|a| a.units),
            usage,
            wall,
            output_bytes: read.map(|a| output_bytes(&a.written)),
            overlap: None,
        }
    }

    /// This account with its compilers' overlap ([`Overlap`]).
    #[must_use]
    pub fn overlapping(mut self, overlap: Option<Overlap>) -> Account {
        self.overlap = overlap;
        self
    }

    /// `compile <step>/<call>: F fresh / C compiled, X CPU-s, Y starved-s, W wall, P peak, B out`;
    /// ` / U unknown` after the compiled count when some units were neither, and `unreported` or
    /// `unread` for what was not read, never 0.
    #[must_use]
    pub fn line(&self) -> String {
        let units = self.units.map_or_else(
            || "units unreported".to_owned(),
            |u| {
                let unknown = if u.unknown > 0 {
                    format!(" / {} unknown", u.unknown)
                } else {
                    String::new()
                };
                format!("{} fresh / {} compiled{unknown}", u.fresh, u.compiled)
            },
        );
        let (cpu, starved, peak) = self.usage.map_or_else(
            || {
                (
                    "CPU-s unread".to_owned(),
                    "starved-s unread".to_owned(),
                    "peak unread".to_owned(),
                )
            },
            |u| {
                (
                    format!("{:.1} CPU-s", u.cpu.as_secs_f64()),
                    format!("{:.1} starved-s", u.starved.as_secs_f64()),
                    format!("{} peak", aterm_capacity::size(u.peak_bytes)),
                )
            },
        );
        let out = self.output_bytes.map_or_else(
            || "out unread".to_owned(),
            |b| format!("{} out", aterm_capacity::size(b)),
        );
        let overlap = self.overlap.map_or_else(String::new, |o| {
            format!(
                "; compilers at once {:.1} avg, {} p95, {} most ({} read); waiting for memory \
                 {:.1} avg, {} most, {:.1} unit-s",
                o.avg, o.p95, o.max, o.timed, o.waiting_avg, o.waiting_max, o.waited_s
            )
        });
        format!(
            "compile {}/{}: {units}, {cpu}, {starved}, {} wall, {peak}, {out}{overlap}",
            self.step,
            self.call,
            crate::report::duration(self.wall.as_secs_f64())
        )
    }

    /// `verify-event: {"schema":1,"event":"compile-finished",…}`: the line's numbers, `null` for
    /// what was not read.
    #[must_use]
    pub fn event(&self) -> String {
        let secs = |pick: fn(&crate::exec::TreeUsage) -> Duration| {
            self.usage.as_ref().map(|u| pick(u).as_secs_f64())
        };
        format!(
            "verify-event: {}",
            crate::event_log::event_value(
                crate::event_log::EventKind::CompileFinished,
                aterm_json::json!({
                      "step": self.step, "call": self.call,
                    "units": self.units.map(|u| aterm_json::json!({
                        "fresh": u.fresh, "compiled": u.compiled, "unknown": u.unknown,
                    })),
                    "cpu_s": secs(|u| u.cpu),
                    "starved_s": secs(|u| u.starved),
                    "waited_s": secs(|u| u.waited),
                    "wall_s": self.wall.as_secs_f64(),
                    "peak_bytes": self.usage.map(|u| u.peak_bytes),
                    "output_bytes": self.output_bytes,
                    "compilers": self.overlap.map(|o| aterm_json::json!({
                        "timed": o.timed, "avg": o.avg, "p95": o.p95, "max": o.max,
                        "waiting_avg": o.waiting_avg, "waiting_max": o.waiting_max,
                        "waited_s": o.waited_s,
                    })),
                })
            )
        )
    }
}

#[cfg(test)]
#[path = "compile_account_tests.rs"]
mod tests;

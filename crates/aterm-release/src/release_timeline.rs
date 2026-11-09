// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where one release run's time and CPU went, read back from the records its steps left
//! (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): the release's `release-stage`
//! events in `transcript.log`, step 4's `qualify-branch` events there (each branch's start, its
//! staging push and its end, and the log it wrote), and in every step log the
//! `step-child-finished` record of each
//! child (`wait4`'s CPU and peak), verify's `stage-started`/`stage-finished`, the slices'
//! `build-child-finished`, `compile-finished`, the inputs seal's `inputs`, verify's `disk`
//! record and the publish step's `publish-line` arrivals. An image continued through the
//! in-place exec counts the preceding image's seal calls and walks with its own. The legacy
//! `handed` event has the same read-time meaning for a cut already in flight at the B1 boundary.
//!
//! Planning data only, never a verdict. What no record says is `null`, never `0`: an older
//! run's logs carry none of the newer records.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The longest record line read; a longer line is skipped, never retained.
const LINE_LIMIT: usize = 64 * 1024;
/// At most this many rows of each kind are kept, so a runaway log bounds the answer.
const MAX_ROWS: usize = 4096;

#[derive(Default)]
struct StepRow {
    step: String,
    started_at_unix_s: Option<u64>,
    ended_at_unix_s: Option<u64>,
    outcome: Option<String>,
    waiting_s: Option<f64>,
    execution_s: Option<f64>,
}

/// One run of a qualify graph branch, from its `qualify-branch` events.
struct QualifyRow {
    branch: String,
    /// The branch's log's file name; its side logs are `<stem>-*.log`.
    log: Option<String>,
    started_at_unix_s: Option<u64>,
    push_began_at_unix_s: Option<u64>,
    ended_at_unix_s: Option<u64>,
    outcome: Option<String>,
    execution_s: Option<u64>,
}

/// One `step-child-finished` record: the log it was in, when the child ended, and what `wait4`
/// booked for it.
struct ChildRow {
    log: String,
    ended_at_unix_ms: Option<u64>,
    cpu_s: Option<f64>,
    peak_bytes: Option<u64>,
}

impl QualifyRow {
    /// Whether the child ended within this row's linked window. `None` means its log is linked but
    /// the times cannot exclude ownership; it must not let another row claim the child.
    fn contains_child(&self, child: &ChildRow) -> Option<bool> {
        let linked = self
            .log
            .as_deref()
            .and_then(|log| log.strip_suffix(".log"))
            .is_some_and(|stem| {
                child.log.strip_prefix(stem).is_some_and(|rest| {
                    rest == ".log" || (rest.starts_with('-') && rest.ends_with(".log"))
                })
            });
        if !linked {
            return Some(false);
        }
        let at = child.ended_at_unix_ms?;
        let from = self.started_at_unix_s?.checked_mul(1000)?;
        let to = self.ended_at_unix_s?.checked_mul(1000)?.checked_add(999)?;
        if from > to {
            None
        } else {
            Some((from..=to).contains(&at))
        }
    }
}

#[derive(Default)]
struct ChildTotals {
    cpu_s: Option<f64>,
    peak_bytes: Option<u64>,
    children: u64,
}

/// The cutter's own log in a run's logs: its lines, and, since it arms the seal's log there
/// ([`crate::build_inputs::arm`]), its own seal calls' records.
pub const TRANSCRIPT: &str = "transcript.log";

/// The accumulated timeline of one run.
#[derive(Default)]
pub struct Timeline {
    run_started_at_unix_s: Option<u64>,
    steps: Vec<StepRow>,
    step_children: BTreeMap<String, ChildTotals>,
    /// Every step child, by the log it was in: a qualify branch's are its log's.
    children: Vec<ChildRow>,
    qualify: Vec<QualifyRow>,
    branches: Vec<aterm_json::Value>,
    cut_gates: Vec<aterm_json::Value>,
    /// Verify starts by (log, diagnostic invocation ID, stage index). Consumed, missing or
    /// repeated start times stay unknown; bounded slots remain to reject reuse of one key.
    /// New keys past MAX_ROWS have unknown starts; this is not a complete activity census.
    stage_starts: BTreeMap<(String, String, u64), Option<u64>>,
    first_red_at_unix_ms: Option<u64>,
    first_red: Option<aterm_json::Value>,
    hashed: Option<u64>,
    rechecked: Option<u64>,
    /// A seal call's record was read from [`TRANSCRIPT`]: the cutter's own calls reach its logs,
    /// so a mode none of the run's records names is one no call took.
    own_seal: bool,
    /// The preceding image's logs (a `continued` record in [`TRANSCRIPT`]): its first steps, its
    /// seal call among them, are in them.
    continued_from: Option<PathBuf>,
    /// Whether the preceding image's own seal calls reached its logs, once [`Timeline::of_logs`]
    /// read them; `None` where it did not (none named, or a read of the records alone).
    preceding_seal: Option<bool>,
    disk_walks: Option<u64>,
    units_fresh: Option<u64>,
    units_compiled: Option<u64>,
    /// The bytes the run's compiles wrote (their built units' files).
    output_bytes: Option<u64>,
    /// The publish step's line arrivals: (unix ms, the line's head).
    publish_lines: Vec<(u64, String)>,
    publish_ended_at_unix_ms: Option<u64>,
}

fn add_f(total: &mut Option<f64>, v: Option<f64>) {
    if let Some(v) = v {
        *total = Some(total.unwrap_or(0.0) + v);
    }
}

fn add_u(total: &mut Option<u64>, v: Option<u64>) {
    if let Some(v) = v {
        *total = Some(total.unwrap_or(0).saturating_add(v));
    }
}

fn failed_decisions(event: &aterm_json::Value) -> bool {
    event["decisions"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|row| row["outcome"].as_str() == Some("FAIL"))
    })
}

impl Timeline {
    fn red(&mut self, at_unix_ms: Option<u64>, event: &aterm_json::Value) {
        let Some(at) = at_unix_ms else {
            return;
        };
        if self.first_red_at_unix_ms.is_none_or(|first| at < first) {
            self.first_red_at_unix_ms = Some(at);
            self.first_red = Some(event.clone());
        }
    }

    /// Book one already decoded diagnostic object. Duplicate keys erased before
    /// construction cannot be detected here; raw log input goes through `read`.
    #[cfg(test)]
    pub fn observe(&mut self, log: &str, event: &aterm_json::Value) {
        let invocation = aterm_verify::event_log::diagnostic_invocation_id(event);
        self.observe_identified(log, event, invocation.as_deref());
    }

    fn observe_identified(
        &mut self,
        log: &str,
        event: &aterm_json::Value,
        invocation: Option<&str>,
    ) {
        if event["schema"].as_u64() != Some(1) {
            return;
        }
        match event["event"].as_str() {
            Some("cut-gate-finished") => {
                if self.cut_gates.len() < MAX_ROWS {
                    self.cut_gates.push(event.clone());
                }
            }
            Some("release-stage") => self.release_stage(event),
            Some("qualify-branch") => self.qualify_branch(event),
            Some("step-child-finished") => {
                let Some(step) = event["step"].as_str() else {
                    return;
                };
                let totals = if let Some(totals) = self.step_children.get_mut(step) {
                    totals
                } else if self.step_children.len() < MAX_ROWS {
                    self.step_children.entry(step.to_owned()).or_default()
                } else {
                    return;
                };
                totals.children += 1;
                add_f(&mut totals.cpu_s, event["child_cpu"]["observed_s"].as_f64());
                if let Some(peak) = event["peak_bytes"].as_u64() {
                    totals.peak_bytes = Some(totals.peak_bytes.map_or(peak, |p| p.max(peak)));
                }
                if self.children.len() < MAX_ROWS {
                    self.children.push(ChildRow {
                        log: log.to_owned(),
                        ended_at_unix_ms: event["ended_at_unix_ms"].as_u64(),
                        cpu_s: event["child_cpu"]["observed_s"].as_f64(),
                        peak_bytes: event["peak_bytes"].as_u64(),
                    });
                }
                if step == crate::release::Step::Publish.name() {
                    self.publish_ended_at_unix_ms = event["ended_at_unix_ms"].as_u64();
                }
            }
            Some("stage-started") => {
                if let Some(index) = event["stage_index"].as_u64()
                    && let Some(invocation) = invocation
                {
                    let key = (log.to_owned(), invocation.to_owned(), index);
                    if let Some(started) = self.stage_starts.get_mut(&key) {
                        *started = None;
                    } else if self.stage_starts.len() < MAX_ROWS {
                        self.stage_starts
                            .insert(key, event["started_at_unix_ms"].as_u64());
                    }
                }
            }
            Some("stage-finished") => self.verify_stage(log, event, invocation),
            Some("build-child-finished") => {
                let fresh = event["units"]["fresh"].as_u64();
                let compiled = event["units"]["compiled"].as_u64();
                add_u(&mut self.units_fresh, fresh);
                add_u(&mut self.units_compiled, compiled);
                add_u(&mut self.output_bytes, event["output_bytes"].as_u64());
                if self.branches.len() < MAX_ROWS {
                    let call = event["call"].as_str().map_or_else(
                        || {
                            format!(
                                "{} {}",
                                event["package"].as_str().unwrap_or("?"),
                                event["target"].as_str().unwrap_or("native")
                            )
                        },
                        str::to_owned,
                    );
                    self.branches
                        .push(aterm_json::Value::Object(aterm_json::Map::from([
                            ("kind".to_owned(), aterm_json::Value::from("slice")),
                            ("log".to_owned(), aterm_json::Value::from(log)),
                            ("call".to_owned(), aterm_json::Value::String(call)),
                            ("outcome".to_owned(), event["outcome"].clone()),
                            ("execution_s".to_owned(), event["execution_s"].clone()),
                            ("cpu_s".to_owned(), event["child_cpu"]["observed_s"].clone()),
                            ("peak_bytes".to_owned(), event["peak_bytes"].clone()),
                            ("units".to_owned(), event["units"].clone()),
                            ("output_bytes".to_owned(), event["output_bytes"].clone()),
                        ])));
                }
            }
            Some("compile-finished") => {
                add_u(&mut self.units_fresh, event["units"]["fresh"].as_u64());
                add_u(
                    &mut self.units_compiled,
                    event["units"]["compiled"].as_u64(),
                );
                add_u(&mut self.output_bytes, event["output_bytes"].as_u64());
            }
            // A case its block spares (refused, out of disk) reads NOT RUN and is no red.
            Some("test-case-failed") if event["outcome"].as_str() == Some("FAIL") => {
                let at = event["at_unix_ms"].as_u64();
                self.red(at, event);
            }
            Some("inputs") => {
                match event["mode"].as_str() {
                    Some("hashed") => add_u(&mut self.hashed, Some(1)),
                    Some("rechecked" | "adopted") => add_u(&mut self.rechecked, Some(1)),
                    _ => return,
                }
                self.own_seal |= log == TRANSCRIPT;
            }
            Some("handed" | "continued") if log == TRANSCRIPT => {
                self.continued_from = event["from"].as_str().map(PathBuf::from);
            }
            Some("disk") => add_u(&mut self.disk_walks, event["walks"].as_u64()),
            Some("publish-line") => {
                if let (Some(at), Some(line)) =
                    (event["at_unix_ms"].as_u64(), event["line"].as_str())
                    && self.publish_lines.len() < MAX_ROWS
                {
                    self.publish_lines.push((at, line.to_owned()));
                }
            }
            _ => {}
        }
    }

    fn release_stage(&mut self, event: &aterm_json::Value) {
        let (Some(step), Some(outcome)) = (event["stage"].as_str(), event["outcome"].as_str())
        else {
            return;
        };
        let at = event["at_unix_s"].as_u64();
        if outcome == "started" {
            if self.run_started_at_unix_s.is_none() {
                self.run_started_at_unix_s = at;
            }
            if self.steps.len() < MAX_ROWS {
                self.steps.push(StepRow {
                    step: step.to_owned(),
                    started_at_unix_s: at,
                    ..StepRow::default()
                });
            }
            return;
        }
        if outcome == "FAIL" {
            self.red(at.map(|s| s.saturating_mul(1000)), event);
        }
        let open = self
            .steps
            .iter()
            .rposition(|row| row.step == step && row.ended_at_unix_s.is_none());
        let at_row = match open {
            Some(at_row) => at_row,
            None if self.steps.len() < MAX_ROWS => {
                self.steps.push(StepRow {
                    step: step.to_owned(),
                    ..StepRow::default()
                });
                self.steps.len() - 1
            }
            None => return,
        };
        let row = &mut self.steps[at_row];
        row.ended_at_unix_s = at;
        row.outcome = Some(outcome.to_owned());
        row.waiting_s = event["waiting_s"].as_f64();
        row.execution_s = event["execution_s"].as_f64();
    }

    /// A qualify branch's start opens its row, its staging push dates the row's push, and its
    /// end (passed, red, could-not-run, stopped) closes the open row; a red dates a red.
    fn qualify_branch(&mut self, event: &aterm_json::Value) {
        let (Some(branch), Some(phase)) = (event["branch"].as_str(), event["phase"].as_str())
        else {
            return;
        };
        let at = event["at_unix_s"].as_u64();
        if phase == "started" {
            if self.qualify.len() < MAX_ROWS {
                self.qualify.push(QualifyRow {
                    branch: branch.to_owned(),
                    log: event["log"].as_str().map(str::to_owned),
                    started_at_unix_s: at,
                    push_began_at_unix_s: None,
                    ended_at_unix_s: None,
                    outcome: None,
                    execution_s: None,
                });
            }
            return;
        }
        if phase == "red" {
            self.red(at.map(|s| s.saturating_mul(1000)), event);
        }
        let open = self
            .qualify
            .iter()
            .rposition(|row| row.branch == branch && row.ended_at_unix_s.is_none());
        let at_row = match open {
            Some(at_row) => at_row,
            // A branch the graph never started ends stopped with no start.
            None if self.qualify.len() < MAX_ROWS => {
                self.qualify.push(QualifyRow {
                    branch: branch.to_owned(),
                    log: event["log"].as_str().map(str::to_owned),
                    started_at_unix_s: None,
                    push_began_at_unix_s: None,
                    ended_at_unix_s: None,
                    outcome: None,
                    execution_s: None,
                });
                self.qualify.len() - 1
            }
            None => return,
        };
        let row = &mut self.qualify[at_row];
        if phase == "push-began" {
            row.push_began_at_unix_s = at;
            return;
        }
        row.ended_at_unix_s = at;
        row.outcome = Some(phase.to_owned());
        row.execution_s = event["seconds"]
            .as_u64()
            .filter(|_| row.started_at_unix_s.is_some());
    }

    /// Qualify rows keep only child contributions with one possible owner by log and time.
    /// Producer seconds include the whole second, so overlapping rounds and uncertain linked
    /// windows cannot establish ownership. These observed contributions are not a child census.
    fn qualify_rows(&self) -> Vec<aterm_json::Value> {
        let mut totals: Vec<ChildTotals> = (0..self.qualify.len())
            .map(|_| ChildTotals::default())
            .collect();
        for child in &self.children {
            let mut candidates = self
                .qualify
                .iter()
                .enumerate()
                .filter(|(_, row)| row.contains_child(child) != Some(false));
            let Some((index, row)) = candidates.next() else {
                continue;
            };
            if candidates.next().is_some() || row.contains_child(child) != Some(true) {
                continue;
            }
            let total = &mut totals[index];
            total.children += 1;
            add_f(&mut total.cpu_s, child.cpu_s);
            if let Some(peak) = child.peak_bytes {
                total.peak_bytes = Some(total.peak_bytes.map_or(peak, |old| old.max(peak)));
            }
        }
        self.qualify
            .iter()
            .zip(totals)
            .map(|(row, totals)| {
                aterm_json::json!({
                    "branch": row.branch, "log": row.log, "outcome": row.outcome,
                    "started_at_unix_s": row.started_at_unix_s,
                    "push_began_at_unix_s": row.push_began_at_unix_s,
                    "ended_at_unix_s": row.ended_at_unix_s, "execution_s": row.execution_s,
                    "cpu_s": totals.cpu_s, "peak_bytes": totals.peak_bytes,
                    "children": totals.children,
                })
            })
            .collect()
    }

    fn verify_stage(&mut self, log: &str, event: &aterm_json::Value, invocation: Option<&str>) {
        let started = if let Some(index) = event["stage_index"].as_u64()
            && let Some(invocation) = invocation
        {
            let key = (log.to_owned(), invocation.to_owned(), index);
            if let Some(started) = self.stage_starts.get_mut(&key) {
                started.take()
            } else {
                // An unmatched completion also consumes its key: a later start cannot repair
                // event order or supply a timestamp to another completion.
                if self.stage_starts.len() < MAX_ROWS {
                    self.stage_starts.insert(key, None);
                }
                None
            }
        } else {
            None
        };
        let execution_s = event["execution_s"].as_f64();
        let ended = event["ended_at_unix_ms"].as_u64().or_else(|| {
            let (started, execution) = (started?, execution_s?);
            // Whole milliseconds of a non-negative duration.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a stage's execution seconds, non-negative and far below u64::MAX ms"
            )]
            let ms = (execution.max(0.0) * 1000.0) as u64;
            Some(started.saturating_add(ms))
        });
        if failed_decisions(event) {
            self.red(ended, event);
        }
        if self.branches.len() < MAX_ROWS {
            self.branches
                .push(aterm_json::Value::Object(aterm_json::Map::from([
                    ("kind".to_owned(), aterm_json::Value::from("verify-stage")),
                    ("log".to_owned(), aterm_json::Value::from(log)),
                    ("call".to_owned(), event["stage"].clone()),
                    (
                        "started_at_unix_ms".to_owned(),
                        aterm_json::Value::from(started),
                    ),
                    (
                        "ended_at_unix_ms".to_owned(),
                        aterm_json::Value::from(ended),
                    ),
                    ("waiting_s".to_owned(), event["waiting_s"].clone()),
                    (
                        "execution_s".to_owned(),
                        aterm_json::Value::from(execution_s),
                    ),
                    ("cpu_s".to_owned(), event["child_cpu"]["observed_s"].clone()),
                    ("starved_s".to_owned(), event["starved_s"].clone()),
                    ("peak_bytes".to_owned(), event["peak_bytes"].clone()),
                ])));
        }
    }

    /// Read every record in `reader`, the log named `log`.
    /// # Errors
    /// The read's.
    pub fn read(&mut self, log: &str, mut reader: impl Read) -> std::io::Result<()> {
        let mut line = Vec::new();
        let mut overflow = false;
        let mut bytes = [0; 8192];
        loop {
            let n = reader.read(&mut bytes)?;
            if n == 0 {
                return Ok(()); // A partial final line is not a record.
            }
            let mut rest = &bytes[..n];
            while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
                let seg = &rest[..pos];
                if line.is_empty() && !overflow {
                    if seg.len() <= LINE_LIMIT
                        && let Some(json) = seg
                            .strip_prefix(b"verify-event: ")
                            .or_else(|| seg.strip_prefix(b"release-event: "))
                    {
                        self.observe_json(log, json);
                    }
                } else {
                    if !overflow && line.len() + seg.len() <= LINE_LIMIT {
                        line.extend_from_slice(seg);
                        if let Some(json) = line
                            .strip_prefix(b"verify-event: ")
                            .or_else(|| line.strip_prefix(b"release-event: "))
                        {
                            self.observe_json(log, json);
                        }
                    }
                    line.clear();
                    overflow = false;
                }
                rest = &rest[pos + 1..];
            }
            if !rest.is_empty() && !overflow {
                let starts_like_event = if line.is_empty() {
                    rest.starts_with(b"verify-event: ")
                        || rest.starts_with(b"release-event: ")
                        || b"verify-event: ".starts_with(rest)
                        || b"release-event: ".starts_with(rest)
                } else {
                    true
                };
                if starts_like_event && line.len() + rest.len() <= LINE_LIMIT {
                    line.extend_from_slice(rest);
                } else {
                    line.clear();
                    overflow = true;
                }
            }
        }
    }

    fn observe_json(&mut self, log: &str, json: &[u8]) {
        if let Ok(json) = std::str::from_utf8(json)
            && let Some((event, invocation)) = aterm_verify::event_log::diagnostic_event(json)
        {
            self.observe_identified(log, &event, invocation.as_deref());
        }
    }

    /// Read `logs`'s transcript and every step log (`NN-<step>*.log`), in name order, and where
    /// an earlier image continued into this one, the seal calls and walks its logs record: the
    /// release's, though another image made them.
    /// # Errors
    /// The directory cannot be listed, or a log present cannot be read.
    pub fn of_logs(logs: &Path) -> std::io::Result<Self> {
        let mut timeline = Self::of_run(logs)?;
        if let Some(from) = timeline.continued_from.clone() {
            let preceding = Self::of_run(&from).ok();
            timeline.preceding_seal = Some(preceding.as_ref().is_some_and(|h| h.own_seal));
            if let Some(preceding) = preceding {
                add_u(&mut timeline.hashed, preceding.hashed);
                add_u(&mut timeline.rechecked, preceding.rechecked);
                add_u(&mut timeline.disk_walks, preceding.disk_walks);
            }
        }
        Ok(timeline)
    }

    /// One run's logs, `logs`, as [`Timeline::of_logs`] reads them, none other followed.
    fn of_run(logs: &Path) -> std::io::Result<Self> {
        let mut timeline = Self::default();
        let mut names: Vec<String> = std::fs::read_dir(logs)?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| {
                name.ends_with(".log")
                    && name.len() > 3
                    && name.as_bytes()[..2].iter().all(u8::is_ascii_digit)
                    && name.as_bytes()[2] == b'-'
            })
            .collect();
        names.sort_unstable();
        for name in std::iter::once(TRANSCRIPT).chain(names.iter().map(String::as_str)) {
            match crate::durable::open_regular_file(&logs.join(name)) {
                Ok(file) => timeline.read(name, file)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(timeline)
    }

    /// The bytes the run's slices and compiles wrote, by their own records; `None` where no
    /// record says.
    #[must_use]
    pub const fn output_bytes(&self) -> Option<u64> {
        self.output_bytes
    }

    fn publish_phases(&self) -> aterm_json::Value {
        let headers: Vec<&(u64, String)> = self
            .publish_lines
            .iter()
            .filter(|(_, line)| line.trim_start().starts_with("== "))
            .collect();
        let Some(&&(first, _)) = headers.first() else {
            return aterm_json::Value::Null;
        };
        let start = self
            .steps
            .iter()
            .rev()
            .find(|row| row.step == crate::release::Step::Publish.name())
            .and_then(|row| row.started_at_unix_s)
            .map(|s| s.saturating_mul(1000));
        let mut phases = Vec::with_capacity(headers.len() + 1);
        // The engine replays promote's sections only after promote ends, so the span before
        // its first section header is promote itself: its guards, hooks and public push.
        phases.push(aterm_json::json!({
            "phase": "before the engine's first section header",
            "started_at_unix_ms": start, "ended_at_unix_ms": first,
            "seconds": start.map(|s| first.saturating_sub(s) as f64 / 1000.0),
        }));
        for (i, (at, line)) in headers.iter().map(|h| (h.0, &h.1)).enumerate() {
            let end = headers
                .get(i + 1)
                .map(|next| next.0)
                .or(self.publish_ended_at_unix_ms);
            phases.push(aterm_json::json!({
                "phase": line.trim(),
                "started_at_unix_ms": at, "ended_at_unix_ms": end,
                "seconds": end.map(|e| e.saturating_sub(at) as f64 / 1000.0),
            }));
        }
        aterm_json::Value::Array(phases)
    }

    /// The timeline as `release status --json` shows it.
    #[must_use]
    pub fn value(&self) -> aterm_json::Value {
        let steps: Vec<_> = self
            .steps
            .iter()
            .map(|row| {
                let children = self.step_children.get(&row.step);
                aterm_json::Value::Object(aterm_json::Map::from([
                    (
                        "step".to_owned(),
                        aterm_json::Value::from(row.step.as_str()),
                    ),
                    (
                        "started_at_unix_s".to_owned(),
                        aterm_json::Value::from(row.started_at_unix_s),
                    ),
                    (
                        "ended_at_unix_s".to_owned(),
                        aterm_json::Value::from(row.ended_at_unix_s),
                    ),
                    (
                        "outcome".to_owned(),
                        aterm_json::Value::from(row.outcome.as_deref()),
                    ),
                    (
                        "waiting_s".to_owned(),
                        aterm_json::Value::from(row.waiting_s),
                    ),
                    (
                        "execution_s".to_owned(),
                        aterm_json::Value::from(row.execution_s),
                    ),
                    (
                        "cpu_s".to_owned(),
                        aterm_json::Value::from(children.and_then(|c| c.cpu_s)),
                    ),
                    (
                        "peak_bytes".to_owned(),
                        aterm_json::Value::from(children.and_then(|c| c.peak_bytes)),
                    ),
                    (
                        "children".to_owned(),
                        aterm_json::Value::from(children.map(|c| c.children)),
                    ),
                ]))
            })
            .collect();
        // The steps run one after another (step 4's branches run beside each other inside its
        // one row), so the critical path is their summed execution (owner waits excluded).
        let mut critical_path_s = None;
        for s in self.steps.iter().filter_map(|row| row.execution_s) {
            *critical_path_s.get_or_insert(0.0) += s;
        }
        let time_to_first_red_s = self
            .first_red_at_unix_ms
            .zip(self.run_started_at_unix_s)
            .map(|(red, start)| red.saturating_sub(start.saturating_mul(1000)) as f64 / 1000.0);
        // Every seal call of the release reached a log where the cutter's own did, and the
        // preceding image's too where one continued: a mode no record names, no call took.
        let seal_known =
            self.own_seal && (self.continued_from.is_none() || self.preceding_seal == Some(true));
        let seal_count = |n: Option<u64>| if seal_known { Some(n.unwrap_or(0)) } else { n };
        let units = match (self.units_fresh, self.units_compiled) {
            (None, None) => aterm_json::Value::Null,
            (fresh, compiled) => aterm_json::Value::Object(aterm_json::Map::from([
                ("fresh".to_owned(), aterm_json::Value::from(fresh)),
                ("compiled".to_owned(), aterm_json::Value::from(compiled)),
            ])),
        };
        aterm_json::Value::Object(aterm_json::Map::from([
            (
                "run_started_at_unix_s".to_owned(),
                aterm_json::Value::from(self.run_started_at_unix_s),
            ),
            (
                "critical_path_s".to_owned(),
                aterm_json::Value::from(critical_path_s),
            ),
            (
                "time_to_first_red_s".to_owned(),
                aterm_json::Value::from(time_to_first_red_s),
            ),
            (
                "first_red".to_owned(),
                self.first_red.clone().unwrap_or(aterm_json::Value::Null),
            ),
            ("steps".to_owned(), aterm_json::Value::Array(steps)),
            (
                "cut_gates".to_owned(),
                aterm_json::Value::Array(self.cut_gates.clone()),
            ),
            (
                "qualify".to_owned(),
                aterm_json::Value::Array(self.qualify_rows()),
            ),
            (
                "branches".to_owned(),
                aterm_json::Value::Array(self.branches.clone()),
            ),
            (
                "full_hashes".to_owned(),
                aterm_json::Value::from(seal_count(self.hashed)),
            ),
            (
                "seal_rechecks".to_owned(),
                aterm_json::Value::from(seal_count(self.rechecked)),
            ),
            (
                "disk_walks".to_owned(),
                aterm_json::Value::from(self.disk_walks),
            ),
            ("units".to_owned(), units),
            (
                "output_bytes".to_owned(),
                aterm_json::Value::from(self.output_bytes),
            ),
            ("publish_phases".to_owned(), self.publish_phases()),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_timeline(logs: &[(&str, &str)]) -> Timeline {
        let dir = aterm_tempfile::tempdir().unwrap();
        for (name, text) in logs {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        Timeline::of_logs(dir.path()).unwrap()
    }

    fn timeline_of(logs: &[(&str, &str)]) -> aterm_json::Value {
        read_timeline(logs).value()
    }

    #[test]
    fn cut_gate_costs_survive_the_log_without_inflating_step_totals() {
        let gate = aterm_json::json!({
            "schema": 1, "event": "cut-gate-finished", "gate": "digest",
            "execution_s": 3.0, "self_cpu_s": 2.0,
            "child_cpu": {"observed_s": 1.0}, "inclusive": true
        });
        let log = format!(
            "{}{}{}release-event: {gate}\n",
            stage("publish", "started", 1000, 0, 0),
            stage("publish", "ok", 1003, 0, 3),
            child("publish", 9.0, 1024),
        );
        let value = timeline_of(&[("08-publish.log", &log)]);
        assert_eq!(value["cut_gates"][0], gate);
        assert_eq!(value["cut_gates"].as_array().unwrap().len(), 1);
        // A nested cost is diagnostic detail, not another child to charge.
        assert_eq!(value["steps"][0]["cpu_s"].as_f64(), Some(9.0));
        assert_eq!(value["steps"][0]["children"].as_u64(), Some(1));
    }

    fn stage(step: &str, outcome: &str, at: u64, waiting: u64, execution: u64) -> String {
        format!(
            "release-event: {}\n",
            aterm_json::json!({
                "schema": 1, "event": "release-stage", "stage": step, "outcome": outcome,
                "at_unix_s": at, "waiting_s": waiting, "execution_s": execution,
            })
        )
    }

    fn child(step: &str, cpu: f64, peak: u64) -> String {
        format!(
            "release-event: {}\n",
            aterm_json::json!({
                "schema": 1, "event": "step-child-finished", "step": step, "exit_code": 0,
                "started_at_unix_ms": 1, "ended_at_unix_ms": 2,
                "child_cpu": {"scope": "reaped-child-and-waited-descendants", "observed_s": cpu},
                "peak_bytes": peak,
            })
        )
    }

    fn attributed(event: aterm_json::Value, id: &str) -> aterm_json::Value {
        let mut event = event;
        event.as_object_mut().unwrap().insert(
            "invocation".into(),
            aterm_json::json!({
                "id":id, "identity_error":null, "source_observation":null,
            }),
        );
        event
    }

    fn attributed_line(line: &str, id: &str) -> String {
        let event =
            aterm_json::from_str(line.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
        format!("verify-event: {}\n", attributed(event, id))
    }

    fn verify_start(id: &str, at: u64) -> aterm_json::Value {
        attributed(
            aterm_json::json!({"schema":1,"event":"stage-started",
            "stage_index":3,"stage":"test","started_at_unix_ms":at}),
            id,
        )
    }

    fn verify_finish(id: &str, at: u64) -> aterm_json::Value {
        attributed(
            aterm_json::json!({"schema":1,"event":"stage-finished",
            "stage_index":3,"stage":"test","ended_at_unix_ms":at,"execution_s":2.0,
            "child_cpu":{"observed_s":7.0},"decisions":[{"outcome":"FAIL","detail":"kept"}]}),
            id,
        )
    }

    #[test]
    fn raw_ambiguous_attribution_cannot_supply_or_consume_a_stage_start() {
        for padding in [0, 9000] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let path = dir.path().join("events");
            let writer = aterm_verify::event_log::EventLog::open(&path).unwrap();
            let mut start = verify_start(&"a".repeat(32), 100);
            let mut finish = verify_finish(&"a".repeat(32), 400);
            finish.as_object_mut().unwrap().remove("ended_at_unix_ms");
            for value in [&mut start, &mut finish] {
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("pad".into(), "x".repeat(padding).into());
            }
            writer.write(&format!("verify-event: {start}"));
            writer.write(&format!("verify-event: {finish}"));
            let recorded = std::fs::read_to_string(&path).unwrap();
            let mut lines = recorded.lines();
            let start = format!("{}\n", lines.next().unwrap());
            let finish = format!("{}\n", lines.next().unwrap());
            assert!(lines.next().is_none());
            let (_, id) = aterm_verify::event_log::diagnostic_event(
                start.trim().strip_prefix("verify-event: ").unwrap(),
            )
            .unwrap();
            assert!(id.is_some());
            for (key, replacement) in [
                ("\"id\":", "\"id\":\"unknown\",\"id\":"),
                ("\"id\":", "\"\\u0069d\":\"unknown\",\"id\":"),
                ("\"invocation\":", "\"invocation\":null,\"invocation\":"),
                (
                    "\"identity_error\":",
                    "\"identity_error\":\"unavailable\",\"identity_error\":",
                ),
            ] {
                let bad_start = start.replacen(key, replacement, 1);
                let bad_finish = finish.replacen(key, replacement, 1);
                assert_ne!(bad_start, start);
                let text = format!("{start}{bad_start}{bad_finish}{finish}");
                let mut timeline = Timeline::default();
                timeline.read("06-verify.log", text.as_bytes()).unwrap();
                let value = timeline.value();
                let rows = value["branches"].as_array().unwrap();
                assert_eq!(rows.len(), 2);
                assert!(rows[0]["started_at_unix_ms"].is_null());
                assert!(rows[0]["ended_at_unix_ms"].is_null());
                assert_eq!(rows[1]["started_at_unix_ms"].as_u64(), Some(100));
                assert_eq!(rows[1]["ended_at_unix_ms"].as_u64(), Some(2100));
            }
            for (key, replacement) in [
                ("\"schema\":", "\"schema\":0,\"schema\":"),
                ("\"event\":", "\"\\u0065vent\":\"other\",\"event\":"),
            ] {
                let malformed = finish.replacen(key, replacement, 1);
                assert_ne!(malformed, finish);
                let text = format!("{start}{malformed}{finish}");
                let mut timeline = Timeline::default();
                timeline.read("06-verify.log", text.as_bytes()).unwrap();
                let value = timeline.value();
                let rows = value["branches"].as_array().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0]["started_at_unix_ms"].as_u64(), Some(100));
            }
        }
    }

    #[test]
    fn a_later_never_started_invocation_cannot_borrow_an_abandoned_start() {
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let text = [
            verify_start(&a, 100),
            verify_finish(&b, 200),
            verify_finish(&a, 300),
        ]
        .iter()
        .map(|event| format!("verify-event: {event}\n"))
        .collect::<String>();
        let mut timeline = Timeline::default();
        timeline.read("06-verify.log", text.as_bytes()).unwrap();
        let value = timeline.value();
        let rows = value["branches"].as_array().unwrap();
        assert!(rows[0]["started_at_unix_ms"].is_null());
        assert_eq!(rows[0]["ended_at_unix_ms"].as_u64(), Some(200));
        assert_eq!(rows[0]["cpu_s"].as_f64(), Some(7.0));
        assert_eq!(rows[0]["execution_s"].as_f64(), Some(2.0));
        assert_eq!(rows[1]["started_at_unix_ms"].as_u64(), Some(100));
        assert_eq!(value["first_red"]["ended_at_unix_ms"].as_u64(), Some(200));
        assert_eq!(
            value["first_red"]["decisions"][0]["detail"].as_str(),
            Some("kept")
        );
    }

    #[test]
    fn stage_pairing_keeps_invocations_and_logs_independent() {
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let mut timeline = Timeline::default();
        timeline.observe("06-verify.log", &verify_start(&a, 100));
        timeline.observe("06-verify.log", &verify_start(&b, 200));
        timeline.observe("04-qualify-tree.log", &verify_start(&a, 300));
        timeline.observe("06-verify.log", &verify_finish(&a, 400));
        timeline.observe("04-qualify-tree.log", &verify_finish(&a, 500));
        timeline.observe("06-verify.log", &verify_finish(&b, 600));
        let value = timeline.value();
        let starts: Vec<_> = value["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["started_at_unix_ms"].as_u64())
            .collect();
        assert_eq!(starts, [Some(100), Some(300), Some(200)]);
    }

    #[test]
    fn unsupported_attribution_cannot_supply_or_consume_a_stage_start() {
        let a = "a".repeat(32);
        for invalid in [
            aterm_json::Value::Null,
            aterm_json::json!({"id":"invalid","identity_error":null,"source_observation":null}),
            aterm_json::json!({"id":a,"identity_error":"unavailable","source_observation":null}),
            aterm_json::json!({"id":a,"identity_error":null,"source_observation":{"unknown":"context"}}),
        ] {
            let mut timeline = Timeline::default();
            timeline.observe("log", &verify_start(&a, 100));
            for mut event in [verify_start(&a, 200), verify_finish(&a, 300)] {
                if invalid.is_null() {
                    event.as_object_mut().unwrap().remove("invocation");
                } else {
                    event
                        .as_object_mut()
                        .unwrap()
                        .insert("invocation".into(), invalid.clone());
                }
                timeline.observe("log", &event);
            }
            timeline.observe("log", &verify_finish(&a, 400));
            let value = timeline.value();
            assert!(value["branches"][0]["started_at_unix_ms"].is_null());
            assert_eq!(
                value["branches"][1]["started_at_unix_ms"].as_u64(),
                Some(100)
            );
        }
    }

    #[test]
    fn repeated_starts_and_consumed_stage_keys_never_choose_a_timestamp() {
        let a = "a".repeat(32);
        let mut timeline = Timeline::default();
        timeline.observe("log", &verify_start(&a, 100));
        timeline.observe("log", &verify_start(&a, 200));
        timeline.observe("log", &verify_finish(&a, 300));
        timeline.observe("log", &verify_start(&a, 400));
        timeline.observe("log", &verify_finish(&a, 500));
        let value = timeline.value();
        assert!(
            value["branches"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["started_at_unix_ms"].is_null())
        );
        let mut out_of_order = Timeline::default();
        out_of_order.observe("log", &verify_finish(&a, 100));
        out_of_order.observe("log", &verify_start(&a, 200));
        out_of_order.observe("log", &verify_finish(&a, 300));
        assert!(
            out_of_order.value()["branches"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["started_at_unix_ms"].is_null())
        );
    }

    #[test]
    fn the_stage_key_bound_keeps_overflow_and_repeated_starts_unknown() {
        let a = "a".repeat(32);
        let mut timeline = Timeline::default();
        for index in 0..MAX_ROWS {
            let mut event = verify_start(&a, 100);
            event
                .as_object_mut()
                .unwrap()
                .insert("stage_index".into(), u64::try_from(index).unwrap().into());
            timeline.observe("log", &event);
        }
        // A repeated key at capacity must still lose its timestamp; overflow supplies none.
        timeline.observe("log", &verify_start(&a, 200));
        timeline.observe("log", &verify_finish(&a, 300));
        let mut overflow = verify_start(&a, 400);
        overflow.as_object_mut().unwrap().insert(
            "stage_index".into(),
            u64::try_from(MAX_ROWS).unwrap().into(),
        );
        timeline.observe("log", &overflow);
        let mut finished = verify_finish(&a, 500);
        finished.as_object_mut().unwrap().insert(
            "stage_index".into(),
            u64::try_from(MAX_ROWS).unwrap().into(),
        );
        timeline.observe("log", &finished);
        assert_eq!(timeline.stage_starts.len(), MAX_ROWS);
        assert!(
            timeline.value()["branches"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["started_at_unix_ms"].is_null())
        );
    }

    /// Steps and their children's CPU add up, a verify stage's red dates the first red from the
    /// run's start, and each record kind is counted where it was written.
    #[test]
    fn the_timeline_sums_steps_and_branches_and_dates_the_first_red() {
        let transcript = [
            stage("checks", "started", 1000, 0, 0),
            stage("checks", "ok", 1010, 0, 10),
            stage("build", "started", 1010, 0, 0),
            stage("build", "ok", 1110, 0, 100),
            stage("verify", "started", 1110, 0, 0),
            stage("verify", "FAIL", 1300, 20, 170),
        ]
        .concat();
        let build = [
            child("build", 30.0, 5 << 30),
            child("build", 10.5, 7 << 30),
            format!(
                "release-event: {}\n",
                aterm_json::json!({
                    "schema": 1, "event": "build-child-finished", "package": "aterm",
                    "target": null, "verb": "build", "outcome": "ok", "execution_s": 90.0,
                    "child_cpu": {"observed_s": 400.0}, "peak_bytes": 3,
                    "units": {"fresh": 10, "compiled": 5}, "output_bytes": 1000,
                })
            ),
            "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"hashed\",\"entries\":9,\"secs\":1.0}\n".to_owned(),
            "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"rechecked\",\"entries\":9,\"secs\":0.1}\n".to_owned(),
        ]
        .concat();
        let verify = [
            "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":0,\"stage\":\"lint\",\"started_at_unix_ms\":1120000}\n",
            "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":0,\"stage\":\"lint\",\"waiting_s\":1.0,\"execution_s\":30.0,\"child_cpu\":{\"observed_s\":200.0},\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":\"x\"}]}\n",
            "verify-event: {\"schema\":1,\"event\":\"compile-finished\",\"units\":{\"fresh\":600,\"compiled\":2,\"unknown\":0},\"output_bytes\":24}\n",
            "verify-event: {\"schema\":1,\"event\":\"disk\",\"mode\":\"claimed\",\"walks\":3}\n",
        ]
        .into_iter().enumerate().map(|(at, line)| if at < 2 {
            attributed_line(line, &"a".repeat(32))
        } else { line.to_owned() }).collect::<String>();
        let value = timeline_of(&[
            ("transcript.log", &transcript),
            ("05-build.log", &build),
            ("06-verify.log", &verify),
        ]);
        assert_eq!(value["run_started_at_unix_s"].as_u64(), Some(1000));
        assert_eq!(value["critical_path_s"].as_f64(), Some(280.0), "{value}");
        let steps = value["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3, "{value}");
        assert_eq!(steps[1]["step"].as_str(), Some("build"));
        assert_eq!(steps[1]["cpu_s"].as_f64(), Some(40.5));
        assert_eq!(steps[1]["peak_bytes"].as_u64(), Some(7 << 30));
        assert_eq!(steps[1]["children"].as_u64(), Some(2));
        assert!(
            steps[0]["cpu_s"].is_null(),
            "no record is null, never 0: {value}"
        );
        // lint started at 1120.000 s and ran 30 s: red at 1150 s, 150 s after checks began,
        // earlier than the verify step's own FAIL at 1300 s.
        assert_eq!(
            value["time_to_first_red_s"].as_f64(),
            Some(150.0),
            "{value}"
        );
        assert_eq!(value["first_red"]["stage"].as_str(), Some("lint"));
        let branches = value["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 2, "{value}");
        assert_eq!(branches[0]["kind"].as_str(), Some("slice"));
        assert_eq!(branches[1]["ended_at_unix_ms"].as_u64(), Some(1_150_000));
        assert_eq!(value["full_hashes"].as_u64(), Some(1));
        assert_eq!(value["seal_rechecks"].as_u64(), Some(1));
        assert_eq!(value["disk_walks"].as_u64(), Some(3));
        assert_eq!(value["units"]["fresh"].as_u64(), Some(610));
        assert_eq!(value["units"]["compiled"].as_u64(), Some(7));
        assert_eq!(value["output_bytes"].as_u64(), Some(1024));
    }

    /// EACH QUALIFY BRANCH IS ITS OWN ROW, BOOKED ONLY ITS OWN CHILDREN: a branch's start, staging
    /// push and end come from its `qualify-branch` events; it is booked the children that ended
    /// in its log or a side log of it (the build's fixtures) while it ran, so a second round's
    /// build in the same log is booked only its own; a branch's red dates the first red; and a
    /// branch never started is a row with no start. NEGATIVE CONTROLS: booking a log's children
    /// without the branch's window gives the second build the first one's CPU too, and booking
    /// the branch's own log alone leaves the fixtures' CPU out of the first build's.
    #[test]
    fn each_qualify_branch_is_its_own_row_booked_only_its_own_children() {
        let branch = |b: &str, phase: &str, at: u64, secs: u64| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema": 1, "event": "qualify-branch", "branch": b,
                    "log": format!("04-qualify-{b}.log"), "phase": phase,
                    "at_unix_s": at, "seconds": secs})
            )
        };
        let child_at = |cpu: f64, ended_ms: u64| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema": 1, "event": "step-child-finished",
                    "step": "qualify", "exit_code": 0, "ended_at_unix_ms": ended_ms,
                    "child_cpu": {"observed_s": cpu}, "peak_bytes": (cpu as u64) << 20})
            )
        };
        let transcript = [
            stage("checks", "started", 990, 0, 0),
            stage("checks", "ok", 1000, 0, 10),
            stage("qualify", "started", 1000, 0, 0),
            branch("tree", "started", 1000, 0),
            branch("build", "started", 1010, 0),
            branch("export", "started", 1011, 0),
            branch("export", "push-began", 1300, 0),
            branch("tree", "red", 1600, 600),
            branch("build", "stopped", 1601, 591),
            branch("export", "passed", 1700, 689),
            branch("build", "started", 2000, 0),
            branch("build", "passed", 2500, 500),
            stage("qualify", "FAIL", 2500, 0, 1500),
        ]
        .concat();
        let value = timeline_of(&[
            ("transcript.log", &transcript),
            ("04-qualify-tree.log", &child_at(100.0, 1_599_000)),
            (
                "04-qualify-build.log",
                &[child_at(300.0, 1_590_000), child_at(50.0, 2_400_000)].concat(),
            ),
            ("04-qualify-build-fixtures.log", &child_at(20.0, 1_595_000)),
            ("04-qualify-export.log", &child_at(5.0, 1_699_500)),
        ]);
        let rows = value["qualify"].as_array().expect("qualify rows");
        let row = |b: &str, n: usize| {
            rows.iter()
                .filter(|r| r["branch"].as_str() == Some(b))
                .nth(n)
                .unwrap_or_else(|| panic!("no row {n} of {b}: {value}"))
        };
        assert_eq!(rows.len(), 4, "{value}");
        assert_eq!(row("tree", 0)["outcome"].as_str(), Some("red"));
        assert_eq!(row("tree", 0)["cpu_s"].as_f64(), Some(100.0), "{value}");
        assert_eq!(row("build", 0)["outcome"].as_str(), Some("stopped"));
        assert_eq!(row("build", 0)["execution_s"].as_u64(), Some(591));
        assert_eq!(row("build", 0)["cpu_s"].as_f64(), Some(320.0), "{value}");
        assert_eq!(row("build", 0)["peak_bytes"].as_u64(), Some(300 << 20));
        assert_eq!(row("build", 1)["cpu_s"].as_f64(), Some(50.0), "{value}");
        assert_eq!(
            row("export", 0)["push_began_at_unix_s"].as_u64(),
            Some(1300)
        );
        assert_eq!(row("export", 0)["cpu_s"].as_f64(), Some(5.0));
        // The tree's red at 1600 s, 610 s after the run began.
        assert_eq!(
            value["time_to_first_red_s"].as_f64(),
            Some(610.0),
            "{value}"
        );
        assert_eq!(value["first_red"]["branch"].as_str(), Some("tree"));
        // Every qualify child is the step's: its sum is the graph's CPU.
        assert_eq!(value["steps"][1]["cpu_s"].as_f64(), Some(475.0), "{value}");

        let never = timeline_of(&[("transcript.log", &branch("export", "stopped", 1601, 0))]);
        let row = &never["qualify"][0];
        assert!(
            row["started_at_unix_s"].is_null() && row["execution_s"].is_null(),
            "{never}"
        );
        assert_eq!(row["outcome"].as_str(), Some("stopped"));
        assert!(
            row["cpu_s"].is_null(),
            "no record is null, never 0: {never}"
        );
    }

    fn qualify_event(branch: &str, phase: &str, log: Option<&str>, at: Option<u64>) -> String {
        format!(
            "release-event: {}\n",
            aterm_json::json!({
                "schema":1, "event":"qualify-branch", "branch":branch,
                "phase":phase, "log":log, "at_unix_s":at,
            })
        )
    }

    fn qualify_child(cpu: f64, at: Option<u64>) -> String {
        format!(
            "release-event: {}\n",
            aterm_json::json!({
                "schema":1, "event":"step-child-finished", "step":"qualify",
                "ended_at_unix_ms":at, "child_cpu":{"observed_s":cpu}, "peak_bytes":1234,
            })
        )
    }

    #[test]
    fn same_second_rounds_leave_shared_children_unassigned_and_keep_unique_contributions() {
        let transcript = [
            qualify_event("build", "started", Some("04-build.log"), Some(1000)),
            qualify_event("build", "passed", Some("04-build.log"), Some(1010)),
            qualify_event("build", "started", Some("04-build.log"), Some(1010)),
            qualify_event("build", "passed", Some("04-build.log"), Some(1020)),
        ]
        .concat();
        let shared = qualify_child(17.0, Some(1_010_500));
        let ambiguous =
            read_timeline(&[("transcript.log", &transcript), ("04-build.log", &shared)]);
        assert_eq!(ambiguous.children.len(), 1);
        let ambiguous = ambiguous.value();
        for row in ambiguous["qualify"].as_array().unwrap() {
            assert!(row["cpu_s"].is_null());
            assert!(row["peak_bytes"].is_null());
            assert_eq!(row["children"].as_u64(), Some(0));
            assert_eq!(row["outcome"].as_str(), Some("passed"));
        }
        let children = [
            qualify_child(2.0, Some(1_009_500)),
            shared,
            qualify_child(3.0, Some(1_011_500)),
        ]
        .concat();
        let timeline =
            read_timeline(&[("transcript.log", &transcript), ("04-build.log", &children)]);
        assert_eq!(timeline.children.len(), 3);
        let value = timeline.value();
        let rows = value["qualify"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["cpu_s"].as_f64(), Some(2.0));
        assert_eq!(rows[1]["cpu_s"].as_f64(), Some(3.0));
        assert!(rows.iter().all(|row| row["children"].as_u64() == Some(1)));
    }

    #[test]
    fn nested_log_stems_do_not_duplicate_children_and_prefix_neighbors_are_not_linked() {
        let transcript = [
            qualify_event("outer", "started", Some("04-build.log"), Some(1000)),
            qualify_event(
                "inner",
                "started",
                Some("04-build-fixtures.log"),
                Some(1000),
            ),
            qualify_event("outer", "passed", Some("04-build.log"), Some(1010)),
            qualify_event("inner", "passed", Some("04-build-fixtures.log"), Some(1010)),
        ]
        .concat();
        let own = qualify_child(2.0, Some(1_005_000));
        let shared = qualify_child(17.0, Some(1_005_000));
        let unrelated = qualify_child(99.0, Some(1_005_000));
        let timeline = read_timeline(&[
            ("transcript.log", &transcript),
            ("04-build.log", &own),
            ("04-build-fixtures.log", &shared),
            ("04-buildish.log", &unrelated),
        ]);
        assert_eq!(timeline.children.len(), 3);
        let value = timeline.value();
        let rows = value["qualify"].as_array().unwrap();
        assert_eq!(rows[0]["cpu_s"].as_f64(), Some(2.0));
        assert_eq!(rows[0]["children"].as_u64(), Some(1));
        assert!(rows[1]["cpu_s"].is_null());
        assert_eq!(rows[1]["children"].as_u64(), Some(0));
    }

    #[test]
    fn uncertain_linked_windows_and_child_times_cannot_establish_unique_ownership() {
        for (start, end) in [
            (None, Some(1010)),
            (Some(1000), None),
            (Some(1012), Some(1000)),
            (Some(u64::MAX), Some(u64::MAX)),
            (Some(1000), Some(u64::MAX)),
        ] {
            let transcript = [
                qualify_event("known", "started", Some("04-build.log"), Some(1000)),
                qualify_event("unknown", "started", Some("04-build.log"), start),
                qualify_event("known", "passed", Some("04-build.log"), Some(1010)),
                qualify_event("unknown", "passed", Some("04-build.log"), end),
            ]
            .concat();
            let child = qualify_child(17.0, Some(1_005_000));
            let timeline =
                read_timeline(&[("transcript.log", &transcript), ("04-build.log", &child)]);
            assert_eq!(timeline.children.len(), 1);
            let value = timeline.value();
            assert!(
                value["qualify"].as_array().unwrap().iter().all(|row| {
                    row["cpu_s"].is_null()
                        && row["peak_bytes"].is_null()
                        && row["children"].as_u64() == Some(0)
                }),
                "{value}"
            );
        }
        let transcript = [
            qualify_event("known", "started", Some("04-build.log"), Some(1000)),
            qualify_event("known", "passed", Some("04-build.log"), Some(1010)),
        ]
        .concat();
        let child = qualify_child(17.0, None);
        let timeline = read_timeline(&[("transcript.log", &transcript), ("04-build.log", &child)]);
        assert_eq!(timeline.children.len(), 1);
        let value = timeline.value();
        assert!(value["qualify"][0]["cpu_s"].is_null());
        assert_eq!(value["qualify"][0]["children"].as_u64(), Some(0));
    }

    /// A RELEASE COUNTS NO FULL HASH ONLY WHERE EVERY CUTTER OF IT LOGGED ITS OWN SEAL CALL: the
    /// cutter's call in its transcript makes a mode no record names known, 0 (A2's "0 on a warm
    /// machine"); a run's children's records alone (an older run, whose cutter's calls reached
    /// its terminal only) leave it null; a continued image counts the preceding image's seal
    /// calls and walks with its own, and is null where those cannot be read or that image's own
    /// call reached no log. NEGATIVE CONTROLS: making any call's record know both counts reads 0
    /// for the older run, and reading the continued image's logs alone reads 0 for a release that
    /// hashed once.
    #[test]
    fn a_release_counts_no_full_hash_only_where_every_cutter_logged_its_own_seal_call() {
        let call = |mode: &str| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema":1,"event":"inputs","mode":mode,"entries":9,"secs":0.5})
            )
        };
        let continued = |event: &str, from: &Path| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema":1,"event":event,"from":from.display().to_string()})
            )
        };
        let counts = |value: &aterm_json::Value| {
            (
                value["full_hashes"].as_u64(),
                value["seal_rechecks"].as_u64(),
                value["disk_walks"].as_u64(),
            )
        };
        let write = |logs: &[(&str, String)]| {
            let dir = aterm_tempfile::tempdir().unwrap();
            for (name, text) in logs {
                std::fs::write(dir.path().join(name), text).unwrap();
            }
            dir
        };
        let of = |dir: &Path| Timeline::of_logs(dir).unwrap().value();

        let warm = write(&[
            (TRANSCRIPT, call("adopted")),
            ("05-build.log", call("rechecked")),
        ]);
        assert_eq!(counts(&of(warm.path())), (Some(0), Some(2), None));

        let older = write(&[(
            "05-build.log",
            [call("adopted"), call("rechecked")].concat(),
        )]);
        assert_eq!(counts(&of(older.path())), (None, Some(2), None));

        // A bump release: the outer image hashed at step 1, then exec'd R's own cutter.
        let outer = write(&[
            (TRANSCRIPT, call("hashed")),
            (
                "01-checks.log",
                "verify-event: {\"schema\":1,\"event\":\"disk\",\"mode\":\"claimed\",\"walks\":1}\n"
                    .to_owned(),
            ),
        ]);
        let rerun = write(&[
            (
                TRANSCRIPT,
                [continued("handed", outer.path()), call("adopted")].concat(),
            ),
            ("06-verify.log", call("rechecked")),
        ]);
        assert_eq!(counts(&of(rerun.path())), (Some(1), Some(2), Some(1)));

        let gone = outer.path().join("gone");
        let lost = write(&[(
            TRANSCRIPT,
            [continued("continued", &gone), call("adopted")].concat(),
        )]);
        assert_eq!(counts(&of(lost.path())), (None, Some(1), None));

        let unlogged = write(&[("01-checks.log", call("adopted"))]);
        let after_unlogged = write(&[(
            TRANSCRIPT,
            [continued("continued", unlogged.path()), call("adopted")].concat(),
        )]);
        assert_eq!(counts(&of(after_unlogged.path())), (None, Some(2), None));

        let none = write(&[(TRANSCRIPT, String::new())]);
        assert_eq!(counts(&of(none.path())), (None, None, None));
    }

    /// A failed test case dates the red when libtest reported it, before its stage ends; a stage
    /// that states when it ended is dated by that. A case its block spared (NOT RUN: refused, out
    /// of disk), said earlier, dates nothing.
    #[test]
    fn a_failed_case_dates_the_red_before_its_stage_ends() {
        let transcript = [
            stage("verify", "started", 2000, 0, 0),
            stage("verify", "FAIL", 2600, 0, 600),
        ]
        .concat();
        let verify = [
            "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":3,\"stage\":\"test\",\"started_at_unix_ms\":2001000}\n",
            "verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"NOT RUN\",\"test\":\"-p x --lib\",\"case\":\"spared\",\"at_unix_ms\":2010000}\n",
            "verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"FAIL\",\"test\":\"-p x --lib\",\"case\":\"t\",\"at_unix_ms\":2042000}\n",
            "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":3,\"stage\":\"test\",\"ended_at_unix_ms\":2500000,\"execution_s\":10.0,\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":\"x\"}]}\n",
        ]
        .concat();
        let value = timeline_of(&[("transcript.log", &transcript), ("06-verify.log", &verify)]);
        assert_eq!(value["time_to_first_red_s"].as_f64(), Some(42.0), "{value}");
        assert_eq!(value["first_red"]["case"].as_str(), Some("t"));
        assert_eq!(
            value["branches"][0]["ended_at_unix_ms"].as_u64(),
            Some(2_500_000)
        );
    }

    /// An older run's logs, with none of the newer records, answer null for what they never
    /// said rather than counting it as nothing.
    #[test]
    fn records_a_run_never_wrote_are_null_not_zero() {
        let value = timeline_of(&[(
            "transcript.log",
            &[
                stage("checks", "started", 5, 0, 0),
                stage("checks", "ok", 6, 0, 1),
            ]
            .concat(),
        )]);
        for field in [
            "time_to_first_red_s",
            "full_hashes",
            "disk_walks",
            "units",
            "output_bytes",
            "publish_phases",
        ] {
            assert!(value[field].is_null(), "{field}: {value}");
        }
        assert_eq!(value["critical_path_s"].as_f64(), Some(1.0));
    }

    /// The publish step splits at the engine's own section headers; the span before the first
    /// is promote, whose sections the engine replays only after it ends.
    #[test]
    fn publish_phases_split_at_the_engines_section_headers() {
        let transcript = [
            stage("publish", "started", 100, 0, 0),
            stage("publish", "ok", 200, 0, 100),
        ]
        .concat();
        let line = |at: u64, text: &str| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema":1,"event":"publish-line","at_unix_ms":at,"line":text})
            )
        };
        let publish = [
            line(110_000, "  delegating to pub promote"),
            line(160_000, "== sign-release aterm v1"),
            line(170_000, "uploading x"),
            line(190_000, "== done"),
            "release-event: {\"schema\":1,\"event\":\"step-child-finished\",\"step\":\"publish\",\"exit_code\":0,\"started_at_unix_ms\":100000,\"ended_at_unix_ms\":195000}\n".to_owned(),
        ]
        .concat();
        let value = timeline_of(&[
            ("transcript.log", &transcript),
            ("08-publish-arrivals.log", &publish),
        ]);
        let phases = value["publish_phases"].as_array().unwrap();
        assert_eq!(phases.len(), 3, "{value}");
        assert_eq!(phases[0]["seconds"].as_f64(), Some(60.0));
        assert_eq!(
            phases[1]["phase"].as_str(),
            Some("== sign-release aterm v1")
        );
        assert_eq!(phases[1]["seconds"].as_f64(), Some(30.0));
        assert_eq!(phases[2]["seconds"].as_f64(), Some(5.0));
    }

    /// A step child's record is never a red: a retried child that exited non-zero is not the
    /// run's first failure.
    #[test]
    fn a_step_childs_exit_is_not_a_red() {
        let value = timeline_of(&[
            (
                "transcript.log",
                &[
                    stage("commit", "started", 1, 0, 0),
                    stage("commit", "ok", 2, 0, 1),
                ]
                .concat(),
            ),
            (
                "03-commit.log",
                "release-event: {\"schema\":1,\"event\":\"step-child-finished\",\"step\":\"commit\",\"exit_code\":1}\n",
            ),
        ]);
        assert!(value["time_to_first_red_s"].is_null(), "{value}");
        assert_eq!(value["steps"][0]["children"].as_u64(), Some(1));
    }
}

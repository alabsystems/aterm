// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where one release run's time and CPU went, read back from the records its steps left
//! (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): the release's `release-stage`
//! events in `transcript.log`, step 4's `qualify-branch` events there (each branch's start, its
//! staging push and its end, and the log it wrote), and in every step log the
//! `step-child-finished` record of each
//! child (sampled descendant CPU, starvation and peak), verify's `stage-started`/`stage-finished`, the slices'
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

/// One `step-child-finished` record: its log, end and observed resources.
struct ChildRow {
    log: String,
    ended_at_unix_ms: Option<u64>,
    cpu_s: Option<f64>,
    starved_s: Option<f64>,
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
    starved_s: Option<f64>,
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
                add_f(&mut totals.starved_s, event["starved_s"].as_f64());
                if let Some(peak) = event["peak_bytes"].as_u64() {
                    totals.peak_bytes = Some(totals.peak_bytes.map_or(peak, |p| p.max(peak)));
                }
                if self.children.len() < MAX_ROWS {
                    self.children.push(ChildRow {
                        log: log.to_owned(),
                        ended_at_unix_ms: event["ended_at_unix_ms"].as_u64(),
                        cpu_s: event["child_cpu"]["observed_s"].as_f64(),
                        starved_s: event["starved_s"].as_f64(),
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
            add_f(&mut total.starved_s, child.starved_s);
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
                    "cpu_s": totals.cpu_s, "starved_s": totals.starved_s, "peak_bytes": totals.peak_bytes,
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
                        "starved_s".to_owned(),
                        aterm_json::Value::from(children.and_then(|c| c.starved_s)),
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
#[path = "release_timeline_tests.rs"]
mod tests;

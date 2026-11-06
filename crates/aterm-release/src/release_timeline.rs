// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where one release run's time and CPU went, read back from the records its steps left
//! (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): the release's `release-stage`
//! events in `transcript.log`, and in every step log the `step-child-finished` record of each
//! child (`wait4`'s CPU and peak), verify's `stage-started`/`stage-finished`, the slices'
//! `build-child-finished`, `compile-finished`, the inputs seal's `inputs`, verify's `disk`
//! record and the publish step's `publish-line` arrivals. A run that another handed the release
//! (a re-run as R's own cutter) counts that run's seal calls and walks with its own.
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
    branches: Vec<aterm_json::Value>,
    /// Verify's open stages, by (log, stage index): when each started.
    open_stages: BTreeMap<(String, u64), Option<u64>>,
    first_red_at_unix_ms: Option<u64>,
    first_red: Option<aterm_json::Value>,
    hashed: Option<u64>,
    rechecked: Option<u64>,
    /// A seal call's record was read from [`TRANSCRIPT`]: the cutter's own calls reach its logs,
    /// so a mode none of the run's records names is one no call took.
    own_seal: bool,
    /// The logs of the run that handed this one the release (a `handed` record in
    /// [`TRANSCRIPT`]): its first steps, its seal call among them, are in them.
    handed_from: Option<PathBuf>,
    /// Whether the handing run's own seal calls reached its logs, once [`Timeline::of_logs`]
    /// read them; `None` where it did not (none named, or a read of the records alone).
    handing_seal: Option<bool>,
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

    /// Book one record read from the log named `log`.
    pub fn observe(&mut self, log: &str, event: &aterm_json::Value) {
        if event["schema"].as_u64() != Some(1) {
            return;
        }
        match event["event"].as_str() {
            Some("release-stage") => self.release_stage(event),
            Some("step-child-finished") => {
                let Some(step) = event["step"].as_str() else {
                    return;
                };
                if self.step_children.len() >= MAX_ROWS && !self.step_children.contains_key(step) {
                    return;
                }
                let totals = self.step_children.entry(step.to_owned()).or_default();
                totals.children += 1;
                add_f(&mut totals.cpu_s, event["child_cpu"]["observed_s"].as_f64());
                if let Some(peak) = event["peak_bytes"].as_u64() {
                    totals.peak_bytes = Some(totals.peak_bytes.map_or(peak, |p| p.max(peak)));
                }
                if step == crate::release::Step::Publish.name() {
                    self.publish_ended_at_unix_ms = event["ended_at_unix_ms"].as_u64();
                }
            }
            Some("stage-started") => {
                if let Some(index) = event["stage_index"].as_u64()
                    && self.open_stages.len() < MAX_ROWS
                {
                    self.open_stages.insert(
                        (log.to_owned(), index),
                        event["started_at_unix_ms"].as_u64(),
                    );
                }
            }
            Some("stage-finished") => self.verify_stage(log, event),
            Some("build-child-finished") => {
                let fresh = event["units"]["fresh"].as_u64();
                let compiled = event["units"]["compiled"].as_u64();
                add_u(&mut self.units_fresh, fresh);
                add_u(&mut self.units_compiled, compiled);
                add_u(&mut self.output_bytes, event["output_bytes"].as_u64());
                self.branch(aterm_json::json!({
                    "kind": "slice", "log": log,
                    "call": event["call"].as_str().map_or_else(
                        || format!(
                            "{} {}",
                            event["package"].as_str().unwrap_or("?"),
                            event["target"].as_str().unwrap_or("native")
                        ),
                        str::to_owned,
                    ),
                    "outcome": event["outcome"],
                    "execution_s": event["execution_s"],
                    "cpu_s": event["child_cpu"]["observed_s"],
                    "peak_bytes": event["peak_bytes"],
                    "units": event["units"],
                    "output_bytes": event["output_bytes"],
                }));
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
            Some("handed") if log == TRANSCRIPT => {
                self.handed_from = event["from"].as_str().map(PathBuf::from);
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

    fn branch(&mut self, row: aterm_json::Value) {
        if self.branches.len() < MAX_ROWS {
            self.branches.push(row);
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

    fn verify_stage(&mut self, log: &str, event: &aterm_json::Value) {
        let started = event["stage_index"]
            .as_u64()
            .and_then(|index| self.open_stages.remove(&(log.to_owned(), index)))
            .flatten();
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
        self.branch(aterm_json::json!({
            "kind": "verify-stage", "log": log,
            "call": event["stage"],
            "started_at_unix_ms": started, "ended_at_unix_ms": ended,
            "waiting_s": event["waiting_s"], "execution_s": execution_s,
            "cpu_s": event["child_cpu"]["observed_s"],
            "starved_s": event["starved_s"],
            "peak_bytes": event["peak_bytes"],
        }));
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
                        && let Ok(event) = aterm_json::from_slice::<aterm_json::Value>(json)
                    {
                        self.observe(log, &event);
                    }
                } else {
                    if !overflow && line.len() + seg.len() <= LINE_LIMIT {
                        line.extend_from_slice(seg);
                        if let Some(json) = line
                            .strip_prefix(b"verify-event: ")
                            .or_else(|| line.strip_prefix(b"release-event: "))
                            && let Ok(event) = aterm_json::from_slice::<aterm_json::Value>(json)
                        {
                            self.observe(log, &event);
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

    /// Read `logs`'s transcript and every step log (`NN-<step>*.log`), in name order, and where
    /// another run handed this one the release, the seal calls and walks its logs record: the
    /// release's, though another run made them.
    /// # Errors
    /// The directory cannot be listed, or a log present cannot be read.
    pub fn of_logs(logs: &Path) -> std::io::Result<Self> {
        let mut timeline = Self::of_run(logs)?;
        if let Some(from) = timeline.handed_from.clone() {
            let handing = Self::of_run(&from).ok();
            timeline.handing_seal = Some(handing.as_ref().is_some_and(|h| h.own_seal));
            if let Some(handing) = handing {
                add_u(&mut timeline.hashed, handing.hashed);
                add_u(&mut timeline.rechecked, handing.rechecked);
                add_u(&mut timeline.disk_walks, handing.disk_walks);
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
        aterm_json::json!(phases)
    }

    /// The timeline as `release status --json` shows it.
    #[must_use]
    pub fn value(&self) -> aterm_json::Value {
        let steps: Vec<_> = self
            .steps
            .iter()
            .map(|row| {
                let children = self.step_children.get(&row.step);
                aterm_json::json!({
                    "step": row.step,
                    "started_at_unix_s": row.started_at_unix_s,
                    "ended_at_unix_s": row.ended_at_unix_s,
                    "outcome": row.outcome,
                    "waiting_s": row.waiting_s,
                    "execution_s": row.execution_s,
                    "cpu_s": children.and_then(|c| c.cpu_s),
                    "peak_bytes": children.and_then(|c| c.peak_bytes),
                    "children": children.map(|c| c.children),
                })
            })
            .collect();
        // Before the qualify graph runs branches beside each other, the steps run one after
        // another, so the critical path is their summed execution (owner waits excluded).
        let mut critical_path_s = None;
        for s in self.steps.iter().filter_map(|row| row.execution_s) {
            *critical_path_s.get_or_insert(0.0) += s;
        }
        let time_to_first_red_s = self
            .first_red_at_unix_ms
            .zip(self.run_started_at_unix_s)
            .map(|(red, start)| red.saturating_sub(start.saturating_mul(1000)) as f64 / 1000.0);
        // Every seal call of the release reached a log where the cutter's own did, and the handing
        // run's too where one handed it on: a mode no record names, no call took.
        let seal_known =
            self.own_seal && (self.handed_from.is_none() || self.handing_seal == Some(true));
        let seal_count = |n: Option<u64>| if seal_known { Some(n.unwrap_or(0)) } else { n };
        let units = match (self.units_fresh, self.units_compiled) {
            (None, None) => aterm_json::Value::Null,
            (fresh, compiled) => aterm_json::json!({"fresh": fresh, "compiled": compiled}),
        };
        aterm_json::json!({
            "run_started_at_unix_s": self.run_started_at_unix_s,
            "critical_path_s": critical_path_s,
            "time_to_first_red_s": time_to_first_red_s,
            "first_red": self.first_red,
            "steps": steps,
            "branches": self.branches,
            "full_hashes": seal_count(self.hashed),
            "seal_rechecks": seal_count(self.rechecked),
            "disk_walks": self.disk_walks,
            "units": units,
            "output_bytes": self.output_bytes,
            "publish_phases": self.publish_phases(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline_of(logs: &[(&str, &str)]) -> aterm_json::Value {
        let dir = aterm_tempfile::tempdir().unwrap();
        for (name, text) in logs {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        Timeline::of_logs(dir.path()).unwrap().value()
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
        .concat();
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

    /// A RELEASE COUNTS NO FULL HASH ONLY WHERE EVERY CUTTER OF IT LOGGED ITS OWN SEAL CALL: the
    /// cutter's call in its transcript makes a mode no record names known, 0 (A2's "0 on a warm
    /// machine"); a run's children's records alone (an older run, whose cutter's calls reached
    /// its terminal only) leave it null; a run handed the release counts the handing run's seal
    /// calls and walks with its own, and is null where those cannot be read or that run's own call
    /// reached no log. NEGATIVE CONTROLS: making any call's record know both counts reads 0 for the
    /// older run, and reading the re-run's logs alone reads 0 for a release that hashed once.
    #[test]
    fn a_release_counts_no_full_hash_only_where_every_cutter_logged_its_own_seal_call() {
        let call = |mode: &str| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema":1,"event":"inputs","mode":mode,"entries":9,"secs":0.5})
            )
        };
        let handed = |from: &Path| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema":1,"event":"handed","from":from.display().to_string()})
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

        // A bump release: the outer cutter hashed at step 1, then handed the release to R's own.
        let outer = write(&[
            (TRANSCRIPT, call("hashed")),
            (
                "01-checks.log",
                "verify-event: {\"schema\":1,\"event\":\"disk\",\"mode\":\"claimed\",\"walks\":1}\n"
                    .to_owned(),
            ),
        ]);
        let rerun = write(&[
            (TRANSCRIPT, [handed(outer.path()), call("adopted")].concat()),
            ("06-verify.log", call("rechecked")),
        ]);
        assert_eq!(counts(&of(rerun.path())), (Some(1), Some(2), Some(1)));

        let gone = outer.path().join("gone");
        let lost = write(&[(TRANSCRIPT, [handed(&gone), call("adopted")].concat())]);
        assert_eq!(counts(&of(lost.path())), (None, Some(1), None));

        let unlogged = write(&[("01-checks.log", call("adopted"))]);
        let after_unlogged = write(&[(
            TRANSCRIPT,
            [handed(unlogged.path()), call("adopted")].concat(),
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

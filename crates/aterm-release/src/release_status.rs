// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only release progress, including failures emitted before verification ends.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

const MAX_OPEN_STAGES: usize = 256;

use crate::ledger::{Error, GitCli};

#[cfg(test)]
#[path = "release_commit_gate_stream_tests.rs"]
mod commit_gate_stream_tests;

struct OpenStage {
    title: String,
    started_at_unix_ms: Option<u64>,
}

#[derive(Default)]
struct Events {
    first_failure: Option<aterm_json::Value>,
    latest: Option<aterm_json::Value>,
    latest_release_stage: Option<aterm_json::Value>,
    count: u64,
    rejected_lines: u64,
    malformed_lines: u64,
    unsupported_lines: u64,
    unprojected_lines: u64,
    unavailable_observations: u64,
    storage_markers: u64,
    incomplete_tail: bool,
    stage_activity_observed: bool,
    open_stages: BTreeMap<u64, OpenStage>,
}

fn valid_stage_title(title: &str) -> bool {
    !title.is_empty() && title.len() <= 4096 && !title.chars().any(char::is_control)
}

impl Events {
    fn complete(&self) -> bool {
        self.rejected_lines == 0 && self.unavailable_observations == 0 && !self.incomplete_tail
    }

    fn reject(&mut self, malformed: bool) {
        self.rejected_lines += 1;
        if malformed {
            self.malformed_lines += 1;
        } else {
            self.unsupported_lines += 1;
        }
    }

    fn observe_record(
        &mut self,
        event: aterm_json::Value,
        project_failure: bool,
        unprojected: bool,
    ) {
        if !self.observe_stage(&event) {
            self.reject(true);
        }
        self.count += 1;
        self.unprojected_lines += u64::from(unprojected);
        if project_failure {
            self.observe_failure(&event);
        }
        if event["event"].as_str() == Some("release-stage") {
            if event["stage"].as_str().is_some_and(valid_stage_title) {
                self.latest_release_stage = Some(event.clone());
            } else {
                self.reject(true);
            }
        }
        self.latest = Some(event);
    }

    fn observe_failure(&mut self, event: &aterm_json::Value) {
        if self.first_failure.is_none() && failed(event) {
            self.first_failure = Some(event.clone());
        }
        let Some(first) = self.first_failure.as_mut() else {
            return;
        };
        if first["event"].as_str() != Some("test-case-failed")
            || first["binary_completed"].as_bool() != Some(false)
            || event["event"].as_str() != Some("test-binary-finished")
        {
            return;
        }
        let Some(test) = first["test"].as_str().filter(|test| !test.is_empty()) else {
            return;
        };
        if event["test"].as_str() != Some(test) {
            return;
        }
        // Keep the first case and its observation time. Completion enriches that
        // failure rather than replacing it with a later success or another case.
        let fields = first.as_object_mut().expect("a failure event is an object");
        if let Some(detail) = fields.get("detail").cloned() {
            fields.insert("initial_detail".into(), detail);
        }
        fields.insert("binary_completed".into(), true.into());
        fields.insert("binary_result".into(), event.clone());
        fields.insert(
            "detail".into(),
            "test binary finished; binary_result records its final outcome and failure details"
                .into(),
        );
    }

    fn observe_stage(&mut self, event: &aterm_json::Value) -> bool {
        let started = event["event"].as_str() == Some("stage-started");
        let finished = event["event"].as_str() == Some("stage-finished");
        if !started && !finished {
            return true;
        }
        let Some(index) = event.get("stage_index") else {
            return !started; // Older completion-only logs carry no activity evidence.
        };
        let (Some(index), Some(title)) = (index.as_u64(), event["stage"].as_str()) else {
            return false;
        };
        if !valid_stage_title(title) {
            return false;
        }
        if self
            .open_stages
            .get(&index)
            .is_some_and(|old| old.title != title)
        {
            return false;
        }
        if started {
            if self.open_stages.len() >= MAX_OPEN_STAGES && !self.open_stages.contains_key(&index) {
                return false;
            }
            let stamp = event.get("started_at_unix_ms");
            if stamp.is_some_and(|stamp| !stamp.is_null() && stamp.as_u64().is_none()) {
                return false;
            }
            self.open_stages.entry(index).or_insert_with(|| OpenStage {
                title: title.to_owned(),
                started_at_unix_ms: stamp.and_then(aterm_json::Value::as_u64),
            });
        } else {
            self.open_stages.remove(&index);
        }
        self.stage_activity_observed = true;
        true
    }

    fn unfinished_stages(&self) -> aterm_json::Value {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
        self.unfinished_stages_at(now)
    }

    fn unfinished_stages_at(&self, now: Option<u64>) -> aterm_json::Value {
        if !self.stage_activity_observed {
            return aterm_json::Value::Null;
        }
        let stages: Vec<_> = self
            .open_stages
            .iter()
            .map(|(index, stage)| {
                let mut fields = aterm_json::Map::from([
                    ("stage_index".to_string(), (*index).into()),
                    ("stage".to_string(), stage.title.clone().into()),
                ]);
                if let Some(started) = stage.started_at_unix_ms {
                    fields.insert("started_at_unix_ms".into(), started.into());
                    fields.insert(
                        "elapsed_wall_ms".into(),
                        now.and_then(|now| now.checked_sub(started)).into(),
                    );
                }
                aterm_json::Value::Object(fields)
            })
            .collect();
        aterm_json::Value::Array(stages)
    }
}

fn failed(event: &aterm_json::Value) -> bool {
    let failure = |value: &aterm_json::Value| matches!(value.as_str(), Some("FAIL" | "NOT RUN"));
    failure(&event["outcome"])
        || event["decisions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| failure(&row["outcome"])))
}

// Producer clocks have different precision. An integer second denotes that
// whole second, not an invented millisecond at its beginning. Only disjoint
// observation intervals establish order across otherwise independent files.
fn failure_time(event: &aterm_json::Value) -> Option<(u128, u128)> {
    let millis = event.get("at_unix_ms");
    let seconds = event.get("at_unix_s");
    match (millis, seconds) {
        (Some(ms), seconds) => {
            let ms = ms.as_u64().filter(|ms| *ms > 0)?;
            if let Some(seconds) = seconds
                && seconds.as_u64()? != ms / 1000
            {
                return None;
            }
            Some((u128::from(ms), u128::from(ms) + 1))
        }
        (None, Some(seconds)) => {
            let seconds = seconds.as_u64().filter(|seconds| *seconds > 0)?;
            Some((u128::from(seconds) * 1000, (u128::from(seconds) + 1) * 1000))
        }
        (None, None) => None,
    }
}

struct FailureReport {
    first: aterm_json::Value,
    candidates: Vec<aterm_json::Value>,
    ordering: &'static str,
}

fn reported_failure(
    verification: Option<&aterm_json::Value>,
    release: Option<&aterm_json::Value>,
) -> FailureReport {
    let known = |first: &aterm_json::Value, ordering| FailureReport {
        first: first.clone(),
        candidates: Vec::new(),
        ordering,
    };
    match (verification, release) {
        (None, None) => FailureReport {
            first: aterm_json::Value::Null,
            candidates: Vec::new(),
            ordering: "none",
        },
        (Some(first), None) | (None, Some(first)) => known(first, "one-stream"),
        (Some(a), Some(b)) if a == b => known(a, "same-observation"),
        (Some(a), Some(b)) => {
            if let (Some((a_start, a_end)), Some((b_start, b_end))) =
                (failure_time(a), failure_time(b))
            {
                if a_end <= b_start {
                    return known(a, "timestamp");
                }
                if b_end <= a_start {
                    return known(b, "timestamp");
                }
            }
            // Channel priority is not time order. Preserve both findings when
            // missing, malformed or overlapping clocks cannot name the first.
            FailureReport {
                first: aterm_json::Value::Null,
                candidates: vec![a.clone(), b.clone()],
                ordering: "unknown",
            }
        }
    }
}

/// The longest log line [`event_lines`] reads: a test may print an arbitrarily long line, and
/// the verifier's and the release's structured records are small.
pub(crate) const LINE_LIMIT: usize = 64 * 1024;

/// One line of a release or verify log, as [`event_lines`] hands it on.
pub(crate) enum Line<'a> {
    /// A `verify-event: ` line's JSON text.
    Verify(&'a [u8]),
    /// A `release-event: ` line's JSON text.
    Release(&'a [u8]),
    /// A line longer than [`LINE_LIMIT`], read past and never retained.
    TooLong,
}

/// Read `reader`'s structured lines (`verify-event: `, `release-event: `) in bounded memory,
/// handing each to `each`, every other line read past: the one reader of these lines that
/// `ship status` and the check records ([`crate::check_record`]) share. Returns whether the read
/// ended inside a line: a partly written last record is not a decision.
///
/// # Errors
/// The read itself.
pub(crate) fn event_lines(
    mut reader: impl Read,
    mut each: impl FnMut(Line<'_>),
) -> std::io::Result<bool> {
    let mut line = Vec::new();
    let mut overflow = false;
    let mut bytes = [0; 8192];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            return Ok(overflow || !line.is_empty());
        }
        let mut rest = &bytes[..n];
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let chunk = &rest[..pos];
            rest = &rest[pos + 1..];
            if line.is_empty() && !overflow {
                if chunk.len() > LINE_LIMIT {
                    each(Line::TooLong);
                } else if let Some(json) = chunk.strip_prefix(b"verify-event: ") {
                    each(Line::Verify(json));
                } else if let Some(json) = chunk.strip_prefix(b"release-event: ") {
                    each(Line::Release(json));
                }
            } else {
                if !overflow {
                    if line.len() + chunk.len() <= LINE_LIMIT {
                        line.extend_from_slice(chunk);
                    } else {
                        overflow = true;
                    }
                }
                if overflow {
                    each(Line::TooLong);
                } else if let Some(json) = line.strip_prefix(b"verify-event: ") {
                    each(Line::Verify(json));
                } else if let Some(json) = line.strip_prefix(b"release-event: ") {
                    each(Line::Release(json));
                }
                line.clear();
                overflow = false;
            }
        }
        if !rest.is_empty() && !overflow {
            let room = LINE_LIMIT.saturating_sub(line.len());
            if rest.len() <= room {
                line.extend_from_slice(rest);
            } else {
                line.extend_from_slice(&rest[..room]);
                overflow = true;
            }
        }
    }
}

fn read_events(reader: impl Read) -> std::io::Result<Events> {
    use aterm_verify::event_log::{EventKind, Observation};
    let mut result = Events::default();
    let incomplete_tail = event_lines(reader, |line| {
        let (json, release) = match line {
            Line::TooLong => {
                result.reject(true);
                return;
            }
            Line::Verify(json) => (json, false),
            Line::Release(json) => (json, true),
        };
        let Ok(json) = std::str::from_utf8(json) else {
            result.reject(true);
            return;
        };
        // Release-owned records have a separate protocol. Verifier kinds never
        // acquire a second name list here, even when mirrored in a release log.
        if release
            && let Some((event, _)) = aterm_verify::event_log::diagnostic_event(json)
            && matches!(
                event["event"].as_str(),
                Some(
                    "commit-gate-failed"
                        | "release-stage"
                        | "build-child-finished"
                        | "step-child-finished"
                        | "inputs"
                        | "handed"
                        | "continued"
                        | "target-retired"
                        | "publish-line"
                )
            )
        {
            result.observe_record(event, true, false);
            return;
        }
        match aterm_verify::event_log::observation(json) {
            Observation::Known { kind, value } => {
                if kind == EventKind::ObservationUnavailable {
                    result.unavailable_observations += 1;
                }
                let projected = kind.projects_failure() || kind == EventKind::StageStarted;
                result.observe_record(value, kind.projects_failure(), !projected);
            }
            Observation::StorageMarker => result.storage_markers += 1,
            Observation::UnknownKind => result.reject(false),
            Observation::Malformed => result.reject(true),
        }
    })?;
    result.incomplete_tail = incomplete_tail;
    Ok(result)
}

struct Journals {
    current: Option<crate::release::Journal>,
    previous: Option<crate::release::Journal>,
    binding: &'static str,
}

fn assign_journal(
    journal: Option<crate::release::Journal>,
    event: Option<&aterm_json::Value>,
    running: bool,
) -> Journals {
    let binding = match (&journal, event) {
        (None, _) => "absent",
        (Some(_), None) if running => "unresolved",
        (Some(_), None) => "retained",
        (Some(journal), Some(event)) => {
            match (event["source"].as_str(), event["version"].as_str()) {
                (Some(source), Some(version))
                    if source == journal.commit && version == journal.version =>
                {
                    "current"
                }
                (None | Some("?"), _) | (_, None | Some("?")) => "unresolved",
                _ => "previous",
            }
        }
    };
    if matches!(binding, "current" | "retained") {
        Journals {
            current: journal,
            previous: None,
            binding,
        }
    } else {
        Journals {
            current: None,
            previous: journal,
            binding,
        }
    }
}

fn valid_log_dir(text: &str) -> Option<std::path::PathBuf> {
    if text.is_empty() || text.len() > 8192 || text.chars().any(char::is_control) {
        return None;
    }
    let path = std::path::PathBuf::from(text);
    path.is_absolute().then_some(path)
}

fn read_latest_run(path: &Path) -> Result<Option<std::path::PathBuf>, Error> {
    let bytes = match crate::durable::read_regular_bytes(path, 8192) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
            return Err(Error::new("latest-run log pointer exceeds size limit"));
        }
        Err(error) => return Err(Error::new(error.to_string())),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("latest-run log pointer is not valid UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    valid_log_dir(text)
        .map(Some)
        .ok_or_else(|| Error::new("latest-run log pointer is empty or invalid"))
}

fn resolve_logs(
    holder: Option<&crate::release::Holder>,
    latest_run: &Path,
) -> Result<Option<std::path::PathBuf>, Error> {
    if let Some(holder) = holder
        && !holder.logs.is_empty()
    {
        return valid_log_dir(&holder.logs)
            .map(Some)
            .ok_or_else(|| Error::new("running holder log pointer is invalid"));
    }
    read_latest_run(latest_run)
}

/// Step 1's preflight verifiers write into its own log, and step 4's `verify release` into its
/// tree branch's; every later step keeps the bundle half's, the final qualification.
fn check_log(logs: &Path, step: Option<&str>) -> std::path::PathBuf {
    use crate::release::{Branch, Step};
    let named = |phase: Step| Some(phase.name()) == step;
    if named(Step::Checks) {
        logs.join(format!(
            "{:02}-{}.log",
            Step::Checks.number(),
            Step::Checks.name()
        ))
    } else if named(Step::Qualify) {
        logs.join(format!(
            "{:02}-{}-{}.log",
            Step::Qualify.number(),
            Step::Qualify.name(),
            Branch::Tree.name()
        ))
    } else {
        logs.join(format!(
            "{:02}-{}.log",
            Step::Bundle.number(),
            Step::Bundle.name()
        ))
    }
}

/// Read the journal and its logs without claiming, resuming or building a release.
///
/// # Errors
/// The repository, journal, log pointer or existing verification log cannot be read.
pub fn run(repo: &Path, json: bool) -> Result<(), Error> {
    let dir = crate::release::release_dir(&GitCli::new(repo)).map_err(Error::new)?;
    let journal = crate::release::Journal::load(&dir.join(crate::release::JOURNAL_FILE))
        .map_err(Error::new)?;
    let holder = crate::release::live_holder(
        &dir.join(crate::release::RUNNING_FILE),
        &mut crate::release_steps::started_of,
    );
    let logs = resolve_logs(holder.as_ref(), &dir.join("latest-run"))?;
    let transcript = logs.as_ref().map(|path| path.join("transcript.log"));
    let release_events = match transcript.as_deref().map(crate::durable::open_regular_file) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let step = release_events
        .latest_release_stage
        .as_ref()
        .and_then(|event| event["stage"].as_str())
        .map(str::to_string)
        .or_else(|| {
            holder
                .as_ref()
                .and_then(|holder| holder.step.as_deref())
                .filter(|step| valid_stage_title(step))
                .map(str::to_string)
        });
    let log = logs.as_ref().map(|path| check_log(path, step.as_deref()));
    let events = match log.as_deref().map(crate::durable::open_regular_file) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let timeline = match logs
        .as_deref()
        .map(crate::release_timeline::Timeline::of_logs)
    {
        Some(Ok(timeline)) => timeline.value(),
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => aterm_json::Value::Null,
    };
    let state = if holder.is_some() {
        "running"
    } else if journal.is_some() || logs.is_some() {
        "not-running"
    } else {
        "none"
    };
    let journals = assign_journal(
        journal,
        release_events.latest_release_stage.as_ref(),
        holder.is_some(),
    );
    let journal = journals.current;
    let measurement_exception = journal
        .as_ref()
        .and_then(|journal| {
            journal.artifact.as_ref().map(|artifact| {
                crate::check_record::accepted(
                    &GitCli::new(repo),
                    &journal.commit,
                    &artifact.macho_sha256,
                    crate::build_inputs::current,
                )
            })
        })
        .transpose()
        .map_err(Error::new)?
        .flatten();
    let unfinished_verification_stages = events.unfinished_stages();
    let observations_complete = events.complete() && release_events.complete();
    let rejected_log_lines = events
        .rejected_lines
        .saturating_add(release_events.rejected_lines);
    let failure = reported_failure(
        events.first_failure.as_ref(),
        release_events.first_failure.as_ref(),
    );
    let latest_release_event = release_events.latest.unwrap_or(aterm_json::Value::Null);
    let latest_verification_event = events.latest.unwrap_or(aterm_json::Value::Null);
    let op_status = operational_status(logs.as_deref());
    let value = aterm_json::Value::Object(aterm_json::Map::from([
        ("schema".to_string(), aterm_json::Value::Number(1u64.into())),
        (
            "state".to_string(),
            aterm_json::Value::String(state.to_string()),
        ),
        (
            "journal_finished".to_string(),
            aterm_json::Value::Bool(
                journal
                    .as_ref()
                    .is_some_and(crate::release::Journal::finished),
            ),
        ),
        (
            "measurement_exception".to_string(),
            aterm_json::json!(measurement_exception),
        ),
        ("journal".to_string(), aterm_json::json!(journal)),
        (
            "journal_binding".to_string(),
            aterm_json::Value::String(journals.binding.to_string()),
        ),
        (
            "previous_journal".to_string(),
            aterm_json::json!(journals.previous),
        ),
        (
            "pid".to_string(),
            holder.as_ref().map_or(aterm_json::Value::Null, |holder| {
                aterm_json::Value::Number((holder.pid as u64).into())
            }),
        ),
        (
            "step".to_string(),
            step.map_or(aterm_json::Value::Null, aterm_json::Value::String),
        ),
        ("release_log".to_string(), aterm_json::json!(transcript)),
        (
            "release_events".to_string(),
            aterm_json::Value::Number((release_events.count as u64).into()),
        ),
        ("latest_release_event".to_string(), latest_release_event),
        (
            "observations_complete".to_string(),
            aterm_json::Value::Bool(observations_complete),
        ),
        (
            "malformed_log_lines".to_string(),
            aterm_json::json!(
                events
                    .malformed_lines
                    .saturating_add(release_events.malformed_lines)
            ),
        ),
        (
            "unsupported_log_lines".to_string(),
            aterm_json::json!(
                events
                    .unsupported_lines
                    .saturating_add(release_events.unsupported_lines)
            ),
        ),
        (
            "unprojected_log_lines".to_string(),
            aterm_json::json!(
                events
                    .unprojected_lines
                    .saturating_add(release_events.unprojected_lines)
            ),
        ),
        (
            "unavailable_observations".to_string(),
            aterm_json::json!(
                events
                    .unavailable_observations
                    .saturating_add(release_events.unavailable_observations)
            ),
        ),
        (
            "partial_log_tail".to_string(),
            aterm_json::json!(events.incomplete_tail || release_events.incomplete_tail),
        ),
        ("logs".to_string(), aterm_json::json!(logs)),
        ("operational_status".to_string(), op_status),
        ("verification_log".to_string(), aterm_json::json!(log)),
        (
            "verification_events".to_string(),
            aterm_json::Value::Number((events.count as u64).into()),
        ),
        (
            "unfinished_verification_stages".to_string(),
            unfinished_verification_stages,
        ),
        ("first_reported_failure".to_string(), failure.first),
        (
            "first_reported_failure_ordering".to_string(),
            failure.ordering.into(),
        ),
        (
            "reported_failure_candidates".to_string(),
            aterm_json::json!(failure.candidates),
        ),
        (
            "latest_verification_event".to_string(),
            latest_verification_event,
        ),
        (
            "rejected_log_lines".to_string(),
            aterm_json::Value::Number((rejected_log_lines as u64).into()),
        ),
        ("timeline".to_string(), timeline),
    ]));
    if json {
        println!("{value}");
    } else {
        println!("{}", text_status(&value));
    }
    Ok(())
}

fn scrub_controls(text: String) -> String {
    if !text.chars().any(char::is_control) {
        return text;
    }
    text.chars()
        .map(|ch| if ch.is_control() { '\u{fffd}' } else { ch })
        .collect()
}

fn text_status(value: &aterm_json::Value) -> String {
    let mut lines = vec![scrub_controls(format!(
        "release: {}",
        value["state"].as_str().unwrap_or("unknown")
    ))];
    for (label, field) in [
        ("stage", "step"),
        ("release log", "release_log"),
        ("verification log", "verification_log"),
    ] {
        if let Some(text) = value[field].as_str() {
            lines.push(scrub_controls(format!("{label}: {text}")));
        }
    }
    if let Some(stages) = value["unfinished_verification_stages"].as_array() {
        for stage in stages {
            if let Some(title) = stage["stage"].as_str() {
                let age = stage["elapsed_wall_ms"]
                    .as_u64()
                    .map_or_else(String::new, |age| {
                        format!(" ({} s since start, wall clock)", age / 1000)
                    });
                lines.push(scrub_controls(format!(
                    "unfinished verification stage: {title}{age}"
                )));
            }
        }
    } else if value["state"].as_str() == Some("running") && value["step"].as_str() == Some("verify")
    {
        lines.push("verification stage activity: not recorded".into());
    }
    for (label, field) in [
        ("first reported failure", "first_reported_failure"),
        ("latest release event", "latest_release_event"),
        ("latest verification event", "latest_verification_event"),
    ] {
        if !value[field].is_null() {
            lines.push(scrub_controls(format!("{label}: {}", value[field])));
        }
    }
    if let Some(line) = timeline_line(&value["timeline"]) {
        lines.push(line);
    }
    if value["first_reported_failure_ordering"].as_str() == Some("unknown") {
        lines.push(scrub_controls(format!(
            "reported failures (order unknown): {}",
            value["reported_failure_candidates"]
        )));
    }
    if value["observations_complete"].as_bool() == Some(false) {
        let mut reasons = Vec::new();
        for (field, reason) in [
            ("malformed_log_lines", "malformed records"),
            ("unsupported_log_lines", "unsupported event kinds"),
            (
                "unavailable_observations",
                "producer observations unavailable",
            ),
        ] {
            if value[field].as_u64().is_some_and(|count| count != 0) {
                reasons.push(reason);
            }
        }
        if value["partial_log_tail"].as_bool() == Some(true) {
            reasons.push("partial final record");
        }
        let detail = if reasons.is_empty() {
            String::new()
        } else {
            format!("; {}", reasons.join(", "))
        };
        lines.push(format!("observations: incomplete{detail}"));
    }
    lines.join("\n")
}

/// One line of where the run's time went, from the timeline's totals; `None` before any step ended.
fn timeline_line(timeline: &aterm_json::Value) -> Option<String> {
    let path = timeline["critical_path_s"].as_f64()?;
    let mut words = vec![format!("timeline: {path:.0} s on the critical path")];
    if let Some(red) = timeline["time_to_first_red_s"].as_f64() {
        words.push(format!("first red after {red:.0} s"));
    }
    if let Some(hashes) = timeline["full_hashes"].as_u64() {
        words.push(format!("{hashes} full input hash(es)"));
    }
    if let Some(walks) = timeline["disk_walks"].as_u64() {
        words.push(format!("{walks} disk walk(s)"));
    }
    if let (Some(fresh), Some(compiled)) = (
        timeline["units"]["fresh"].as_u64(),
        timeline["units"]["compiled"].as_u64(),
    ) {
        words.push(format!("{fresh} units fresh / {compiled} compiled"));
    }
    Some(words.join(", "))
}

fn operational_status(logs: Option<&Path>) -> aterm_json::Value {
    let Some(logs) = logs else {
        return aterm_json::Value::Null;
    };
    let result = (|| -> Result<aterm_json::Value, String> {
        let bytes =
            crate::durable::read_regular_bytes(&logs.join("operational-status.json"), 256 * 1024)
                .map_err(|e| {
                if e.kind() == std::io::ErrorKind::FileTooLarge {
                    "operational record exceeds size limit".to_owned()
                } else {
                    e.to_string()
                }
            })?;
        let record: aterm_json::Value =
            aterm_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if record["schema"].as_u64() != Some(1)
            || record["attempt"].as_str() != Some(logs.to_string_lossy().as_ref())
        {
            return Err("operational record belongs to another attempt".into());
        }
        Ok(record)
    })();
    result.unwrap_or_else(|reason| {
        aterm_json::Value::Object(aterm_json::Map::from([
            (
                "state".to_string(),
                aterm_json::Value::String("unknown".to_string()),
            ),
            ("reason".to_string(), aterm_json::Value::String(reason)),
        ]))
    })
}

#[cfg(test)]
#[path = "release_status_tests.rs"]
mod tests;

#[cfg(test)]
#[test]
fn a_running_test_case_failure_is_visible_before_the_binary_finishes() {
    let events = read_events(b"verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"FAIL\",\"case\":\"broken\",\"binary_completed\":false}\n".as_slice()).unwrap();
    assert_eq!(
        events.first_failure.unwrap()["case"].as_str(),
        Some("broken")
    );
    assert!(!events.incomplete_tail);
}

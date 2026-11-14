// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's own file of its events, apart from its output (`--events`): what a release's cutter
//! keeps its records from (A19) and `verify land` judges by (B10).
//!
//! Check, binary and observed case events go to the run's output too, which its children share: there a child's unfinished line,
//! or a line longer than a pipe writes at once, can land inside one. Here each line is one
//! `write_all` under one lock, to a file opened close-on-exec, so no child inherits it and no two
//! lines interleave.

use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Discriminators emitted by this verifier, including its two explicit measurement imports.
/// Recognizing a kind establishes no execution, payload truth, custody or verdict authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    InvocationStarted,
    ObservationUnavailable,
    MeasureDriver,
    CellPrerequisiteUnavailable,
    /// What one cross-target cell's cargo calls took.
    CellFinished,
    Disk,
    RunFinished,
    StageStarted,
    StageFinished,
    RowStarted,
    RowFinished,
    CheckFinished,
    CompileUnitMessageUnavailable,
    CompileUnitDispatched,
    CompileUnitFinished,
    CompileUnitTimed,
    CompileAdmissionWait,
    CompileFinished,
    CompileMessageObserved,
    TestCaseRunningLong,
    TestCaseFailed,
    TestBinaryFinished,
    TestDispatchWait,
    /// What a test run's claim of cores asked and was granted, with the load as it was granted.
    TestCoreGrant,
    TestCpuAccounted,
    TestPhaseStarted,
    TestPhaseEnded,
    InputItemPart,
    InputItemFinished,
    LintPackageFinished,
    AppkitGroupFinished,
    MeasureRowStarted,
    MeasureRowFinished,
}

impl EventKind {
    /// The existing release-status outcome projections, not a check verdict.
    #[must_use]
    pub const fn projects_failure(self) -> bool {
        matches!(
            self,
            Self::StageFinished
                | Self::CheckFinished
                | Self::TestCaseFailed
                | Self::TestBinaryFinished
        )
    }
}

/// Construct an ordinary producer envelope from a typed discriminator and unwrapped metadata.
/// A malformed producer payload loses this observation rather than inventing a check result.
pub(crate) fn event_value(kind: EventKind, mut payload: aterm_json::Value) -> aterm_json::Value {
    if let Some(fields) = payload.as_object_mut()
        && !fields.contains_key("schema")
        && !fields.contains_key("event")
    {
        fields.insert("schema".into(), 1_u64.into());
        fields.insert("event".into(), aterm_json::json!(kind));
        return payload;
    }
    aterm_json::json!({"schema":1,"event":EventKind::ObservationUnavailable,
        "producer_event":kind,"reason":"producer payload is not an unwrapped object"})
}

/// Diagnostic classification only. Known metadata need not have a semantic projection.
pub enum Observation {
    /// A known kind and valid envelope; supported strict progress payloads also passed their decoder.
    Known {
        kind: EventKind,
        value: aterm_json::Value,
    },
    /// An exact private storage marker, not an invocation or execution observation.
    StorageMarker,
    /// A valid envelope with an unsupported discriminator.
    UnknownKind,
    /// Invalid JSON/envelope or malformed fields of a supported strict progress record.
    Malformed,
}

/// Read a raw diagnostic without granting its metadata execution or qualification authority.
/// Envelope bounds and duplicate-header checks are shared with all verifier log readers.
#[must_use]
pub fn observation(json: &str) -> Observation {
    if [
        crate::run_log::PRIVATE_EVENTS_HEADER,
        crate::run_log::PRIVATE_HUMAN_HEADER,
    ]
    .iter()
    .any(|header| {
        header
            .strip_prefix("verify-event: ")
            .and_then(|s| s.strip_suffix('\n'))
            == Some(json)
    }) {
        return Observation::StorageMarker;
    }
    let Some(record) = raw_event(json) else {
        return Observation::Malformed;
    };
    let Ok(kind) = aterm_json::from_value::<EventKind>(record.value["event"].clone()) else {
        return Observation::UnknownKind;
    };
    if crate::status::supported_progress_payload(kind, json) == Some(false) {
        return Observation::Malformed;
    }
    Observation::Known {
        kind,
        value: record.value,
    }
}

/// Prepare once, mirror without holding a file lock, then persist the identical bytes.
pub(crate) fn emit(events: Option<&EventLog>, event: &str) -> String {
    let line = events.map_or_else(|| terminated(event), |events| events.prepare(event));
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
    if let Some(events) = events {
        events.write_prepared(&line);
    }
    line
}

/// Optional live progress: stamp each line once and try each sink once for the whole sample.
/// Busy sinks lose observations; this is neither terminal persistence nor a complete census.
/// Sample and lock-attempt bounds do not bound stderr or regular-file IO backpressure.
pub(crate) fn emit_progress_batch(events: Option<&EventLog>, batch: &[String]) {
    if batch.is_empty() {
        return;
    }
    let bytes: String = batch
        .iter()
        .map(|event| events.map_or_else(|| terminated(event), |events| events.prepare(event)))
        .collect();
    let _ = std::io::stderr().lock().write_all(bytes.as_bytes());
    let Some(events) = events else { return };
    let mut sinks = match events.sinks.try_lock() {
        Ok(sinks) => sinks,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if let Some(retained) = &mut sinks.retained
        && let Ok(_held) = AppendLock::try_once(&retained.file)
    {
        let _ = (&retained.file).write_all(bytes.as_bytes());
    }
    if let Some(file) = &mut sinks.external
        && let Ok(_held) = AppendLock::try_once(file)
    {
        let _ = (&*file).write_all(bytes.as_bytes());
    }
}

fn terminated(line: &str) -> String {
    let mut line = line.trim_end_matches('\n').to_owned();
    line.push('\n');
    line
}

/// Captured selection facts, never a claim that the checkout is clean or equals HEAD.
#[derive(Clone, Debug)]
pub(crate) struct Context(aterm_json::Value);

impl Context {
    pub(crate) fn captured(ctx: &crate::Ctx) -> Self {
        let selection = ctx.scope.label();
        let bounded = crate::report::clip_json(&selection, 512);
        let head = ctx.head_at_start.as_deref().filter(|head| {
            [40, 64].contains(&head.len()) && head.bytes().all(|b| b.is_ascii_hexdigit())
        });
        Self(aterm_json::json!({
            "observed_head": head,
            "selection": bounded, "selection_truncated": bounded != selection,
            "checks": ctx.checks.iter().take(32).map(|c| c.name()).collect::<Vec<_>>(),
            "checks_omitted": ctx.checks.len().saturating_sub(32),
            "release": ctx.release, "fix": ctx.fix, "no_windows": ctx.no_windows,
            "compile_only": ctx.compile_only,
        }))
    }
}

fn source_observation(value: &aterm_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let keys = [
        "observed_head",
        "selection",
        "selection_truncated",
        "checks",
        "checks_omitted",
        "release",
        "fix",
        "no_windows",
        "compile_only",
    ];
    if object.len() != keys.len() || object.keys().any(|k| !keys.contains(&k.as_str())) {
        return None;
    }
    let head = &value["observed_head"];
    if !head.is_null()
        && !head.as_str().is_some_and(|s| {
            [40, 64].contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return None;
    }
    let selection = value["selection"].as_str()?;
    if crate::report::clip_json(selection, 512) != selection {
        return None;
    }
    if value["checks_omitted"].as_u64().is_none()
        || value["selection_truncated"].as_bool().is_none()
    {
        return None;
    }
    let checks = value["checks"].as_array()?;
    if checks.len() > 32
        || checks.iter().any(|c| {
            !c.as_str().is_some_and(|s| {
                s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        })
    {
        return None;
    }
    if ["release", "fix", "no_windows", "compile_only"]
        .iter()
        .any(|key| value[*key].as_bool().is_none())
    {
        return None;
    }
    Some(value.to_string())
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) struct Attribution {
    pub(crate) id: String,
    pub(crate) source_observation: Option<String>,
}

pub(crate) const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// A raw event's header and attribution are checked before map insertion can
/// erase repeated decoded keys. Payload interpretation remains with its owner.
pub(crate) struct RawEvent {
    pub(crate) value: aterm_json::Value,
    pub(crate) attribution: Option<Attribution>,
}

impl<'de> serde::Deserialize<'de> for RawEvent {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Event;
        impl<'de> serde::de::Visitor<'de> for Event {
            type Value = RawEvent;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a verifier event with an unambiguous header")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut fields = aterm_json::Map::new();
                let mut ambiguous_binding = false;
                while let Some((key, checked)) =
                    input.next_entry::<String, crate::compiler_message::CheckedValue>()?
                {
                    let (value, duplicate) = checked.into_parts();
                    if matches!(key.as_str(), "schema" | "event") && fields.contains_key(&key) {
                        return Err(serde::de::Error::custom("ambiguous verifier event header"));
                    }
                    if key == "invocation" {
                        ambiguous_binding |= duplicate || fields.contains_key(&key);
                    }
                    // Keep the existing payload map semantics. An ambiguous
                    // binding cannot erase an otherwise observed check row.
                    fields.insert(key, value);
                }
                let value = aterm_json::Value::Object(fields);
                if value["schema"].as_u64() != Some(1) || value["event"].as_str().is_none() {
                    return Err(serde::de::Error::custom(
                        "unsupported verifier event header",
                    ));
                }
                let attribution = if ambiguous_binding {
                    None
                } else {
                    attribution(&value)
                };
                Ok(RawEvent { value, attribution })
            }
        }
        decoder.deserialize_map(Event)
    }
}

/// Bound one raw JSON event, retaining attribution ambiguity as unknown. The
/// decoder's existing recursion limit also bounds nested objects and arrays.
pub(crate) fn raw_event(json: &str) -> Option<RawEvent> {
    (json.len() <= MAX_RECORD_BYTES)
        .then(|| aterm_json::from_str(json).ok())
        .flatten()
}

/// Decode one raw diagnostic event (at most 1 MiB) with its validated invocation
/// ID. Ambiguous schema/event keys refuse the record; ambiguous attribution
/// returns an unknown ID while retaining the payload. No source, execution,
/// custody or qualification authority follows from either returned value.
#[must_use]
pub fn diagnostic_event(json: &str) -> Option<(aterm_json::Value, Option<String>)> {
    let record = raw_event(json)?;
    Some((record.value, record.attribution.map(|binding| binding.id)))
}

/// Decode diagnostic attribution only, never source or execution authority.
/// A Value has already lost duplicate keys; raw readers must use `raw_event`.
pub(crate) fn attribution(value: &aterm_json::Value) -> Option<Attribution> {
    if value["schema"].as_u64() != Some(1) || value["event"].as_str().is_none() {
        return None;
    }
    let binding = value["invocation"].as_object()?;
    if binding.len() != 3
        || binding
            .keys()
            .any(|key| !["id", "source_observation", "identity_error"].contains(&key.as_str()))
        || !value["invocation"]["identity_error"].is_null()
    {
        return None;
    }
    let id = value["invocation"]["id"]
        .as_str()
        .filter(|id| valid_id(id))?;
    let source = source_observation(&value["invocation"]["source_observation"]);
    if !value["invocation"]["source_observation"].is_null() && source.is_none() {
        return None;
    }
    Some(Attribution {
        id: id.to_owned(),
        source_observation: source,
    })
}

/// The validated per-event diagnostic invocation ID, without inferring attribution from
/// neighboring records. This establishes no source, execution, custody or qualification authority.
/// Duplicate keys erased before this Value was constructed cannot be detected here.
#[must_use]
pub fn diagnostic_invocation_id(value: &aterm_json::Value) -> Option<String> {
    attribution(value).map(|binding| binding.id)
}

/// The companion writer shares only file custody, not the verifier or any authority.
#[derive(Debug)]
pub(crate) struct Retained {
    pub(crate) file: File,
    // Close every writer before the last custody reference releases its lock.
    pub(crate) _custody: Arc<aterm_uds::file_lock::LockFile>,
}

#[derive(Debug, Default)]
struct Sinks {
    external: Option<File>,
    retained: Option<Retained>,
}

/// One invocation's optional external append stream and private retained companion.
#[derive(Clone, Debug)]
pub struct EventLog {
    prepared: Arc<AtomicBool>,
    id: Option<String>,
    context: Option<Context>,
    sinks: Arc<Mutex<Sinks>>,
}

impl EventLog {
    pub(crate) fn unrecorded() -> Self {
        Self::with_entropy(aterm_uds::rand::hex_token::<16>)
    }

    fn with_entropy(entropy: impl FnOnce() -> io::Result<String>) -> Self {
        Self {
            prepared: Arc::new(AtomicBool::new(false)),
            id: entropy().ok(),
            context: None,
            sinks: Arc::new(Mutex::new(Sinks::default())),
        }
    }

    pub(crate) fn bind(&mut self, context: Context) {
        // Binding happens once, after Ctx selection capture and before any stages run.
        if self.context.is_none() && !self.prepared.load(Ordering::Relaxed) {
            self.context = Some(context);
        }
    }

    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(crate) fn retain(&mut self, retained: Retained) {
        let mut sinks = self
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sinks.retained = Some(retained);
    }

    /// Open an external regular append file. Independent writers serialize each record on
    /// this actual file's lock, including when its path is a hard-link alias. Read access is
    /// required to classify its file role; an unreadable role is refused, never assumed public.
    ///
    /// # Errors
    /// The file cannot be opened, is not a stable regular file, or is a private managed log.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut open = std::fs::OpenOptions::new();
        open.read(true).append(true).create(true);
        private_flags(&mut open);
        let file = open.open(path)?;
        if !file.metadata()?.is_file() || !crate::run_log::same_file(&file, path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "events is no stable regular file",
            ));
        }
        if crate::run_log::private_managed_file(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private managed logs cannot be opened as public append streams",
            ));
        }
        let log = Self::unrecorded();
        log.sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .external = Some(file);
        Ok(log)
    }

    pub(crate) fn prepare(&self, event: &str) -> String {
        self.prepared.store(true, Ordering::Relaxed);
        let raw = event.trim_end_matches('\n');
        let Some(json) = raw.strip_prefix("verify-event: ") else {
            return terminated(raw);
        };
        let Ok(mut value) = aterm_json::from_str::<aterm_json::Value>(json) else {
            return terminated(raw);
        };
        if value["schema"].as_u64() != Some(1) || value["event"].as_str().is_none() {
            return terminated(raw);
        }
        let Some(object) = value.as_object_mut() else {
            return terminated(raw);
        };
        // Only raw producer events enter here; mirrored sinks use write_prepared.
        object.insert("invocation".into(), aterm_json::json!({
            "id": self.id,
            "source_observation": self.context.as_ref().map(|context| &context.0),
            "identity_error": if self.id.is_none() { Some("entropy-unavailable") } else { None },
        }));
        let line = format!("verify-event: {value}\n");
        if line.len() <= 64 << 10 {
            return line;
        }
        // An unreadable-sized diagnostic is unknown, never a clipped invented check row.
        format!(
            "verify-event: {}\n",
            crate::event_log::event_value(
                crate::event_log::EventKind::ObservationUnavailable,
                aterm_json::json!({
                    "producer_event":value["event"].as_str().map(|s| crate::report::clip_json(s, 128)),
                    "reason":"record exceeds encoded line bound", "invocation":value["invocation"],
                })
            )
        )
    }

    /// Append one attributed observation. IO failure remains a missing observation, never a
    /// verdict. No fsync or power-loss completeness is promised.
    pub fn write(&self, event: &str) {
        self.write_prepared(&self.prepare(event));
    }

    pub(crate) fn write_prepared(&self, line: &str) {
        let mut sinks = self
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Always attempt our own companion, even when the external append is unavailable.
        if let Some(retained) = &mut sinks.retained
            && let Ok(_held) = AppendLock::take(&retained.file)
        {
            let _ = (&retained.file).write_all(line.as_bytes());
        }
        if let Some(file) = &mut sinks.external
            && let Ok(_held) = AppendLock::take(file)
        {
            let _ = (&*file).write_all(line.as_bytes());
        }
    }
}

struct AppendLock<'a>(&'a File);
impl<'a> AppendLock<'a> {
    fn try_once(file: &'a File) -> io::Result<Self> {
        aterm_uds::file_lock::lock_exclusive_until(
            file,
            Instant::now(),
            "progress append is busy",
        )?;
        Ok(Self(file))
    }

    fn take(file: &'a File) -> io::Result<Self> {
        aterm_uds::file_lock::lock_exclusive_until(
            file,
            Instant::now() + Duration::from_secs(1),
            "event append is busy",
        )?;
        Ok(Self(file))
    }
}
impl Drop for AppendLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(unix)]
pub(crate) fn private_flags(open: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    open.mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
}
#[cfg(windows)]
pub(crate) fn private_flags(open: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt as _;
    open.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
}
#[cfg(not(any(unix, windows)))]
pub(crate) fn private_flags(_: &mut std::fs::OpenOptions) {}

#[cfg(test)]
#[path = "event_log_identity_tests.rs"]
mod identity_tests;

/// One dev-set run: its exit code, each finished check's row, its wall seconds, and what its
/// last event says it finished.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Judged {
    /// The run's exit code ([`crate::exit`]).
    pub code: i32,
    /// Each check the run finished, in the order it finished them.
    pub checks: Vec<CheckRow>,
    /// The run's wall seconds.
    pub secs: u64,
    /// The run's own `run-finished` event ([`crate::report::run_event`]); `None` where none was read.
    pub finished: Option<Finished>,
}

impl Judged {
    /// Fold the raw file's completed record lines. A final fragment cannot
    /// establish a check or terminal; later partial diagnostics do not erase
    /// an earlier completed terminal or change its existing accounting policy.
    pub(crate) fn from_events(text: &str, code: i32, secs: u64) -> Self {
        let lines = text
            .split_inclusive('\n')
            .filter_map(|line| line.strip_suffix('\n'))
            .map(|line| line.strip_suffix('\r').unwrap_or(line));
        Self {
            code,
            checks: lines.clone().filter_map(CheckRow::of_event).collect(),
            secs,
            finished: lines.rev().find_map(Finished::of_event),
        }
    }

    /// Whether its rows are the run's whole record: its `run-finished` event names its exit code
    /// and exactly the checks its rows name, each with its word. A line the events file lost
    /// leaves a check the event names with no row, or the event itself missing.
    #[must_use]
    pub fn whole(&self) -> bool {
        let mut rows: Vec<(&str, &str)> = self
            .checks
            .iter()
            .map(|row| (row.check.as_str(), row.word.as_str()))
            .collect();
        rows.sort_unstable();
        self.finished.as_ref().is_some_and(|finished| {
            let mut named: Vec<(&str, &str)> = finished
                .checks
                .iter()
                .map(|(check, word)| (check.as_str(), word.as_str()))
                .collect();
            named.sort_unstable();
            finished.code == self.code && named == rows
        })
    }
}

/// What a run's `run-finished` event states: its exit code and each check it finished, with its
/// outcome word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The run's exit code.
    pub code: i32,
    /// Each check and its word.
    pub checks: Vec<(String, String)>,
}

impl Finished {
    /// What a `verify-event: {…}` line states, when it is a `run-finished` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let value: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ")?).ok()?;
        if value.get("event")?.as_str()? != "run-finished" {
            return None;
        }
        let checks = value
            .get("checks")?
            .as_array()?
            .iter()
            .map(|c| {
                Some((
                    c.get("check")?.as_str()?.to_owned(),
                    c.get("outcome")?.as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            code: i32::try_from(value.get("code")?.as_i64()?).ok()?,
            checks,
        })
    }
}

/// One finished check, as its `check-finished` event states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRow {
    /// The check's name.
    pub check: String,
    /// Its outcome word: `ok`, `skip`, `FAIL` or `NOT RUN`.
    pub word: String,
    /// The ids of the findings the event carries (at most `report::FINDINGS_SHOWN`).
    pub findings: Vec<String>,
    /// How many findings the check had.
    pub findings_total: usize,
}

impl CheckRow {
    /// The row a `verify-event: {…}` line states, when it is a `check-finished` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let json = line.strip_prefix("verify-event: ")?;
        let value: aterm_json::Value = aterm_json::from_str(json).ok()?;
        if value.get("event")?.as_str()? != "check-finished" {
            return None;
        }
        let findings = value
            .get("findings")?
            .as_array()?
            .iter()
            .map(|f| f.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            check: value.get("check")?.as_str()?.to_owned(),
            word: value.get("outcome")?.as_str()?.to_owned(),
            findings,
            findings_total: usize::try_from(value.get("findings_total")?.as_u64()?).ok()?,
        })
    }
}

#[cfg(test)]
#[path = "event_log_tests.rs"]
mod tests;

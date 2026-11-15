// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's own file of its events, apart from its output (`--events`): what a release's cutter
//! keeps its records from (A19) and `verify land` judges by (B10).
//!
//! Check, binary and observed case events go to the run's output too, which its children share: there a child's unfinished line,
//! or a line longer than a pipe writes at once, can land inside one. Here each line is one
//! `write_all` under one lock, to a file opened close-on-exec, so no child inherits it and no two
//! lines interleave. A release's cutter reads its run's output for them (`ship status`); a run
//! whose reader takes its events from this file alone (`verify land`'s judgments, which name
//! [`crate::cli::EVENTS_FILE_ONLY`]) keeps them off its output ([`keep_off_console`],
//! [`WithoutEvents`]), so a person reads only what the run says.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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
    /// A process a cleanup sweep left, with its observed identity and reason.
    /// Diagnostic evidence; it establishes no signal authority or check verdict.
    ProcessSurvivor,
    RunFinished,
    RunPlanned,
    RunResealed,
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
    /// A heavy run's first look found the judgment turn taken (`crate::turn`).
    JudgmentQueued,
    /// A heavy run holds the judgment turn, having waited for it.
    JudgmentTurn,
    /// A heavy run's CPU against its prediction, at its end.
    JudgmentCpu,
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

/// Whether this process's event lines go to its console (stderr, and the output
/// [`WithoutEvents`] does not wrap) beside its files: until [`keep_off_console`].
static CONSOLE: AtomicBool = AtomicBool::new(true);

/// Keep this process's event lines off its stderr from now on: its events file and the one kept
/// beside its log still take each one whole ([`crate::cli::EVENTS_FILE_ONLY`]).
pub(crate) fn keep_off_console() {
    CONSOLE.store(false, Ordering::Relaxed);
}

fn on_console() -> bool {
    CONSOLE.load(Ordering::Relaxed)
}

/// Prepare once, mirror without holding a file lock, then persist the identical bytes.
pub(crate) fn emit(events: Option<&EventLog>, event: &str) -> String {
    let line = events.map_or_else(|| terminated(event), |events| events.prepare(event));
    if on_console() {
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
    if let Some(events) = events {
        events.write_prepared(&line);
    }
    line
}

/// A run's output with its event lines left out ([`crate::cli::EVENTS_FILE_ONLY`]): a line that
/// begins `verify-event: ` is dropped whole, and every other byte passes in order. A line is
/// held only while what it holds so far could still begin that way, so a partial line that
/// cannot is written at once; one still held when the writer drops is written then.
pub struct WithoutEvents<W: Write> {
    inner: W,
    /// The start of the current line, held while it may still be an event's.
    held: Vec<u8>,
    /// What the current line is, once known.
    line: Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    /// Not yet known: [`WithoutEvents::held`] is a prefix of an event line's start.
    Open,
    /// Not an event's: it passes.
    Passes,
    /// An event's: it is dropped.
    Dropped,
}

const EVENT_START: &[u8] = b"verify-event: ";

impl<W: Write> WithoutEvents<W> {
    #[must_use]
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            held: Vec::new(),
            line: Line::Open,
        }
    }
}

impl<W: Write> Write for WithoutEvents<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let end = rest.iter().position(|b| *b == b'\n');
            let (part, ends) = match end {
                Some(at) => (&rest[..=at], true),
                None => (rest, false),
            };
            rest = &rest[part.len()..];
            if self.line == Line::Open {
                let take = EVENT_START
                    .len()
                    .saturating_sub(self.held.len())
                    .min(part.len());
                self.held.extend_from_slice(&part[..take]);
                let known = self.held.len().min(EVENT_START.len());
                self.line = if self.held[..known] != EVENT_START[..known] {
                    Line::Passes
                } else if known == EVENT_START.len() {
                    Line::Dropped
                } else {
                    Line::Open
                };
                if self.line == Line::Passes {
                    self.inner.write_all(&std::mem::take(&mut self.held))?;
                    self.inner.write_all(&part[take..])?;
                } else if self.line == Line::Open && ends {
                    // A line shorter than an event's start (an empty one included) passes.
                    self.inner.write_all(&std::mem::take(&mut self.held))?;
                }
            } else if self.line == Line::Passes {
                self.inner.write_all(part)?;
            }
            if ends {
                self.held.clear();
                self.line = Line::Open;
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Drop for WithoutEvents<W> {
    fn drop(&mut self) {
        if self.line == Line::Open && !self.held.is_empty() {
            let _ = self.inner.write_all(&self.held);
        }
        let _ = self.inner.flush();
    }
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
    if on_console() {
        let _ = std::io::stderr().lock().write_all(bytes.as_bytes());
    }
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
    /// The lines written before the retained companion attached, the first [`EARLY_KEPT`]: what
    /// a run said before its log opened (the judgment turn's wait, `crate::turn::take`), which
    /// [`EventLog::retain_early`] writes into the companion once its first record is there.
    early: Vec<String>,
}

/// How many lines written before the retained companion attaches it keeps ([`Sinks::early`]).
const EARLY_KEPT: usize = 16;

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

    /// Write into the retained companion what was written before it attached
    /// ([`Sinks::early`]), in the order it was written, and forget it. Called once the
    /// companion's first record, its `invocation-started`, is there, which its pairing reads
    /// first (`crate::run_log`). The external stream had each line as it was written.
    pub(crate) fn retain_early(&self) {
        let mut sinks = self
            .sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let early = std::mem::take(&mut sinks.early);
        if let Some(retained) = &sinks.retained
            && let Ok(_held) = AppendLock::take(&retained.file)
        {
            for line in &early {
                let _ = (&retained.file).write_all(line.as_bytes());
            }
        }
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
        if let Some(retained) = &mut sinks.retained {
            if let Ok(_held) = AppendLock::take(&retained.file) {
                let _ = (&retained.file).write_all(line.as_bytes());
            }
        } else if sinks.early.len() < EARLY_KEPT {
            sinks.early.push(line.to_owned());
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
    /// The run's `run-planned` event ([`crate::report::run_planned`]); `None` where the run kept
    /// no land plan or the line was lost.
    pub planned: Option<Box<Planned>>,
    /// The run's `run-resealed` event ([`crate::report::run_resealed`]); `None` where none was read.
    pub resealed: Option<Box<Resealed>>,
    /// The run's invocation id ([`invocation_of`]), read off its plan or its end; `None` where
    /// neither line names one.
    pub invocation: Option<String>,
    /// The run's reds in the order its events came, the first [`REDS_KEPT`] of them
    /// ([`Red::of_event`]): a case's red comes as the case fails, and again in its check's
    /// findings, so one red may be named twice.
    pub reds: Vec<Red>,
    /// The run's own log and the events file kept beside it, as its `invocation-started` event
    /// names them ([`Kept::of_event`]); `None` where it kept no log.
    pub kept: Option<Kept>,
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
            planned: lines.clone().find_map(Planned::of_event).map(Box::new),
            resealed: lines
                .clone()
                .rev()
                .find_map(Resealed::of_event)
                .map(Box::new),
            invocation: lines
                .clone()
                .filter(|line| {
                    Planned::of_event(line).is_some() || Finished::of_event(line).is_some()
                })
                .find_map(invocation_of),
            finished: lines.clone().rev().find_map(Finished::of_event),
            reds: lines
                .clone()
                .flat_map(Red::of_event)
                .take(REDS_KEPT)
                .collect(),
            kept: lines.clone().find_map(Kept::of_event),
        }
    }

    /// Whether the run's inputs stood through it: its `run-resealed` says so, and each check
    /// `run-planned` keyed is resealed to its planned seal under the planned local digest. A
    /// run without either line was not sealed through. Each record's `[run]` says which
    /// ([`crate::land_record::Run::sealed_through`]).
    #[must_use]
    pub fn sealed_through(&self) -> bool {
        let (Some(planned), Some(resealed)) = (&self.planned, &self.resealed) else {
            return false;
        };
        resealed.sealed_through
            && resealed.local == planned.local
            && planned.required.iter().all(|row| {
                resealed
                    .seals
                    .iter()
                    .any(|(check, seal)| *check == row.check && *seal == row.seal)
            })
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

/// The invocation id a `verify-event: {…}` line is attributed to ([`diagnostic_event`]): the
/// run's own 32-hex id, which names it in a land record for local diagnostics only.
#[must_use]
pub fn invocation_of(line: &str) -> Option<String> {
    diagnostic_event(line.strip_prefix("verify-event: ")?).and_then(|(_, id)| id)
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckRow {
    /// The check's name.
    pub check: String,
    /// Its outcome word: `ok`, `skip`, `FAIL` or `NOT RUN`.
    pub word: String,
    /// What the check said of itself (the event's `detail`, clipped): why it did not run or was
    /// skipped, or what failed; empty where the event says nothing.
    pub detail: String,
    /// The ids of the findings the event carries (at most `report::FINDINGS_SHOWN`).
    pub findings: Vec<String>,
    /// How many findings the check had.
    pub findings_total: usize,
    /// Its wall time, as its event states it; `None` where it states none.
    pub took: Option<Duration>,
    /// Whether the run's first red kept it from deciding anything
    /// ([`crate::report::CheckResult::stopped`]): a NOT RUN the halt made, not the machine.
    pub stopped: bool,
    /// Its land key ([`crate::land_seal::key`]), from a run that kept a land plan.
    pub key: Option<String>,
    /// The crates a crate-scoped check judged, and whether its selection was widened to the
    /// whole workspace.
    pub scope: Option<(Vec<String>, bool)>,
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
        let scope = match value.get("scope") {
            None => None,
            Some(scope) => Some((
                strings(scope.get("crates")?)?,
                scope.get("widened")?.as_bool()?,
            )),
        };
        let key = match value.get("key") {
            None => None,
            Some(key) => Some(key.as_str()?.to_owned()),
        };
        Some(Self {
            check: value.get("check")?.as_str()?.to_owned(),
            word: value.get("outcome")?.as_str()?.to_owned(),
            detail: match value.get("detail") {
                None => String::new(),
                Some(detail) => detail.as_str()?.to_owned(),
            },
            findings,
            findings_total: usize::try_from(value.get("findings_total")?.as_u64()?).ok()?,
            took: value
                .get("seconds")
                .and_then(aterm_json::Value::as_f64)
                .and_then(|secs| Duration::try_from_secs_f64(secs).ok()),
            stopped: value
                .get("stopped")
                .and_then(aterm_json::Value::as_bool)
                .unwrap_or(false),
            key,
            scope,
        })
    }
}

/// `value` as an array of strings.
fn strings(value: &aterm_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// `value` as an object of strings.
fn string_map(value: &aterm_json::Value) -> Option<std::collections::BTreeMap<String, String>> {
    value
        .as_object()?
        .iter()
        .map(|(name, item)| Some((name.clone(), item.as_str()?.to_owned())))
        .collect()
}

/// The JSON of a `verify-event: {…}` line whose event is `event`.
fn event_of(line: &str, event: &str) -> Option<aterm_json::Value> {
    let value: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ")?).ok()?;
    (value.get("event")?.as_str()? == event).then_some(value)
}

/// One required check of a `run-planned` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCheck {
    pub check: crate::Check,
    /// Its planned part ([`crate::checks::part`]).
    pub part: String,
    /// Its key.
    pub key: String,
    /// Its portable seal.
    pub seal: String,
    /// Each portable field its seal names, with its digest.
    pub fields: std::collections::BTreeMap<String, String>,
}

/// What a run's `run-planned` event states ([`crate::report::run_planned`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The commit judged.
    pub commit: String,
    /// Its tree.
    pub tree: String,
    /// The base commit the cone was cut from.
    pub base: String,
    /// Whether the checkout was clean.
    pub clean: bool,
    /// The full plan's checks, in order.
    pub required: Vec<PlannedCheck>,
    /// The run's local digest.
    pub local: String,
    /// Each local attribute.
    pub local_fields: std::collections::BTreeMap<String, String>,
    /// The digest of the required checks' `(check, key)` pairs.
    pub plan_digest: String,
    /// Each field the run could not seal, and why.
    pub unsealed: Vec<(String, String)>,
}

impl Planned {
    /// What a `verify-event: {…}` line states, when it is a `run-planned` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let value = event_of(line, "run-planned")?;
        let text = |name: &str| Some(value.get(name)?.as_str()?.to_owned());
        let required = value
            .get("required")?
            .as_array()?
            .iter()
            .map(|row| {
                let text = |name: &str| Some(row.get(name)?.as_str()?.to_owned());
                let Some(crate::checks::Word::Check(check)) =
                    crate::checks::lookup(row.get("check")?.as_str()?)
                else {
                    return None;
                };
                Some(PlannedCheck {
                    check,
                    part: text("part")?,
                    key: text("key")?,
                    seal: text("seal")?,
                    fields: string_map(row.get("fields")?)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let local = value.get("local")?;
        let unsealed = value
            .get("unsealed")?
            .as_array()?
            .iter()
            .map(|row| {
                Some((
                    row.get("field")?.as_str()?.to_owned(),
                    row.get("why")?.as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            commit: text("commit")?,
            tree: text("tree")?,
            base: text("base")?,
            clean: value.get("clean")?.as_bool()?,
            required,
            local: local.get("digest")?.as_str()?.to_owned(),
            local_fields: string_map(local.get("fields")?)?,
            plan_digest: text("plan_digest")?,
            unsealed,
        })
    }

    /// The required row of `check`, when the plan holds it.
    #[must_use]
    pub fn row(&self, check: crate::Check) -> Option<&PlannedCheck> {
        self.required.iter().find(|row| row.check == check)
    }
}

/// What a run's `run-resealed` event states ([`crate::report::run_resealed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resealed {
    /// Each required check's seal as its fields read at the run's end.
    pub seals: Vec<(crate::Check, String)>,
    /// The run's local digest at its end.
    pub local: String,
    /// The fields whose reading moved.
    pub moved: Vec<String>,
    /// Every seal and the local digest stood through the run, as the run judged it.
    pub sealed_through: bool,
}

impl Resealed {
    /// What a `verify-event: {…}` line states, when it is a `run-resealed` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let value = event_of(line, "run-resealed")?;
        let seals = value
            .get("seals")?
            .as_array()?
            .iter()
            .map(|row| {
                let Some(crate::checks::Word::Check(check)) =
                    crate::checks::lookup(row.get("check")?.as_str()?)
                else {
                    return None;
                };
                Some((check, row.get("seal")?.as_str()?.to_owned()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            seals,
            local: value.get("local")?.get("digest")?.as_str()?.to_owned(),
            moved: strings(value.get("moved")?)?,
            sealed_through: value.get("sealed_through")?.as_bool()?,
        })
    }
}

/// The most reds a run's record keeps ([`Judged::reds`]).
pub const REDS_KEPT: usize = 256;

/// One red a run's events name: what went red, its finding's id, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Red {
    /// A test binary's spec and a failed case (or, where none is named, why the binary failed),
    /// or a check and one of its findings (or its detail).
    pub what: String,
    /// The id the check's findings name it by (`<spec> -- <case>` for a case, as
    /// `findings::test_findings` spells it; the finding itself for a check's); `None` where its
    /// event names no case or finding.
    pub id: Option<String>,
    /// The failed case's source line, else the line its panic names (`path:line`); `None` where
    /// its event names neither.
    pub at: Option<String>,
}

impl Red {
    /// The reds a `verify-event: {…}` line states, in its order: a `test-case-failed` event's
    /// case, a `test-binary-finished` event's failed cases (or the binary, where it names none)
    /// and a `check-finished` event's findings (or the check, where it names none), each whose
    /// outcome is FAIL. A case event comes as the case fails, before its binary's and its
    /// check's. A failed check's NOT RUN rows (`report::judge` folds them into its findings) are
    /// no red.
    #[must_use]
    pub fn of_event(line: &str) -> Vec<Self> {
        Self::parse(line).unwrap_or_default()
    }

    fn parse(line: &str) -> Option<Vec<Self>> {
        let json = line.strip_prefix("verify-event: ")?;
        let kind = ["test-case-failed", "test-binary-finished", "check-finished"]
            .into_iter()
            .find(|kind| json.contains(&format!("\"{kind}\"")))?;
        let value: aterm_json::Value = aterm_json::from_str(json).ok()?;
        if value.get("event")?.as_str()? != kind || value.get("outcome")?.as_str()? != "FAIL" {
            return None;
        }
        let text = |value: &aterm_json::Value, key: &str| {
            value
                .get(key)
                .and_then(aterm_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        if kind == "check-finished" {
            let check = text(&value, "check")?;
            let unrun = format!(
                "{} ",
                crate::report::word(crate::ladder::Outcome::Fail(
                    crate::ladder::Severity::CouldNotRun
                ))
            );
            let findings: Vec<Self> = value
                .get("findings")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(aterm_json::Value::as_str)
                .filter(|finding| !finding.starts_with(&unrun))
                .map(|finding| Self {
                    what: format!("{check}: {finding}"),
                    id: Some(finding.to_owned()),
                    at: None,
                })
                .collect();
            if !findings.is_empty() {
                return Some(findings);
            }
            return Some(vec![Self {
                what: text(&value, "detail")
                    .map_or_else(|| check.clone(), |detail| format!("{check}: {detail}")),
                id: None,
                at: None,
            }]);
        }
        let test = text(&value, "test")?;
        // A binary's event names its failed cases in `failures`; a case's event is the case.
        let cases: Vec<aterm_json::Value> = if kind == "test-binary-finished" {
            value
                .get("failures")
                .and_then(aterm_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        } else {
            vec![value.clone()]
        };
        let reds: Vec<Self> = cases
            .iter()
            .filter_map(|case| {
                let name = text(case, "case")?;
                let declared = text(case, "source_path").and_then(|path| {
                    let line = case.get("start_line")?.as_u64()?;
                    Some(format!("{path}:{line}"))
                });
                Some(Self {
                    what: format!("{test}: {name}"),
                    id: Some(format!("{test} -- {name}")),
                    at: declared.or_else(|| text(case, "output").as_deref().and_then(panicked_at)),
                })
            })
            .collect();
        if !reds.is_empty() {
            return Some(reds);
        }
        Some(vec![Self {
            what: text(&value, "detail")
                .map_or_else(|| test.clone(), |detail| format!("{test}: {detail}")),
            id: None,
            at: None,
        }])
    }
}

/// The `path:line` a panic's message names (`panicked at path:line:col:`), the first one in
/// `output`.
#[must_use]
pub fn panicked_at(output: &str) -> Option<String> {
    let rest = &output[output.find("panicked at ")? + "panicked at ".len()..];
    let place = rest.split_whitespace().next()?.trim_end_matches(':');
    let mut parts = place.rsplitn(3, ':');
    let last = parts.next()?;
    let (line, path) = match (parts.next(), parts.next()) {
        // `path:line:col`.
        (Some(line), Some(path)) => (line, path),
        // `path:line`.
        (Some(path), None) => (last, path),
        _ => return None,
    };
    (!path.is_empty() && !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{path}:{line}"))
}

/// Where a run kept its own log and the events file beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// The run's log.
    pub log: PathBuf,
    /// The events file kept beside it (`verify status --events` reads it); `None` where the
    /// run kept none.
    pub events: Option<PathBuf>,
}

impl Kept {
    /// Where an `invocation-started` event says its run keeps its log, when it names one.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let json = line.strip_prefix("verify-event: ")?;
        if !json.contains("\"invocation-started\"") {
            return None;
        }
        let value: aterm_json::Value = aterm_json::from_str(json).ok()?;
        if value.get("event")?.as_str()? != "invocation-started" {
            return None;
        }
        let path = |key: &str| {
            value
                .get(key)
                .and_then(aterm_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        };
        Some(Self {
            log: path("log")?,
            events: path("events"),
        })
    }
}

#[cfg(test)]
#[path = "event_log_tests.rs"]
mod tests;

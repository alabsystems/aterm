// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE effect seam (design §21.2).

pub mod canon;
pub mod child;
#[cfg(any(test, debug_assertions))]
mod sim;
pub mod tape;

use std::io::Read;
use std::path::Path;
#[cfg(any(unix, test))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use astream_effects::{
    EffectRecord, EffectSeam, EffectsLog, LiveSeam, OsEffects, RecordingSeam, ReplayDivergence,
    ReplaySeam,
};

use crate::error::{Error, Replay, Result, Sub};
use crate::policy::{Decision, RouteRequest};
use crate::redact::Redactor;
use crate::wire::{Endpoints, Transport, WireError, WireRequest, WireResponse};
use tape::TapeMode;

/// A GitHub round trip's door: an exchange.
pub const DOOR_GH: &str = "gh";
/// A sleep's door: an exchange, the length in decimal nanoseconds out and nothing back.
pub const DOOR_SLEEP: &str = "sleep";
/// stdout's door: an emit of the bytes as rendered.
pub const DOOR_STDOUT: &str = "stdout";
/// The exit's door: an emit of the exit code in decimal.
pub const DOOR_EXIT: &str = "exit";
/// A routing decision's door (design §21.3): an exchange, the decision's inputs out and the
/// decision back, so a credentialed tape replays with no agh.toml and no token.
pub const DOOR_ROUTE: &str = "route";
/// The write journal's door: an emit of each record's body (design §6.8).
pub const DOOR_JOURNAL: &str = "journal";
/// The fabric's door: an emit of each post's request line (design §21.5).
pub const DOOR_FABRIC: &str = "fabric";
/// The git runner's door: an exchange, the run's canonical command out and its exit,
/// stdout and stderr back.
pub const DOOR_GIT: &str = "git";
/// A child's door (`gh auth token`): never taped. `auth import` is a Provision verb, and
/// tapes refuse those.
pub const DOOR_CHILD: &str = "child";

/// Which world the doors reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The shipped seam: the OS clocks, real sleeps, the transport called directly.
    Live,
    /// Tests: a virtual clock only sleeps move; the transport called directly.
    #[cfg(any(test, debug_assertions))]
    Simulated,
    /// `--record`: live, every door taped into a new file.
    Recording,
    /// `--replay`: every door answered from a tape; nothing sent, no time passes.
    Replaying,
    /// `--replay OLD --record NEW`.
    Rerecording,
}

impl Mode {
    /// Whether a tape is involved: recording, replaying or rerecording.
    #[inline]
    #[must_use]
    pub const fn is_taped(self) -> bool {
        matches!(self, Mode::Recording | Mode::Replaying | Mode::Rerecording)
    }

    /// Whether the doors are answered from a tape: replaying or rerecording.
    #[inline]
    #[must_use]
    pub const fn replays(self) -> bool {
        matches!(self, Mode::Replaying | Mode::Rerecording)
    }
}

enum Seam {
    /// The OS clocks, behind a mutex held for one clock read.
    Live(Mutex<LiveSeam<OsEffects>>),
    /// A test's virtual clocks.
    #[cfg(any(test, debug_assertions))]
    Simulated(sim::SimClock),
    /// The OS.
    Recording {
        /// The recording seam, which owns one clone of the sink.
        seam: Mutex<RecordingSeam<OsEffects, tape::TapeSink>>,
        /// The other clone: `finish` flushes the file through it.
        sink: tape::TapeSink,
    },
    /// A tape's records, answered in order.
    Replaying {
        /// The replay seam over the tape's records.
        seam: Mutex<ReplaySeam>,
        /// How many records the tape holds.
        len: usize,
        /// The `next` of a divergence: `record it live: agh --record NEW {argv}` (`live_next`).
        live: String,
    },
    /// A tape's exchanges and clocks.
    Rerecording {
        /// The replay seam over the old tape's records, less its emits.
        from: Mutex<ReplaySeam>,
        /// How many records `from` holds.
        len: usize,
        /// The new tape.
        to: tape::TapeSink,
        /// The `next` of a divergence.
        live: String,
    },
}

/// The effect seam.
pub struct Fx {
    seam: Seam,
    /// The first divergence or refusal under a tape: every later door answers it (plan §4.6.27).
    latch: Mutex<Option<Error>>,
    /// The tape header's mode (`Anonymous` live and simulated): `route`, `gh` and `gh_stream`
    /// read it (plan §14, body 8).
    tape: TapeMode,
    /// The endpoints a taped run speaks to (`with_endpoints`): a credentialed streamed
    /// request's canonical bytes name its origin's role through them.
    endpoints: Option<Endpoints>,
    /// The tape a replay or rerecording reads (`with_source`): its streamed bodies are in
    /// `{source}.d`.
    #[cfg(unix)]
    source: Option<std::path::PathBuf>,
    /// The journal emits taped so far: `emit_journal`'s step.
    #[cfg(any(unix, test))]
    journal_steps: AtomicU64,
    /// The streamed answers taped so far: a recording's partial body file is named by it.
    #[cfg(unix)]
    streams: AtomicU64,
    /// The verdicts of the replayed bodies still streaming (`tape::DigestReader`): `latched`
    /// moves the first one found into `latch`.
    stream_verdicts: Mutex<Vec<Arc<Mutex<Option<Error>>>>>,
    /// Tests: the deadlines of every request `gh` and `gh_stream` were handed, in order
    /// (`requested_deadlines`), so a test reads the backstop a client was built with.
    #[cfg(any(test, debug_assertions))]
    requested: Mutex<Vec<crate::wire::Deadlines>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn infallible<T>(read: std::result::Result<T, std::convert::Infallible>) -> T {
    match read {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

/// A recording's exchange at `door`: `request` taped with its `reply`.
fn tape_exchange(
    seam: &Mutex<RecordingSeam<OsEffects, tape::TapeSink>>,
    door: &str,
    request: &[u8],
    reply: &[u8],
) {
    infallible(lock(seam).exchange(door, request, &mut |_| reply.to_vec()));
}

fn read_whole(
    transport: &dyn Transport,
    mut req: WireRequest<'_>,
    cut: impl FnOnce(Vec<(String, String)>) -> Vec<(String, String)>,
) -> std::result::Result<canon::Reply, WireError> {
    let limit = req.limit;
    req.limit = u64::MAX;
    let mut response = transport.round_trip(req)?;
    let body = crate::wire::read_capped(&mut response.body, limit)?;
    Ok(canon::Reply {
        status: response.status,
        headers: cut(response.headers),
        body,
    })
}

fn kept_response(endpoints: &Endpoints, reply: canon::Reply) -> WireResponse {
    canon::Reply {
        headers: canon::keep(endpoints, reply.headers),
        ..reply
    }
    .into_response()
}

fn sleep_request(d: Duration) -> Vec<u8> {
    d.as_nanos().to_string().into_bytes()
}

fn diverged(d: &ReplayDivergence) -> Error {
    Error::replay(Replay::of(d), Redactor::shapes(&d.to_string()))
}

/// `Replay::StreamNotTaped` for a streamed answer under a tape of `mode` that cannot hold it:
/// an anonymous tape holds whole answers alone; a credentialed one holds it by digest on unix
/// only, through an `Fx` that `with_endpoints` gave the run's endpoints (`has_endpoints`).
fn stream_not_taped(mode: TapeMode, has_endpoints: bool) -> Error {
    let (why, next) = match mode {
        TapeMode::Anonymous => (
            "a streamed answer is not taped: an anonymous tape holds whole answers alone",
            "give --as PROFILE beside --record: a credentialed tape holds a streamed answer by digest",
        ),
        TapeMode::Credentialed if !cfg!(unix) => (
            "a streamed answer is taped on unix only",
            "run it without --record or --replay on this system",
        ),
        TapeMode::Credentialed if !has_endpoints => (
            "a streamed answer is not taped: this tape's seam was built without the run's \
             endpoints (Fx::with_endpoints)",
            "run it without --record or --replay",
        ),
        TapeMode::Credentialed => (
            "a streamed answer is taped only by a recording, a replay or a rerecording",
            "run it without --record or --replay",
        ),
    };
    Error::replay(Replay::StreamNotTaped, why).with_next(next)
}

/// `Replay::StreamNotTaped` for a replay or rerecording built without its tape's path, beside
/// which the streamed bodies are.
#[cfg(unix)]
fn stream_without_source() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "a streamed answer is not replayed: this replay was built without its tape's path \
         (Fx::with_source)",
    )
    .with_next("run it without --record or --replay")
}

fn child_not_taped() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "a child process is not taped: the verbs that run one provision, and tapes refuse those",
    )
    .with_next("run it without --record or --replay: a tape never runs a child")
}

fn ended(d: &ReplayDivergence, len: usize) -> Error {
    match d {
        ReplayDivergence::Unreached { position, .. } => Error::replay(
            Replay::of(d),
            format!(
                "the tape holds {} more records after the run's exit",
                len.saturating_sub(*position)
            ),
        ),
        _ => diverged(d),
    }
}

fn shaped_request() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "this request holds a token shape, which a tape redacts: a tape of it could not replay",
    )
    .with_next("run it without --record: a tape cannot hold this request")
}

fn shaped_reply() -> Error {
    Error::replay(
        Replay::BodyNotTaped,
        "GitHub's answer holds a token shape, which a tape redacts: a tape of it could not \
         replay",
    )
    .with_next("run it without --record: a tape cannot hold this answer")
}

fn shell_word(arg: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"_./:=@%+,-".contains(&b);
    if !arg.is_empty() && arg.bytes().all(plain) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

fn live_next(header: &tape::TapeHeader) -> String {
    let mut live = String::from("record it live: agh --record NEW");
    for arg in &header.argv {
        live.push(' ');
        live.push_str(&shell_word(&Redactor::shapes(arg)));
    }
    live
}

fn not_read(path: &Path, e: &std::io::Error) -> Error {
    Error::local(format!(
        "the tape {} could not be read: {e}",
        path.display()
    ))
}

#[cfg(target_os = "macos")]
const NONBLOCK_OPEN_FLAG: i32 = 0x0004;
#[cfg(all(unix, not(target_os = "macos")))]
const NONBLOCK_OPEN_FLAG: i32 = 0o4000;

#[cfg(unix)]
const TAPE_STREAM_BUDGET: Duration = Duration::from_secs(5);

#[cfg(unix)]
fn read_pipe(file: &std::fs::File, budget: Duration) -> std::io::Result<Vec<u8>> {
    use std::os::fd::AsFd as _;
    use std::time::Instant;

    aterm_uds::ready::with_nonblocking(file.as_fd(), || {
        let mut reader = file;
        let deadline = Instant::now() + budget;
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        let limit = tape::TAPE_MAX.saturating_add(1);
        while (bytes.len() as u64) < limit {
            if Instant::now() >= deadline {
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
            }
            let room = usize::try_from(limit - bytes.len() as u64)
                .unwrap_or(usize::MAX)
                .min(chunk.len());
            match reader.read(&mut chunk[..room]) {
                Ok(0) => return Ok(bytes),
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        file.as_fd(),
                        aterm_uds::ready::Interest::Read,
                        deadline,
                    )?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(bytes)
    })
}

/// The tape at `path`, read whole (at most `tape::TAPE_MAX` bytes) and parsed.
/// Unix pipes are nonblocking and must finish within five seconds.
/// # Errors
/// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
pub(crate) fn read_tape(path: &Path) -> Result<(tape::TapeHeader, Vec<EffectRecord>)> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK_OPEN_FLAG)
            .open(path)
            .map_err(|e| not_read(path, &e))?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path).map_err(|e| not_read(path, &e))?;
    let meta = file.metadata().map_err(|e| not_read(path, &e))?;
    let kind = meta.file_type();
    #[cfg(unix)]
    let pipe = {
        use std::os::unix::fs::FileTypeExt as _;
        kind.is_fifo()
    };
    #[cfg(not(unix))]
    let pipe = false;
    if !kind.is_file() && !pipe {
        return Err(not_read(
            path,
            &std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file or pipe",
            ),
        ));
    }
    let mut bytes = Vec::new();
    #[cfg(unix)]
    if pipe {
        bytes = read_pipe(&file, TAPE_STREAM_BUDGET).map_err(|e| not_read(path, &e))?;
    }
    if !pipe {
        bytes.reserve(
            usize::try_from(meta.len().min(tape::TAPE_MAX).saturating_add(1)).unwrap_or(0),
        );
        file.take(tape::TAPE_MAX.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| not_read(path, &e))?;
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > tape::TAPE_MAX {
        return Err(Error::replay(
            Replay::Format,
            format!(
                "the tape {} is over {} bytes, the most a replay reads",
                path.display(),
                tape::TAPE_MAX
            ),
        )
        .with_next(tape::RECORD_AGAIN));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::replay(
            Replay::Format,
            format!("the tape {} is not UTF-8 text", path.display()),
        )
        .with_next(tape::RECORD_AGAIN)
    })?;
    tape::parse(&text)
}

impl Fx {
    const fn over(seam: Seam) -> Fx {
        Fx {
            seam,
            latch: Mutex::new(None),
            tape: TapeMode::Anonymous,
            endpoints: None,
            #[cfg(unix)]
            source: None,
            #[cfg(any(unix, test))]
            journal_steps: AtomicU64::new(0),
            #[cfg(unix)]
            streams: AtomicU64::new(0),
            stream_verdicts: Mutex::new(Vec::new()),
            #[cfg(any(test, debug_assertions))]
            requested: Mutex::new(Vec::new()),
        }
    }

    /// The same `Fx`, its taped doors in the tape's `mode`.
    fn taped(mut self, mode: TapeMode) -> Fx {
        self.tape = mode;
        self
    }

    /// The same `Fx`, speaking to `endpoints`: a credentialed tape's streamed requests name
    /// their origin's role through them. Without it a credentialed streamed answer is not
    /// taped (`Replay::StreamNotTaped`).
    #[must_use]
    pub(crate) fn with_endpoints(mut self, endpoints: &Endpoints) -> Fx {
        self.endpoints = Some(endpoints.clone());
        self
    }

    /// The same replaying or rerecording `Fx`, reading its streamed bodies from beside the
    /// tape at `tape` (`{tape}.d`).
    #[must_use]
    pub(crate) fn with_source(self, tape: &Path) -> Fx {
        #[cfg(unix)]
        {
            let mut fx = self;
            fx.source = Some(tape.to_path_buf());
            fx
        }
        #[cfg(not(unix))]
        {
            let _ = tape;
            self
        }
    }

    /// Tape this run into `path`, which must not exist, as a credentialed tape: every record
    /// passes `redactor.apply`, whose clone shares the needles the run learns later, and so does
    /// the header item (`TapeSink::create_redacted`, and again at `finish`).
    /// # Errors
    /// As `record`; CouldNotRun, `Sub::Local`, before any file is made, for a command line that
    /// holds a credential `redactor` has learned.
    pub(crate) fn record_with(
        path: &Path,
        header: &tape::TapeHeader,
        redactor: Redactor,
    ) -> Result<Fx> {
        let sink = tape::TapeSink::create_redacted(path, header, redactor)?;
        Ok(Fx::over(Seam::Recording {
            seam: Mutex::new(RecordingSeam::with_sink(OsEffects::new(), sink.clone())),
            sink,
        })
        .taped(header.mode))
    }

    /// The shipped seam.
    #[must_use]
    pub fn live() -> Fx {
        Fx::over(Seam::Live(Mutex::new(LiveSeam::new(OsEffects::new()))))
    }

    /// Tests: the deadlines of every request `gh` and `gh_stream` were handed so far, in order:
    /// the backstop the client that sent them was built with, read with no clock.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn requested_deadlines(&self) -> Vec<crate::wire::Deadlines> {
        lock(&self.requested).clone()
    }

    /// Tests: `mono` starts at 0 and `wall_secs` at `start_unix_secs`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn simulated(start_unix_secs: u64) -> Fx {
        Fx::over(Seam::Simulated(sim::SimClock::new(start_unix_secs)))
    }

    /// Tape this run into `path`, which must not exist.
    /// # Errors
    /// CouldNotRun, `Sub::Local`.
    pub(crate) fn record(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        let sink = tape::TapeSink::create(path, header)?;
        Ok(Fx::over(Seam::Recording {
            seam: Mutex::new(RecordingSeam::with_sink(OsEffects::new(), sink.clone())),
            sink,
        })
        .taped(header.mode))
    }

    /// Replay the tape at `path`.
    /// # Errors
    /// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn replay(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(path)?;
        Ok((Fx::replay_from(&header, records).with_source(path), header))
    }

    /// `replay` over a tape's header and records already read (`read_tape`).
    pub(crate) fn replay_from(header: &tape::TapeHeader, records: Vec<EffectRecord>) -> Fx {
        let len = records.len();
        Fx::over(Seam::Replaying {
            seam: Mutex::new(ReplaySeam::new(EffectsLog(records))),
            len,
            live: live_next(header),
        })
        .taped(header.mode)
    }

    /// Answer every exchange, clock and sleep from `old`, and tape them again, with this run's
    /// stdout and exit, into `new`.
    /// # Errors
    /// `replay`'s for `old`, `record`'s for `new`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn rerecord(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(old)?;
        let fx = Fx::rerecord_from(&header, records, new)?.with_source(old);
        Ok((fx, header))
    }

    /// `rerecord` over `old`'s header and records already read (`read_tape`).
    /// # Errors
    /// `record`'s for `new`.
    pub(crate) fn rerecord_from(
        header: &tape::TapeHeader,
        mut records: Vec<EffectRecord>,
        new: &Path,
    ) -> Result<Fx> {
        records.retain(|record| !matches!(record, EffectRecord::Emit { .. }));
        let renewed = tape::TapeHeader {
            agh_version: crate::VERSION.to_string(),
            ..header.clone()
        };
        let to = tape::TapeSink::create(new, &renewed)?;
        let len = records.len();
        Ok(Fx::over(Seam::Rerecording {
            from: Mutex::new(ReplaySeam::new(EffectsLog(records))),
            len,
            to,
            live: live_next(header),
        })
        .taped(header.mode))
    }

    /// `record`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `record`.
    #[cfg(any(test, debug_assertions))]
    pub fn record_for_tests(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        Fx::record(path, header)
    }

    /// `replay`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `replay`.
    #[cfg(any(test, debug_assertions))]
    pub fn replay_for_tests(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::replay(path)
    }

    /// `rerecord`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `rerecord`.
    #[cfg(any(test, debug_assertions))]
    pub fn rerecord_for_tests(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::rerecord(old, new)
    }

    /// Which world the doors reach.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        match &self.seam {
            Seam::Live(_) => Mode::Live,
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Mode::Simulated,
            Seam::Recording { .. } => Mode::Recording,
            Seam::Replaying { .. } => Mode::Replaying,
            Seam::Rerecording { .. } => Mode::Rerecording,
        }
    }

    fn latched(&self) -> Result<()> {
        let fault = lock(&self.stream_verdicts)
            .iter()
            .find_map(|verdict| lock(verdict).clone());
        if let Some(e) = fault {
            lock(&self.latch).get_or_insert(e);
        }
        match &*lock(&self.latch) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn latch(&self, e: Error) -> Error {
        if !matches!(e.sub, Sub::Replay(_)) {
            return e;
        }
        let e = match (&self.seam, &e.sub) {
            (
                Seam::Rerecording { live, .. },
                Sub::Replay(
                    Replay::Exhausted
                    | Replay::KindMismatch
                    | Replay::DoorDiverged
                    | Replay::RequestDiverged
                    | Replay::Unconsumed,
                ),
            )
            | (
                Seam::Replaying { live, .. },
                Sub::Replay(Replay::Exhausted | Replay::KindMismatch | Replay::DoorDiverged),
            ) => e.with_next(live.clone()),
            _ => e,
        };
        let mut latch = lock(&self.latch);
        latch.get_or_insert(e).clone()
    }

    fn replayed<T>(&self, answer: std::result::Result<T, ReplayDivergence>) -> Result<T> {
        answer.map_err(|d| self.latch(diverged(&d)))
    }

    /// A recording that cannot tape this exchange as it was (a request or reply the tape's
    /// shapes would change): the tape discarded, and `error` latched.
    fn shaped_out(&self, sink: &tape::TapeSink, error: Error) -> Error {
        sink.discard();
        self.latch(error)
    }

    /// The replaying and rerecording arms of an exchange door (`sleep`, `gh`, `route`,
    /// `fact`, `git`): the latch, then `from`'s reply at `door` to `request()`, decoded by
    /// `decode` (a divergence or a decode error latched). Rerecording, `rerecorded` judges the
    /// decoded reply before the exchange is taped again into the new tape; replaying never
    /// runs it.
    fn replay_exchange<T>(
        &self,
        from: &Mutex<ReplaySeam>,
        door: &str,
        request: impl FnOnce() -> Result<Vec<u8>>,
        decode: impl FnOnce(&[u8]) -> Result<T>,
        rerecorded: impl FnOnce(&[u8]) -> Result<()>,
    ) -> Result<T> {
        self.latched()?;
        let request = request()?;
        let reply = self.replayed(lock(from).exchange(door, &request, &mut |_| Vec::new()))?;
        let answer = decode(&reply).map_err(|e| self.latch(e))?;
        if let Seam::Rerecording { to, .. } = &self.seam {
            rerecorded(&reply)?;
            to.append(&EffectRecord::Exchange {
                door: door.to_string(),
                request,
                reply,
            });
        }
        Ok(answer)
    }

    /// Time since this Fx was made.
    /// # Errors
    /// A replay's divergence (`Sub::Replay`), which the Fx latches.
    pub fn mono(&self) -> Result<Duration> {
        let nanos = match &self.seam {
            Seam::Live(seam) => infallible(lock(seam).mono_nanos()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => return Ok(clock.mono()),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).mono_nanos())
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).mono_nanos())?
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let nanos = self.replayed(lock(from).mono_nanos())?;
                to.append(&EffectRecord::Mono(nanos));
                nanos
            }
        };
        Ok(Duration::from_nanos(nanos))
    }

    /// Unix seconds: the wall clock's nanoseconds over 10^9, rounded down.
    /// # Errors
    /// As `mono`.
    pub fn wall_secs(&self) -> Result<u64> {
        let nanos = match &self.seam {
            Seam::Live(seam) => infallible(lock(seam).now_nanos()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => return Ok(clock.wall_secs()),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).now_nanos())
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).now_nanos())?
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let nanos = self.replayed(lock(from).now_nanos())?;
                to.append(&EffectRecord::Clock(nanos));
                nanos
            }
        };
        Ok(nanos / 1_000_000_000)
    }

    /// Sleep `d`.
    /// # Errors
    /// As `mono`.
    pub fn sleep(&self, d: Duration) -> Result<()> {
        match &self.seam {
            Seam::Live(_) => std::thread::sleep(d),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => clock.sleep(d),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                std::thread::sleep(d);
                let request = sleep_request(d);
                infallible(lock(seam).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()));
            }
            Seam::Replaying { seam: from, .. } | Seam::Rerecording { from, .. } => {
                self.replay_exchange(
                    from,
                    DOOR_SLEEP,
                    || Ok(sleep_request(d)),
                    |_| Ok(()),
                    |_| Ok(()),
                )?;
            }
        }
        Ok(())
    }

    /// One GitHub round trip, its body read whole inside the door under `req.limit`.
    /// # Errors
    /// The outer `Result`: a replay's divergence.
    pub(crate) fn gh(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        let keep = |headers| canon::keep(endpoints, headers);
        #[cfg(any(test, debug_assertions))]
        lock(&self.requested).push(req.deadlines);
        match &self.seam {
            Seam::Live(_) => Ok(read_whole(transport, req, keep).map(canon::Reply::into_response)),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {
                Ok(read_whole(transport, req, keep).map(canon::Reply::into_response))
            }
            Seam::Recording { seam, sink } => {
                self.latched()?;
                let request = self.canonical(&req, endpoints)?;
                if tape::redacts(&request) {
                    return Err(self.shaped_out(sink, shaped_request()));
                }
                let answer = read_whole(transport, req, keep);
                let reply = match &answer {
                    Ok(reply) => canon::reply(reply.status, &reply.headers, &reply.body),
                    Err(e) => canon::failure(e),
                };
                if tape::redacts_a_reply(&reply) {
                    return Err(self.shaped_out(sink, shaped_reply()));
                }
                tape_exchange(seam, DOOR_GH, &request, &reply);
                Ok(answer.map(canon::Reply::into_response))
            }
            Seam::Replaying { seam: from, .. } | Seam::Rerecording { from, .. } => {
                let answer = self.replay_exchange(
                    from,
                    DOOR_GH,
                    || self.canonical(&req, endpoints),
                    canon::decode,
                    |reply| {
                        if tape::redacts_a_reply(reply) {
                            Err(self.latch(shaped_reply()))
                        } else {
                            Ok(())
                        }
                    },
                )?;
                Ok(answer.map(|r| kept_response(endpoints, r)))
            }
        }
    }

    /// `gh`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `gh`.
    #[cfg(any(test, debug_assertions))]
    pub fn gh_for_tests(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        self.gh(transport, endpoints, req)
    }

    /// A request's canonical bytes on this tape: `canon::request` under a `mode anonymous`
    /// tape, `canon::request_credentialed` under a `mode credentialed` one (plan §4.6.19).
    fn canonical(&self, req: &WireRequest<'_>, endpoints: &Endpoints) -> Result<Vec<u8>> {
        let canonical = match self.tape {
            TapeMode::Credentialed => canon::request_credentialed(req, endpoints),
            _ => canon::request(req, endpoints),
        };
        canonical.map_err(|e| self.latch(e))
    }

    /// A streamed round trip (`Github::call_stream`: downloads).
    ///
    /// Under a `mode credentialed` tape (unix, with the endpoints `with_endpoints` set), the
    /// answer is an exchange at `DOOR_GH` whose reply holds its status, its kept headers and
    /// `body-file sha256:{hex} {len}` (`canon::stream_reply`). Recording, the body is read
    /// whole inside the door (at most `req.limit + 1` bytes) into `{tape}.d/.partial-{n}`,
    /// hashed and scanned for a credential or a token shape as it is written, and renamed
    /// `{tape}.d/{hex}` (`TapeSink::store_body`); the caller streams the file it was written to.
    /// Replaying, the caller streams `{source}.d/{hex}`, hashed as it is read
    /// (`tape::DigestReader`). Rerecording, that file is copied, checked, into the new tape's
    /// `.d` directory under the same name, and the caller streams the copy.
    ///
    /// # Errors
    ///
    /// Recording, replaying and rerecording an anonymous tape, or a credentialed one off unix:
    /// `Replay::StreamNotTaped`, before anything is sent. Replaying or rerecording a
    /// credentialed tape: `Replay::StreamDigest` when the body's file is missing, before any
    /// byte is given; when its bytes are not the taped digest and length, at their end (the
    /// read fails, and the run's exit is the latched replay error). A recording whose body
    /// file cannot be written is could_not_run, `Sub::Local`, and its tape is removed. A body
    /// that holds a credential or a token shape is `Replay::BodyNotTaped`, recording or
    /// rerecording, before the caller gets a byte: its tape and the body files it stored are
    /// removed.
    pub(crate) fn gh_stream(
        &self,
        transport: &dyn Transport,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        #[cfg(any(test, debug_assertions))]
        lock(&self.requested).push(req.deadlines);
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.latched()?;
                #[cfg(unix)]
                if self.tape == TapeMode::Credentialed
                    && let Some(endpoints) = &self.endpoints
                {
                    return self.stream_taped(transport, endpoints, req);
                }
                return Err(self.latch(stream_not_taped(self.tape, self.endpoints.is_some())));
            }
        }
        Ok(transport.round_trip(req).map(|response| WireResponse {
            headers: canon::cut(response.headers),
            ..response
        }))
    }

    /// `gh_stream`'s credentialed tape arms (unix).
    #[cfg(unix)]
    fn stream_taped(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        let request = self.canonical(&req, endpoints)?;
        let n = self.streams.fetch_add(1, Ordering::Relaxed);
        match &self.seam {
            Seam::Recording { seam, sink } => {
                if tape::redacts(&request) {
                    return Err(self.shaped_out(sink, shaped_request()));
                }
                let limit = req.limit;
                let mut response = match transport.round_trip(req) {
                    Ok(response) => response,
                    Err(e) => {
                        let reply = canon::failure(&e);
                        tape_exchange(seam, DOOR_GH, &request, &reply);
                        return Ok(Err(e));
                    }
                };
                let stored = match sink.store_body(n, &mut response.body, limit) {
                    Ok(stored) => stored,
                    Err(tape::StoreFault::Read(e)) => {
                        let wire = WireError {
                            phase: crate::wire::Phase::Answered,
                            kind: crate::wire::wire_kind_of(&e),
                            detail: e.to_string(),
                        };
                        let reply = canon::failure(&wire);
                        tape_exchange(seam, DOOR_GH, &request, &reply);
                        return Ok(Err(wire));
                    }
                    Err(tape::StoreFault::Write(e)) => {
                        sink.discard();
                        return Err(e);
                    }
                    Err(tape::StoreFault::Shaped) => {
                        return Err(self.shaped_out(sink, shaped_reply()));
                    }
                };
                let headers = canon::cut(response.headers);
                let kept = canon::keep(endpoints, headers.clone());
                let reply = canon::stream_reply(response.status, &kept, &stored.hex, stored.len);
                if tape::redacts(&reply) {
                    return Err(self.shaped_out(sink, shaped_reply()));
                }
                tape_exchange(seam, DOOR_GH, &request, &reply);
                Ok(Ok(WireResponse {
                    status: response.status,
                    headers,
                    body: Box::new(stored.file),
                }))
            }
            Seam::Replaying { seam, .. } => {
                let reply =
                    self.replayed(lock(seam).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let head = match canon::stream_decode(&reply).map_err(|e| self.latch(e))? {
                    Ok(head) => head,
                    Err(wire) => return Ok(Err(wire)),
                };
                let verdict = Arc::new(Mutex::new(None));
                let body = tape::DigestReader::open(
                    &self.source_dir()?,
                    &head.hex,
                    head.len,
                    verdict.clone(),
                )
                .map_err(|e| self.latch(e))?;
                lock(&self.stream_verdicts).push(verdict);
                Ok(Ok(WireResponse {
                    status: head.status,
                    headers: canon::keep(endpoints, head.headers),
                    body: Box::new(body),
                }))
            }
            Seam::Rerecording { from, to, .. } => {
                let reply =
                    self.replayed(lock(from).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let head = match canon::stream_decode(&reply).map_err(|e| self.latch(e))? {
                    Ok(head) => head,
                    Err(wire) => {
                        to.append(&EffectRecord::Exchange {
                            door: DOOR_GH.to_string(),
                            request,
                            reply,
                        });
                        return Ok(Err(wire));
                    }
                };
                if tape::redacts(&reply) {
                    return Err(self.latch(shaped_reply()));
                }
                let old = self.source_dir()?;
                let verdict = Arc::new(Mutex::new(None));
                let mut body = tape::DigestReader::open(&old, &head.hex, head.len, verdict.clone())
                    .map_err(|e| self.latch(e))?;
                let other = || {
                    tape::stream_digest(&old, &head.hex, "holds other bytes than the tape names")
                };
                // A copy that is not the taped body is the new sink's, which `finish` discards
                // with the rest of a rerecording that latched.
                let copy = match to.store_body(n, &mut body, head.len) {
                    Ok(copy) if copy.hex == head.hex && copy.len == head.len => copy,
                    Ok(_) => return Err(self.latch(other())),
                    Err(tape::StoreFault::Read(_)) => {
                        return Err(self.latch(lock(&verdict).clone().unwrap_or_else(other)));
                    }
                    Err(tape::StoreFault::Write(e)) => {
                        to.discard();
                        return Err(e);
                    }
                    Err(tape::StoreFault::Shaped) => {
                        to.discard();
                        return Err(self.latch(shaped_reply()));
                    }
                };
                to.append(&EffectRecord::Exchange {
                    door: DOOR_GH.to_string(),
                    request,
                    reply,
                });
                Ok(Ok(WireResponse {
                    status: head.status,
                    headers: canon::keep(endpoints, head.headers),
                    body: Box::new(copy.file),
                }))
            }
            Seam::Live(_) => Err(self.latch(stream_not_taped(self.tape, self.endpoints.is_some()))),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {
                Err(self.latch(stream_not_taped(self.tape, self.endpoints.is_some())))
            }
        }
    }

    /// `{source}.d`, where a replayed tape's streamed bodies are.
    /// # Errors
    /// With no source (`with_source` never set it): `Replay::StreamNotTaped`, latched.
    #[cfg(unix)]
    fn source_dir(&self) -> Result<std::path::PathBuf> {
        match &self.source {
            Some(source) => Ok(tape::sidecar_dir(source)),
            None => Err(self.latch(stream_without_source())),
        }
    }
}

impl Fx {
    /// `gh_stream`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `gh_stream`.
    #[cfg(any(test, debug_assertions))]
    pub fn gh_stream_for_tests(
        &self,
        transport: &dyn Transport,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        self.gh_stream(transport, req)
    }

    /// One stdout write, as rendered, before the terminal filter.
    /// # Errors
    /// A replay's divergence, latched.
    pub fn emit_stdout(&self, bytes: &[u8]) -> Result<()> {
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).emit(DOOR_STDOUT, bytes));
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).emit(DOOR_STDOUT, &tape::shaped(bytes)))?;
            }
            Seam::Rerecording { to, .. } => {
                self.latched()?;
                to.append(&EffectRecord::Emit {
                    door: DOOR_STDOUT.to_string(),
                    bytes: bytes.to_vec(),
                });
            }
        }
        Ok(())
    }

    /// The run's end.
    /// # Errors
    /// Under a tape: the latched error, or `tape::TapeSink::finish`'s; replaying, a divergence
    /// at the exit; replaying and rerecording, `Replay::Unconsumed`, why
    /// `the tape holds {n} more records after the run's exit`.
    pub fn finish(&self, exit: u8) -> Result<()> {
        let code = exit.to_string();
        match &self.seam {
            Seam::Live(_) => Ok(()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(()),
            Seam::Recording { seam, sink } => {
                if sink.discarded() {
                    return self.latched();
                }
                infallible(lock(seam).emit(DOOR_EXIT, code.as_bytes()));
                sink.finish()
            }
            Seam::Replaying { seam, len, .. } => {
                self.latched()?;
                let mut seam = lock(seam);
                self.replayed(seam.emit(DOOR_EXIT, code.as_bytes()))?;
                seam.finish().map_err(|d| self.latch(ended(&d, *len)))
            }
            Seam::Rerecording { from, len, to, .. } => {
                let replayed = self
                    .latched()
                    .and_then(|()| lock(from).finish().map_err(|d| self.latch(ended(&d, *len))));
                if replayed.is_err() {
                    to.discard();
                    return replayed;
                }
                to.append(&EffectRecord::Emit {
                    door: DOOR_EXIT.to_string(),
                    bytes: code.into_bytes(),
                });
                to.finish()
            }
        }
    }

    /// Every sleep so far, in order: simulated only, empty in every other mode.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn slept(&self) -> Vec<Duration> {
        match &self.seam {
            Seam::Simulated(clock) => clock.slept(),
            _ => Vec::new(),
        }
    }

    /// A routing decision. Live and simulated: `decide()`. Recording a `mode credentialed`
    /// tape: `decide()`, taped as an exchange at `DOOR_ROUTE` (`canon::route_request`,
    /// `canon::route_reply`). Replaying one: the tape's decision for the same inputs, and
    /// `decide` is never called; another input is `replay.request`. Rerecording: as
    /// replaying, taped again. Under a `mode anonymous` tape, in every mode: `decide()`,
    /// untaped, over the empty router M1's tapes run with, so every M1 tape replays unchanged.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched; recording, `Replay::StreamNotTaped`, latched with the
    /// tape discarded, for a request the tape would change (a token shape); in any taped mode
    /// of a `mode credentialed` tape, the error an earlier door latched.
    pub(crate) fn route(
        &self,
        req: &RouteRequest,
        decide: &mut dyn FnMut() -> Decision,
    ) -> Result<Decision> {
        if self.tape != TapeMode::Credentialed {
            return Ok(decide());
        }
        match &self.seam {
            Seam::Live(_) => Ok(decide()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(decide()),
            Seam::Recording { seam, sink } => {
                self.latched()?;
                let request = canon::route_request(req);
                if tape::redacts(&request) {
                    return Err(self.shaped_out(sink, shaped_request()));
                }
                let decision = decide();
                let reply = canon::route_reply(&decision);
                tape_exchange(seam, DOOR_ROUTE, &request, &reply);
                Ok(decision)
            }
            Seam::Replaying { seam: from, .. } | Seam::Rerecording { from, .. } => self
                .replay_exchange(
                    from,
                    DOOR_ROUTE,
                    || Ok(canon::route_request(req)),
                    canon::route_decode,
                    |_| Ok(()),
                ),
        }
    }

    /// A routing fact a decision does not carry (`question` about `profile`: whether its
    /// drafts are vouched, where its token lives), which a replay's empty router cannot read.
    /// Live and simulated, and under a `mode anonymous` tape: `answer()`. Recording a
    /// `mode credentialed` tape: `answer()`, taped as an exchange at `DOOR_ROUTE`
    /// (`canon::fact_request`, `canon::fact_reply`). Replaying one: the tape's answer for the
    /// same question, and `answer` is never called; another question is `replay.request`.
    /// Rerecording: as replaying, taped again.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched; recording, `Replay::StreamNotTaped`, latched with the
    /// tape discarded, for a question, profile or answer the tape would change (a token shape,
    /// a CR or an LF); in any taped mode of a `mode credentialed` tape, the error an earlier
    /// door latched.
    pub(crate) fn fact(
        &self,
        question: &str,
        profile: &str,
        answer: &mut dyn FnMut() -> String,
    ) -> Result<String> {
        if self.tape != TapeMode::Credentialed {
            return Ok(answer());
        }
        match &self.seam {
            Seam::Live(_) => Ok(answer()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(answer()),
            Seam::Recording { seam, sink } => {
                self.latched()?;
                // Judged before `canon` shapes and flattens them: a text the tape would
                // change could not replay as this run reads it, so the tape is discarded.
                let raw = answer();
                let changes =
                    |text: &str| Redactor::shapes(text) != text || text.contains(['\r', '\n']);
                if changes(question) || changes(profile) || changes(&raw) {
                    return Err(self.shaped_out(sink, shaped_request()));
                }
                let request = canon::fact_request(question, profile);
                let reply = canon::fact_reply(&raw);
                tape_exchange(seam, DOOR_ROUTE, &request, &reply);
                canon::fact_decode(&reply).map_err(|e| self.latch(e))
            }
            Seam::Replaying { seam: from, .. } | Seam::Rerecording { from, .. } => self
                .replay_exchange(
                    from,
                    DOOR_ROUTE,
                    || Ok(canon::fact_request(question, profile)),
                    canon::fact_decode,
                    |_| Ok(()),
                ),
        }
    }

    /// The tape's next journal step: its own count of journal emits, from 0.
    #[cfg(any(unix, test))]
    fn next_journal_step(&self) -> u64 {
        self.journal_steps.fetch_add(1, Ordering::Relaxed)
    }

    /// One emit at `door` under a tape: recorded, compared on replay as stdout is, or taped
    /// again when rerecording. Live and simulated: nothing.
    fn emit_taped(&self, door: &str, bytes: &[u8]) -> Result<()> {
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).emit(door, bytes));
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).emit(door, &tape::shaped(bytes)))?;
            }
            Seam::Rerecording { to, .. } => {
                self.latched()?;
                to.append(&EffectRecord::Emit {
                    door: door.to_string(),
                    bytes: bytes.to_vec(),
                });
            }
        }
        Ok(())
    }

    /// One journal record's body. Live and simulated: `Ok(None)`, and the journal writes its
    /// file. Under any tape: an emit at `DOOR_JOURNAL`, compared on replay as stdout is, and
    /// `Ok(Some(step))` from the tape's own counter of journal emits, from 0.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched.
    #[cfg(any(unix, test))]
    pub(crate) fn emit_journal(&self, body: &[u8]) -> Result<Option<u64>> {
        match &self.seam {
            Seam::Live(_) => Ok(None),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(None),
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.emit_taped(DOOR_JOURNAL, body)?;
                Ok(Some(self.next_journal_step()))
            }
        }
    }

    /// One fabric post. Live and simulated: `send()`, its answer returned. Under any tape: an
    /// emit at `DOOR_FABRIC` of `line`, compared on replay; `send` is never called, and the
    /// answer is `Posted::Skipped("taped")`.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched.
    pub(crate) fn emit_fabric(
        &self,
        line: &str,
        send: &mut dyn FnMut() -> crate::notify::Posted,
    ) -> Result<crate::notify::Posted> {
        match &self.seam {
            Seam::Live(_) => Ok(send()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(send()),
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.emit_taped(DOOR_FABRIC, line.as_bytes())?;
                Ok(crate::notify::Posted::Skipped("taped".to_string()))
            }
        }
    }

    /// One child process (`gh auth token`). Live: `child::run(spec)`. Simulated: first the
    /// live arm's refusal of a relative program (`child::check`, so a run the live seam would
    /// refuse fails its simulated test too), then the next scripted child, whose program and
    /// args must equal `spec`'s (a mismatch panics in the test that scripted it); an empty
    /// scripted queue is could_not_run, why `{program} could not start: no scripted child`, as
    /// a missing program is live. Under any tape: `Replay::StreamNotTaped`, before anything
    /// runs.
    ///
    /// # Errors
    ///
    /// As above; the run's could_not_run.
    pub(crate) fn child(&self, spec: &child::ChildSpec) -> Result<child::ChildOut> {
        match &self.seam {
            Seam::Live(_) => child::run(spec),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => {
                child::check(spec)?;
                clock.log(spec);
                let Some(scripted) = clock.next_child() else {
                    return Err(Error::local(format!(
                        "{} could not start: no scripted child",
                        spec.program.display()
                    )));
                };
                let args: Vec<String> = spec
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(
                    (&scripted.program, &scripted.args),
                    (&spec.program, &args),
                    "the scripted child is not the one this run asked for"
                );
                Ok(scripted.out)
            }
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.latched()?;
                Err(self.latch(child_not_taped()))
            }
        }
    }

    /// One git run (`Github::run_git`). Live and simulated: `child::run(spec)`, the real
    /// `git` (a simulated test runs the host's git against agh-fake or a local repository;
    /// simulated, `child::check` passes before the spec is logged, as in `child`).
    /// Recording: run, and taped as an exchange at `DOOR_GIT` (`canon::git_request`,
    /// `canon::git_reply`); a run that ended with no exit is taped as its error
    /// (`canon::git_failure`). Replaying: the tape's answer, its output or its error
    /// (`canon::git_answer`), and nothing runs. Rerecording: as replaying, taped again.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched; the run's could_not_run, recorded or replayed (not
    /// latched: it is the run's own answer, as a gh round trip's failure is).
    #[cfg(any(unix, test))]
    pub(crate) fn git(&self, spec: &child::ChildSpec, canonical: &[u8]) -> Result<child::ChildOut> {
        match &self.seam {
            Seam::Live(_) => child::run(spec),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => {
                child::check(spec)?;
                clock.log(spec);
                child::run(spec)
            }
            Seam::Recording { seam, sink } => {
                self.latched()?;
                if tape::redacts(canonical) {
                    return Err(self.shaped_out(sink, shaped_request()));
                }
                let ran = child::run(spec);
                let reply = match &ran {
                    Ok(out) => canon::git_reply(out),
                    Err(e) => canon::git_failure(e),
                };
                tape_exchange(seam, DOOR_GIT, canonical, &reply);
                ran
            }
            Seam::Replaying { seam: from, .. } | Seam::Rerecording { from, .. } => self
                .replay_exchange(
                    from,
                    DOOR_GIT,
                    || Ok(canonical.to_vec()),
                    canon::git_answer,
                    |_| Ok(()),
                )?,
        }
    }

    /// Tests: the same simulated `Fx`, answering `child` from `children` in order.
    ///
    /// # Panics
    ///
    /// When the `Fx` is not simulated: a live `Fx` would run the real child (the host's
    /// `gh auth token`) in place of the scripted one, and a taped one answers every child
    /// `Replay::StreamNotTaped` and latches. A test built either way fails here, before it
    /// reads a real credential or poisons a tape.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_children(self, children: Vec<child::ScriptedChild>) -> Fx {
        let Seam::Simulated(clock) = &self.seam else {
            panic!(
                "Fx::with_children scripts only a simulated Fx, not {:?}",
                self.mode()
            );
        };
        clock.script(children);
        self
    }

    /// Tests: every spec `child` or `git` accepted (one `child::check` passed) under a simulated
    /// `Fx`, in call order, so a test of the git runner reads the environment and directories
    /// its children got.
    /// Empty in every other mode.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn children_run(&self) -> Vec<child::ChildSpec> {
        match &self.seam {
            Seam::Simulated(clock) => clock.children_run(),
            _ => Vec::new(),
        }
    }
}

impl std::fmt::Debug for Fx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fx({:?})", self.mode())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use astream_effects::EffectRecord;

    use super::child::{ChildOut, ChildSpec};
    use super::tape::{TapeHeader, TapeMode};
    use super::{DOOR_EXIT, DOOR_FABRIC, DOOR_GIT, DOOR_JOURNAL, DOOR_ROUTE, Fx, canon, read_tape};
    use crate::error::{Replay, Sub};
    use crate::notify::Posted;
    use crate::policy::{Decision, RouteRequest, RouteTarget, Tier};
    use crate::redact::Redactor;
    use crate::slug::Slug;
    use crate::wire::Endpoints;

    #[cfg(unix)]
    #[test]
    fn a_pipe_writer_that_never_finishes_expires() {
        use std::io::{ErrorKind, Write as _};
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::time::Duration;

        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"partial tape").expect("partial input");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(super::NONBLOCK_OPEN_FLAG)
            .open(format!("/dev/fd/{}", reader.as_raw_fd()))
            .expect("nonblocking pipe");
        let error = super::read_pipe(&file, Duration::from_millis(10))
            .expect_err("an open writer cannot hold replay forever");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        drop(writer);
        drop(reader);
    }

    fn scratch() -> aterm_tempfile::TempDir {
        crate::test_fixtures::scratch_dir("fx")
    }

    /// A streamed asset's GET, as `dev`: the request the streamed-body tests tape.
    #[cfg(unix)]
    fn asset_request() -> crate::wire::WireRequest<'static> {
        let mut req = crate::wire::WireRequest::new(
            crate::wire::Method::Get,
            "https://api.github.com/repos/o/r/releases/assets/1",
            vec![("Accept".to_string(), "application/octet-stream".to_string())],
            1 << 20,
            crate::wire::Deadlines::JSON,
        );
        req.as_profile = Some("dev");
        req
    }

    fn header(mode: TapeMode) -> TapeHeader {
        let argv: Vec<String> = ["repo", "view", "o/r"].map(String::from).to_vec();
        TapeHeader {
            mode,
            ..TapeHeader::for_run(&argv, &Endpoints::github_com())
        }
    }

    fn records(path: &Path) -> Vec<EffectRecord> {
        read_tape(path).expect("the tape reads").1
    }

    fn write(path: &Path, mode: TapeMode, records: &[EffectRecord]) {
        std::fs::write(path, super::tape::render(&header(mode), records)).expect("the tape");
    }

    fn exit() -> EffectRecord {
        EffectRecord::Emit {
            door: DOOR_EXIT.to_string(),
            bytes: b"0".to_vec(),
        }
    }

    fn route() -> RouteRequest {
        RouteRequest::read(RouteTarget::Repo(Slug::parse("o/r").expect("a slug")))
            .with_as(Some("dev".to_string()))
    }

    fn sent() -> Decision {
        Decision::Send {
            profile: "dev".to_string(),
            tier: Tier::Dev,
        }
    }

    /// A credentialed recording tapes each decision; its replay answers it, never deciding,
    /// and another question is a request divergence.
    #[test]
    fn a_credentialed_tape_replays_the_route_without_deciding() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
            .expect("a recording");
        let mut decided = 0;
        let decision = fx
            .route(&route(), &mut || {
                decided += 1;
                sent()
            })
            .expect("decided");
        assert_eq!((decision, decided), (sent(), 1));
        fx.finish(0).expect("the tape is whole");
        assert_eq!(
            records(&path),
            [
                EffectRecord::Exchange {
                    door: DOOR_ROUTE.to_string(),
                    request: canon::route_request(&route()),
                    reply: b"send dev dev".to_vec(),
                },
                exit()
            ]
        );

        let (replay, _) = Fx::replay(&path).expect("a replay");
        let never = &mut || -> Decision { panic!("a replay never decides") };
        assert_eq!(replay.route(&route(), never).expect("taped"), sent());
        replay.finish(0).expect("replayed whole");

        let (other, _) = Fx::replay(&path).expect("a replay");
        let refused = other
            .route(&route().with_as(None), never)
            .expect_err("another question");
        assert_eq!(refused.sub, Sub::Replay(Replay::RequestDiverged));

        let again = dir.path().join("again.atape");
        let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
        assert_eq!(rerecord.route(&route(), never).expect("taped"), sent());
        rerecord.finish(0).expect("rerecorded whole");
        assert_eq!(records(&again), records(&path));
    }

    /// An anonymous tape decides every time and tapes no route record, in every tape mode.
    #[test]
    fn an_anonymous_tape_never_tapes_the_route() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let fx = Fx::record(&path, &header(TapeMode::Anonymous)).expect("a recording");
        assert_eq!(fx.route(&route(), &mut sent).expect("decided"), sent());
        fx.finish(0).expect("whole");
        assert_eq!(records(&path), [exit()]);
        let (replay, _) = Fx::replay(&path).expect("a replay");
        assert_eq!(replay.route(&route(), &mut sent).expect("decided"), sent());
        replay.finish(0).expect("replayed whole");
    }

    /// The journal's and the fabric's doors are emits under a tape: the journal's step is the
    /// tape's own count from 0, the fabric's `send` never runs, and a replay compares both.
    #[test]
    fn the_journal_and_fabric_doors_emit_under_a_tape() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let line = "@self post to=say:agh.write kind=note key=agh-1 -- v=1";
        let unsent = &mut || -> Posted { panic!("a tape never posts") };
        let fx = Fx::record(&path, &header(TapeMode::Anonymous)).expect("a recording");
        assert_eq!(fx.emit_journal(b"{\"intent\":1}").expect("taped"), Some(0));
        assert_eq!(fx.emit_journal(b"{\"outcome\":1}").expect("taped"), Some(1));
        assert_eq!(
            fx.emit_fabric(line, unsent).expect("taped"),
            Posted::Skipped("taped".to_string())
        );
        fx.finish(0).expect("whole");
        let emit = |door: &str, bytes: &[u8]| EffectRecord::Emit {
            door: door.to_string(),
            bytes: bytes.to_vec(),
        };
        assert_eq!(
            records(&path),
            [
                emit(DOOR_JOURNAL, b"{\"intent\":1}"),
                emit(DOOR_JOURNAL, b"{\"outcome\":1}"),
                emit(DOOR_FABRIC, line.as_bytes()),
                exit()
            ]
        );

        let (replay, _) = Fx::replay(&path).expect("a replay");
        assert_eq!(
            replay.emit_journal(b"{\"intent\":1}").expect("same"),
            Some(0)
        );
        assert_eq!(
            replay.emit_journal(b"{\"outcome\":1}").expect("same"),
            Some(1)
        );
        assert_eq!(
            replay.emit_fabric(line, unsent).expect("same"),
            Posted::Skipped("taped".to_string())
        );
        replay.finish(0).expect("replayed whole");

        let (other, _) = Fx::replay(&path).expect("a replay");
        let refused = other
            .emit_journal(b"{\"intent\":2}")
            .expect_err("another body");
        assert!(matches!(refused.sub, Sub::Replay(_)), "{refused:?}");

        let simulated = Fx::simulated(0);
        assert_eq!(simulated.emit_journal(b"x").expect("untaped"), None);
        let mut posted = 0;
        let answer = simulated
            .emit_fabric(line, &mut || {
                posted += 1;
                Posted::Sent("1".to_string())
            })
            .expect("sent");
        assert_eq!((answer, posted), (Posted::Sent("1".to_string()), 1));
    }

    /// A replayed git run is the tape's answer and runs nothing: the spec names the bare `git`,
    /// which `child::check` would refuse. A recording that cannot run tapes its failure, which
    /// replays as the same error.
    #[test]
    fn the_git_door_replays_with_nothing_run() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let out = ChildOut {
            code: Some(0),
            stdout: b"0123\trefs/tags/v1\n".to_vec(),
            stderr: Vec::new(),
        };
        let canonical = b"git ls-remote o/r\nref refs/tags/v1";
        let exchange = EffectRecord::Exchange {
            door: DOOR_GIT.to_string(),
            request: canonical.to_vec(),
            reply: canon::git_reply(&out),
        };
        write(&path, TapeMode::Credentialed, &[exchange.clone(), exit()]);
        let bare = ChildSpec::new(Path::new("git"));

        let (replay, _) = Fx::replay(&path).expect("a replay");
        assert_eq!(replay.git(&bare, canonical).expect("taped"), out);
        replay.finish(0).expect("replayed whole");

        let (other, _) = Fx::replay(&path).expect("a replay");
        let refused = other
            .git(&bare, b"git ls-remote o/r")
            .expect_err("another run");
        assert_eq!(refused.sub, Sub::Replay(Replay::RequestDiverged));

        let again = dir.path().join("again.atape");
        let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
        assert_eq!(rerecord.git(&bare, canonical).expect("taped"), out);
        rerecord.finish(0).expect("rerecorded whole");
        assert_eq!(records(&again), [exchange, exit()]);

        let unrun = dir.path().join("unrun.atape");
        let fx = Fx::record(&unrun, &header(TapeMode::Credentialed)).expect("a recording");
        let refused = fx.git(&bare, canonical).expect_err("a relative git");
        assert_eq!(refused.sub, Sub::Local);
        fx.finish(1).expect("whole");
        let unrun_records = records(&unrun);
        let failed = EffectRecord::Exchange {
            door: DOOR_GIT.to_string(),
            request: canonical.to_vec(),
            reply: canon::git_failure(&refused),
        };
        assert_eq!(unrun_records.first(), Some(&failed), "{unrun_records:?}");
        let (replay, _) = Fx::replay(&unrun).expect("a replay");
        let replayed = replay.git(&bare, canonical).expect_err("the taped failure");
        assert_eq!(
            (
                replayed.kind,
                replayed.sub.clone(),
                replayed.side_effect,
                replayed.why.clone()
            ),
            (
                refused.kind,
                refused.sub.clone(),
                refused.side_effect,
                refused.why.clone()
            )
        );
        replay.finish(1).expect("replayed whole");
    }

    /// A routing fact whose answer holds a token shape is never taped: the recording refuses
    /// it, latches, and discards the tape, whose fact would replay `[redacted]`.
    #[test]
    fn a_shaped_fact_discards_the_recording() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
            .expect("a recording");
        let token = agh_fake::token("fact");
        let refused = fx
            .fact("login", "dev", &mut || token.clone())
            .expect_err("a shaped answer");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
        let mut plain = || "fixture-dev-bot".to_string();
        assert!(fx.fact("login", "dev", &mut plain).is_err(), "latched");
        assert!(fx.finish(0).is_err(), "a discarded tape is not whole");
        assert!(!path.exists(), "the tape is removed");
    }

    /// A child is never taped, in any tape mode.
    #[test]
    fn a_child_is_never_taped() {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
            .expect("a recording");
        let refused = fx
            .child(&ChildSpec::new(Path::new("/bin/gh")))
            .expect_err("not taped");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
    }

    /// A credentialed streamed answer is taped by its digest: a 0600 file in a 0700
    /// `{tape}.d`, a `body-file` line on the tape, a replay that streams the same bytes, a
    /// rerecording that copies the file, and `Replay::StreamDigest` for a file that is
    /// missing or holds other bytes.
    #[cfg(unix)]
    #[test]
    fn a_streamed_body_is_taped_by_its_digest() {
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;

        use crate::wire::scripted::ScriptedTransport;

        let e = Endpoints::github_com();
        let request = asset_request;
        let body = b"the bytes of one asset".to_vec();
        let hex = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&body));
        let read = |fx: &Fx, transport: &ScriptedTransport| {
            let mut answer = fx
                .gh_stream(transport, request())
                .expect("the door")
                .expect("an answer");
            let mut got = Vec::new();
            answer.body.read_to_end(&mut got).map(|_| got)
        };

        let dir = scratch();
        let path = dir.path().join("t.atape");
        let sidecars = super::tape::sidecar_dir(&path);
        // A partial file a killed recording left is in the way of nothing.
        aterm_uds::private_file::create_private_dir(&sidecars).expect("the body directory");
        std::fs::write(sidecars.join(".partial-0"), b"a killed run's").expect("a stale partial");
        let transport = ScriptedTransport::new();
        transport.answer(200, &[("Content-Type", "application/octet-stream")], &body);
        let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
            .expect("a recording")
            .with_endpoints(&e);
        assert_eq!(read(&fx, &transport).expect("streams"), body);
        fx.finish(0).expect("whole");

        let mode = |p: &Path| std::fs::metadata(p).expect("there").permissions().mode() & 0o777;
        assert_eq!(mode(&sidecars), 0o700);
        assert_eq!(mode(&sidecars.join(&hex)), 0o600);
        assert_eq!(
            std::fs::read_dir(&sidecars).expect("the directory").count(),
            1,
            "no partial file is left"
        );
        let taped = records(&path);
        let Some(EffectRecord::Exchange { reply, .. }) = taped.first() else {
            panic!("an exchange first: {taped:?}");
        };
        let reply = String::from_utf8(reply.clone()).expect("UTF-8");
        assert!(
            reply.ends_with(&format!("\nbody-file sha256:{hex} {}", body.len())),
            "{reply}"
        );

        let silent = ScriptedTransport::new();
        let (replay, _) = Fx::replay(&path).expect("a replay");
        assert_eq!(
            read(&replay.with_endpoints(&e), &silent).expect("streams"),
            body
        );

        let again = dir.path().join("again.atape");
        let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
        let rerecord = rerecord.with_endpoints(&e);
        assert_eq!(read(&rerecord, &silent).expect("streams"), body);
        rerecord.finish(0).expect("rerecorded whole");
        assert_eq!(
            std::fs::read(super::tape::sidecar_dir(&again).join(&hex)).expect("the copy"),
            body
        );
        assert!(silent.requests().is_empty());

        let mut other = body.clone();
        other[0] ^= 1;
        std::fs::write(sidecars.join(&hex), &other).expect("a changed body");
        let (replay, _) = Fx::replay(&path).expect("a replay");
        let replay = replay.with_endpoints(&e);
        assert!(read(&replay, &silent).is_err(), "the read fails at its end");
        let refused = replay.finish(0).expect_err("the latched digest");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));

        std::fs::remove_file(sidecars.join(&hex)).expect("removed");
        let (replay, _) = Fx::replay(&path).expect("a replay");
        let refused = replay
            .with_endpoints(&e)
            .gh_stream(&silent, request())
            .expect_err("no body file");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));

        // A link in its place is not followed, even to the taped bytes.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, &body).expect("the bytes elsewhere");
        std::os::unix::fs::symlink(&elsewhere, sidecars.join(&hex)).expect("a link");
        let (replay, _) = Fx::replay(&path).expect("a replay");
        let refused = replay
            .with_endpoints(&e)
            .gh_stream(&silent, request())
            .expect_err("a link is no body file");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));
        assert!(silent.requests().is_empty());
    }

    /// A streamed body that holds a credential the run learned, or a token shape, is not
    /// taped: the recording stops with `Replay::BodyNotTaped` before its caller reads a byte,
    /// and removes its tape and every body file it stored, `{tape}.d` with them.
    #[cfg(unix)]
    #[test]
    fn a_streamed_body_holding_a_credential_is_not_taped() {
        use crate::wire::scripted::ScriptedTransport;

        let e = Endpoints::github_com();
        let request = asset_request;
        let learned = b"a-learned-credential-of-no-shape".to_vec();
        let shaped = agh_fake::token("streamed-body");
        let mut far = vec![b'x'; 70_000];
        far.extend_from_slice(shaped.as_bytes());
        for (label, body) in [
            (
                "learned",
                [b"head ".as_slice(), learned.as_slice(), b" tail".as_slice()].concat(),
            ),
            ("a shape past the first read", far),
        ] {
            let dir = scratch();
            let path = dir.path().join("t.atape");
            let redactor = Redactor::new();
            redactor.learn(&learned);
            let transport = ScriptedTransport::new();
            transport.answer(
                200,
                &[("Content-Type", "application/octet-stream")],
                b"first",
            );
            transport.answer(200, &[("Content-Type", "application/octet-stream")], &body);
            let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), redactor)
                .expect("a recording")
                .with_endpoints(&e);
            let mut first = fx
                .gh_stream(&transport, request())
                .expect("the door")
                .expect("an answer");
            let mut got = Vec::new();
            std::io::Read::read_to_end(&mut first.body, &mut got).expect("the first body");
            assert!(super::tape::sidecar_dir(&path).exists(), "{label}");
            let refused = fx.gh_stream(&transport, request()).expect_err("not taped");
            assert_eq!(refused.sub, Sub::Replay(Replay::BodyNotTaped), "{label}");
            assert!(fx.finish(0).is_err(), "{label}");
            assert!(!path.exists(), "{label}: the tape is removed");
            assert!(
                !super::tape::sidecar_dir(&path).exists(),
                "{label}: its bodies are removed"
            );
        }
    }

    /// An anonymous tape keeps M1's refusal of a streamed answer.
    #[test]
    fn an_anonymous_tape_never_streams() {
        use crate::wire::scripted::ScriptedTransport;
        use crate::wire::{Deadlines, Method, WireRequest};

        let dir = scratch();
        let path = dir.path().join("t.atape");
        let fx = Fx::record(&path, &header(TapeMode::Anonymous))
            .expect("a recording")
            .with_endpoints(&Endpoints::github_com());
        let transport = ScriptedTransport::new();
        let req = WireRequest::new(
            Method::Get,
            "https://api.github.com/repos/o/r/releases/assets/1",
            Vec::new(),
            1 << 20,
            Deadlines::JSON,
        );
        let refused = fx.gh_stream(&transport, req).expect_err("not taped");
        assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
        assert!(transport.requests().is_empty());
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE effect seam (design §21.2).

pub mod canon;
#[cfg(any(test, debug_assertions))]
mod sim;
pub mod tape;

use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use astream_effects::{
    EffectRecord, EffectSeam, EffectsLog, LiveSeam, OsEffects, RecordingSeam, ReplayDivergence,
    ReplaySeam,
};

use crate::error::{Error, Replay, Result, Sub};
use crate::redact::Redactor;
use crate::wire::{Endpoints, Transport, WireError, WireRequest, WireResponse};

/// A GitHub round trip's door: an exchange.
pub const DOOR_GH: &str = "gh";
/// A sleep's door: an exchange, the length in decimal nanoseconds out and nothing back.
pub const DOOR_SLEEP: &str = "sleep";
/// stdout's door: an emit of the bytes as rendered.
pub const DOOR_STDOUT: &str = "stdout";
/// The exit's door: an emit of the exit code in decimal.
pub const DOOR_EXIT: &str = "exit";

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
    /// `--replay OLD --record NEW`: exchanges and clocks from OLD, taped again, with this run's own stdout and exit, into NEW.
    Rerecording,
}

/// The world behind an `Fx`.
enum Seam {
    /// The OS clocks, behind a mutex held for one clock read.
    Live(Mutex<LiveSeam<OsEffects>>),
    /// A test's virtual clocks.
    #[cfg(any(test, debug_assertions))]
    Simulated(sim::SimClock),
    /// The OS, every door taped through `sink`; the Fx keeps its own clone of the sink to
    /// finish the file.
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
    /// A tape's exchanges and clocks, answered in order and taped again into `to`, with this
    /// run's own emits.
    Rerecording {
        /// The replay seam over the old tape's records, less its emits.
        from: Mutex<ReplaySeam>,
        /// How many records `from` holds.
        len: usize,
        /// The new tape.
        to: tape::TapeSink,
        /// The `next` of a divergence: `record it live: agh --record NEW {argv}`.
        live: String,
    },
}

/// The effect seam.
pub struct Fx {
    seam: Seam,
    /// The first divergence or refusal under a tape: every later door answers it (plan §4.6.27).
    latch: Mutex<Option<Error>>,
}

/// A lock, through a poisoned one: a panicking door leaves no state half-made that the next
/// door could misread, because each door's record is written in one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An infallible clock read, its value.
fn infallible<T>(read: std::result::Result<T, std::convert::Infallible>) -> T {
    match read {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

/// One round trip whose body is read whole under `req.limit`: the status, the headers cut by
/// `cut`, and the body.
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

/// A taped answer as the pipeline reads it, its headers `canon::keep`'s as in every mode: a
/// tape this build did not write (edited by hand, or from a build that keeps more) delivers
/// no header a live run would have dropped.
fn kept_response(endpoints: &Endpoints, reply: canon::Reply) -> WireResponse {
    canon::Reply {
        headers: canon::keep(endpoints, reply.headers),
        ..reply
    }
    .into_response()
}

/// The sleep exchange's request: the length in decimal nanoseconds.
fn sleep_request(d: Duration) -> Vec<u8> {
    d.as_nanos().to_string().into_bytes()
}

/// A replay seam's divergence as agh's error: `Replay::of`, the text through
/// `Redactor::shapes`.
fn diverged(d: &ReplayDivergence) -> Error {
    Error::replay(Replay::of(d), Redactor::shapes(&d.to_string()))
}

/// The refusal a streamed round trip meets under any tape.
fn stream_not_taped() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "a streamed answer is not taped: M1's tapes hold whole answers alone",
    )
    .with_next("run it without --record or --replay: a tape never holds a streamed answer")
}

/// `ReplaySeam::finish`'s refusal of a tape of `len` records: its `Unreached { position, .. }`
/// through `Replay::of` (→ `Unconsumed`), why `the tape holds {n} more records after the run's
/// exit`.
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

/// `Replay::StreamNotTaped` for a recorded request the shapes would change: its tape would
/// hold another question than the one a replay asks.
fn shaped_request() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "this request holds a token shape, which a tape redacts: a tape of it could not replay",
    )
    .with_next("run it without --record: a tape cannot hold this request")
}

/// `Replay::BodyNotTaped` for an answer the shapes would change: its replay would answer the
/// redacted bytes, and render other stdout than this run.
fn shaped_reply() -> Error {
    Error::replay(
        Replay::BodyNotTaped,
        "GitHub's answer holds a token shape, which a tape redacts: a tape of it could not \
         replay",
    )
    .with_next("run it without --record: a tape cannot hold this answer")
}

/// `arg` as one POSIX shell word: as it is when every byte is in `[A-Za-z0-9_./:=@%+,-]` and
/// it is not empty, else in single quotes, each `'` written `'\''`.
fn shell_word(arg: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"_./:=@%+,-".contains(&b);
    if !arg.is_empty() && arg.bytes().all(plain) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The `next` of a seam divergence under a tape: `record it live: agh --record NEW {argv}`,
/// each argument of `header.argv` one POSIX shell word.
fn live_next(header: &tape::TapeHeader) -> String {
    let mut live = String::from("record it live: agh --record NEW");
    for arg in &header.argv {
        live.push(' ');
        live.push_str(&shell_word(&Redactor::shapes(arg)));
    }
    live
}

/// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
fn not_read(path: &Path, e: &std::io::Error) -> Error {
    Error::could_not_run(format!(
        "the tape {} could not be read: {e}",
        path.display()
    ))
    .with_sub(Sub::Local)
}

/// The tape at `path`, read whole (at most `tape::TAPE_MAX` bytes) and parsed.
///
/// # Errors
/// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
pub(crate) fn read_tape(path: &Path) -> Result<(tape::TapeHeader, Vec<EffectRecord>)> {
    let file = std::fs::File::open(path).map_err(|e| not_read(path, &e))?;
    let mut bytes = Vec::new();
    file.take(tape::TAPE_MAX.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| not_read(path, &e))?;
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
    /// An Fx over `seam`, its latch empty.
    fn over(seam: Seam) -> Fx {
        Fx {
            seam,
            latch: Mutex::new(None),
        }
    }

    /// The shipped seam.
    #[must_use]
    pub fn live() -> Fx {
        Fx::over(Seam::Live(Mutex::new(LiveSeam::new(OsEffects::new()))))
    }

    /// Tests: `mono` starts at 0 and `wall_secs` at `start_unix_secs`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn simulated(start_unix_secs: u64) -> Fx {
        Fx::over(Seam::Simulated(sim::SimClock::new(start_unix_secs)))
    }

    /// Tape this run into `path`, which must not exist.
    ///
    /// # Errors
    /// CouldNotRun, `Sub::Local`.
    pub(crate) fn record(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        let sink = tape::TapeSink::create(path, header)?;
        Ok(Fx::over(Seam::Recording {
            seam: Mutex::new(RecordingSeam::with_sink(OsEffects::new(), sink.clone())),
            sink,
        }))
    }

    /// Replay the tape at `path`, read whole (at most `tape::TAPE_MAX` bytes), and hand back its header.
    ///
    /// # Errors
    /// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn replay(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(path)?;
        Ok((Fx::replay_from(&header, records), header))
    }

    /// `replay` over a tape's header and records already read (`read_tape`), so `cli::taping` can read a tape once.
    pub(crate) fn replay_from(header: &tape::TapeHeader, records: Vec<EffectRecord>) -> Fx {
        let len = records.len();
        Fx::over(Seam::Replaying {
            seam: Mutex::new(ReplaySeam::new(EffectsLog(records))),
            len,
            live: live_next(header),
        })
    }

    /// Answer every exchange, clock and sleep from `old`, and tape them again, with this run's stdout and exit, into `new`.
    ///
    /// # Errors
    /// `replay`'s for `old`, `record`'s for `new`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn rerecord(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(old)?;
        let fx = Fx::rerecord_from(&header, records, new)?;
        Ok((fx, header))
    }

    /// `rerecord` over `old`'s header and records already read (`read_tape`), so `cli::taping` reads a tape once for a command line that names its verb.
    ///
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
        }))
    }

    /// `record`, for the integration tests (plan §1, rule 9): its body is that one call.
    ///
    /// # Errors
    /// As `record`.
    #[cfg(any(test, debug_assertions))]
    pub fn record_for_tests(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        Fx::record(path, header)
    }

    /// `replay`, for the integration tests (plan §1, rule 9): its body is that one call.
    ///
    /// # Errors
    /// As `replay`.
    #[cfg(any(test, debug_assertions))]
    pub fn replay_for_tests(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::replay(path)
    }

    /// `rerecord`, for the integration tests (plan §1, rule 9): its body is that one call.
    ///
    /// # Errors
    /// As `rerecord`.
    #[cfg(any(test, debug_assertions))]
    pub fn rerecord_for_tests(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::rerecord(old, new)
    }

    /// Which world the doors reach.
    #[must_use]
    pub fn mode(&self) -> Mode {
        match &self.seam {
            Seam::Live(_) => Mode::Live,
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Mode::Simulated,
            Seam::Recording { .. } => Mode::Recording,
            Seam::Replaying { .. } => Mode::Replaying,
            Seam::Rerecording { .. } => Mode::Rerecording,
        }
    }

    /// The latched error, if a door under a tape has met one.
    fn latched(&self) -> Result<()> {
        match &*lock(&self.latch) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Keep `e` as the latch's error when it is a replay's (`Sub::Replay`) and the latch is
    /// empty; hand back the latch's error, so every door after the first answers that one.
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

    /// A replay seam's answer, its divergence latched.
    fn replayed<T>(&self, answer: std::result::Result<T, ReplayDivergence>) -> Result<T> {
        answer.map_err(|d| self.latch(diverged(&d)))
    }

    /// Time since this Fx was made.
    ///
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
    ///
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
    ///
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
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                let request = sleep_request(d);
                self.replayed(lock(seam).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()))?;
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let request = sleep_request(d);
                let reply =
                    self.replayed(lock(from).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()))?;
                to.append(&EffectRecord::Exchange {
                    door: DOOR_SLEEP.to_string(),
                    request,
                    reply,
                });
            }
        }
        Ok(())
    }

    /// One GitHub round trip, its body read whole inside the door under `req.limit`.
    ///
    /// # Errors
    /// The outer `Result`: a replay's divergence.
    pub(crate) fn gh(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        let keep = |headers| canon::keep(endpoints, headers);
        match &self.seam {
            Seam::Live(_) => Ok(read_whole(transport, req, keep).map(canon::Reply::into_response)),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {
                Ok(read_whole(transport, req, keep).map(canon::Reply::into_response))
            }
            Seam::Recording { seam, sink } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                if tape::redacts(&request) {
                    sink.discard();
                    return Err(self.latch(shaped_request()));
                }
                let answer = read_whole(transport, req, keep);
                let reply = match &answer {
                    Ok(reply) => canon::reply(reply.status, &reply.headers, &reply.body),
                    Err(e) => canon::failure(e),
                };
                if tape::redacts_a_reply(&reply) {
                    sink.discard();
                    return Err(self.latch(shaped_reply()));
                }
                infallible(lock(seam).exchange(DOOR_GH, &request, &mut |_| reply.clone()));
                Ok(answer.map(canon::Reply::into_response))
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                let reply =
                    self.replayed(lock(seam).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let answer = canon::decode(&reply).map_err(|e| self.latch(e))?;
                Ok(answer.map(|r| kept_response(endpoints, r)))
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                let reply =
                    self.replayed(lock(from).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let answer = canon::decode(&reply).map_err(|e| self.latch(e))?;
                if tape::redacts_a_reply(&reply) {
                    return Err(self.latch(shaped_reply()));
                }
                to.append(&EffectRecord::Exchange {
                    door: DOOR_GH.to_string(),
                    request,
                    reply,
                });
                Ok(answer.map(|r| kept_response(endpoints, r)))
            }
        }
    }

    /// `gh`, for the integration tests (plan §1, rule 9): its body is that one call.
    ///
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

    /// A streamed round trip (`Github::call_stream`: downloads).
    ///
    /// # Errors
    /// Recording, replaying and rerecording: `Replay::StreamNotTaped`, before anything is sent.
    pub(crate) fn gh_stream(
        &self,
        transport: &dyn Transport,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.latched()?;
                return Err(self.latch(stream_not_taped()));
            }
        }
        Ok(transport.round_trip(req).map(|response| WireResponse {
            headers: canon::cut(response.headers),
            ..response
        }))
    }

    /// `gh_stream`, for the integration tests (plan §1, rule 9): its body is that one call.
    ///
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
    ///
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

    /// The run's end. - Recording and rerecording: the exit is taped as an emit at `DOOR_EXIT` and the file flushed.
    ///
    /// # Errors
    /// As above.
    pub fn finish(&self, exit: u8) -> Result<()> {
        let code = exit.to_string();
        match &self.seam {
            Seam::Live(_) => Ok(()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(()),
            Seam::Recording { seam, sink } => {
                if sink.discarded() {
                    // A run that met an answer no tape can hold removed its tape: the
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
}

impl std::fmt::Debug for Fx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fx({:?})", self.mode())
    }
}

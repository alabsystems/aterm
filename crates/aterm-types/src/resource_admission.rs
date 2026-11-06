// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Private local resource-admission v3 wire. Byte strings preserve Unix arguments.
use std::io;

/// Exact protocol version; older encodings are refused.
pub const VERSION: u8 = 3;
/// Maximum frame size, excluding the four-byte big-endian length prefix.
pub const MAX_PAYLOAD: usize = 1024 * 1024;
/// Maximum byte string size.
pub const MAX_FIELD: usize = 65536;
/// Maximum argument or environment entry count.
pub const MAX_ITEMS: usize = 4096;
/// Server permission to send exactly four rights, after authentication.
pub const ADMISSION: &str = "OK resource-admission-v3 fds=4";
/// Callback accepted attachment ownership; does not mean launched.
pub const ATTACHED: &str = "OK resource-admission-v3 attached";
/// Single payload byte accompanying the four rights.
pub const FD_MARKER: u8 = b'r';
/// Control request, followed by one length-prefixed submission frame.
pub const REQUEST: &str = "resource-admission-v3 attach\n";
/// Owner endpoint is ready to receive the submission frame. Prevents control
/// line readers from prefetching binary payload before the raw socket handoff.
pub const PREPARE: &str = "OK resource-admission-v3 request";

/// Exact session and unique submission-attempt binding, never a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Exact canonical session identity (`s-` plus 1..62 lowercase hex digits).
    pub session: String,
    /// Unique attempt identity, checked for replay by the host.
    pub attempt: [u8; 16],
}
/// Contains private environment values. Intentionally does not implement Debug.
#[derive(PartialEq, Eq)]
pub struct Submission {
    /// Exact session and attempt.
    pub binding: Binding,
    /// Kernel PID of the submitting peer.
    pub peer_pid: u32,
    /// Native `aterm_uds::process::Birth` scalar of that peer.
    pub peer_birth: u64,
    /// Lossless Unix arguments including the executable at index zero.
    pub argv: Vec<Vec<u8>>,
    /// Absolute lossless Unix working directory.
    pub cwd: Vec<u8>,
    /// Lossless environment key/value pairs, private to this transport.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    /// Positive requested CPU units; allocation policy belongs to the host.
    pub cpu_units: u32,
    /// Positive requested memory bytes.
    pub memory_bytes: u64,
    /// Bounded job label.
    pub label: String,
    /// Host-owned execution budget; starts only at actual launch.
    pub timeout_ms: Option<u64>,
}
/// Child termination, reported only by the launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Exit code.
    Exit(i32),
    /// Positive terminating signal number.
    Signal(i32),
}
/// Why execution ended, independent from the actual exit code or signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionCause {
    /// Natural child termination, including an externally delivered signal.
    Exited,
    /// The host's execution budget expired after actual launch.
    Timeout,
    /// Cancellation of this exact session and attempt.
    Cancelled,
    /// The host ended execution after resource coverage exceeded its claim or
    /// could no longer be measured safely. This is not a solver verdict.
    ResourceLimit,
}
/// Lifecycle facts. Queue time is never execution time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Waiting for allocation; no solver timeout has begun.
    Queued,
    /// Actual launch with timestamps sampled by the launcher.
    Started {
        /// Actual child PID.
        pid: u32,
        /// Native process birth scalar.
        birth: u64,
        /// Host monotonic launch timestamp in nanoseconds.
        monotonic_ns: u64,
        /// Unix launch timestamp in nanoseconds.
        unix_ns: u64,
    },
    /// Actual termination with execution duration excluding queue time.
    Finished {
        /// Exit code or terminating signal.
        outcome: Outcome,
        /// Natural termination, host timeout, or exact-attempt cancellation.
        cause: CompletionCause,
        /// Runtime measured from actual launch.
        runtime_ns: u64,
    },
    /// Host refused this attempt.
    Refused {
        /// Public diagnostic; must never contain request environment values.
        reason: String,
    },
}
/// Versioned lifecycle event for exactly one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFrame {
    /// Exact session and attempt.
    pub binding: Binding,
    /// Launcher or queue lifecycle fact.
    pub event: Event,
}
/// Cancellation of exactly one attempt; never carries a raw PID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancel {
    /// Exact session and attempt to cancel.
    pub binding: Binding,
}
fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid resource admission frame",
    )
}
struct Encoder(Vec<u8>);
impl Encoder {
    fn new(kind: u8, b: &Binding) -> io::Result<Self> {
        validate_binding(b)?;
        let mut v = vec![VERSION, kind];
        v.extend((b.session.len() as u64).to_be_bytes());
        v.extend(b.session.as_bytes());
        v.extend(b.attempt);
        Ok(Self(v))
    }
    fn number(&mut self, n: u64) {
        self.0.extend(n.to_be_bytes());
    }
    fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        if b.len() > MAX_FIELD {
            return Err(bad());
        }
        self.number(b.len() as u64);
        self.0.extend(b);
        if self.0.len() > MAX_PAYLOAD {
            return Err(bad());
        }
        Ok(())
    }
    fn finish(self) -> io::Result<Vec<u8>> {
        if self.0.len() > MAX_PAYLOAD {
            Err(bad())
        } else {
            Ok(self.0)
        }
    }
}
struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let v = self.0.get(..n).ok_or_else(bad)?;
        self.0 = &self.0[n..];
        Ok(v)
    }
    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| bad())?,
        ))
    }
    fn u32(&mut self) -> io::Result<u32> {
        self.number()?.try_into().map_err(|_| bad())
    }
    fn count(&mut self) -> io::Result<usize> {
        let n = self.number()?;
        if n > MAX_ITEMS as u64 {
            return Err(bad());
        }
        Ok(n as usize)
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.number()?;
        if n > MAX_FIELD as u64 {
            return Err(bad());
        }
        Ok(self.take(n as usize)?.to_vec())
    }
    fn text(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| bad())
    }
    fn done(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad())
        }
    }
}
fn header(bytes: &[u8], kind: u8) -> io::Result<(Binding, Decoder<'_>)> {
    if bytes.len() > MAX_PAYLOAD {
        return Err(bad());
    }
    let mut d = Decoder(bytes);
    if d.take(2)? != [VERSION, kind] {
        return Err(bad());
    }
    let session = d.text()?;
    let attempt = d.take(16)?.try_into().map_err(|_| bad())?;
    let binding = Binding { session, attempt };
    validate_binding(&binding)?;
    Ok((binding, d))
}
fn validate_binding(b: &Binding) -> io::Result<()> {
    let suffix = b.session.strip_prefix("s-").ok_or_else(bad)?;
    if suffix.is_empty()
        || suffix.len() > 62
        || !suffix
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(bad());
    }
    Ok(())
}
impl Submission {
    /// Check field counts, Unix byte-string constraints, and positive resources.
    /// Errors contain no submitted field values.
    pub fn validate(&self) -> io::Result<()> {
        validate_binding(&self.binding)?;
        if self.peer_pid == 0
            || self.peer_birth == 0
            || self.cpu_units == 0
            || self.memory_bytes == 0
            || self.timeout_ms == Some(0)
            || self.argv.is_empty()
            || self.argv.len() > MAX_ITEMS
            || self.env.len() > MAX_ITEMS
            || self.cwd.first() != Some(&b'/')
            || self.cwd.contains(&0)
            || self.label.is_empty()
            || self.label.len() > MAX_FIELD
        {
            return Err(bad());
        }
        if self.argv[0].is_empty()
            || self
                .argv
                .iter()
                .any(|v| v.contains(&0) || v.len() > MAX_FIELD)
            || self.env.iter().any(|(k, v)| {
                k.is_empty()
                    || k.contains(&0)
                    || k.contains(&b'=')
                    || v.contains(&0)
                    || k.len() > MAX_FIELD
                    || v.len() > MAX_FIELD
            })
        {
            return Err(bad());
        }
        Ok(())
    }
    /// Encode a bounded v3 payload, without its transport length prefix.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut e = Encoder::new(1, &self.binding)?;
        e.number(self.peer_pid.into());
        e.number(self.peer_birth);
        e.number(self.cpu_units.into());
        e.number(self.memory_bytes);
        e.number(self.timeout_ms.unwrap_or(0));
        e.bytes(self.label.as_bytes())?;
        e.bytes(&self.cwd)?;
        e.number(self.argv.len() as u64);
        for a in &self.argv {
            e.bytes(a)?
        }
        e.number(self.env.len() as u64);
        for (k, v) in &self.env {
            e.bytes(k)?;
            e.bytes(v)?
        }
        e.finish()
    }
    /// Decode exactly one bounded payload, rejecting truncation and trailing bytes.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let (binding, mut d) = header(bytes, 1)?;
        let peer_pid = d.u32()?;
        let peer_birth = d.number()?;
        let cpu_units = d.u32()?;
        let memory_bytes = d.number()?;
        let timeout = d.number()?;
        let label = d.text()?;
        let cwd = d.bytes()?;
        let mut argv = Vec::new();
        for _ in 0..d.count()? {
            argv.push(d.bytes()?)
        }
        let mut env = Vec::new();
        for _ in 0..d.count()? {
            env.push((d.bytes()?, d.bytes()?))
        }
        d.done()?;
        let s = Self {
            binding,
            peer_pid,
            peer_birth,
            argv,
            cwd,
            env,
            cpu_units,
            memory_bytes,
            label,
            timeout_ms: (timeout != 0).then_some(timeout),
        };
        s.validate()?;
        Ok(s)
    }
}
impl Cancel {
    /// Encode an exact session/attempt cancellation with no PID.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        Ok(Encoder::new(3, &self.binding)?.0)
    }
    /// Decode only a cancellation matching the host's expected binding.
    pub fn decode(bytes: &[u8], expected: &Binding) -> io::Result<Self> {
        let (binding, d) = header(bytes, 3)?;
        d.done()?;
        if &binding != expected {
            return Err(bad());
        }
        Ok(Self { binding })
    }
}
impl EventFrame {
    /// Encode a bounded v3 lifecycle event.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut e = Encoder::new(2, &self.binding)?;
        match &self.event {
            Event::Queued => e.number(0),
            Event::Started {
                pid,
                birth,
                monotonic_ns,
                unix_ns,
            } => {
                if *pid == 0 || *birth == 0 {
                    return Err(bad());
                }
                e.number(1);
                e.number((*pid).into());
                e.number(*birth);
                e.number(*monotonic_ns);
                e.number(*unix_ns)
            }
            Event::Finished {
                outcome,
                cause,
                runtime_ns,
            } => {
                e.number(2);
                match outcome {
                    Outcome::Exit(n) => {
                        e.number(0);
                        e.number(*n as u32 as u64)
                    }
                    Outcome::Signal(n) => {
                        if *n <= 0 {
                            return Err(bad());
                        }
                        e.number(1);
                        e.number(*n as u64)
                    }
                }
                e.number(match cause {
                    CompletionCause::Exited => 0,
                    CompletionCause::Timeout => 1,
                    CompletionCause::Cancelled => 2,
                    CompletionCause::ResourceLimit => 3,
                });
                e.number(*runtime_ns)
            }
            Event::Refused { reason } => {
                e.number(3);
                e.bytes(reason.as_bytes())?
            }
        }
        e.finish()
    }
    /// Decode exactly one event for the expected session and attempt.
    pub fn decode(bytes: &[u8], expected: &Binding) -> io::Result<Self> {
        let (binding, mut d) = header(bytes, 2)?;
        if &binding != expected {
            return Err(bad());
        }
        let event = match d.number()? {
            0 => Event::Queued,
            1 => {
                let pid = d.u32()?;
                let birth = d.number()?;
                if pid == 0 || birth == 0 {
                    return Err(bad());
                }
                Event::Started {
                    pid,
                    birth,
                    monotonic_ns: d.number()?,
                    unix_ns: d.number()?,
                }
            }
            2 => {
                let tag = d.number()?;
                let n = d.u32()? as i32;
                let outcome = match tag {
                    0 => Outcome::Exit(n),
                    1 if n > 0 => Outcome::Signal(n),
                    _ => return Err(bad()),
                };
                Event::Finished {
                    outcome,
                    cause: match d.number()? {
                        0 => CompletionCause::Exited,
                        1 => CompletionCause::Timeout,
                        2 => CompletionCause::Cancelled,
                        3 => CompletionCause::ResourceLimit,
                        _ => return Err(bad()),
                    },
                    runtime_ns: d.number()?,
                }
            }
            3 => Event::Refused { reason: d.text()? },
            _ => return Err(bad()),
        };
        d.done()?;
        Ok(Self { binding, event })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Submission {
        Submission {
            binding: Binding {
                session: "s-a912afd80a5a598f9ace".into(),
                attempt: [2; 16],
            },
            peer_pid: 1,
            peer_birth: 2,
            argv: vec![b"/bin/x".to_vec(), vec![255, 10]],
            cwd: b"/tmp".to_vec(),
            env: vec![(b"PRIVATE".to_vec(), vec![254, 10])],
            cpu_units: 1,
            memory_bytes: 1,
            label: "job".into(),
            timeout_ms: Some(12),
        }
    }
    #[test]
    fn lossless_and_truncated() {
        let s = sample();
        let encoded = s.encode().unwrap();
        assert!(Submission::decode(&encoded).unwrap() == s);
        for n in 0..encoded.len() {
            assert!(Submission::decode(&encoded[..n]).is_err())
        }
        let mut extra = encoded;
        extra.push(0);
        assert!(Submission::decode(&extra).is_err());
    }
    #[test]
    fn cancel_is_bound_to_attempt_and_session() {
        let b = sample().binding;
        let bytes = Cancel { binding: b.clone() }.encode().unwrap();
        assert!(Cancel::decode(&bytes, &b).is_ok());
        for other in [
            Binding {
                session: "s-3".into(),
                ..b.clone()
            },
            Binding {
                attempt: [3; 16],
                ..b.clone()
            },
        ] {
            assert!(Cancel::decode(&bytes, &other).is_err())
        }
        for session in ["s-", "s-G", "s-A", "a-12", "s-12\n"] {
            let malformed = Cancel {
                binding: Binding {
                    session: session.into(),
                    ..b.clone()
                },
            };
            assert!(malformed.encode().is_err());
        }
        // Exact spelling remains distinct, even when two IDs parse to the
        // same integer. Session identity is never padded or normalized.
        let short = Binding {
            session: "s-1".into(),
            ..b.clone()
        };
        let padded = Binding {
            session: "s-01".into(),
            ..b
        };
        let bytes = Cancel { binding: short }.encode().unwrap();
        assert!(Cancel::decode(&bytes, &padded).is_err());
    }
    #[test]
    fn started_and_finished_are_separate_from_queue() {
        let b = sample().binding;
        for event in [
            Event::Queued,
            Event::Started {
                pid: 7,
                birth: 8,
                monotonic_ns: 99,
                unix_ns: 100,
            },
            Event::Finished {
                outcome: Outcome::Signal(9),
                cause: CompletionCause::Cancelled,
                runtime_ns: 1,
            },
            Event::Finished {
                outcome: Outcome::Signal(9),
                cause: CompletionCause::Timeout,
                runtime_ns: 1,
            },
            Event::Finished {
                outcome: Outcome::Signal(9),
                cause: CompletionCause::Exited,
                runtime_ns: 1,
            },
            Event::Finished {
                outcome: Outcome::Signal(9),
                cause: CompletionCause::ResourceLimit,
                runtime_ns: 1,
            },
        ] {
            let f = EventFrame {
                binding: b.clone(),
                event,
            };
            assert_eq!(EventFrame::decode(&f.encode().unwrap(), &b).unwrap(), f)
        }
        let finished = EventFrame {
            binding: b.clone(),
            event: Event::Finished {
                outcome: Outcome::Exit(0),
                cause: CompletionCause::Timeout,
                runtime_ns: 4,
            },
        };
        let mut bytes = finished.encode().unwrap();
        for n in 0..bytes.len() {
            assert!(EventFrame::decode(&bytes[..n], &b).is_err());
        }
        let cause_offset = bytes.len() - 16;
        bytes[cause_offset..cause_offset + 8].copy_from_slice(&99u64.to_be_bytes());
        assert!(EventFrame::decode(&bytes, &b).is_err());
    }
}

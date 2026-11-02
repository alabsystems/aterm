// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private restart observations. A registration retains a live process's exit witness;
//! it grants no native operation, terminal input, deadline, or permission to restart.

use std::collections::BTreeMap;

use super::native_act::{Image, MAX_PATH, Process};

/// The independent passive-observation wire contract.
pub const CONTRACT: u32 = 1;
/// Maximum bytes in a complete observation request or reply.
pub const MAX_REQUEST: usize = 24 * 1024;

/// A caller-generated registration, persisted before its first exchange. Retries preserve
/// every field. A guard change needs a new registration and fresh live process proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// Exactly 32 lowercase hexadecimal digits; independent of every native operation id.
    pub id: String,
    /// The actual host process epoch obtained from its read-only native information.
    pub host_epoch: String,
    /// The exact live vendor whose death the host will watch.
    pub vendor: Process,
    /// Its actual foreground shell, retained for later fresh comparison.
    pub shell: Process,
}

/// An observation capability, deliberately a different type from a native mutation binding.
/// Possession alone proves neither death nor current authority.
#[derive(Clone, PartialEq, Eq)]
pub struct ObservationBinding {
    /// The immutable registration id.
    pub id: String,
    /// The host process that owns the kernel witness.
    pub host_epoch: String,
    /// Exactly 64 unpredictable lowercase hexadecimal digits.
    pub capability: String,
}

/// Data-only handoff of an already retained observation. The host still checks its exact
/// witness and current authority; these words grant no mutation or deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedObservation {
    /// The original captured vendor/shell and registration id.
    pub registration: Registration,
    /// The same host-issued observation binding.
    pub binding: ObservationBinding,
}

impl SavedObservation {
    /// Canonical bounded persistence shared by the private native receipt and local state.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        if self.registration.id != self.binding.id
            || self.registration.host_epoch != self.binding.host_epoch
        {
            return None;
        }
        Some(format!(
            "{}\n{}",
            Request::Register(self.registration.clone()).encode()?,
            Request::Refresh(self.binding.clone()).encode()?
        ))
    }

    /// Decode only one exact registration/binding pair, without acquiring a witness or grant.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_REQUEST * 2 {
            return None;
        }
        let mut lines = text.lines();
        let Request::Register(registration) = Request::parse(lines.next()?)? else {
            return None;
        };
        let Request::Refresh(binding) = Request::parse(lines.next()?)? else {
            return None;
        };
        if lines.next().is_some() {
            return None;
        }
        let saved = Self {
            registration,
            binding,
        };
        (saved.encode().as_deref() == Some(text)).then_some(saved)
    }
}

impl std::fmt::Debug for ObservationBinding {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("ObservationBinding")
            .field("id", &self.id)
            .field("host_epoch", &self.host_epoch)
            .field("capability", &"redacted")
            .finish()
    }
}

/// Read-only registration and live-only refresh. Neither request admits a mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Capture a new exact live vendor and an owned kernel exit witness.
    Register(Registration),
    /// Extend observation retention only while all original facts remain live and current.
    Refresh(ObservationBinding),
}

/// Registration acknowledgement or uncertainty; there is no action-admitted reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The host retained this same registration. It may still be evicted or expire later.
    Registered(ObservationBinding),
    /// Missing, stale, unreadable, expired, or otherwise unproved observation.
    Unknown,
}

fn fields(text: &str) -> Option<(&str, BTreeMap<&str, &str>)> {
    if text.len() > MAX_REQUEST || text.contains(['\n', '\r', '\0']) {
        return None;
    }
    let mut words = text.split_ascii_whitespace();
    let verb = words.next()?;
    let mut fields = BTreeMap::new();
    for word in words {
        let (key, value) = word.split_once('=')?;
        if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
            return None;
        }
        if fields.len() > 19 {
            return None;
        }
    }
    Some((verb, fields))
}

fn take<'a>(words: &mut BTreeMap<&str, &'a str>, key: &str) -> Option<&'a str> {
    words.remove(key)
}

fn number<T: std::str::FromStr + ToString>(text: &str) -> Option<T> {
    let value: T = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

fn id(text: &str, size: usize) -> Option<String> {
    aterm_codec::hex::is_lower(text, size).then(|| text.to_string())
}

fn process(words: &mut BTreeMap<&str, &str>, prefix: &str) -> Option<Process> {
    let pid = number::<u32>(take(words, &format!("{prefix}-pid"))?)?;
    let birth = number::<u64>(take(words, &format!("{prefix}-birth"))?)?;
    let parent = number::<u32>(take(words, &format!("{prefix}-parent"))?)?;
    let group = number::<u32>(take(words, &format!("{prefix}-group"))?)?;
    let session = number::<u32>(take(words, &format!("{prefix}-session"))?)?;
    let hex = take(words, &format!("{prefix}-path"))?;
    if pid <= 1
        || pid > i32::MAX as u32
        || birth == 0
        || parent == 0
        || parent > i32::MAX as u32
        || group <= 1
        || group > i32::MAX as u32
        || session <= 1
        || session > i32::MAX as u32
        || hex.len() > MAX_PATH * 2
        || !aterm_codec::hex::is_lower(hex, hex.len())
    {
        return None;
    }
    let path = aterm_codec::hex::decode_vec(hex.as_bytes())?;
    if path.first() != Some(&b'/') || path.contains(&0) {
        return None;
    }
    let device = number(take(words, &format!("{prefix}-dev"))?)?;
    let inode = number(take(words, &format!("{prefix}-ino"))?)?;
    Some(Process {
        pid,
        birth,
        parent,
        group,
        session,
        image: Image {
            path,
            device,
            inode,
        },
    })
}

fn binding(words: &mut BTreeMap<&str, &str>) -> Option<ObservationBinding> {
    Some(ObservationBinding {
        id: id(take(words, "id")?, 32)?,
        host_epoch: id(take(words, "epoch")?, 32)?,
        capability: id(take(words, "cap")?, 64)?,
    })
}

fn write_process(out: &mut String, prefix: &str, process: &Process) {
    use std::fmt::Write;
    let _ = write!(
        out,
        " {prefix}-pid={} {prefix}-birth={} {prefix}-parent={} {prefix}-group={} {prefix}-session={} {prefix}-path={} {prefix}-dev={} {prefix}-ino={}",
        process.pid,
        process.birth,
        process.parent,
        process.group,
        process.session,
        aterm_codec::hex::encode_lower(&process.image.path),
        process.image.device,
        process.image.inode
    );
}

fn valid_process(process: &Process) -> bool {
    process.pid > 1
        && process.pid <= i32::MAX as u32
        && process.birth != 0
        && process.parent != 0
        && process.parent <= i32::MAX as u32
        && process.group > 1
        && process.group <= i32::MAX as u32
        && process.session > 1
        && process.session <= i32::MAX as u32
        && process.image.path.len() <= MAX_PATH
        && process.image.path.first() == Some(&b'/')
        && !process.image.path.contains(&0)
}

impl Registration {
    /// Validate all identifiers and process/path bounds before cloning or allocating an
    /// encoded request. Kernel liveness and relationships remain independent host checks.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        aterm_codec::hex::is_lower(&self.id, 32)
            && aterm_codec::hex::is_lower(&self.host_epoch, 32)
            && valid_process(&self.vendor)
            && valid_process(&self.shell)
    }
}

impl Request {
    /// Parse a bounded complete request, rejecting duplicate/unknown fields, noncanonical
    /// numbers, invalid identifiers, and invalid Unix byte paths.
    #[must_use]
    pub fn parse(tail: &str) -> Option<Self> {
        let (verb, mut words) = fields(tail)?;
        if take(&mut words, "v")? != "1" {
            return None;
        }
        let request = match verb {
            "register" => Self::Register(Registration {
                id: id(take(&mut words, "id")?, 32)?,
                host_epoch: id(take(&mut words, "epoch")?, 32)?,
                vendor: process(&mut words, "vendor")?,
                shell: process(&mut words, "shell")?,
            }),
            "refresh" => Self::Refresh(binding(&mut words)?),
            _ => return None,
        };
        words.is_empty().then_some(request)
    }

    /// Encode and validate every constructed field before any request can be sent.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        match self {
            Self::Register(registration) if !registration.is_valid() => {
                return None;
            }
            Self::Refresh(binding)
                if !aterm_codec::hex::is_lower(&binding.id, 32)
                    || !aterm_codec::hex::is_lower(&binding.host_epoch, 32)
                    || !aterm_codec::hex::is_lower(&binding.capability, 64) =>
            {
                return None;
            }
            _ => {}
        }
        let mut out = match self {
            Self::Register(registration) => format!(
                "register v={CONTRACT} id={} epoch={}",
                registration.id, registration.host_epoch
            ),
            Self::Refresh(binding) => format!(
                "refresh v={CONTRACT} id={} epoch={} cap={}",
                binding.id, binding.host_epoch, binding.capability
            ),
        };
        if let Self::Register(registration) = self {
            write_process(&mut out, "vendor", &registration.vendor);
            write_process(&mut out, "shell", &registration.shell);
        }
        (Self::parse(&out).as_ref() == Some(self)).then_some(out)
    }
}

impl Reply {
    /// Parse one complete bounded reply; an unrecognized acknowledgement is uncertainty.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > MAX_REQUEST {
            return None;
        }
        let tail = line.strip_suffix('\n').unwrap_or(line);
        let tail = tail.strip_suffix('\r').unwrap_or(tail);
        let tail = tail.strip_prefix("OK native-observe ")?;
        let (state, mut words) = fields(tail)?;
        if take(&mut words, "v")? != "1" {
            return None;
        }
        let reply = match state {
            "state=registered" => Self::Registered(binding(&mut words)?),
            "state=unknown" => Self::Unknown,
            _ => return None,
        };
        words.is_empty().then_some(reply)
    }

    /// Encode a canonical acknowledgement. This never describes native admission.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        if let Self::Registered(binding) = self
            && (!aterm_codec::hex::is_lower(&binding.id, 32)
                || !aterm_codec::hex::is_lower(&binding.host_epoch, 32)
                || !aterm_codec::hex::is_lower(&binding.capability, 64))
        {
            return None;
        }
        let out = match self {
            Self::Registered(binding) => format!(
                "OK native-observe state=registered v={CONTRACT} id={} epoch={} cap={}\n",
                binding.id, binding.host_epoch, binding.capability
            ),
            Self::Unknown => format!("OK native-observe state=unknown v={CONTRACT}\n"),
        };
        (Self::parse(&out).as_ref() == Some(self)).then_some(out)
    }
}

fn acknowledged(request: &Request, line: &str) -> Option<Reply> {
    let reply = Reply::parse(line)?;
    if let Reply::Registered(binding) = &reply {
        let matches = match request {
            Request::Register(registration) => {
                binding.id == registration.id && binding.host_epoch == registration.host_epoch
            }
            Request::Refresh(expected) => binding == expected,
        };
        if !matches {
            return None;
        }
    }
    Some(reply)
}

/// Register or refresh over an authenticated private host control stream. The actual host
/// independently validates principal, kernel actor and live facts; this exchange grants none.
/// # Errors
/// Invalid request/session, transport failure, or a refused or malformed acknowledgement.
pub fn exchange<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    request: &Request,
) -> std::io::Result<Reply> {
    if sid.is_empty()
        || sid.len() > 128
        || !sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid observation session",
        ));
    }
    let tail = request.encode().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid observation")
    })?;
    let line = client.request_line(&format!("@{sid} native-observe {tail}"))?;
    acknowledged(request, &line).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "unknown observation reply")
    })
}

#[cfg(test)]
#[path = "native_observe_tests.rs"]
mod tests;

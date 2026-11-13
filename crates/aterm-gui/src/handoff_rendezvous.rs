// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Single-use `AF_UNIX` rendezvous socket for transferring seamless-handoff file descriptors on macOS.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, BorrowedFd, IntoRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::{CtlListener, CtlStream, fdpass};

use crate::handoff_env::HandoffName;

pub(crate) const ENV_RENDEZVOUS: HandoffName = crate::seamless::ENV_RENDEZVOUS;
pub(crate) const ENV_CLAIM: HandoffName = crate::seamless::ENV_CLAIM;

const CLAIM_SECRET_BYTES: usize = 32;
const CLAIM_HEX_LEN: usize = CLAIM_SECRET_BYTES * 2;
const CLAIM_MAGIC: &[u8; 6] = b"ATRZ1C";
const CLAIM_FRAME_LEN: usize = CLAIM_MAGIC.len() + CLAIM_HEX_LEN;
const GRANT_MAGIC: &[u8; 6] = b"ATRZ1G";
const CLAIM_CHUNKS_MAGIC: &[u8; 6] = b"ATRZ2C";
const GRANT_CHUNKS_MAGIC: &[u8; 6] = b"ATRZ2G";
const GRANT_CHUNKS_HEADER_LEN: usize = GRANT_CHUNKS_MAGIC.len() + 2 + 1;
const GRANT_CONTINUATION_MAGIC: &[u8; 6] = b"ATRZ2K";
const GRANT_CONTINUATION_LEN: usize = GRANT_CONTINUATION_MAGIC.len() + 1;

pub(crate) const ENV_GRANT_CAPS: HandoffName = HandoffName::new("ATERM_HANDOFF_GRANT_CAPS");
pub(crate) const GRANT_CAPS_CHUNKS: &str = "chunks1";
pub(crate) const GRANT_CAPS_EMPTY: &str = "empty1";

/// The `ENV_GRANT_CAPS` value that offers a successor the chunked and the sessionless grant
/// its bundle declares, or `None` when it declares neither.
#[must_use]
pub(crate) fn grant_caps(chunks: bool, empty: bool) -> Option<String> {
    let caps = [(chunks, GRANT_CAPS_CHUNKS), (empty, GRANT_CAPS_EMPTY)]
        .into_iter()
        .filter_map(|(offered, cap)| offered.then_some(cap))
        .collect::<Vec<_>>();
    (!caps.is_empty()).then(|| caps.join(","))
}

// ATRZ1/ATRZ1C keep their existing packet shape across app versions. A larger
// generic rights buffer for resource delegation cannot enlarge this wire lane.
const GRANT_MESSAGE_FDS: usize = 64;
const _: () = assert!(GRANT_MESSAGE_FDS <= fdpass::MAX_FDS);
pub(crate) const MAX_CHUNKED_SESSIONS: usize = crate::seamless::MAX_HANDOFF_SESSIONS;
const MAX_GRANT_FDS: usize = MAX_CHUNKED_SESSIONS + RENDEZVOUS_CHANNEL_FDS;
const MAX_GRANT_CHUNKS: usize = MAX_GRANT_FDS.div_ceil(GRANT_MESSAGE_FDS);
const GRANT_HEADER_LEN: usize = GRANT_MAGIC.len() + 2;
const MAX_GRANT_BODY_BYTES: usize = 32 * 1024;

pub(crate) const RENDEZVOUS_CHANNEL_FDS: usize = 2;

pub(crate) const MAX_RENDEZVOUS_SESSIONS: usize = GRANT_MESSAGE_FDS - RENDEZVOUS_CHANNEL_FDS;

const _: () = assert!(
    MAX_RENDEZVOUS_SESSIONS < 256,
    "the existing handoff session count must fit its one-byte field"
);

/// Returns the maximum number of sessions a grant may carry (`MAX_CHUNKED_SESSIONS` or `MAX_GRANT_SESSIONS`).
#[must_use]
pub(crate) const fn grant_session_limit(chunks: bool) -> usize {
    if chunks {
        MAX_CHUNKED_SESSIONS
    } else {
        MAX_RENDEZVOUS_SESSIONS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantShape {
    OneMessage,
    Chunked,
}

const fn grant_shape(
    chunks: bool,
    empty_ok: bool,
    sessions: usize,
) -> Result<GrantShape, RendezvousError> {
    let limit = grant_session_limit(chunks);
    if (sessions == 0 && !empty_ok) || sessions > limit {
        return Err(RendezvousError::TooManySessions { sessions, limit });
    }
    Ok(if sessions > MAX_RENDEZVOUS_SESSIONS {
        GrantShape::Chunked
    } else {
        GrantShape::OneMessage
    })
}

#[must_use]
fn grant_chunk_sizes(sessions: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut left = sessions;
    while left > GRANT_MESSAGE_FDS {
        sizes.push(GRANT_MESSAGE_FDS);
        left -= GRANT_MESSAGE_FDS;
    }
    if left + RENDEZVOUS_CHANNEL_FDS <= GRANT_MESSAGE_FDS {
        sizes.push(left + RENDEZVOUS_CHANNEL_FDS);
    } else {
        sizes.push(left);
        sizes.push(RENDEZVOUS_CHANNEL_FDS);
    }
    sizes
}

const CODE_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

const ACCEPT_POLL_SLICE: Duration = Duration::from_millis(10);

/// How long an accepted dialer has to present its claim frame, from its accept. A unit test's
/// dialers are its own threads and children, which write theirs, so they wait the hang detector.
#[cfg(not(test))]
const CLAIM_FRAME_BUDGET: Duration = Duration::from_secs(2);
#[cfg(test)]
const CLAIM_FRAME_BUDGET: Duration = aterm_spec::wait::MUST_HAPPEN;

const MAX_REFUSED_DIALS: usize = 32;

const MIN_IO_TIMEOUT: Duration = Duration::from_millis(1);

/// Reason a seamless-handoff rendezvous failed and fell back to keeping the parent's windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendezvousError {
    NoControlDir,
    PathTooLong {
        path: String,
        limit: usize,
    },
    TooManySessions {
        sessions: usize,
        limit: usize,
    },
    Secret(String),
    Bind(String),
    Cancelled,
    Deadline,
    Accept(String),
    Claim,
    PeerPid {
        expected: i32,
        actual: i32,
    },
    PeerUnattested(String),
    PeerCode {
        pid: i32,
        refusal: CodeRefusal,
    },
    TransferNotSent(String),
    GrantDeadline,
    TransferPartial(String),

    ClaimDeadline,
    NoRendezvous,
    HalfRendezvous {
        present: HandoffName,
        missing: HandoffName,
    },
    Env(HandoffName),
    Dial(String),
    Grant(String),
    NonceMismatch,
    DescriptorCount {
        expected: usize,
        received: usize,
    },
}

impl std::fmt::Display for RendezvousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoControlDir => f.write_str("there is no private control directory to bind in"),
            Self::PathTooLong { path, limit } => write!(
                f,
                "the rendezvous path {path} is longer than this platform's {limit}-byte sun_path"
            ),
            Self::TooManySessions { sessions, limit } => write!(
                f,
                "{sessions} sessions exceed the {limit} this successor's descriptor grant can carry"
            ),
            Self::Secret(error) => write!(f, "no claim secret could be minted: {error}"),
            Self::Bind(error) => write!(f, "the rendezvous could not be bound: {error}"),
            Self::Cancelled => f.write_str("the rendezvous wait was cancelled"),
            Self::Deadline => {
                f.write_str("no successor dialed the rendezvous before the handoff deadline")
            }
            Self::Accept(error) => write!(f, "the rendezvous connection failed: {error}"),
            Self::Claim => f.write_str("the dialer did not present this attempt's claim secret"),
            Self::PeerPid { expected, actual } => write!(
                f,
                "the dialer is pid {actual}, not the launched successor {expected}"
            ),
            Self::PeerUnattested(error) => {
                write!(f, "the kernel would not attest the dialer: {error}")
            }
            Self::PeerCode { pid, refusal } => {
                write!(
                    f,
                    "the dialer (pid {pid}) presented the claim, and {refusal}"
                )
            }
            Self::GrantDeadline => f.write_str(
                "the handoff's proof deadline passed before the grant was sent (the successor \
                 had dialed; this process's preparation after the park outran it), so no \
                 descriptor left",
            ),
            Self::TransferNotSent(error) => {
                write!(
                    f,
                    "the descriptor transfer failed before any descriptor left: {error}"
                )
            }
            Self::TransferPartial(error) => write!(
                f,
                "the descriptors were delivered but the grant body was not: {error}"
            ),
            Self::ClaimDeadline => f.write_str(
                "this successor dialed, but its handoff deadline passed before the parent's \
                 grant arrived",
            ),
            Self::NoRendezvous => f.write_str("this launch carries no rendezvous"),
            Self::HalfRendezvous { present, missing } => write!(
                f,
                "this launch carries only half a rendezvous: {present} is set and {missing} is \
                 not, so there is nothing here that could be claimed"
            ),
            Self::Env(key) => write!(f, "the rendezvous variable {key} is malformed"),
            Self::Dial(error) => write!(f, "the rendezvous could not be dialed: {error}"),
            Self::Grant(detail) => write!(f, "the rendezvous grant is malformed: {detail}"),
            Self::NonceMismatch => f.write_str("the rendezvous grant names a different attempt"),
            Self::DescriptorCount { expected, received } => write!(
                f,
                "the grant describes {expected} descriptors and {received} arrived"
            ),
        }
    }
}

/// Returns `fstat(2)` `st_rdev` for `master`, used as the out-of-band PTY adoption proof term.
#[must_use]
pub(crate) fn pty_device_term(master: i32) -> Option<i32> {
    if master < 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes one `struct stat` through `info.as_mut_ptr()` without consuming `master`.
    let queried = unsafe { libc::fstat(master, info.as_mut_ptr()) };
    if queried != 0 {
        return None;
    }
    // SAFETY: `fstat` returned 0, so `info` is initialized.
    let info = unsafe { info.assume_init() };
    i32::try_from(i128::from(info.st_rdev)).ok()
}

/// Rewrites session adoption proof identities to use PTY `st_rdev` device terms instead of fd numbers.
#[must_use]
pub(crate) fn proof_identities_in_device_terms(
    identities: &[crate::seamless::SessionIdentity],
) -> Option<Vec<crate::seamless::SessionIdentity>> {
    identities
        .iter()
        .map(|(local_id, master, pid)| Some((*local_id, pty_device_term(*master)?, *pid)))
        .collect()
}

fn rendezvous_path(dir: &Path, nonce: &str) -> PathBuf {
    let short = &nonce[..nonce.len().min(RENDEZVOUS_NONCE_HEX)];
    dir.join(format!("seamless-{}-{short}.sock", std::process::id()))
}

const RENDEZVOUS_NONCE_HEX: usize = 16;

#[cfg(all(unix, not(test)))]
fn admitted_temp_base() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let base = std::env::temp_dir();
    let lstat = std::fs::symlink_metadata(&base).ok().map(|meta| BaseFacts {
        is_dir: meta.file_type().is_dir(),
        uid: meta.uid(),
        mode: meta.permissions().mode(),
    });
    // SAFETY: `getuid` has no preconditions.
    let uid = unsafe { libc::getuid() };
    admits_temp_base(&base, lstat, uid).then_some(base)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
struct BaseFacts {
    is_dir: bool,
    uid: u32,
    mode: u32,
}

#[cfg(unix)]
fn admits_temp_base(base: &Path, lstat: Option<BaseFacts>, uid: u32) -> bool {
    // Absolute only: a relative `$TMPDIR` binds against this process's cwd, which a LaunchServices
    // successor (cwd `/`) cannot dial, so every attempt would park for the whole readiness deadline
    // and never fall back to the fork lane.
    if !base.is_absolute() {
        return false;
    }
    if base.starts_with("/tmp") || base.starts_with("/private/tmp") {
        return false;
    }
    lstat.is_some_and(|facts| facts.is_dir && facts.uid == uid && facts.mode & 0o022 == 0)
}

#[must_use]
fn rendezvous_dir_candidate() -> Option<PathBuf> {
    #[cfg(all(unix, test))]
    {
        Some(crate::test_scratch::short_process_root("aterm-rz-"))
    }
    #[cfg(all(unix, not(test)))]
    {
        admitted_temp_base().map(|base| base.join("aterm"))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Returns the per-UID private directory for rendezvous sockets, or `None` if validation fails.
#[must_use]
pub(crate) fn rendezvous_dir() -> Option<PathBuf> {
    if let Some(dir) = rendezvous_dir_candidate()
        && aterm_update_core::ensure_private_dir(&dir).is_ok()
    {
        return Some(dir);
    }
    crate::control_auth::socket_dir().filter(|dir| dir.is_absolute())
}

#[cfg(unix)]
fn prove_bound_socket_is_ours(dir: &Path, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{
        FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    // SAFETY: `getuid` has no preconditions.
    let uid = unsafe { libc::getuid() };
    let dir_handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(dir)
        .map_err(|e| format!("rendezvous directory is not a real directory of ours: {e}"))?;
    let dmeta = dir_handle
        .metadata()
        .map_err(|e| format!("rendezvous directory fstat: {e}"))?;
    if !dmeta.file_type().is_dir() || dmeta.uid() != uid || dmeta.permissions().mode() & 0o022 != 0
    {
        return Err("rendezvous directory is not owned by this uid or is shared".to_string());
    }
    let smeta =
        std::fs::symlink_metadata(path).map_err(|e| format!("rendezvous socket lstat: {e}"))?;
    if !smeta.file_type().is_socket() || smeta.uid() != uid {
        return Err("rendezvous node is not a socket owned by this uid".to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn sweep_dead_rendezvous(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("seamless-") else {
            continue;
        };
        let node = name.strip_suffix(LAUNCH_STAMP_SUFFIX).unwrap_or(&name);
        if !node.ends_with(".sock") {
            continue;
        }
        let Some((pid, _)) = rest.split_once('-') else {
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        // SAFETY: `kill(pid, 0)` with `pid > 1` only probes single-process existence.
        let alive = pid > 1
            && (unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM));
        if !alive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Returns whether a rendezvous socket path for this process fits within `sockaddr_un::sun_path`.
#[must_use]
pub(crate) fn rendezvous_path_fits() -> bool {
    let Some(dir) = rendezvous_dir_candidate()
        .or_else(crate::control_auth::socket_dir)
        .filter(|dir| dir.is_absolute())
    else {
        return false;
    };
    rendezvous_path(&dir, &"0".repeat(32))
        .to_str()
        .is_some_and(crate::control_auth::sun_path_ok)
}

pub(crate) const RELEASE_SIGNING_IDENTIFIER: &str = "com.aterm.aterm";

/// Code-signing requirement a dialer must satisfy to claim the handoff grant.
pub(crate) struct DialerCode {
    rule: DialerCodeRule,
}

enum DialerCodeRule {
    Stated(aterm_keeper::identity::Identity),
    CodeAt(PathBuf),
    #[cfg(all(test, target_vendor = "apple"))]
    Scripted {
        start: Box<dyn Fn() -> aterm_keeper::identity::PeerCheck + Send + Sync>,
        in_line: Duration,
    },
}

/// Reason [`DialerCode`] did not admit a dialer that presented the claim secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodeRefusal {
    Refused(String),
    TrustdSilent(String),
    Unchecked(String),
}

impl CodeRefusal {
    /// Returns whether the refusal is a definitive code-signing rejection (`Self::Refused`).
    #[must_use]
    pub(crate) const fn is_verdict(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

impl std::fmt::Display for CodeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => {
                write!(
                    f,
                    "its code does not meet the successor's requirement ({why})"
                )
            }
            Self::TrustdSilent(why) => write!(f, "its code could not be checked: {why}"),
            Self::Unchecked(why) => write!(f, "its code could not be checked ({why})"),
        }
    }
}

impl DialerCode {
    /// Requires a dialer signed as an aterm release with `RELEASE_SIGNING_IDENTIFIER` and `team`.
    pub(crate) fn release(team: &str) -> Result<Self, String> {
        let source = aterm_update_core::codesign::developer_id_requirement(team)
            .map_err(|error| error.to_string())?;
        let developer_id = source.strip_prefix('=').unwrap_or(&source);
        let text = format!("identifier \"{RELEASE_SIGNING_IDENTIFIER}\" and {developer_id}");
        Ok(Self {
            rule: DialerCodeRule::Stated(aterm_keeper::identity::Identity::for_requirement(&text)?),
        })
    }

    /// Requires a dialer matching the designated code requirement of the bundle at `path`.
    pub(crate) const fn code_at(path: PathBuf) -> Self {
        Self {
            rule: DialerCodeRule::CodeAt(path),
        }
    }

    /// Requires a dialer matching the current test binary's code identity.
    #[cfg(test)]
    pub(crate) fn this_process() -> Self {
        Self {
            rule: DialerCodeRule::Stated(
                aterm_keeper::identity::Identity::for_self(
                    aterm_keeper::identity::IdentityPolicy::Designated,
                )
                .expect("this test binary has a designated requirement"),
            ),
        }
    }

    /// Disables code-signing checks in tests, relying only on the `0600` socket UID gate.
    #[cfg(test)]
    pub(crate) fn same_uid_floor_for_test() -> Self {
        Self {
            rule: DialerCodeRule::Stated(aterm_keeper::identity::Identity::same_uid()),
        }
    }

    #[cfg(all(test, target_vendor = "apple"))]
    fn scripted(
        in_line: Duration,
        start: impl Fn() -> aterm_keeper::identity::PeerCheck + Send + Sync + 'static,
    ) -> Self {
        Self {
            rule: DialerCodeRule::Scripted {
                start: Box::new(start),
                in_line,
            },
        }
    }

    /// Formats a single-line log description of the expected dialer code requirement.
    pub(crate) fn describe(&self) -> String {
        match &self.rule {
            DialerCodeRule::Stated(identity) => identity.describe().to_string(),
            DialerCodeRule::CodeAt(path) => {
                format!(
                    "the designated requirement of the code at {}",
                    path.display()
                )
            }
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { in_line, .. } => format!(
                "a scripted check, waited for in line {} ms",
                in_line.as_millis()
            ),
        }
    }

    const fn in_line(&self) -> Duration {
        match &self.rule {
            DialerCodeRule::Stated(_) | DialerCodeRule::CodeAt(_) => aterm_uds::peer::CHECK_HANG,
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { in_line, .. } => *in_line,
        }
    }

    fn start(
        &self,
        stream: &CtlStream,
        attested: i32,
        token: Option<aterm_uds::peer::AuditToken>,
    ) -> Result<aterm_keeper::identity::PeerCheck, CodeRefusal> {
        if let Some(named) = token
            && u32::try_from(attested).ok() != Some(named.pid())
        {
            return Err(CodeRefusal::Unchecked(format!(
                "the kernel's audit token names pid {}, not the attested dialer",
                named.pid()
            )));
        }
        let read;
        let identity = match &self.rule {
            DialerCodeRule::Stated(identity) => identity,
            DialerCodeRule::CodeAt(path) => {
                read = aterm_keeper::identity::Identity::for_code_at(path).map_err(|why| {
                    CodeRefusal::Unchecked(format!(
                        "the code at {} has no requirement to check it against: {why}",
                        path.display()
                    ))
                })?;
                &read
            }
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { start, .. } => return Ok(start()),
        };
        Ok(identity.start_check_named(stream, token))
    }
}

impl From<aterm_keeper::identity::CheckError> for CodeRefusal {
    fn from(error: aterm_keeper::identity::CheckError) -> Self {
        match error {
            aterm_keeper::identity::CheckError::Refused(why) => Self::Refused(why),
            aterm_keeper::identity::CheckError::Unanswered(why) => Self::TrustdSilent(why),
            aterm_keeper::identity::CheckError::Unchecked(why) => Self::Unchecked(why),
        }
    }
}

/// Diagnostic record for a dialer that presented the claim secret but failed code admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeRefusedDialer {
    pub(crate) pid: i32,
    pub(crate) refusal: CodeRefusal,
    pub(crate) end: CodeRefusedEnd,
}

/// Outcome of a connection reported by [`CodeRefusedDialer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeRefusedEnd {
    Closed,
    LeftWhileHeld,
}

struct Admitted {
    pid: i32,
    chunks: bool,
    token: Option<aterm_uds::peer::AuditToken>,
}

#[derive(Debug)]
enum Gated {
    Refused(RendezvousError),
    WaitEnded {
        end: RendezvousError,
        held: RendezvousError,
    },
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_rendezvous_admission",
        action = "DeadlineWhileHeld",
        project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
    )
)]
fn admit_code(
    stream: &CtlStream,
    pid: i32,
    token: Option<aterm_uds::peer::AuditToken>,
    code: &DialerCode,
    deadline: Instant,
    abort: &dyn Fn() -> bool,
    refused: &dyn Fn(&CodeRefusedDialer),
) -> Result<(), Gated> {
    let mut held_since: Option<Instant> = None;
    loop {
        let check = match code.start(stream, pid, token) {
            Ok(check) => check,
            Err(refusal) => {
                return Err(refuse_dialer(
                    stream,
                    pid,
                    refusal,
                    held_since.is_some(),
                    refused,
                ));
            }
        };
        let in_line = if held_since.is_some() {
            Duration::ZERO
        } else {
            code.in_line()
        };
        let (silent, mut waiting) = match check.wait(in_line) {
            Ok(Ok(())) => {
                admitted_after_hold(pid, held_since);
                return Ok(());
            }
            Ok(Err(error)) => match CodeRefusal::from(error) {
                CodeRefusal::TrustdSilent(why) => (why, None),
                refusal => {
                    return Err(refuse_dialer(
                        stream,
                        pid,
                        refusal,
                        held_since.is_some(),
                        refused,
                    ));
                }
            },
            Err(still) => (still.unanswered(), Some(still)),
        };
        let held = || RendezvousError::PeerCode {
            pid,
            refusal: CodeRefusal::TrustdSilent(silent.clone()),
        };
        if held_since.is_none() {
            held_since = Some(Instant::now());
            #[cfg(test)]
            HOLDS_FOR_TEST.with(|holds| holds.set(holds.get() + 1));
            aterm_log::warn!(
                "overlap handoff: holding the rendezvous dialer (pid {pid}) whose code could not \
                 be checked ({silent}); waiting for the check's answer (asked again every {} ms \
                 while the process's trustd gate refuses it) until it is admitted, refused or \
                 gone, or the dial deadline",
                CODE_RECHECK_INTERVAL.as_millis()
            );
        }
        let recheck_at = Instant::now() + CODE_RECHECK_INTERVAL;
        loop {
            if abort() {
                if dialer_left(stream, Duration::ZERO) {
                    refused(&CodeRefusedDialer {
                        pid,
                        refusal: CodeRefusal::TrustdSilent(silent.clone()),
                        end: CodeRefusedEnd::LeftWhileHeld,
                    });
                }
                return Err(Gated::WaitEnded {
                    end: RendezvousError::Cancelled,
                    held: held(),
                });
            }
            let now = io_now();
            if now >= deadline {
                return Err(Gated::WaitEnded {
                    end: RendezvousError::Deadline,
                    held: held(),
                });
            }
            let slice = deadline
                .saturating_duration_since(now)
                .min(ACCEPT_POLL_SLICE);
            let watch = match waiting.take() {
                Some(check) => match check.wait(slice) {
                    Ok(Ok(())) => {
                        admitted_after_hold(pid, held_since);
                        return Ok(());
                    }
                    Ok(Err(error)) => match CodeRefusal::from(error) {
                        CodeRefusal::TrustdSilent(_) => break,
                        refusal => {
                            return Err(refuse_dialer(stream, pid, refusal, true, refused));
                        }
                    },
                    Err(still) => {
                        waiting = Some(still);
                        Duration::ZERO
                    }
                },
                None if now >= recheck_at => break,
                None => recheck_at.saturating_duration_since(now).min(slice),
            };
            if dialer_left(stream, watch) {
                refused(&CodeRefusedDialer {
                    pid,
                    refusal: CodeRefusal::TrustdSilent(silent.clone()),
                    end: CodeRefusedEnd::LeftWhileHeld,
                });
                return Err(Gated::Refused(held()));
            }
        }
    }
}

fn refuse_dialer(
    stream: &CtlStream,
    pid: i32,
    refusal: CodeRefusal,
    held: bool,
    refused: &dyn Fn(&CodeRefusedDialer),
) -> Gated {
    let end = if held && dialer_left(stream, Duration::ZERO) {
        CodeRefusedEnd::LeftWhileHeld
    } else {
        CodeRefusedEnd::Closed
    };
    refused(&CodeRefusedDialer {
        pid,
        refusal: refusal.clone(),
        end,
    });
    Gated::Refused(RendezvousError::PeerCode { pid, refusal })
}

fn admitted_after_hold(pid: i32, held_since: Option<Instant>) {
    if let Some(since) = held_since {
        aterm_log::info!(
            "overlap handoff: macOS's code-signing service answered; admitted the rendezvous \
             dialer (pid {pid}) after holding it {} ms",
            since.elapsed().as_millis()
        );
    }
}

fn dialer_left(stream: &CtlStream, wait: Duration) -> bool {
    let mut fds = [libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    }];
    // SAFETY: `fds` is a valid initialized `pollfd` for `stream` with a bounded timeout.
    let ready = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            1,
            i32::try_from(wait.as_millis()).unwrap_or(i32::MAX),
        )
    };
    if ready <= 0 {
        return false;
    }
    if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    let mut byte = 0u8;
    // SAFETY: 1-byte `MSG_PEEK | MSG_DONTWAIT` read into a stack local on a valid socket fd.
    let peeked = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast::<libc::c_void>(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if peeked == 0 {
        return true;
    }
    std::thread::sleep(wait);
    false
}

/// Bound single-use `AF_UNIX` rendezvous listener and its per-attempt claim secret.
pub(crate) struct Rendezvous {
    path: PathBuf,
    listener: CtlListener,
    claim: String,
}

impl Rendezvous {
    /// Binds the single-use rendezvous listener for `nonce` and mints its claim secret.
    pub(crate) fn bind(nonce: &str) -> Result<Self, RendezvousError> {
        let dir = rendezvous_dir().ok_or(RendezvousError::NoControlDir)?;
        Self::bind_in(&dir, nonce)
    }

    fn bind_in(dir: &Path, nonce: &str) -> Result<Self, RendezvousError> {
        use std::os::unix::fs::PermissionsExt as _;

        #[cfg(unix)]
        sweep_dead_rendezvous(dir);
        let path = rendezvous_path(dir, nonce);
        let text = path
            .to_str()
            .ok_or_else(|| RendezvousError::Bind("the control directory is not UTF-8".to_string()))?
            .to_string();
        if !crate::control_auth::sun_path_ok(&text) {
            return Err(RendezvousError::PathTooLong {
                path: text,
                limit: crate::control_auth::MAX_SUN_PATH,
            });
        }
        let claim = aterm_uds::rand::hex_token::<CLAIM_SECRET_BYTES>()
            .map_err(|error| RendezvousError::Secret(error.to_string()))?;
        let listener = aterm_uds::kept_out(|| CtlListener::bind(&path))
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        let bound = Self {
            path,
            listener,
            claim,
        };
        #[cfg(unix)]
        prove_bound_socket_is_ours(dir, &bound.path).map_err(RendezvousError::Bind)?;
        std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        Ok(bound)
    }

    /// Socket path to publish in the successor's launch environment.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Hex-encoded claim secret to publish in the successor's launch environment.
    #[must_use]
    pub(crate) fn claim(&self) -> &str {
        &self.claim
    }

    /// Waits for the successor to dial, verifies UID/PID/code gates, and returns the [`ClaimedPeer`].
    pub(crate) fn accept_claim_observed(
        &self,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
        refused: &dyn Fn(&CodeRefusedDialer),
    ) -> Result<ClaimedPeer, RendezvousError> {
        let mut refusals = 0usize;
        let mut last_refusal: Option<RendezvousError> = None;
        loop {
            let stream = match self.accept_one(deadline, abort) {
                Ok(stream) => stream,
                Err(RendezvousError::Deadline) => {
                    return Err(last_refusal.unwrap_or(RendezvousError::Deadline));
                }
                Err(error) => return Err(error),
            };
            let refusal = match self.gate_one(&stream, expected_pid, code, deadline, abort, refused)
            {
                Ok(Admitted { pid, chunks, token }) => {
                    return Ok(ClaimedPeer {
                        stream,
                        pid,
                        chunks,
                        empty: false,
                        token,
                    });
                }
                Err(Gated::Refused(refusal)) => refusal,
                Err(Gated::WaitEnded { end, held }) => {
                    return Err(match end {
                        RendezvousError::Deadline => held,
                        other => other,
                    });
                }
            };
            refusals += 1;
            aterm_log::warn!(
                "overlap handoff: refused rendezvous dialer #{refusals} ({refusal}); the \
                 rendezvous stays open for the successor until the deadline"
            );
            if refusals >= MAX_REFUSED_DIALS {
                return Err(refusal);
            }
            last_refusal = Some(refusal);
        }
    }

    /// Calls [`Self::accept_claim_observed`] without a code-refusal observer callback.
    #[cfg(test)]
    pub(crate) fn accept_claim(
        &self,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
    ) -> Result<ClaimedPeer, RendezvousError> {
        self.accept_claim_observed(expected_pid, code, deadline, abort, &|_| {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_rendezvous_admission",
            action = "Grant",
            project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
        )
    )]
    fn gate_one(
        &self,
        stream: &CtlStream,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
        refused: &dyn Fn(&CodeRefusedDialer),
    ) -> Result<Admitted, Gated> {
        let frame_deadline = deadline.min(Instant::now() + CLAIM_FRAME_BUDGET);
        let mut frame = [0u8; CLAIM_FRAME_LEN];
        read_frame_by_deadline(stream, &mut frame, frame_deadline, &|error| {
            RendezvousError::Accept(error.to_string())
        })
        .map_err(Gated::Refused)?;
        let Some(chunks) = claim_frame_kind(&frame, &self.claim) else {
            return Err(Gated::Refused(RendezvousError::Claim));
        };
        let attested = fdpass::peer_pid(stream)
            .map_err(|error| Gated::Refused(RendezvousError::PeerUnattested(error.to_string())))?;
        let attested = i32::try_from(attested).map_err(|_| {
            Gated::Refused(RendezvousError::PeerPid {
                expected: expected_pid.unwrap_or(-1),
                actual: -1,
            })
        })?;
        if let Some(expected) = expected_pid
            && attested != expected
        {
            return Err(Gated::Refused(RendezvousError::PeerPid {
                expected,
                actual: attested,
            }));
        }
        let token = aterm_uds::peer::peer_audit_token(stream);
        admit_code(stream, attested, token, code, deadline, abort, refused)?;
        stream
            .set_write_timeout(Some(remaining_io_budget(deadline).map_err(Gated::Refused)?))
            .map_err(|error| Gated::Refused(RendezvousError::Accept(error.to_string())))?;
        Ok(Admitted {
            pid: attested,
            chunks,
            token,
        })
    }

    fn accept_one(
        &self,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
    ) -> Result<CtlStream, RendezvousError> {
        loop {
            if abort() {
                return Err(RendezvousError::Cancelled);
            }
            let now = io_now();
            if now >= deadline {
                return Err(RendezvousError::Deadline);
            }
            let slice = deadline
                .saturating_duration_since(now)
                .min(ACCEPT_POLL_SLICE);
            match aterm_uds::accept(
                &self.listener,
                aterm_uds::AcceptWait::Until(Instant::now() + slice),
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((stream, _)) => return Ok(stream),
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    // A peer that connected and reset before we accepted, or a
                    // signal. Neither is this attempt's dialer arriving.
                    continue;
                }
                Err(error) => return Err(RendezvousError::Accept(error.to_string())),
            }
        }
    }
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(launch_stamp_path(&self.path));
    }
}

const LAUNCH_STAMP_SUFFIX: &str = ".launch";

/// Returns the sibling `.launched` stamp path for a `rendezvous` socket path.
#[must_use]
pub(crate) fn launch_stamp_path(rendezvous: &Path) -> PathBuf {
    let mut stamp = rendezvous.as_os_str().to_os_string();
    stamp.push(LAUNCH_STAMP_SUFFIX);
    PathBuf::from(stamp)
}

/// Writes the `.launched` stamp file if this process was launched with a rendezvous socket path.
pub(crate) fn stamp_this_launch(env: &crate::handoff_env::HandoffEnv) {
    let Some(rendezvous) = env.os(ENV_RENDEZVOUS) else {
        return;
    };
    if let Err(error) = aterm_update::stamp_trial_launch(&launch_stamp_path(Path::new(rendezvous)))
    {
        aterm_log::warn!(
            "overlap handoff: could not stamp this launch beside the rendezvous ({error}); a \
             boot-trial launch it counts stays counted if it dies before its dial"
        );
    }
}

/// Accepted successor connection that passed secret, UID, PID, and code-signing verification.
pub(crate) struct ClaimedPeer {
    stream: CtlStream,
    pid: i32,
    chunks: bool,
    /// The successor reads a grant of no session (`empty1`), which its bundle declares; the
    /// claim itself never says so, so the producer sets it ([`Self::reading_empty_grant`]).
    empty: bool,
    token: Option<aterm_uds::peer::AuditToken>,
}

impl ClaimedPeer {
    /// Returns whether the accepted peer socket has hung up (`POLLHUP | POLLERR | POLLNVAL`).
    #[must_use]
    pub(crate) fn poll_hangup(&self) -> bool {
        #[cfg(unix)]
        {
            let mut fds = [libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            }];
            // SAFETY: single initialized `pollfd` with a non-blocking zero timeout.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
            if ready <= 0 {
                return false;
            }
            fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Kernel-attested PID of the accepted successor process.
    #[must_use]
    pub(crate) const fn pid(&self) -> i32 {
        self.pid
    }

    fn dialer_unchanged(&self) -> Result<(), RendezvousError> {
        let now = aterm_uds::peer::peer_audit_token(&self.stream);
        if now == self.token {
            return Ok(());
        }
        Err(RendezvousError::TransferNotSent(format!(
            "the dialer's audit token changed since its code was checked (pid {} then, {} now)",
            self.token
                .map_or_else(|| "unnamed".to_string(), |token| token.pid().to_string()),
            now.map_or_else(|| "unnamed".to_string(), |token| token.pid().to_string())
        )))
    }

    /// Maximum number of sessions [`Self::transfer`] may send to this peer.
    #[must_use]
    pub(crate) const fn grant_session_limit(&self) -> usize {
        grant_session_limit(self.chunks)
    }

    /// Marks this peer as a successor that reads a grant of no session (`empty1`), as its
    /// bundle declares; [`Self::transfer`] sends an empty grant only to such a peer.
    #[must_use]
    pub(crate) const fn reading_empty_grant(mut self, empty: bool) -> Self {
        self.empty = empty;
        self
    }

    /// Transfers the readiness pipe, commit pipe, and session PTY masters over `SCM_RIGHTS`;
    /// a grant of no session goes only to a peer [`Self::reading_empty_grant`] marked.
    pub(crate) fn transfer(
        &self,
        nonce: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
        parked_at: Option<Instant>,
    ) -> Result<(), RendezvousError> {
        let shape = grant_shape(self.chunks, self.empty, sessions.len())?;
        let malformed = || {
            RendezvousError::TransferNotSent("the grant body exceeds its wire format".to_string())
        };
        let parked_for = parked_at.map(|parked| parked.elapsed());
        let body = encode_grant_body(nonce, sessions, parked_for).ok_or_else(malformed)?;
        if shape == GrantShape::Chunked {
            return self.transfer_chunked(&body, sessions, ready, commit, deadline);
        }
        let header = grant_header(body.len()).ok_or_else(malformed)?;
        let remaining = grant_budget(deadline)?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        let mut descriptors = Vec::with_capacity(sessions.len() + RENDEZVOUS_CHANNEL_FDS);
        descriptors.extend(sessions.iter().map(|(_, _, master)| *master));
        descriptors.push(ready);
        descriptors.push(commit);
        self.dialer_unchanged()?;
        let sent = fdpass::send_with_fds(&self.stream, &header, &descriptors)
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        if sent < header.len() {
            (&self.stream)
                .write_all(&header[sent..])
                .map_err(|error| RendezvousError::TransferPartial(error.to_string()))?;
        }
        (&self.stream)
            .write_all(body.as_bytes())
            .and_then(|()| (&self.stream).flush())
            .map_err(|error| RendezvousError::TransferPartial(error.to_string()))?;
        Ok(())
    }

    fn transfer_chunked(
        &self,
        body: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
    ) -> Result<(), RendezvousError> {
        let sizes = grant_chunk_sizes(sessions.len());
        let header = grant_chunks_header(body.len(), sizes.len()).ok_or_else(|| {
            RendezvousError::TransferNotSent("the grant body exceeds its wire format".to_string())
        })?;
        let mut descriptors = Vec::with_capacity(sessions.len() + RENDEZVOUS_CHANNEL_FDS);
        descriptors.extend(sessions.iter().map(|(_, _, master)| *master));
        descriptors.push(ready);
        descriptors.push(commit);
        let mut chunks = Vec::with_capacity(sizes.len());
        let mut rest = descriptors.as_slice();
        for size in &sizes {
            let (chunk, tail) = rest.split_at(*size);
            chunks.push(chunk);
            rest = tail;
        }
        debug_assert!(rest.is_empty(), "the layout places every descriptor");
        let remaining = grant_budget(deadline)?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        self.dialer_unchanged()?;
        let sent = fdpass::send_with_fds(&self.stream, &header, chunks[0])
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        let partial = |error: std::io::Error| RendezvousError::TransferPartial(error.to_string());
        if sent < header.len() {
            (&self.stream).write_all(&header[sent..]).map_err(partial)?;
        }
        (&self.stream).write_all(body.as_bytes()).map_err(partial)?;
        for (index, chunk) in chunks.iter().enumerate().skip(1) {
            let frame = grant_continuation(index).ok_or_else(|| {
                RendezvousError::TransferPartial("a chunk index past its wire format".to_string())
            })?;
            let remaining = remaining_io_budget(deadline).map_err(|_| {
                RendezvousError::TransferPartial(
                    "the grant deadline passed between its chunks".to_string(),
                )
            })?;
            self.stream
                .set_write_timeout(Some(remaining))
                .map_err(partial)?;
            let sent = fdpass::send_with_fds(&self.stream, &frame, chunk).map_err(partial)?;
            if sent < frame.len() {
                (&self.stream).write_all(&frame[sent..]).map_err(partial)?;
            }
        }
        (&self.stream).flush().map_err(partial)?;
        Ok(())
    }
}

/// Connects to `path` and writes the v2 claim header with `secret` for tests.
#[cfg(test)]
pub(crate) fn dial_for_test(path: &Path, secret: &str) -> std::io::Result<CtlStream> {
    let frame = claim_frame(secret);
    let stream = CtlStream::connect(path)?;
    (&stream).write_all(&frame)?;
    (&stream).flush()?;
    Ok(stream)
}

#[cfg(test)]
pub(crate) const TRUSTD_SILENT: &str =
    "the process trustd gate was silent — the machine's, not the code's";

/// Aborts the current test with a `COULD NOT RUN` message when `trustd` is unresponsive.
#[cfg(test)]
#[track_caller]
pub(crate) fn trustd_silent(why: impl std::fmt::Display) -> ! {
    aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {why}"))
}

/// Returns whether `error` was caused by a code-signing check that timed out waiting on `trustd`.
#[cfg(test)]
pub(crate) fn held_for_trustd(error: &RendezvousError) -> bool {
    matches!(
        error,
        RendezvousError::PeerCode {
            refusal: CodeRefusal::TrustdSilent(_),
            ..
        }
    )
}

/// Unwraps an accepted claim result in tests, skipping if `trustd` was unresponsive.
#[cfg(test)]
pub(crate) fn admitted<T>(accepted: Result<T, RendezvousError>, what: &str) -> T {
    match accepted {
        Ok(peer) => peer,
        Err(error) if held_for_trustd(&error) => trustd_silent(error),
        Err(error) => panic!("{what}: {error}"),
    }
}

/// Dials `path` and completes the successor claim handshake for cross-version fixture tests.
#[cfg(test)]
pub(crate) fn dial_and_claim_for_fixture(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
) -> Result<(String, Vec<OwnedFd>, OwnedFd, OwnedFd), RendezvousError> {
    let parent = libc::pid_t::try_from(std::process::id())
        .ok()
        .and_then(crate::seamless::AttestedParent::attest_live_for_test);
    let claimed = dial_and_claim(path, secret, expected_nonce, deadlines, parent, false)?;
    Ok((
        claimed.fds_wire,
        claimed.masters,
        claimed.ready,
        claimed.commit,
    ))
}

#[cfg(test)]
fn claim_frame(secret: &str) -> [u8; CLAIM_FRAME_LEN] {
    claim_frame_with(secret, false)
}

fn claim_frame_with(secret: &str, chunks: bool) -> [u8; CLAIM_FRAME_LEN] {
    let mut frame = [0u8; CLAIM_FRAME_LEN];
    frame[..CLAIM_MAGIC.len()].copy_from_slice(if chunks {
        CLAIM_CHUNKS_MAGIC
    } else {
        CLAIM_MAGIC
    });
    let bytes = secret.as_bytes();
    let carried = bytes.len().min(CLAIM_HEX_LEN);
    frame[CLAIM_MAGIC.len()..CLAIM_MAGIC.len() + carried].copy_from_slice(&bytes[..carried]);
    frame
}

fn claim_frame_kind(frame: &[u8; CLAIM_FRAME_LEN], secret: &str) -> Option<bool> {
    let magic = &frame[..CLAIM_MAGIC.len()];
    let chunks = if magic == CLAIM_MAGIC {
        false
    } else if magic == CLAIM_CHUNKS_MAGIC {
        true
    } else {
        return None;
    };
    aterm_digest::ct_eq(&frame[CLAIM_MAGIC.len()..], secret.as_bytes()).then_some(chunks)
}

#[cfg(test)]
fn claim_frame_matches(frame: &[u8; CLAIM_FRAME_LEN], secret: &str) -> bool {
    claim_frame_kind(frame, secret).is_some()
}

fn grant_header(body_len: usize) -> Option<[u8; GRANT_HEADER_LEN]> {
    if body_len > MAX_GRANT_BODY_BYTES {
        return None;
    }
    let mut header = [0u8; GRANT_HEADER_LEN];
    header[..GRANT_MAGIC.len()].copy_from_slice(GRANT_MAGIC);
    header[GRANT_MAGIC.len()..].copy_from_slice(&u16::try_from(body_len).ok()?.to_be_bytes());
    Some(header)
}

fn grant_chunks_header(body_len: usize, chunks: usize) -> Option<[u8; GRANT_CHUNKS_HEADER_LEN]> {
    if body_len > MAX_GRANT_BODY_BYTES || chunks == 0 || chunks > MAX_GRANT_CHUNKS {
        return None;
    }
    let mut header = [0u8; GRANT_CHUNKS_HEADER_LEN];
    header[..GRANT_CHUNKS_MAGIC.len()].copy_from_slice(GRANT_CHUNKS_MAGIC);
    let length = GRANT_CHUNKS_MAGIC.len();
    header[length..length + 2].copy_from_slice(&u16::try_from(body_len).ok()?.to_be_bytes());
    header[length + 2] = u8::try_from(chunks).ok()?;
    Some(header)
}

fn grant_continuation(index: usize) -> Option<[u8; GRANT_CONTINUATION_LEN]> {
    if index == 0 || index >= MAX_GRANT_CHUNKS {
        return None;
    }
    let mut frame = [0u8; GRANT_CONTINUATION_LEN];
    frame[..GRANT_CONTINUATION_MAGIC.len()].copy_from_slice(GRANT_CONTINUATION_MAGIC);
    frame[GRANT_CONTINUATION_MAGIC.len()] = u8::try_from(index).ok()?;
    Some(frame)
}

const GRANT_PARK_KEY: &str = "park_us=";

fn encode_grant_body(
    nonce: &str,
    sessions: &[(u64, i32, BorrowedFd<'_>)],
    parked_for: Option<Duration>,
) -> Option<String> {
    if !aterm_codec::hex::is_hex(nonce, 32) {
        return None;
    }
    let entries = sessions
        .iter()
        .map(|(local_id, pid, _)| format!("{local_id}:{pid}"))
        .collect::<Vec<_>>()
        .join(",");
    let park = match parked_for {
        Some(parked) => format!(
            "{GRANT_PARK_KEY}{}\n",
            u64::try_from(parked.as_micros()).ok()?,
        ),
        None => String::new(),
    };
    let body = format!("{nonce}\n{entries}\n{park}");
    (body.len() <= MAX_GRANT_BODY_BYTES).then_some(body)
}

#[derive(Debug, PartialEq, Eq)]
struct GrantBody {
    sessions: Vec<(u64, i32)>,
    parked_for: Option<Duration>,
}

fn is_canonical_decimal(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s.len() == 1 || !s.starts_with('0'))
}

fn parse_grant_body(
    body: &str,
    expected_nonce: &str,
    empty_ok: bool,
) -> Result<GrantBody, RendezvousError> {
    let Some((nonce, rest)) = body.split_once('\n') else {
        return Err(RendezvousError::Grant("no nonce line".to_string()));
    };
    if nonce != expected_nonce {
        return Err(RendezvousError::NonceMismatch);
    }
    let Some(rest) = rest.strip_suffix('\n') else {
        return Err(RendezvousError::Grant(
            "unterminated session list".to_string(),
        ));
    };
    let (entries, parked_for) = match rest.split_once('\n') {
        None => (rest, None),
        Some((entries, park)) => {
            let micros = park
                .strip_prefix(GRANT_PARK_KEY)
                .filter(|digits| is_canonical_decimal(digits))
                .and_then(|digits| digits.parse::<u64>().ok())
                .ok_or_else(|| RendezvousError::Grant(format!("malformed park stamp {park}")))?;
            (entries, Some(Duration::from_micros(micros)))
        }
    };
    if entries.is_empty() {
        // A grant of no session hands over the windows alone, and only a successor the
        // parent offered `empty1` takes one: every build before it refuses the empty list,
        // and an empty list it was not offered is a malformed grant.
        if !empty_ok {
            return Err(RendezvousError::Grant("no sessions".to_string()));
        }
        return Ok(GrantBody {
            sessions: Vec::new(),
            parked_for,
        });
    }
    let mut sessions = Vec::new();
    for entry in entries.split(',') {
        let Some((local_id, pid)) = entry.split_once(':') else {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        };
        if !is_canonical_decimal(local_id) || !is_canonical_decimal(pid) {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        }
        let (Ok(local_id), Ok(pid)) = (local_id.parse::<u64>(), pid.parse::<i32>()) else {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        };
        if pid <= 0 {
            return Err(RendezvousError::Grant(format!(
                "implausible pid in {entry}"
            )));
        }
        sessions.push((local_id, pid));
    }
    let mut ids = sessions.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(RendezvousError::Grant("duplicate session id".to_string()));
    }
    Ok(GrantBody {
        sessions,
        parked_for,
    })
}

/// Descriptors and synthesized `ATERM_SEAMLESS_FDS` wire received by the successor.
pub(crate) struct ClaimedHandoff {
    fds_wire: String,
    masters: Vec<OwnedFd>,
    ready: OwnedFd,
    commit: OwnedFd,
    parked_for: Option<Duration>,
}

impl ClaimedHandoff {
    /// Number of terminal sessions carried in the claimed handoff grant.
    #[must_use]
    pub(crate) const fn session_count(&self) -> usize {
        self.masters.len()
    }

    /// Elapsed duration the parent was parked before sending the grant, if reported.
    #[must_use]
    pub(crate) const fn parked_before_grant(&self) -> Option<Duration> {
        self.parked_for
    }

    /// Converts this handoff into a [`ClaimedIntake`] ready for intake installation.
    #[must_use]
    pub(crate) fn into_intake(self) -> ClaimedIntake {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
            parked_for: _,
        } = self;
        ClaimedIntake {
            fds_wire,
            masters,
            ready,
            commit,
        }
    }
}

/// Claimed handoff descriptors and wire string staged for
/// [`HandoffEnv`](crate::handoff_env::HandoffEnv) installation.
pub(crate) struct ClaimedIntake {
    fds_wire: String,
    masters: Vec<OwnedFd>,
    ready: OwnedFd,
    commit: OwnedFd,
}

impl ClaimedIntake {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_successor_warm_before_claim",
            action = "ClaimOk",
            project = "aterm_gui::seamless::handoff_env_conformance::project"
        )
    )]
    /// Installs the claimed descriptor wire and owned descriptors into `env`.
    pub(crate) fn install(self, env: &mut crate::handoff_env::HandoffEnv) {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
        } = self;
        env.set(crate::seamless::ENV_FDS, fds_wire);
        env.set(crate::seamless::ENV_READY_FD, ready.as_raw_fd().to_string());
        env.set(
            crate::seamless::ENV_COMMIT_FD,
            commit.as_raw_fd().to_string(),
        );
        for master in masters {
            let _ = master.into_raw_fd();
        }
        let _ = ready.into_raw_fd();
        let _ = commit.into_raw_fd();
    }
}

/// Returns whether `env` contains any rendezvous environment variable.
#[must_use]
pub(crate) fn rendezvous_present(env: &crate::handoff_env::HandoffEnv) -> bool {
    env.present(ENV_RENDEZVOUS) || env.present(ENV_CLAIM)
}

pub(crate) const ENV_PROOF_TERM: HandoffName = HandoffName::new("ATERM_HANDOFF_PROOF_TERM");

/// Consumes `ATERM_SEAMLESS_PROOF_TERM` from `env` (`Some(true)` for `device`, `Some(false)` for `fd`).
#[must_use]
pub(crate) fn take_device_proof_term(env: &mut crate::handoff_env::HandoffEnv) -> Option<bool> {
    let value = env.take(ENV_PROOF_TERM)?;
    parse_proof_term(&value.to_string_lossy())
}

/// Parses an `ATERM_SEAMLESS_PROOF_TERM` value (`"device"` or `"fd"`).
#[must_use]
pub(crate) fn parse_proof_term(value: &str) -> Option<bool> {
    match value {
        "device" => Some(true),
        "fd" => Some(false),
        _ => None,
    }
}

/// Simulated pre-dial successor termination mode for the `debug_die_before_dial` dev seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugDeathBeforeDial {
    Exit,
    Kill,
    ExitAfterCheckIn,
    KillAfterCheckIn,
}

impl DebugDeathBeforeDial {
    /// Returns whether this mode registers with LaunchServices before terminating.
    #[must_use]
    pub(crate) const fn checks_in_first(self) -> bool {
        matches!(self, Self::ExitAfterCheckIn | Self::KillAfterCheckIn)
    }

    /// Returns whether this mode terminates via `SIGKILL` instead of `_exit`.
    #[must_use]
    pub(crate) const fn kills(self) -> bool {
        matches!(self, Self::Kill | Self::KillAfterCheckIn)
    }
}

pub(crate) const DEBUG_DEATH_EXIT_CODE: i32 = 70;

/// Reads `ATERM_DEBUG_DIE_BEFORE_DIAL` when dev seams are enabled.
#[must_use]
pub(crate) fn debug_die_before_dial() -> Option<DebugDeathBeforeDial> {
    parse_debug_die_before_dial(aterm_types::dev_seam!("ATERM_DEBUG_DIE_BEFORE_DIAL").as_deref())
}

#[must_use]
fn parse_debug_die_before_dial(value: Option<&std::ffi::OsStr>) -> Option<DebugDeathBeforeDial> {
    let value = value?.to_str()?;
    match value {
        "kill" => return Some(DebugDeathBeforeDial::Kill),
        "checked-in" => return Some(DebugDeathBeforeDial::ExitAfterCheckIn),
        "checked-in-kill" => return Some(DebugDeathBeforeDial::KillAfterCheckIn),
        _ => {}
    }
    aterm_types::control_socket::env_flag_engaged(Some(value)).then_some(DebugDeathBeforeDial::Exit)
}

/// Calls `+[NSApplication sharedApplication]` to check in with LaunchServices before a simulated crash.
pub(crate) fn debug_check_in_with_launch_services(_windowed: crate::platform::Windowed) -> bool {
    use aterm_objc::{class, sel};

    let Some(_main_thread) = crate::appkit::MainThread::new() else {
        return false;
    };
    // SAFETY: `+[NSApplication sharedApplication]` is called on the main thread (`_main_thread` witness).
    let app =
        unsafe { crate::appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
    !app.is_null()
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "Dial",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "ClaimFail",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
/// Consumes rendezvous environment variables from `env`, dials the parent, and returns the [`ClaimedHandoff`].
pub(crate) fn claim_incoming(
    env: &mut crate::handoff_env::HandoffEnv,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
) -> Result<ClaimedHandoff, RendezvousError> {
    let caps = env.take(ENV_GRANT_CAPS);
    let offered = OfferedGrants {
        chunks: offers_chunked_grant(caps.as_deref()),
        empty: offers_empty_grant(caps.as_deref()),
    };
    let (path, secret) = rendezvous_env(env.take(ENV_RENDEZVOUS), env.take(ENV_CLAIM))?;
    let claimed =
        dial_and_claim_offered(&path, &secret, expected_nonce, deadlines, parent, offered)
            .map_err(claim_side);
    if claimed.is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    claimed
}

fn claim_side(error: RendezvousError) -> RendezvousError {
    match error {
        RendezvousError::Deadline => RendezvousError::ClaimDeadline,
        other => other,
    }
}

fn offers_chunked_grant(caps: Option<&std::ffi::OsStr>) -> bool {
    offers_grant(caps, GRANT_CAPS_CHUNKS)
}

fn offers_empty_grant(caps: Option<&std::ffi::OsStr>) -> bool {
    offers_grant(caps, GRANT_CAPS_EMPTY)
}

fn offers_grant(caps: Option<&std::ffi::OsStr>, wanted: &str) -> bool {
    caps.and_then(std::ffi::OsStr::to_str)
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == wanted))
}

/// The grant extensions the parent offered this successor in `ENV_GRANT_CAPS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OfferedGrants {
    chunks: bool,
    empty: bool,
}

fn rendezvous_env(
    path: Option<std::ffi::OsString>,
    secret: Option<std::ffi::OsString>,
) -> Result<(PathBuf, String), RendezvousError> {
    let (path, secret) = match (path, secret) {
        (Some(path), Some(secret)) => (path, secret),
        (None, None) => return Err(RendezvousError::NoRendezvous),
        (Some(_), None) => {
            return Err(RendezvousError::HalfRendezvous {
                present: ENV_RENDEZVOUS,
                missing: ENV_CLAIM,
            });
        }
        (None, Some(_)) => {
            return Err(RendezvousError::HalfRendezvous {
                present: ENV_CLAIM,
                missing: ENV_RENDEZVOUS,
            });
        }
    };
    let secret = secret
        .into_string()
        .map_err(|_| RendezvousError::Env(ENV_CLAIM))?;
    if !aterm_codec::hex::is_hex(&secret, CLAIM_HEX_LEN) {
        return Err(RendezvousError::Env(ENV_CLAIM));
    }
    Ok((PathBuf::from(path), secret))
}

/// Connect/claim deadline and total grant deadline for a successor dial attempt.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClaimDeadlines {
    pub dial: Instant,
    pub grant: Instant,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_rendezvous_claim",
        action = "PresentClaim",
        project = "aterm_gui::handoff_rendezvous::claim_presentation_conformance::project"
    )
)]
fn listener_may_receive_claim(
    listener_uid: Option<u32>,
    own_uid: u32,
    listener_pid: Option<u32>,
    parent: Option<crate::seamless::AttestedParent>,
) -> Result<(), RendezvousError> {
    if listener_uid != Some(own_uid) {
        return Err(RendezvousError::Dial(
            "rendezvous listener is not owned by this uid; refusing to present the claim"
                .to_string(),
        ));
    }
    let Some(parent) = parent else {
        return Err(RendezvousError::Dial(
            "no attested outgoing process to present the claim to".to_string(),
        ));
    };
    if !listener_pid.is_some_and(|pid| parent.owns_live_listener(pid)) {
        return Err(RendezvousError::Dial(format!(
            "rendezvous listener (pid {}) is not the attested outgoing process; refusing to \
             present the claim",
            listener_pid.map_or_else(|| "unattested".to_string(), |pid| pid.to_string())
        )));
    }
    Ok(())
}

/// [`dial_and_claim_offered`] for a successor offered the chunked grant or nothing.
#[cfg(test)]
fn dial_and_claim(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
    chunks: bool,
) -> Result<ClaimedHandoff, RendezvousError> {
    dial_and_claim_offered(
        path,
        secret,
        expected_nonce,
        deadlines,
        parent,
        OfferedGrants {
            chunks,
            empty: false,
        },
    )
}

fn dial_and_claim_offered(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
    offered: OfferedGrants,
) -> Result<ClaimedHandoff, RendezvousError> {
    let chunks = offered.chunks;
    let stream =
        CtlStream::connect(path).map_err(|error| RendezvousError::Dial(error.to_string()))?;
    // SAFETY: `getuid` has no preconditions.
    let own_uid = unsafe { libc::getuid() };
    listener_may_receive_claim(
        crate::control_auth::peer_uid(&stream),
        own_uid,
        fdpass::peer_pid(&stream).ok(),
        parent,
    )?;
    let remaining = remaining_io_budget(deadlines.dial)?;
    stream
        .set_read_timeout(Some(remaining))
        .and_then(|()| stream.set_write_timeout(Some(remaining)))
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;
    (&stream)
        .write_all(&claim_frame_with(secret, chunks))
        .and_then(|()| (&stream).flush())
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;

    let grant_failed = |error: std::io::Error| RendezvousError::Grant(error.to_string());
    let mut header = [0u8; GRANT_HEADER_LEN];
    let received = recv_fds_by_deadline(&stream, &mut header, deadlines.grant)?;
    let mut fds = received.fds;
    if received.bytes == 0 && fds.is_empty() {
        return Err(RendezvousError::Grant(
            "the parent closed the rendezvous".to_string(),
        ));
    }
    if received.bytes < GRANT_HEADER_LEN {
        read_frame_by_deadline(
            &stream,
            &mut header[received.bytes..],
            deadlines.grant,
            &grant_failed,
        )?;
    }
    let magic = &header[..GRANT_MAGIC.len()];
    let chunk_count = if magic == GRANT_MAGIC {
        None
    } else if chunks && magic == GRANT_CHUNKS_MAGIC {
        let mut count = [0u8; 1];
        read_frame_by_deadline(&stream, &mut count, deadlines.grant, &grant_failed)?;
        let count = usize::from(count[0]);
        if count == 0 || count > MAX_GRANT_CHUNKS {
            return Err(RendezvousError::Grant(format!(
                "a chunked grant of {count} messages"
            )));
        }
        Some(count)
    } else {
        return Err(RendezvousError::Grant("wrong magic".to_string()));
    };
    let body_len = usize::from(u16::from_be_bytes([
        header[GRANT_MAGIC.len()],
        header[GRANT_MAGIC.len() + 1],
    ]));
    if body_len > MAX_GRANT_BODY_BYTES {
        return Err(RendezvousError::Grant("oversized body".to_string()));
    }
    let mut body = vec![0u8; body_len];
    read_frame_by_deadline(&stream, &mut body, deadlines.grant, &grant_failed)?;
    let body =
        String::from_utf8(body).map_err(|_| RendezvousError::Grant("not UTF-8".to_string()))?;
    let GrantBody {
        sessions,
        parked_for,
    } = parse_grant_body(&body, expected_nonce, offered.empty)?;

    let expected = sessions.len() + RENDEZVOUS_CHANNEL_FDS;
    if let Some(count) = chunk_count {
        // The layout is computed, not believed: the body's session count fixes how many
        // messages there are and what each carries, every message must be exactly that under the
        // one grant deadline, and the first refusal drops (closes) everything received so far.
        if sessions.len() > MAX_CHUNKED_SESSIONS {
            return Err(RendezvousError::DescriptorCount {
                expected,
                received: fds.len(),
            });
        }
        let sizes = grant_chunk_sizes(sessions.len());
        if sizes.len() != count {
            return Err(RendezvousError::Grant(format!(
                "a grant of {} sessions in {count} messages, where its layout is {}",
                sessions.len(),
                sizes.len()
            )));
        }
        if fds.len() != sizes[0] {
            return Err(RendezvousError::DescriptorCount {
                expected,
                received: fds.len(),
            });
        }
        for (index, size) in sizes.iter().enumerate().skip(1) {
            let mut frame = [0u8; GRANT_CONTINUATION_LEN];
            let received = recv_fds_by_deadline(&stream, &mut frame, deadlines.grant)?;
            let arrived = received.fds.len();
            fds.extend(received.fds);
            if received.bytes == 0 && arrived == 0 {
                return Err(RendezvousError::Grant(format!(
                    "the parent closed the rendezvous before chunk {index} of {count}"
                )));
            }
            if received.bytes < GRANT_CONTINUATION_LEN {
                read_frame_by_deadline(
                    &stream,
                    &mut frame[received.bytes..],
                    deadlines.grant,
                    &grant_failed,
                )?;
            }
            if &frame[..GRANT_CONTINUATION_MAGIC.len()] != GRANT_CONTINUATION_MAGIC
                || usize::from(frame[GRANT_CONTINUATION_MAGIC.len()]) != index
            {
                return Err(RendezvousError::Grant(format!(
                    "chunk {index} of {count} is not the continuation its layout names"
                )));
            }
            if arrived != *size {
                return Err(RendezvousError::DescriptorCount {
                    expected,
                    received: fds.len(),
                });
            }
        }
    }
    if fds.len() != expected || expected > MAX_GRANT_FDS {
        return Err(RendezvousError::DescriptorCount {
            expected,
            received: fds.len(),
        });
    }
    if fds.iter().any(|fd| fd.as_raw_fd() < 3) {
        return Err(RendezvousError::Grant(
            "a received descriptor names stdio".to_string(),
        ));
    }
    let mut masters = fds;
    let (Some(commit), Some(ready)) = (masters.pop(), masters.pop()) else {
        return Err(RendezvousError::DescriptorCount {
            expected,
            received: masters.len(),
        });
    };
    let fds_wire = sessions
        .iter()
        .zip(masters.iter())
        .map(|((local_id, pid), master)| format!("{local_id}={}:{pid}", master.as_raw_fd()))
        .collect::<Vec<_>>()
        .join(",");
    Ok(ClaimedHandoff {
        fds_wire,
        masters,
        ready,
        commit,
        parked_for,
    })
}

fn recv_fds_by_deadline(
    stream: &CtlStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<fdpass::Received, RendezvousError> {
    loop {
        arm_read_timeout(stream, remaining_io_budget(deadline)?)
            .map_err(|error| RendezvousError::Grant(error.to_string()))?;
        match fdpass::recv_with_fds(stream, buf, GRANT_MESSAGE_FDS) {
            Ok(received) => return Ok(received),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(RendezvousError::Grant(error.to_string())),
        }
    }
}

fn arm_read_timeout(stream: &CtlStream, timeout: Duration) -> std::io::Result<()> {
    match stream.set_read_timeout(Some(timeout)) {
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        other => other,
    }
}

fn read_frame_by_deadline(
    stream: &CtlStream,
    buf: &mut [u8],
    deadline: Instant,
    wrap: &dyn Fn(std::io::Error) -> RendezvousError,
) -> Result<(), RendezvousError> {
    let mut source = stream;
    let mut filled = 0usize;
    while filled < buf.len() {
        let remaining = remaining_io_budget(deadline)?;
        arm_read_timeout(source, remaining).map_err(wrap)?;
        match source.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(wrap(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the peer closed the connection part-way through a frame",
                )));
            }
            Ok(read) => filled += read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(wrap(error)),
        }
    }
    Ok(())
}

fn grant_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    remaining_io_budget(deadline).map_err(|_| RendezvousError::GrantDeadline)
}

fn remaining_io_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    let now = io_now();
    if now >= deadline {
        return Err(RendezvousError::Deadline);
    }
    Ok(deadline.saturating_duration_since(now).max(MIN_IO_TIMEOUT))
}

#[cfg(test)]
thread_local! {
    static IO_CLOCK_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn() -> Instant>>> =
        const { std::cell::RefCell::new(None) };
    static HOLDS_FOR_TEST: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Returns how many dialers on this thread were held due to an unanswered `trustd` check.
#[cfg(test)]
pub(crate) fn holds_for_test() -> usize {
    HOLDS_FOR_TEST.with(std::cell::Cell::get)
}

/// Installs or clears a custom clock for rendezvous timeout tests on the current thread.
#[cfg(test)]
pub(crate) fn set_io_clock_for_test(clock: Option<Box<dyn Fn() -> Instant>>) {
    IO_CLOCK_FOR_TEST.with(|slot| *slot.borrow_mut() = clock);
}

fn io_now() -> Instant {
    #[cfg(test)]
    if let Some(now) = IO_CLOCK_FOR_TEST.with(|slot| slot.borrow().as_ref().map(|clock| clock())) {
        return now;
    }
    Instant::now()
}

#[cfg(test)]
#[path = "handoff_rendezvous_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "handoff_rendezvous_claim_presentation_conformance_tests.rs"]
mod claim_presentation_conformance;

#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "ForeignUidListener",
    reason = "Environment: a listener of another uid at the published path; the Tier-1 bind \
              realizes it as the kernel's getpeereid answer."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "SameUidImpostor",
    reason = "Environment: a same-uid process other than the attested parent listening at the \
              published path; realized by a forked listener and by a foreign live pid."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "ParentGone",
    reason = "Environment: the attested parent exited and its pid may be recycled; realized \
              by an attestation whose birth witness no longer matches."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "PresentClaimUidOnly",
    reason = "The Buggy=1 mutant (the pre-2026-09-27 uid-only gate); no shipping code \
              implements it, and the negative control above catches it."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn rendezvous_claim_waivers() {}

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

pub(crate) const MAX_CHUNKED_SESSIONS: usize = crate::seamless::MAX_HANDOFF_SESSIONS;
const MAX_GRANT_FDS: usize = MAX_CHUNKED_SESSIONS + RENDEZVOUS_CHANNEL_FDS;
const MAX_GRANT_CHUNKS: usize = MAX_GRANT_FDS.div_ceil(fdpass::MAX_FDS);
const GRANT_HEADER_LEN: usize = GRANT_MAGIC.len() + 2;
const MAX_GRANT_BODY_BYTES: usize = 32 * 1024;

pub(crate) const RENDEZVOUS_CHANNEL_FDS: usize = 2;

pub(crate) const MAX_RENDEZVOUS_SESSIONS: usize = fdpass::MAX_FDS - RENDEZVOUS_CHANNEL_FDS;

const _: () = assert!(
    MAX_RENDEZVOUS_SESSIONS < 256,
    "the transport, not the protocol, is what bounds this lane"
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

fn grant_shape(chunks: bool, sessions: usize) -> Result<GrantShape, RendezvousError> {
    let limit = grant_session_limit(chunks);
    if sessions == 0 || sessions > limit {
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
    while left > fdpass::MAX_FDS {
        sizes.push(fdpass::MAX_FDS);
        left -= fdpass::MAX_FDS;
    }
    if left + RENDEZVOUS_CHANNEL_FDS <= fdpass::MAX_FDS {
        sizes.push(left + RENDEZVOUS_CHANNEL_FDS);
    } else {
        sizes.push(left);
        sizes.push(RENDEZVOUS_CHANNEL_FDS);
    }
    sizes
}

const CODE_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

const ACCEPT_POLL_SLICE: Duration = Duration::from_millis(10);

const CLAIM_FRAME_BUDGET: Duration = Duration::from_secs(2);

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
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
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
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        // SAFETY: `kill(pid, 0)` only probes process existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
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
    pub(crate) fn is_verdict(&self) -> bool {
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
    pub(crate) fn code_at(path: PathBuf) -> Self {
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

    fn in_line(&self) -> Duration {
        match &self.rule {
            DialerCodeRule::Stated(_) | DialerCodeRule::CodeAt(_) => {
                aterm_keeper::identity::PEER_CHECK_HANG
            }
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
        let listener =
            CtlListener::bind(&path).map_err(|error| RendezvousError::Bind(error.to_string()))?;
        let bound = Self {
            path,
            listener,
            claim,
        };
        #[cfg(unix)]
        prove_bound_socket_is_ours(dir, &bound.path).map_err(RendezvousError::Bind)?;
        std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        bound
            .listener
            .set_nonblocking(true)
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
            let now = Instant::now();
            if now >= deadline {
                return Err(RendezvousError::Deadline);
            }
            let slice = deadline
                .saturating_duration_since(now)
                .min(ACCEPT_POLL_SLICE);
            let mut pollfd = libc::pollfd {
                fd: self.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: single initialized `pollfd` owned by `self` with a bounded timeout.
            let polled = unsafe {
                libc::poll(
                    &mut pollfd,
                    1,
                    i32::try_from(slice.as_millis()).unwrap_or(0),
                )
            };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(RendezvousError::Accept(error.to_string()));
            }
            if polled == 0 {
                continue;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .map_err(|error| RendezvousError::Accept(error.to_string()))?;
                    return Ok(stream);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
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
    pub(crate) fn pid(&self) -> i32 {
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
    pub(crate) fn grant_session_limit(&self) -> usize {
        grant_session_limit(self.chunks)
    }

    /// Transfers the readiness pipe, commit pipe, and session PTY masters over `SCM_RIGHTS`.
    pub(crate) fn transfer(
        &self,
        nonce: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
        parked_at: Option<Instant>,
    ) -> Result<(), RendezvousError> {
        let shape = grant_shape(self.chunks, sessions.len())?;
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

fn parse_grant_body(body: &str, expected_nonce: &str) -> Result<GrantBody, RendezvousError> {
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
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|digits| digits.parse::<u64>().ok())
                .ok_or_else(|| RendezvousError::Grant(format!("malformed park stamp {park}")))?;
            (entries, Some(Duration::from_micros(micros)))
        }
    };
    if entries.is_empty() {
        // than a valid empty one.
        return Err(RendezvousError::Grant("no sessions".to_string()));
    }
    let mut sessions = Vec::new();
    for entry in entries.split(',') {
        let Some((local_id, pid)) = entry.split_once(':') else {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        };
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
    pub(crate) fn session_count(&self) -> usize {
        self.masters.len()
    }

    /// Elapsed duration the parent was parked before sending the grant, if reported.
    #[must_use]
    pub(crate) fn parked_before_grant(&self) -> Option<Duration> {
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

/// Claimed handoff descriptors and wire string staged for [`HandoffEnv`] installation.
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
    pub(crate) fn checks_in_first(self) -> bool {
        matches!(self, Self::ExitAfterCheckIn | Self::KillAfterCheckIn)
    }

    /// Returns whether this mode terminates via `SIGKILL` instead of `_exit`.
    #[must_use]
    pub(crate) fn kills(self) -> bool {
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
    let chunks = offers_chunked_grant(env.take(ENV_GRANT_CAPS).as_deref());
    let (path, secret) = rendezvous_env(env.take(ENV_RENDEZVOUS), env.take(ENV_CLAIM))?;
    let claimed = dial_and_claim(&path, &secret, expected_nonce, deadlines, parent, chunks)
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
    caps.and_then(std::ffi::OsStr::to_str)
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == GRANT_CAPS_CHUNKS))
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

fn dial_and_claim(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
    chunks: bool,
) -> Result<ClaimedHandoff, RendezvousError> {
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
    } = parse_grant_body(&body, expected_nonce)?;

    let expected = sessions.len() + RENDEZVOUS_CHANNEL_FDS;
    if let Some(count) = chunk_count {
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
        match fdpass::recv_with_fds(stream, buf, fdpass::MAX_FDS) {
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
mod tests {
    use super::*;
    use std::os::fd::{AsFd as _, FromRawFd as _};

    const TEST_NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn own_pid() -> i32 {
        i32::try_from(std::process::id()).expect("a pid fits pid_t")
    }

    fn own_parent() -> Option<crate::seamless::AttestedParent> {
        let parent = crate::seamless::AttestedParent::attest_live_for_test(own_pid());
        assert!(
            parent.is_some(),
            "this live process attests by its birth record"
        );
        parent
    }

    #[test]
    fn a_claim_frame_matches_only_its_own_secret() {
        let secret = "a".repeat(CLAIM_HEX_LEN);
        let frame = claim_frame(&secret);
        assert_eq!(frame.len(), CLAIM_FRAME_LEN);
        assert!(claim_frame_matches(&frame, &secret));

        let mut wrong = frame;
        let last = wrong.len() - 1;
        wrong[last] = b'b';
        assert!(
            !claim_frame_matches(&wrong, &secret),
            "one differing byte refuses"
        );

        let mut mismagicked = frame;
        mismagicked[0] = b'X';
        assert!(
            !claim_frame_matches(&mismagicked, &secret),
            "the frame magic is part of the claim"
        );

        assert!(
            !claim_frame_matches(&frame, &"a".repeat(CLAIM_HEX_LEN - 1)),
            "a shorter secret is not a prefix match"
        );
        assert!(!claim_frame_matches(&claim_frame("abc"), "abc"));
    }

    #[test]
    fn a_grant_body_round_trips_in_transfer_order() {
        let held = std::io::stdin();
        let borrowed = held.as_fd();
        let sessions = vec![(7u64, 4242i32, borrowed), (3u64, 99i32, borrowed)];
        let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("encodes");
        assert_eq!(body, format!("{TEST_NONCE}\n7:4242,3:99\n"));
        assert_eq!(
            parse_grant_body(&body, TEST_NONCE).expect("parses"),
            GrantBody {
                sessions: vec![(7, 4242), (3, 99)],
                parked_for: None,
            },
            "order is preserved, because order IS the addressing"
        );
    }

    #[test]
    fn a_grant_carries_how_long_before_it_the_parent_parked() {
        let held = std::io::stdin();
        let sessions = vec![(7u64, 4242i32, held.as_fd())];
        let parked = Duration::from_micros(1_500);
        let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked)).expect("encodes");
        assert_eq!(body, format!("{TEST_NONCE}\n7:4242\npark_us=1500\n"));
        assert_eq!(
            parse_grant_body(&body, TEST_NONCE),
            Ok(GrantBody {
                sessions: vec![(7, 4242)],
                parked_for: Some(parked),
            })
        );
        let older = format!("{TEST_NONCE}\n7:4242\n");
        assert_eq!(
            parse_grant_body(&older, TEST_NONCE).map(|body| body.parked_for),
            Ok(None),
            "an older parent's grant has no stamp"
        );
        for malformed in [
            "park_us=\n",
            "park_us=-1\n",
            "park_us=1.5\n",
            "park=1500\n",
            "\n",
        ] {
            let body = format!("{TEST_NONCE}\n7:4242\n{malformed}");
            assert!(parse_grant_body(&body, TEST_NONCE).is_err(), "{body:?}");
        }
        for parked in [
            Duration::ZERO,
            Duration::from_nanos(999),
            Duration::from_micros(u64::MAX),
            Duration::from_micros(u64::MAX) + Duration::from_nanos(999),
        ] {
            let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked))
                .expect("a stamp in the reader's domain encodes");
            let parsed = parse_grant_body(&body, TEST_NONCE).expect("the stamp parses");
            assert_eq!(parsed.sessions, vec![(7, 4242)]);
            assert_eq!(
                parsed.parked_for,
                Some(Duration::from_micros(
                    u64::try_from(parked.as_micros()).expect("the ordinary boundary fits"),
                )),
            );
        }
        let refused = [
            Duration::from_micros(u64::MAX) + Duration::from_micros(1),
            Duration::MAX,
        ]
        .map(|parked| encode_grant_body(TEST_NONCE, &sessions, Some(parked)).is_none());
        assert!(
            refused.into_iter().all(|refused| refused),
            "GRANT_PARK_STAMP_ENCODER_EXCEEDS_READER_DOMAIN",
        );
    }

    #[test]
    fn a_malformed_grant_body_is_refused_rather_than_partially_believed() {
        assert_eq!(
            parse_grant_body(&format!("{TEST_NONCE}\n1:2\n"), &"f".repeat(32)),
            Err(RendezvousError::NonceMismatch),
            "a grant for another attempt"
        );
        for body in [
            String::new(),
            TEST_NONCE.to_string(),
            format!("{TEST_NONCE}\n"),
            format!("{TEST_NONCE}\n1:2"),
            format!("{TEST_NONCE}\n1:2,\n"),
            format!("{TEST_NONCE}\nx:2\n"),
            format!("{TEST_NONCE}\n1:y\n"),
            format!("{TEST_NONCE}\n1:0\n"),
            format!("{TEST_NONCE}\n1:-4\n"),
            format!("{TEST_NONCE}\n1:2,1:3\n"),
        ] {
            assert!(
                parse_grant_body(&body, TEST_NONCE).is_err(),
                "{body:?} must not parse"
            );
        }
    }

    #[test]
    fn a_grant_header_frames_its_body_exactly() {
        let header = grant_header(11).expect("fits");
        assert_eq!(&header[..GRANT_MAGIC.len()], GRANT_MAGIC);
        assert_eq!(
            u16::from_be_bytes([header[GRANT_MAGIC.len()], header[GRANT_MAGIC.len() + 1]]),
            11
        );
        assert!(
            grant_header(MAX_GRANT_BODY_BYTES + 1).is_none(),
            "an oversized body is refused, never truncated"
        );
    }

    #[test]
    fn only_a_well_formed_nonce_can_be_granted() {
        let held = std::io::stdin();
        let sessions = vec![(1u64, 2i32, held.as_fd())];
        assert!(encode_grant_body("short", &sessions, None).is_none());
        assert!(encode_grant_body(&"z".repeat(32), &sessions, None).is_none());
        assert!(encode_grant_body(TEST_NONCE, &sessions, None).is_some());
    }

    #[test]
    fn the_device_term_distinguishes_masters() {
        let ptys = (0..3).map(|_| open_pty()).collect::<Vec<_>>();
        let terms = ptys
            .iter()
            .map(|(master, _)| pty_device_term(*master).expect("a master answers fstat"))
            .collect::<Vec<_>>();
        let mut distinct = terms.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            terms.len(),
            "three simultaneously-open masters must have three different device numbers, or \
             the term could not exclude a substitution"
        );
        for (master, slave) in ptys {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    #[test]
    fn an_expired_deadline_is_a_deadline_and_not_a_zero_timeout() {
        let expired = Instant::now() - Duration::from_secs(1);
        assert_eq!(remaining_io_budget(expired), Err(RendezvousError::Deadline));
        let live = Instant::now() + Duration::from_secs(5);
        assert!(remaining_io_budget(live).expect("live budget") >= MIN_IO_TIMEOUT);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_code_check_judges_the_token_read_at_the_claim_and_only_for_its_pid() {
        let (a, _b) = CtlStream::pair().expect("pair");
        let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
        let code = DialerCode::this_process();
        let judged = |attested: i32, token: aterm_uds::peer::AuditToken| match admit_code(
            &a,
            attested,
            Some(token),
            &code,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
            &|_| {},
        ) {
            Ok(()) => Ok(()),
            Err(Gated::Refused(RendezvousError::PeerCode { refusal, .. })) => Err(refusal),
            Err(Gated::Refused(other)) => panic!("refused for no code check: {other}"),
            Err(Gated::WaitEnded { held, .. }) => trustd_silent(held),
        };
        if let Err(refusal) = judged(own_pid(), token) {
            panic!("the token names the attested dialer, which runs this code: {refusal:?}")
        }
        let elsewhere = judged(own_pid() + 1, token);
        assert!(
            matches!(&elsewhere, Err(CodeRefusal::Unchecked(why)) if why.contains("not the attested dialer")),
            "a token naming another pid was checked for the attested dialer: {elsewhere:?}"
        );
        let mut later = token;
        later.0[7] = later.0[7].wrapping_add(1);
        let stale = judged(own_pid(), later);
        assert!(
            matches!(stale, Err(CodeRefusal::Unchecked(_))),
            "a token for another image at the pid was judged as the live one: {stale:?}"
        );
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_check_answered_after_its_in_line_wait_is_waited_for_and_never_asked_again() {
        use aterm_keeper::identity::{CheckError, PeerCheck};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex, PoisonError, mpsc};

        const IN_LINE: Duration = Duration::from_millis(50);

        if !crate::test_alone::alone() {
            return;
        }
        let answer_at =
            2 * usize::try_from(CODE_RECHECK_INTERVAL.as_nanos() / ACCEPT_POLL_SLICE.as_nanos())
                .expect("a slice count");
        let through_the_hold = |cut: bool| {
            let (a, _b) = CtlStream::pair().expect("pair");
            let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
            let (trustd_answers, unanswered) = mpsc::channel::<()>();
            let unanswered = Arc::new(Mutex::new(unanswered));
            let asked = Arc::new(AtomicUsize::new(0));
            let answered = Arc::new(AtomicUsize::new(0));
            let code = DialerCode::scripted(IN_LINE, {
                let (asked, answered) = (Arc::clone(&asked), Arc::clone(&answered));
                move || {
                    let again = asked.fetch_add(1, Ordering::SeqCst) > 0;
                    assert!(
                        cut || !again,
                        "asked again while the first check was still waited for"
                    );
                    let (unanswered, answered) = (Arc::clone(&unanswered), Arc::clone(&answered));
                    let check = PeerCheck::start(IN_LINE, move || {
                        let heard = unanswered
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .recv();
                        if heard.is_ok() {
                            answered.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    });
                    if !cut {
                        return check;
                    }
                    let answer = check.wait(IN_LINE).unwrap_or_else(|abandoned| {
                        Err(CheckError::Unanswered(abandoned.unanswered()))
                    });
                    PeerCheck::start(IN_LINE, move || answer)
                }
            });
            let slices = std::cell::Cell::new(0usize);
            let asked_when_heard = std::cell::Cell::new(None::<usize>);
            let abort = || {
                let slice = slices.get() + 1;
                slices.set(slice);
                if slice == answer_at {
                    trustd_answers
                        .send(())
                        .expect("the first check waits for trustd");
                }
                if asked_when_heard.get().is_none() && answered.load(Ordering::SeqCst) > 0 {
                    asked_when_heard.set(Some(asked.load(Ordering::SeqCst)));
                }
                cut && asked_when_heard
                    .get()
                    .is_some_and(|then| asked.load(Ordering::SeqCst) > then)
            };
            let turned_away = std::cell::RefCell::new(Vec::new());
            let end = admit_code(
                &a,
                own_pid(),
                Some(token),
                &code,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &abort,
                &|dialer| turned_away.borrow_mut().push(dialer.clone()),
            );
            (
                end,
                asked.load(Ordering::SeqCst),
                answered.load(Ordering::SeqCst),
                slices.get(),
                turned_away.into_inner(),
            )
        };

        let (end, asked, answered, slices, turned_away) = through_the_hold(false);
        assert!(
            matches!(end, Ok(())),
            "a check trustd answered within the wait did not admit its dialer: {end:?}"
        );
        assert_eq!(asked, 1, "the check the gate let through was asked again");
        assert_eq!(answered, 1, "admitted on trustd's answer to that check");
        assert!(
            slices >= answer_at,
            "admitted only once trustd answered, {answer_at} held slices in: {slices}"
        );
        assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

        let (end, asked, answered, _, turned_away) = through_the_hold(true);
        assert!(
            matches!(
                end,
                Err(Gated::WaitEnded {
                    end: RendezvousError::Cancelled,
                    ..
                })
            ),
            "the shape that asks again admitted its dialer: {end:?}"
        );
        assert!(asked > 1, "the old shape asks again: {asked}");
        assert_eq!(
            answered, 1,
            "trustd answered the first check, and nobody heard it"
        );
        assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the trustd gate's helpers return",
            || (aterm_uds::deadline::TRUST.running() == 0).then_some(()),
        );
    }

    fn dial_and_count(
        path: PathBuf,
        claim: String,
        chunks: bool,
    ) -> std::thread::JoinHandle<usize> {
        std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial the rendezvous");
            (&stream)
                .write_all(&claim_frame_with(&claim, chunks))
                .expect("present the claim");
            let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
            let mut received = 0;
            let mut buf = [0u8; 4096];
            while let Ok(got) =
                aterm_uds::fdpass::recv_with_fds(&stream, &mut buf, aterm_uds::fdpass::MAX_FDS)
            {
                if got.bytes == 0 && got.fds.is_empty() {
                    break;
                }
                received += got.fds.len();
            }
            received
        })
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_grant_is_not_sent_when_the_dialers_token_is_not_the_one_checked() {
        let sessions = 100;
        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(sessions);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let dialer = dial_and_count(
            rendezvous.path().to_path_buf(),
            rendezvous.claim().to_string(),
            true,
        );
        let mut peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the dialer presents the claim and is this very process",
        );
        assert!(peer.chunks, "the dialer claimed the chunked grant");
        let mut later = peer.token.expect("the gate read the dialer's token");
        assert_eq!(
            later.pid(),
            std::process::id(),
            "the token the gate kept names the attested dialer"
        );
        later.0[7] = later.0[7].wrapping_add(1);
        peer.token = Some(later);
        let granted = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        assert_eq!(
            grant_shape(true, sessions),
            Ok(GrantShape::Chunked),
            "PRECONDITION: {sessions} sessions take the chunked grant"
        );
        let refused = peer.transfer(
            TEST_NONCE,
            &granted,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        );
        assert!(
            matches!(&refused, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
            "a chunked grant left for a token other than the one checked: {refused:?}"
        );
        drop((peer, rendezvous));
        assert_eq!(
            dialer.join().expect("dialer thread"),
            0,
            "the dialer received a descriptor the gate refused to send"
        );
        drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
    }

    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_PATH: &str = "ATERM_TEST_EXEC_DIALER_RENDEZVOUS";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_CLAIM: &str = "ATERM_TEST_EXEC_DIALER_CLAIM";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_GO: &str = "ATERM_TEST_EXEC_DIALER_GO";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_EXECD: &str = "ATERM_TEST_EXEC_DIALER_EXECD";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_STAYS: &str = "ATERM_TEST_EXEC_DIALER_STAYS";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_FD: &str = "ATERM_TEST_EXEC_DIALER_FD";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_RECEIVED: &str = "EXEC DIALER RECEIVED";

    #[cfg(target_vendor = "apple")]
    fn count_until_closed(stream: &CtlStream) -> usize {
        let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
        let mut received = 0;
        let mut buf = [0u8; 4096];
        while let Ok(got) =
            aterm_uds::fdpass::recv_with_fds(stream, &mut buf, aterm_uds::fdpass::MAX_FDS)
        {
            if got.bytes == 0 && got.fds.is_empty() {
                break;
            }
            received += got.fds.len();
        }
        received
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn exec_dialer_stand_in() {
        use std::os::unix::process::CommandExt as _;
        if std::env::var_os(EXEC_DIALER_FD).is_some() {
            let stream = CtlStream::from(crate::test_alone::handed(EXEC_DIALER_FD));
            let execd = std::env::var_os(EXEC_DIALER_EXECD).expect("the marker's path");
            std::fs::write(execd, b"").expect("say the new image runs");
            println!(
                "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
                count_until_closed(&stream)
            );
            return;
        }
        let (Some(path), Ok(claim), Some(go)) = (
            std::env::var_os(EXEC_DIALER_PATH),
            std::env::var(EXEC_DIALER_CLAIM),
            std::env::var_os(EXEC_DIALER_GO),
        ) else {
            return;
        };
        let stream = dial_for_test(Path::new(&path), &claim).expect("dial and claim");
        let go = PathBuf::from(go);
        let told = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || go.exists().then_some(()),
        );
        assert!(told.is_some(), "the parent never said go");
        if std::env::var_os(EXEC_DIALER_STAYS).is_some() {
            println!(
                "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
                count_until_closed(&stream)
            );
            return;
        }
        let fd = stream.into_raw_fd();
        let mut image = std::process::Command::new(std::env::current_exe().expect("this binary"));
        image.args(std::env::args_os().skip(1));
        crate::test_alone::hand(&mut image, EXEC_DIALER_FD, fd);
        let error = image.exec();
        panic!("exec the new image: {error}");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_grant_is_not_sent_once_the_dialers_image_changed() {
        for stays in [true, false] {
            let rendezvous = bind_for_test();
            let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
            let (go, execd) = (scratch.path().join("go"), scratch.path().join("execd"));
            let mut stand_in =
                crate::test_alone::StandIn::helper(module_path!(), "exec_dialer_stand_in");
            stand_in
                .command()
                .env(EXEC_DIALER_PATH, rendezvous.path())
                .env(EXEC_DIALER_CLAIM, rendezvous.claim())
                .env(EXEC_DIALER_GO, &go)
                .env(EXEC_DIALER_EXECD, &execd);
            if stays {
                stand_in.command().env(EXEC_DIALER_STAYS, "1");
            }
            let mut stand_ins = [stand_in.spawn()];
            let pid = i32::try_from(stand_ins[0].id()).expect("a pid fits pid_t");
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(pid),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                    &|| false,
                ),
                "the stand-in runs this binary's code and is the attested dialer",
            );
            assert_eq!(peer.pid(), pid, "LOCAL_PEERPID names the stand-in");
            std::fs::write(&go, b"").expect("say go");
            if !stays {
                crate::test_alone::until_or_ended(
                    "the stand-in's new image runs",
                    &mut stand_ins,
                    || execd.exists(),
                );
            }
            let (reads, writes) = pipes_for_test(1);
            let (ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, commit_wr) = pipe_for_test();
            let granted = peer.transfer(
                TEST_NONCE,
                &[(11, 4242, writes[0].as_fd())],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            );
            if stays {
                granted.expect("the dialer the gate checked, unchanged, is granted");
            } else {
                assert!(
                    matches!(&granted, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
                    "a grant left for an image the gate never checked: {granted:?}"
                );
            }
            drop((peer, rendezvous));
            let [stand_in] = stand_ins;
            let ran = stand_in.passed();
            let received = ran
                .stdout
                .lines()
                .find_map(|line| line.split(EXEC_DIALER_RECEIVED).nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or_else(|| {
                    panic!("the stand-in never said what it received:\n{}", ran.stdout)
                });
            assert_eq!(
                received,
                if stays { 1 + RENDEZVOUS_CHANNEL_FDS } else { 0 },
                "stays={stays}: what reached the dialer"
            );
            drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
        }
    }

    #[test]
    fn a_grant_whose_connection_closes_at_once_is_still_claimed() {
        for _ in 0..20 {
            let rendezvous = bind_for_test();
            let (master, slave) = open_pty();
            let (ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, commit_wr) = pipe_for_test();
            let claim = rendezvous.claim().to_string();
            let path = rendezvous.path().to_path_buf();
            let dialer = std::thread::spawn(move || {
                dial_and_claim(
                    &path,
                    &claim,
                    TEST_NONCE,
                    deadlines_in(120),
                    own_parent(),
                    false,
                )
            });
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(own_pid()),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                    &|| false,
                ),
                "the dialer presents the claim",
            );
            // SAFETY: `master` remains open for the duration of `transfer`.
            let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
            peer.transfer(
                TEST_NONCE,
                &[(11, 4242, borrowed)],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            )
            .expect("transfer");
            drop(peer);
            drop(rendezvous);
            let claimed = dialer
                .join()
                .expect("dialer thread")
                .expect("a grant already queued is claimed though the parent hung up");
            assert_eq!(claimed.session_count(), 1);
            let received = claimed.masters[0].as_raw_fd();
            assert_eq!(
                claimed.fds_wire,
                format!("11={received}:4242"),
                "the wire names the descriptor at the number THIS process received it at"
            );
            assert_ne!(received, master, "SCM_RIGHTS installs a different number");
            assert_eq!(
                pty_device_term(received),
                pty_device_term(master),
                "the device term survives the transfer"
            );
            drop(claimed);
            drop((ready_rd, ready_wr, commit_rd, commit_wr));
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    #[test]
    fn a_successor_process_claims_from_the_real_outgoing_listener() {
        const CHILD: &str = "RZ_CROSS_PROCESS_SUCCESSOR";
        if let Some(spec) = std::env::var_os(CHILD) {
            let spec = spec.into_string().expect("the child spec is UTF-8");
            let mut fields = spec.split('\n');
            let mut field = || fields.next().expect("a complete child spec");
            let (path, claim) = (PathBuf::from(field()), field().to_string());
            let parent_pid: libc::pid_t = field().parse().expect("the parent pid");
            let term: i32 = field().parse().expect("the master's device term");
            let parent = crate::seamless::AttestedParent::attest_live_for_test(parent_pid);
            assert!(
                parent.is_some(),
                "the outgoing process attests by its birth record"
            );
            let claimed =
                dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(120), parent, false)
                    .expect("the successor's identity gate admits the real outgoing listener");
            assert_eq!(claimed.session_count(), 1);
            assert_eq!(
                pty_device_term(claimed.masters[0].as_raw_fd()),
                Some(term),
                "the granted master is the outgoing process's PTY"
            );
            return;
        }

        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let term = pty_device_term(master).expect("a master answers fstat");
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let spec = format!(
            "{}\n{}\n{}\n{term}",
            rendezvous.path().display(),
            rendezvous.claim(),
            own_pid()
        );
        let mut child = crate::test_alone::StandIn::this_test();
        child.command().env(CHILD, spec);
        let child = std::cell::RefCell::new(child.spawn());
        let successor = i32::try_from(child.borrow().id()).expect("a pid fits pid_t");

        let turned_away = std::cell::RefCell::new(Vec::new());
        let claimed = rendezvous.accept_claim_observed(
            Some(successor),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| child.borrow_mut().exited(),
            &|dialer| turned_away.borrow_mut().push(dialer.clone()),
        );
        let peer = match claimed {
            Ok(peer) => peer,
            Err(error) => {
                let turned_away = turned_away.into_inner();
                if held_for_trustd(&error)
                    || turned_away
                        .iter()
                        .any(|dialer| matches!(dialer.refusal, CodeRefusal::TrustdSilent(_)))
                {
                    trustd_silent(format_args!("{error}: {turned_away:?}"));
                }
                child.into_inner().passed();
                panic!("the successor process passed without presenting its claim: {error}");
            }
        };
        assert_eq!(
            peer.pid(),
            successor,
            "LOCAL_PEERPID on accept names the successor"
        );
        // SAFETY: `master` remains open for the duration of `transfer`.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect("transfer");

        child.into_inner().passed();
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    fn a_transfer_failure_says_whether_the_descriptors_left() {
        let rendezvous = bind_for_test();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let (fake_master_rd, _fake_master_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();

        let (close_tx, close_rx) = std::sync::mpsc::channel::<()>();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial");
            (&stream)
                .write_all(&claim_frame(&claim))
                .and_then(|()| (&stream).flush())
                .expect("claim");
            close_rx.recv().expect("told to close");
            drop(stream);
            closed_tx.send(()).expect("signal");
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the claim was presented",
        );
        close_tx.send(()).expect("tell the dialer to close");
        closed_rx.recv().expect("the dialer closed");
        dialer.join().expect("dialer thread");
        wait_for_hangup(|| peer.poll_hangup());
        let borrowed = fake_master_rd.as_fd();
        let error = peer
            .transfer(
                TEST_NONCE,
                &[(11, 4242, borrowed)],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            )
            .expect_err("a closed peer cannot take the descriptors");
        assert!(
            matches!(error, RendezvousError::TransferNotSent(_)),
            "the sendmsg itself failed, so no descriptor left: {error}"
        );
        drop(peer);
        drop(rendezvous);

        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial");
            (&stream)
                .write_all(&claim_frame(&claim))
                .and_then(|()| (&stream).flush())
                .expect("claim");
            let mut header = [0u8; GRANT_HEADER_LEN];
            let received = fdpass::recv_with_fds(&stream, &mut header, fdpass::MAX_FDS)
                .expect("the header and its descriptors");
            let count = received.fds.len();
            drop(received);
            drop(stream);
            count
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "claimed",
        );
        let outcome = peer.transfer(
            TEST_NONCE,
            &[(11, 4242, fake_master_rd.as_fd())],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        );
        let received = dialer.join().expect("dialer thread");
        assert_eq!(received, 3, "the master and both pipes reached the peer");
        match outcome {
            Ok(()) => {}
            Err(RendezvousError::TransferPartial(_)) => {}
            Err(other) => panic!(
                "after a delivered header the failure can never claim the descriptors \
                 stayed home: {other}"
            ),
        }
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
    }

    #[test]
    fn a_dialer_without_the_secret_is_refused() {
        let rendezvous = bind_for_test();
        let stream = CtlStream::connect(rendezvous.path()).expect("connect");
        (&stream)
            .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
            .and_then(|()| (&stream).flush())
            .expect("the dialer presents a wrong secret");
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_millis(400),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        drop(stream);
    }

    #[test]
    fn a_wrong_dialer_does_not_end_the_attempt_and_the_successor_still_claims() {
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();

        let bogus = CtlStream::connect(&path).expect("the wrong dialer connects");
        (&bogus)
            .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
            .and_then(|()| (&bogus).flush())
            .expect("the wrong dialer presents a wrong secret");
        bogus
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the wrong dialer's own read");

        let successor = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the wrong dialer is refused and the wait resumes for the real successor",
        );
        // SAFETY: `master` remains open for the duration of `transfer`.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        let parked_at = Instant::now().checked_sub(Duration::from_millis(5));
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            parked_at,
        )
        .expect("transfer");
        let claimed = successor
            .join()
            .expect("dialer thread")
            .expect("the successor behind a wrong dialer still claims its handoff");
        assert_eq!(claimed.session_count(), 1);
        assert!(
            parked_at.is_none()
                || claimed
                    .parked_before_grant()
                    .is_some_and(|parked| parked >= Duration::from_millis(5)),
            "the park stamp crossed the real socket: {:?}",
            claimed.parked_before_grant()
        );

        let mut sink = [0u8; 1];
        assert_eq!(
            (&bogus).read(&mut sink).ok(),
            Some(0),
            "and the refused connection was CLOSED, which is how the wrong dialer learns it lost"
        );

        drop(claimed);
        drop((bogus, ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    fn a_flood_of_wrong_dialers_is_bounded_rather_than_served_forever() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        let wrong = claim_frame(&"f".repeat(CLAIM_HEX_LEN));
        let flood = (0..MAX_REFUSED_DIALS)
            .map(|_| {
                let stream = CtlStream::connect(&path).expect("connect");
                (&stream)
                    .write_all(&wrong)
                    .and_then(|()| (&stream).flush())
                    .expect("present a wrong claim");
                stream
            })
            .collect::<Vec<_>>();

        let started = Instant::now();
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(120),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(60),
            "the cap ended the attempt; nothing here waited out the deadline: {elapsed:?}"
        );
        drop(flood);
    }

    #[test]
    fn a_dribbling_dialer_is_refused_at_the_deadline_rather_than_stretching_it() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();
        let code = crate::handoff_rendezvous::DialerCode::this_process();
        let stream = CtlStream::connect(&path).expect("connect");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dribbler = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut wrote = 0usize;
                for byte in claim_frame(&claim) {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if (&stream).write_all(&[byte]).is_err() {
                        break;
                    }
                    wrote += 1;
                    let _ = (&stream).flush();
                    std::thread::sleep(Duration::from_millis(10));
                }
                wrote
            })
        };

        let budget = Duration::from_millis(300);
        let refused =
            rendezvous.accept_claim(Some(own_pid()), &code, Instant::now() + budget, &|| false);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);

        assert!(
            refused.is_err(),
            "a frame that does not arrive inside the deadline is refused, not awaited"
        );
        let wrote = dribbler.join().expect("dribbler thread");
        assert!(
            wrote < CLAIM_FRAME_LEN,
            "the frame read waited past the attempt's deadline for the whole \
             {CLAIM_FRAME_LEN}-byte frame"
        );
    }

    #[test]
    fn half_a_rendezvous_environment_says_so_instead_of_claiming_to_be_neither() {
        let some = |value: &str| Some(std::ffi::OsString::from(value));
        let secret = "a".repeat(CLAIM_HEX_LEN);

        assert_eq!(
            rendezvous_env(some("/tmp/aterm/seamless.sock"), None).err(),
            Some(RendezvousError::HalfRendezvous {
                present: ENV_RENDEZVOUS,
                missing: ENV_CLAIM,
            })
        );
        assert_eq!(
            rendezvous_env(None, some(&secret)).err(),
            Some(RendezvousError::HalfRendezvous {
                present: ENV_CLAIM,
                missing: ENV_RENDEZVOUS,
            })
        );
        assert_eq!(
            rendezvous_env(None, None).err(),
            Some(RendezvousError::NoRendezvous),
            "and an empty environment is still plain absence"
        );
        assert_eq!(
            rendezvous_env(some("/tmp/aterm/seamless.sock"), some(&secret)).expect("both halves"),
            (PathBuf::from("/tmp/aterm/seamless.sock"), secret),
        );
    }

    #[test]
    fn a_rendezvous_nobody_dials_expires_and_unlinks() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        assert!(path.exists(), "the listener is on disk while it is bound");
        let expired = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_millis(50),
            &|| false,
        );
        assert_eq!(expired.err(), Some(RendezvousError::Deadline));
        drop(rendezvous);
        assert!(
            !path.exists(),
            "a dropped rendezvous unlinks, so a late dial fails ENOENT rather than hanging"
        );
        assert!(
            CtlStream::connect(&path).is_err(),
            "and a late dialer cannot connect to what is no longer there"
        );
    }

    #[test]
    fn an_aborted_wait_returns_promptly() {
        let rendezvous = bind_for_test();
        let started = Instant::now();
        let cancelled = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(120),
            &|| true,
        );
        assert_eq!(cancelled.err(), Some(RendezvousError::Cancelled));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(60),
            "the abort predicate is checked between poll slices, not after the deadline: \
             {elapsed:?}"
        );
    }

    fn deadlines_in(secs: u64) -> ClaimDeadlines {
        let at = Instant::now() + Duration::from_secs(secs);
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    #[test]
    fn a_held_claim_is_granted_after_the_hold_under_the_grant_budget() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dial = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let deadlines = ClaimDeadlines {
            dial,
            grant: dial + 2 * aterm_spec::wait::MUST_HAPPEN,
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicBool::new(false));
        let dialer = {
            let (reads, ended) = (Arc::clone(&reads), Arc::clone(&ended));
            std::thread::spawn(move || {
                set_io_clock_for_test(Some(Box::new(move || {
                    if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                        Instant::now()
                    } else {
                        dial + MIN_IO_TIMEOUT
                    }
                })));
                let claimed =
                    dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false);
                ended.store(true, Ordering::SeqCst);
                claimed
            })
        };
        let accepted = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| ended.load(Ordering::SeqCst),
        );
        if accepted.is_err() && ended.load(Ordering::SeqCst) {
            let gave_up = dialer.join().expect("dialer thread").err();
            panic!("the dialer ended before its claim was accepted: {gave_up:?}");
        }
        let peer = admitted(accepted, "claimed");
        assert!(!peer.poll_hangup(), "a held, idle dialer is not a hangup");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the dialer's grant wait judged its budget past the dial",
            || (reads.load(Ordering::SeqCst) >= 2).then_some(()),
        );
        assert!(
            !peer.poll_hangup(),
            "…nor once the hold outlasted its dial budget"
        );
        // SAFETY: `master` remains open for the duration of `transfer`.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect("transfer after the hold");
        let claimed = dialer
            .join()
            .expect("dialer thread")
            .expect("claimed after the hold");
        assert_eq!(claimed.session_count(), 1);
        drop(claimed);
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    #[test]
    fn an_accept_short_of_a_descriptor_ends_the_wait_because_the_kernel_dropped_the_dial() {
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let parent = own_parent();
        let code = crate::handoff_rendezvous::DialerCode::this_process();
        let mut filler = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => filler.push(file),
                Err(error) if error.raw_os_error() == Some(libc::EMFILE) => break,
                Err(error) => panic!("filling the table: {error}"),
            }
        }
        drop(filler.pop());
        let dialer = std::thread::spawn(move || {
            let started = Instant::now();
            let outcome =
                dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(180), parent, false);
            (outcome, started.elapsed())
        });
        let queued_by = Instant::now() + Duration::from_secs(60);
        loop {
            let mut pollfd = libc::pollfd {
                fd: rendezvous.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: single initialized `pollfd` for the open listener.
            if unsafe { libc::poll(&mut pollfd, 1, 50) } > 0 {
                break;
            }
            assert!(Instant::now() < queued_by, "the dial never queued");
        }

        let started = Instant::now();
        let starved = rendezvous.accept_claim(
            Some(own_pid()),
            &code,
            Instant::now() + Duration::from_secs(180),
            &|| false,
        );
        match starved {
            Err(RendezvousError::Accept(detail)) => assert!(
                detail.contains(&std::io::Error::from_raw_os_error(libc::EMFILE).to_string()),
                "the attempt says what ended it: {detail}"
            ),
            Err(other) => panic!("expected the accept's own error, got {other}"),
            Ok(_) => panic!("PRECONDITION: the table was full"),
        }
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "ended at once, not at the deadline: {:?}",
            started.elapsed()
        );
        let (outcome, elapsed) = dialer.join().expect("dialer thread");
        match outcome {
            Err(RendezvousError::Grant(reason)) => {
                assert_eq!(reason, "the parent closed the rendezvous");
            }
            Err(RendezvousError::Dial(_)) => {}
            Err(other) => panic!("expected the dropped dial's end, got {other}"),
            Ok(_) => panic!("expected the dropped dial's end, got a grant"),
        }
        assert!(
            elapsed < aterm_spec::wait::MUST_HAPPEN,
            "the dialer is told at once, not at its grant budget: {elapsed:?}"
        );
        drop(filler);
        drop(rendezvous);
    }

    #[test]
    fn a_grant_that_never_arrives_ends_as_the_claim_deadline() {
        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let deadlines = ClaimDeadlines {
            dial: Instant::now() + Duration::from_secs(60),
            grant: Instant::now() + Duration::from_millis(300),
        };
        let dialer = std::thread::spawn(move || {
            dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false)
        });
        let held = rendezvous
            .accept_one(Instant::now() + Duration::from_secs(60), &|| false)
            .expect("accepted");
        let outcome = dialer.join().expect("dialer thread");
        assert!(
            Instant::now() >= deadlines.grant,
            "the wait lasted the grant budget"
        );
        match outcome {
            Err(RendezvousError::Deadline) => {}
            Err(other) => panic!("expected the grant budget's deadline, got {other}"),
            Ok(_) => panic!("expected the grant budget's deadline, got a grant"),
        }
        assert_eq!(
            claim_side(RendezvousError::Deadline),
            RendezvousError::ClaimDeadline,
            "which the claim says as this successor's own deadline"
        );
        drop(held);
    }

    fn wait_for_hangup(hung_up: impl Fn() -> bool) {
        let seen = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || hung_up().then_some(()),
        );
        assert!(seen.is_some(), "the closed dialer never read as a hangup");
    }

    #[cfg(unix)]
    #[test]
    fn the_rendezvous_base_is_admitted_only_private_absolute_and_off_tmp() {
        use super::{BaseFacts, admits_temp_base};
        use std::path::Path;
        let uid = 501;
        let private = BaseFacts {
            is_dir: true,
            uid,
            mode: 0o40700,
        };
        let base = Path::new("/var/folders/xy/T");
        assert!(admits_temp_base(base, Some(private), uid), "a private base");
        let readable = BaseFacts {
            mode: 0o40755,
            ..private
        };
        assert!(
            admits_temp_base(base, Some(readable), uid),
            "others may read it, as long as they cannot write it"
        );
        for (why, base, facts) in [
            ("relative", Path::new("tmp"), Some(private)),
            ("/tmp itself", Path::new("/tmp"), Some(private)),
            ("under /tmp", Path::new("/tmp/x"), Some(private)),
            (
                "under /private/tmp",
                Path::new("/private/tmp/x"),
                Some(private),
            ),
            ("unreadable", base, None),
            (
                "a symlink or a file",
                base,
                Some(BaseFacts {
                    is_dir: false,
                    ..private
                }),
            ),
            (
                "another uid's",
                base,
                Some(BaseFacts {
                    uid: uid + 1,
                    ..private
                }),
            ),
            (
                "group-writable",
                base,
                Some(BaseFacts {
                    mode: 0o40770,
                    ..private
                }),
            ),
            (
                "other-writable",
                base,
                Some(BaseFacts {
                    mode: 0o40707,
                    ..private
                }),
            ),
        ] {
            assert!(!admits_temp_base(base, facts, uid), "{why} is admitted");
        }
    }

    fn bind_for_test() -> Rendezvous {
        let nonce = aterm_uds::rand::hex_token::<16>()
            .unwrap_or_else(|error| panic!("a rendezvous test cannot mint its nonce: {error}"));
        Rendezvous::bind(&nonce).unwrap_or_else(|error| panic!("bind a rendezvous: {error}"))
    }

    fn open_pty() -> (i32, i32) {
        aterm_pty::open_pty_pair().expect("open_pty_pair")
    }

    fn pipe_for_test() -> (OwnedFd, OwnedFd) {
        let mut raw = [0i32; 2];
        // SAFETY: `pipe` initializes two file descriptors in `raw`.
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: `raw` holds two freshly created pipe descriptors owned exclusively here.
        unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
    }

    fn pipes_for_test(count: usize) -> (Vec<OwnedFd>, Vec<OwnedFd>) {
        (0..count).map(|_| pipe_for_test()).unzip()
    }

    fn inode(fd: i32) -> (libc::dev_t, libc::ino_t) {
        let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes one `struct stat` into `info`.
        assert_eq!(unsafe { libc::fstat(fd, info.as_mut_ptr()) }, 0, "fstat");
        // SAFETY: `fstat` returned 0, so `info` is initialized.
        let info = unsafe { info.assume_init() };
        (info.st_dev, info.st_ino)
    }

    fn assert_every_write_end_closed(reads: &[OwnedFd], what: &str) {
        for read in reads {
            // SAFETY: `F_SETFL` on a valid descriptor owned by this test.
            unsafe {
                libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        for (index, read) in reads.iter().enumerate() {
            loop {
                let mut byte = [0u8; 1];
                // SAFETY: 1-byte read into a stack buffer from a valid descriptor.
                let got = unsafe {
                    libc::read(
                        read.as_raw_fd(),
                        byte.as_mut_ptr().cast::<libc::c_void>(),
                        1,
                    )
                };
                if got == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what}: descriptor {index} is still open somewhere in this process"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn body_for(sessions: usize) -> String {
        let entries = (0..sessions)
            .map(|id| format!("{id}:{}", 5000 + id))
            .collect::<Vec<_>>()
            .join(",");
        format!("{TEST_NONCE}\n{entries}\n")
    }

    #[test]
    fn the_chunk_layout_places_every_descriptor_and_the_pipes_last() {
        for sessions in 1..=MAX_CHUNKED_SESSIONS {
            let sizes = grant_chunk_sizes(sessions);
            assert_eq!(
                sizes.iter().sum::<usize>(),
                sessions + RENDEZVOUS_CHANNEL_FDS,
                "{sessions}: every descriptor, once"
            );
            assert!(
                sizes
                    .iter()
                    .all(|size| (1..=fdpass::MAX_FDS).contains(size)),
                "{sessions}: {sizes:?} fits the message ceiling"
            );
            assert!(
                *sizes.last().expect("a message") >= RENDEZVOUS_CHANNEL_FDS,
                "{sessions}: the readiness and Commit ends travel last"
            );
            assert!(sizes.len() <= MAX_GRANT_CHUNKS, "{sessions}: {sizes:?}");
            assert_eq!(
                sizes.len() == 1,
                sessions <= MAX_RENDEZVOUS_SESSIONS,
                "{sessions}: one message exactly when one message carries it"
            );
        }
        assert_eq!(
            grant_chunk_sizes(MAX_CHUNKED_SESSIONS).len(),
            MAX_GRANT_CHUNKS,
            "the protocol ceiling spans the most messages the header allows"
        );
    }

    #[test]
    fn both_claim_magics_carry_the_secret_and_say_which_grant() {
        let secret = "a".repeat(CLAIM_HEX_LEN);
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, false), &secret),
            Some(false)
        );
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, true), &secret),
            Some(true)
        );
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, true), &"b".repeat(CLAIM_HEX_LEN)),
            None,
            "the chunked claim still needs this attempt's secret"
        );
        let mut unknown = claim_frame_with(&secret, true);
        unknown[4] = b'9';
        assert_eq!(claim_frame_kind(&unknown, &secret), None);
        assert_eq!(&claim_frame(&secret)[..6], b"ATRZ1C", "the default claim");
    }

    #[test]
    fn only_the_parents_offer_makes_a_chunked_claim() {
        let offered = |caps: &str| offers_chunked_grant(Some(std::ffi::OsStr::new(caps)));
        assert!(offered("chunks1"));
        assert!(offered("later2, chunks1"));
        assert!(!offered(""));
        assert!(!offered("chunks2"));
        assert!(!offered("chunks10"));
        assert!(!offers_chunked_grant(None));
    }

    #[test]
    fn a_successor_raises_its_descriptor_limit_before_claiming() {
        use crate::descriptor_limit::{raise_descriptor_limit_for_claim, raised_soft_limit};
        assert_eq!(raised_soft_limit(256, u64::MAX), Some(4096));
        assert_eq!(raised_soft_limit(256, 1024), Some(1024));
        assert_eq!(raised_soft_limit(10_240, u64::MAX), None, "never lowered");
        assert_eq!(raised_soft_limit(1024, 1024), None, "at the hard limit");
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let raised = raise_descriptor_limit_for_claim().expect("the raise is permitted");
        let (from, to) = raised.expect("a 64-spare limit is below the floor");
        assert!(from < to, "{from} -> {to}");
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource constant and writable `rlimit` out-pointer.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        assert_eq!(limit.rlim_cur, to);
        a_grant_of_100_sessions_crosses_in_chunks();
        crate::descriptor_limit::tests::a_shell_spawned_after_the_raise_gets_the_launchers_limit(
            from, to,
        );
    }

    #[test]
    fn a_grant_past_its_deadline_says_the_successor_dialed_and_sends_nothing() {
        for (sessions, chunks) in [(3_usize, false), (100, true)] {
            let rendezvous = bind_for_test();
            let (_reads, writes) = pipes_for_test(sessions);
            let (_ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, _commit_wr) = pipe_for_test();
            let claim = rendezvous.claim().to_string();
            let path = rendezvous.path().to_path_buf();
            let dialer = std::thread::spawn(move || {
                dial_and_claim(
                    &path,
                    &claim,
                    TEST_NONCE,
                    deadlines_in(120),
                    own_parent(),
                    chunks,
                )
            });
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(own_pid()),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + Duration::from_secs(60),
                    &|| false,
                ),
                "the claim is this attempt's",
            );
            let handed = writes
                .iter()
                .enumerate()
                .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
                .collect::<Vec<_>>();
            let expired = Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("an instant in the past");
            let error = peer
                .transfer(
                    TEST_NONCE,
                    &handed,
                    ready_wr.as_fd(),
                    commit_rd.as_fd(),
                    expired,
                    None,
                )
                .expect_err("a grant past its deadline is not sent");
            assert_eq!(error, RendezvousError::GrantDeadline, "{sessions} sessions");
            drop((peer, rendezvous));
            let received = dialer.join().expect("dialer thread");
            assert!(
                received.is_err(),
                "{sessions} sessions: the dialer received no grant"
            );
        }
    }

    #[test]
    fn a_grant_of_100_sessions_crosses_in_chunks() {
        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(100);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                true,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the chunked claim is this attempt's",
        );
        assert!(peer.chunks, "the dialer claimed the chunked grant");
        let sessions = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        peer.transfer(
            TEST_NONCE,
            &sessions,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(60),
            None,
        )
        .expect("a chunked grant of 100 sessions is sent");
        let claimed = dialer
            .join()
            .expect("dialer thread")
            .expect("the chunked grant is taken whole");
        assert_eq!(claimed.session_count(), 100);
        for (id, (master, sent)) in claimed.masters.iter().zip(&writes).enumerate() {
            assert_eq!(
                inode(master.as_raw_fd()),
                inode(sent.as_raw_fd()),
                "session {id} is the descriptor sent for it"
            );
        }
        assert_eq!(
            inode(claimed.ready.as_raw_fd()),
            inode(ready_wr.as_raw_fd())
        );
        assert_eq!(
            inode(claimed.commit.as_raw_fd()),
            inode(commit_rd.as_raw_fd())
        );
        let wire = claimed.fds_wire.split(',').collect::<Vec<_>>();
        assert_eq!(wire.len(), 100);
        for (id, entry) in wire.iter().enumerate() {
            assert!(
                entry.starts_with(&format!("{id}=")) && entry.ends_with(&format!(":{}", 5000 + id)),
                "the wire pairs session {id} with its shell: {entry}"
            );
        }
        drop((claimed, peer, rendezvous));
        drop((writes, ready_wr, commit_wr, ready_rd, commit_rd));
        drop(reads);
    }

    #[test]
    fn an_atrz1c_claim_never_receives_a_chunked_grant() {
        use std::io::Read as _;

        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(100);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the one-message claim is this attempt's",
        );
        assert!(!peer.chunks);
        let sessions = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        assert_eq!(
            peer.transfer(
                TEST_NONCE,
                &sessions,
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + Duration::from_secs(60),
                None,
            ),
            Err(RendezvousError::TooManySessions {
                sessions: 100,
                limit: MAX_RENDEZVOUS_SESSIONS,
            }),
            "refused before any descriptor left"
        );
        drop((peer, rendezvous));
        assert!(
            matches!(
                dialer.join().expect("dialer thread"),
                Err(RendezvousError::Grant(_))
            ),
            "the ATRZ1C successor reads the close and takes nothing"
        );
        drop((writes, ready_wr, commit_wr, ready_rd, commit_rd, reads));

        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let dialer = CtlStream::connect(rendezvous.path()).expect("connect");
        (&dialer)
            .write_all(&claim_frame_with(rendezvous.claim(), true))
            .expect("claim");
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the chunked claim is this attempt's",
        );
        // SAFETY: `master` remains open for the duration of `transfer`.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        let sessions = [
            (0u64, 4000, borrowed),
            (1, 4001, borrowed),
            (7, 4002, borrowed),
        ];
        peer.transfer(
            TEST_NONCE,
            &sessions,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(60),
            None,
        )
        .expect("three sessions are granted");
        dialer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("a read bound");
        let mut header = [0u8; GRANT_HEADER_LEN];
        let received =
            fdpass::recv_with_fds(&dialer, &mut header, fdpass::MAX_FDS).expect("the header");
        assert_eq!(received.bytes, GRANT_HEADER_LEN);
        assert_eq!(received.fds.len(), 5, "every descriptor in the one message");
        let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("a body");
        assert_eq!(
            header,
            grant_header(body.len()).expect("a header"),
            "ATRZ1G, byte for byte"
        );
        let mut sent_body = vec![0u8; body.len()];
        (&dialer).read_exact(&mut sent_body).expect("the body");
        assert_eq!(sent_body, body.as_bytes());
        drop((received, dialer, peer, rendezvous));
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);

        let (reads, writes) = pipes_for_test(3);
        let outcome = stub_grant(
            &writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>(),
            &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
                let body = body_for(1);
                let header = grant_chunks_header(body.len(), 1).expect("a header");
                fdpass::send_with_fds(stream, &header, fds)?;
                (&*stream).write_all(body.as_bytes())
            },
            false,
        );
        assert_eq!(
            outcome,
            Err(RendezvousError::Grant("wrong magic".to_string())),
            "a successor that never claimed the chunked grant refuses it"
        );
        drop(writes);
        assert_every_write_end_closed(&reads, "a refused ATRZ2G");
    }

    fn stub_grant(
        fds: &[i32],
        send: &mut (dyn FnMut(&CtlStream, &[BorrowedFd<'_>]) -> std::io::Result<()> + Send),
        chunks: bool,
    ) -> Result<(), RendezvousError> {
        use std::io::Read as _;

        let dir = crate::test_scratch::short_dir("a stub parent's socket");
        let path = dir.path().join("p.sock");
        let listener = aterm_uds::CtlListener::bind(&path).expect("a stub listener");
        let secret = "c".repeat(CLAIM_HEX_LEN);
        let outcome = std::thread::scope(|scope| {
            let parent = scope.spawn(|| {
                let (stream, _) = listener.accept().expect("the dial");
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .expect("a read bound");
                let mut claim = [0u8; CLAIM_FRAME_LEN];
                (&stream).read_exact(&mut claim).expect("the claim");
                let borrowed = fds
                    .iter()
                    // SAFETY: caller keeps every descriptor in `fds` open until `transfer` returns.
                    .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) })
                    .collect::<Vec<_>>();
                let _ = send(&stream, &borrowed);
                stream
            });
            let outcome = dial_and_claim(
                &path,
                &secret,
                TEST_NONCE,
                deadlines_in(60),
                own_parent(),
                chunks,
            )
            .map(drop);
            drop(parent.join().expect("the stub parent"));
            outcome
        });
        drop((listener, dir));
        outcome
    }

    #[test]
    fn a_short_chunk_is_refused_and_closes_every_fd() {
        for (what, second) in [
            ("a short second chunk", Some(30usize)),
            ("no second chunk", None),
        ] {
            let (reads, writes) = pipes_for_test(100 + RENDEZVOUS_CHANNEL_FDS);
            let raw = writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>();
            let sizes = grant_chunk_sizes(100);
            assert_eq!(sizes, [64, 38], "the layout this test shortens");
            let outcome = stub_grant(
                &raw,
                &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
                    let body = body_for(100);
                    let header = grant_chunks_header(body.len(), sizes.len()).expect("a header");
                    fdpass::send_with_fds(stream, &header, &fds[..sizes[0]])?;
                    (&*stream).write_all(body.as_bytes())?;
                    match second {
                        Some(short) => {
                            let frame = grant_continuation(1).expect("a continuation");
                            fdpass::send_with_fds(
                                stream,
                                &frame,
                                &fds[sizes[0]..sizes[0] + short],
                            )?;
                        }
                        None => stream.shutdown(std::net::Shutdown::Write)?,
                    }
                    Ok(())
                },
                true,
            );
            match second {
                Some(_) => assert_eq!(
                    outcome,
                    Err(RendezvousError::DescriptorCount {
                        expected: 102,
                        received: 94,
                    }),
                    "{what}: refused"
                ),
                None => assert!(
                    matches!(outcome, Err(RendezvousError::Grant(_))),
                    "{what}: refused, not {outcome:?}"
                ),
            }
            drop(writes);
            assert_every_write_end_closed(&reads, what);
        }
    }

    fn step_grant(
        model: &aterm_spec::derive::Model,
        state: &mut std::collections::BTreeMap<&'static str, i64>,
        action: &'static str,
    ) {
        let successors = model.successors(action, state);
        assert_eq!(successors.len(), 1, "{action} is not admitted at {state:?}");
        *state = successors[0].clone();
    }

    fn grant_disagreement(
        decide: &dyn Fn(bool, usize) -> Result<GrantShape, RendezvousError>,
    ) -> Option<String> {
        let model = aterm_models_update::native_update_rendezvous_grant_model();
        for capable in [false, true] {
            for declared in [false, true] {
                for offered in [false, true] {
                    for large in [false, true] {
                        if (declared && !capable) || (offered && !declared) {
                            continue;
                        }
                        let mut state = model.init_state();
                        if capable {
                            step_grant(&model, &mut state, "Upgrade");
                        }
                        if declared {
                            step_grant(&model, &mut state, "Declare");
                        }
                        if large {
                            step_grant(&model, &mut state, "Grow");
                        }
                        if offered {
                            step_grant(&model, &mut state, "Offer");
                        }
                        step_grant(&model, &mut state, "Launch");
                        let caps = offered.then(|| std::ffi::OsString::from(GRANT_CAPS_CHUNKS));
                        let claims_chunks = capable && offers_chunked_grant(caps.as_deref());
                        if model.action_enabled("ClaimChunks", &state) != claims_chunks
                            || model.action_enabled("ClaimOne", &state) == claims_chunks
                        {
                            return Some(format!(
                                "the claim of capable={capable} offered={offered}"
                            ));
                        }
                        let secret = "d".repeat(CLAIM_HEX_LEN);
                        let claimed =
                            claim_frame_kind(&claim_frame_with(&secret, claims_chunks), &secret)
                                .expect("the claim carries the secret");
                        step_grant(
                            &model,
                            &mut state,
                            if claimed { "ClaimChunks" } else { "ClaimOne" },
                        );
                        let pools: &[usize] = if large {
                            &[MAX_RENDEZVOUS_SESSIONS + 1, 100, MAX_CHUNKED_SESSIONS]
                        } else {
                            &[1, 3, MAX_RENDEZVOUS_SESSIONS]
                        };
                        for &sessions in pools {
                            let decided = decide(claimed, sessions);
                            let expected = (
                                matches!(decided, Ok(GrantShape::OneMessage)),
                                matches!(decided, Ok(GrantShape::Chunked)),
                                matches!(decided, Err(RendezvousError::TooManySessions { .. })),
                            );
                            let modelled = (
                                model.action_enabled("GrantOne", &state),
                                model.action_enabled("GrantChunks", &state),
                                model.action_enabled("RefuseTooMany", &state),
                            );
                            if expected != modelled {
                                return Some(format!(
                                    "{sessions} sessions for claim chunks={claimed}: the parent \
                                     decides {decided:?}, the model enables {modelled:?}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    #[test]
    fn rendezvous_grant_conformance() {
        assert_eq!(grant_disagreement(&grant_shape), None);
        assert!(
            grant_disagreement(&|_, sessions| grant_shape(true, sessions)).is_some(),
            "a parent that ignores the claim must disagree with the model"
        );
    }
}

#[cfg(test)]
mod claim_presentation_conformance {
    use super::{
        CLAIM_FRAME_LEN, ClaimDeadlines, RendezvousError, dial_and_claim,
        listener_may_receive_claim,
    };
    use crate::seamless::AttestedParent;
    use aterm_models_update::native_update_rendezvous_claim_model;
    use aterm_spec::derive::Model;
    use aterm_spec::interp::{State, with_buggy};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::PathBuf;
    use std::time::Instant;

    /// Dialer-observed listener properties projected into the formal rendezvous model.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct ListenerFacts {
        same_uid: bool,
        is_parent: bool,
        parent_live: bool,
    }

    /// Projects [`ListenerFacts`] onto the formal rendezvous model's state prior to claim presentation.
    pub(crate) fn project(model: &Model, facts: ListenerFacts) -> State {
        let mut state = model.init_state();
        state.insert("same_uid", i64::from(facts.same_uid));
        state.insert("is_parent", i64::from(facts.is_parent));
        state.insert("parent_live", i64::from(facts.parent_live));
        state
    }

    fn every_combination() -> impl Iterator<Item = ListenerFacts> {
        (0u8..8).map(|bits| ListenerFacts {
            same_uid: bits & 1 != 0,
            is_parent: bits & 2 != 0,
            parent_live: bits & 4 != 0,
        })
    }

    fn own_pid() -> libc::pid_t {
        libc::pid_t::try_from(std::process::id()).expect("a pid fits pid_t")
    }

    fn real_gate(facts: ListenerFacts) -> Result<(), RendezvousError> {
        // SAFETY: `getuid` and `getppid` have no preconditions.
        let (own_uid, launcher) = unsafe { (libc::getuid(), libc::getppid()) };
        let listener_uid = if facts.same_uid {
            own_uid
        } else {
            own_uid.wrapping_add(1)
        };
        let listener_pid = if facts.is_parent { own_pid() } else { launcher };
        let parent = if facts.parent_live {
            AttestedParent::attest_live_for_test(own_pid())
                .expect("this live process attests by its birth record")
        } else {
            AttestedParent::recycled_for_test(own_pid())
        };
        listener_may_receive_claim(
            Some(listener_uid),
            own_uid,
            u32::try_from(listener_pid).ok(),
            Some(parent),
        )
    }

    #[test]
    fn the_dial_gate_admits_exactly_what_the_model_admits() {
        let model = native_update_rendezvous_claim_model();
        for facts in every_combination() {
            let admitted = real_gate(facts).is_ok();
            let modelled = model.action_enabled("PresentClaim", &project(&model, facts));
            assert_eq!(
                admitted, modelled,
                "the dial gate and the model's PresentClaim disagree at {facts:?}"
            );
        }
        assert!(
            real_gate(ListenerFacts {
                same_uid: true,
                is_parent: true,
                parent_live: true,
            })
            .is_ok()
        );
    }

    #[test]
    fn the_real_gate_refuses_where_the_uid_only_mutant_presents() {
        let buggy = with_buggy(&native_update_rendezvous_claim_model(), 1);
        let mut told_apart = Vec::new();
        for facts in every_combination() {
            let mutant = buggy.action_enabled("PresentClaimUidOnly", &project(&buggy, facts));
            assert_eq!(
                mutant, facts.same_uid,
                "the Buggy guard is the uid half at {facts:?}"
            );
            if real_gate(facts).is_ok() != mutant {
                told_apart.push(facts);
            }
        }
        assert_eq!(
            told_apart.len(),
            3,
            "the real gate refuses the uid-only gate's impostor, gone-parent and both \
             presentations, and only those: {told_apart:?}"
        );
        assert!(
            told_apart
                .iter()
                .all(|facts| facts.same_uid && !(facts.is_parent && facts.parent_live)),
            "every disagreement is a same-uid presentation to something other than the live \
             attested parent: {told_apart:?}"
        );
    }

    #[test]
    fn no_attested_parent_refuses_even_our_own_listener() {
        // SAFETY: `getuid` has no preconditions.
        let own_uid = unsafe { libc::getuid() };
        assert!(matches!(
            listener_may_receive_claim(Some(own_uid), own_uid, Some(std::process::id()), None),
            Err(RendezvousError::Dial(_))
        ));
        assert!(matches!(
            listener_may_receive_claim(
                Some(own_uid),
                own_uid,
                None,
                AttestedParent::attest_live_for_test(own_pid())
            ),
            Err(RendezvousError::Dial(_))
        ));
    }

    struct ForkedListener {
        pid: libc::pid_t,
        report: OwnedFd,
        path: PathBuf,
        _dir: aterm_tempfile::TempDir,
    }

    impl ForkedListener {
        fn start() -> Self {
            let dir = crate::test_scratch::short_dir("a forked listener's socket");
            let path = dir.path().join("l.sock");
            // SAFETY: all-zero `sockaddr_un` is a valid empty socket address.
            let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            let bytes = path.as_os_str().as_bytes();
            assert!(
                bytes.len() < addr.sun_path.len(),
                "the scratch path fits sun_path"
            );
            addr.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).expect("AF_UNIX");
            for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
                *slot = libc::c_char::from_ne_bytes([*byte]);
            }
            let addr_len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>())
                .expect("sockaddr_un size");
            addr.sun_len = u8::try_from(addr_len).expect("sockaddr_un fits sun_len");
            let mut report = [0i32; 2];
            // SAFETY: `pipe` initializes two file descriptors in `report`.
            assert_eq!(unsafe { libc::pipe(report.as_mut_ptr()) }, 0, "report pipe");
            let table = aterm_uds::spawnfd::descriptor_bound();

            // SAFETY: child only invokes async-signal-safe syscalls before `_exit`.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // SAFETY: async-signal-safe syscalls on child-owned descriptors and stack memory before `_exit`.
                unsafe {
                    for fd in 3..table {
                        if fd != report[1] {
                            libc::close(fd);
                        }
                    }
                    let tell = |byte: u8| {
                        libc::write(report[1], core::ptr::from_ref(&byte).cast(), 1);
                    };
                    let code: i32 = 'serve: {
                        let listener = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                        if listener < 0
                            || libc::bind(listener, core::ptr::from_ref(&addr).cast(), addr_len)
                                != 0
                            || libc::listen(listener, 1) != 0
                        {
                            break 'serve 2;
                        }
                        tell(b'L');
                        let mut ready = libc::pollfd {
                            fd: listener,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        if libc::poll(&mut ready, 1, 30_000) != 1 {
                            break 'serve 3;
                        }
                        let served =
                            libc::accept(listener, core::ptr::null_mut(), core::ptr::null_mut());
                        if served < 0 {
                            break 'serve 4;
                        }
                        let mut buf = [0u8; 128];
                        let mut total = 0usize;
                        while total < CLAIM_FRAME_LEN {
                            let mut readable = libc::pollfd {
                                fd: served,
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            if libc::poll(&mut readable, 1, 10_000) != 1 {
                                break;
                            }
                            let read = libc::read(served, buf.as_mut_ptr().cast(), buf.len());
                            if read <= 0 {
                                break;
                            }
                            total += read.unsigned_abs();
                        }
                        tell(u8::try_from(total.min(255)).unwrap_or(255));
                        libc::close(served);
                        0
                    };
                    libc::_exit(code);
                }
            }
            // SAFETY: `report` holds freshly created pipe descriptors owned exclusively here.
            let (read_end, write_end) = unsafe {
                (
                    OwnedFd::from_raw_fd(report[0]),
                    OwnedFd::from_raw_fd(report[1]),
                )
            };
            drop(write_end);
            let listener = Self {
                pid,
                report: read_end,
                path,
                _dir: dir,
            };
            assert_eq!(
                listener.next_report(),
                Some(b'L'),
                "the forked listener is up"
            );
            listener
        }

        fn next_report(&self) -> Option<u8> {
            let mut byte = 0u8;
            // SAFETY: 1-byte read into a stack local from a valid descriptor owned by `self`.
            let read = unsafe {
                libc::read(
                    self.report.as_raw_fd(),
                    core::ptr::from_mut(&mut byte).cast(),
                    1,
                )
            };
            (read == 1).then_some(byte)
        }

        fn claim_bytes_received(&self) -> Option<u8> {
            self.next_report()
        }
    }

    impl Drop for ForkedListener {
        fn drop(&mut self) {
            // SAFETY: signals and reaps the child PID spawned by `ForkedListener::spawn`.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                let mut status = 0;
                libc::waitpid(self.pid, &mut status, 0);
            }
        }
    }

    fn deadlines() -> ClaimDeadlines {
        let at = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn a_same_uid_listener_that_is_not_the_attested_parent_gets_no_claim() {
        let listener = ForkedListener::start();
        let attested_elsewhere = AttestedParent::attest_live_for_test(own_pid());
        let outcome = dial_and_claim(
            &listener.path,
            SECRET,
            NONCE,
            deadlines(),
            attested_elsewhere,
            false,
        );
        match outcome {
            Err(RendezvousError::Dial(reason)) => assert!(
                reason.contains(&format!("(pid {})", listener.pid)),
                "the refusal names the listening process the kernel reported: {reason}"
            ),
            Err(other) => panic!("expected the identity refusal, got {other}"),
            Ok(_) => panic!("expected the identity refusal, got a grant"),
        }
        assert_eq!(
            listener.claim_bytes_received(),
            Some(0),
            "not one byte of the claim reached the impostor"
        );
    }

    #[test]
    fn the_attested_parent_listener_receives_the_claim() {
        let listener = ForkedListener::start();
        let parent = AttestedParent::attest_live_for_test(listener.pid);
        assert!(
            parent.is_some(),
            "the forked listener attests by its birth record"
        );
        let outcome = dial_and_claim(&listener.path, SECRET, NONCE, deadlines(), parent, false);
        match outcome {
            Err(RendezvousError::Grant(_)) => {}
            Err(other) => panic!("expected a grant-stage end after the claim, got {other}"),
            Ok(_) => panic!("the fixture never grants"),
        }
        assert_eq!(
            listener.claim_bytes_received(),
            u8::try_from(CLAIM_FRAME_LEN).ok(),
            "the attested parent received the whole claim frame"
        );
    }
}

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

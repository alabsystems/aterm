// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who may talk to the keeper, both ways (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.7).

use std::path::Path;
#[cfg(target_vendor = "apple")]
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::CtlStream;
#[cfg(target_vendor = "apple")]
use trust_resource_provenance::signing::{CapturedPeer, Requirement, StaticValidation};

#[cfg(target_vendor = "apple")]
impl From<trust_resource_provenance::signing::Error> for CheckError {
    fn from(error: trust_resource_provenance::signing::Error) -> Self {
        match error {
            trust_resource_provenance::signing::Error::Refused(why) => Self::Refused(why),
            trust_resource_provenance::signing::Error::Unchecked(why) => Self::Unchecked(why),
        }
    }
}

/// How long the on-disk check of a copy of this build ([`Identity::admits_code_at`]) may take.
pub const CODE_CHECK_HANG: Duration = Duration::from_secs(60);

/// The floor a test set on this process's peer waits, in milliseconds (0: none set).
#[cfg(any(test, debug_assertions))]
static PEER_WAITS_FOR_TEST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Wait at least `within` for each peer wait in this process: a serving keeper's check of each
/// dial, a CLI verb's connect, check and answer, and launchd's answer to each `launchctl`.
///
/// A test whose keeper and verbs must answer sets the hang detector here in each process it
/// starts: past the product's bounds a loaded machine would read as a silent `trustd`.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn set_peer_waits_for_test(within: Duration) {
    let ms = u64::try_from(within.as_millis()).unwrap_or(u64::MAX).max(1);
    PEER_WAITS_FOR_TEST_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// `bound`, or the longer floor a test set with [`set_peer_waits_for_test`].
#[cfg(unix)]
pub(crate) fn peer_wait(bound: Duration) -> Duration {
    #[cfg(any(test, debug_assertions))]
    match PEER_WAITS_FOR_TEST_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {}
        ms => return bound.max(Duration::from_millis(ms)),
    }
    bound
}

fn unanswered(what: &str) -> String {
    format!("macOS's code-signing service (trustd) is not answering; {what} was not checked")
}

const THE_PEERS_CODE: &str = "the peer's code";

/// Why [`through_trust_gate`] has no answer, in words that name the cause.
#[cfg(target_vendor = "apple")]
enum GateRefusal {
    /// `trustd` did not answer.
    Unanswered(String),
    /// The check could not run at all (no thread, or it panicked).
    Failed(String),
}

#[cfg(target_vendor = "apple")]
impl From<GateRefusal> for String {
    fn from(refused: GateRefusal) -> Self {
        match refused {
            GateRefusal::Unanswered(why) | GateRefusal::Failed(why) => why,
        }
    }
}

#[cfg(target_vendor = "apple")]
impl From<GateRefusal> for CheckError {
    fn from(refused: GateRefusal) -> Self {
        match refused {
            GateRefusal::Unanswered(why) => Self::Unanswered(why),
            GateRefusal::Failed(why) => Self::Unchecked(why),
        }
    }
}

/// `work`, a code check that waits on `trustd`, through the process's one `trustd` gate.
#[cfg(target_vendor = "apple")]
fn through_trust_gate<T: Send + 'static>(
    suspect: Duration,
    deadline: Duration,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, GateRefusal> {
    aterm_uds::deadline::TRUST
        .run(suspect, deadline, work)
        .map_err(|refused| gate_refusal(refused, what))
}

/// The gate's `refused`, for a check of `what`, in words that name the cause.
#[cfg(target_vendor = "apple")]
fn gate_refusal(refused: aterm_uds::deadline::Refused, what: &str) -> GateRefusal {
    use aterm_uds::deadline::Refused;
    match refused {
        Refused::Unanswered => GateRefusal::Unanswered(unanswered(what)),
        Refused::Failed(why) => {
            GateRefusal::Failed(format!("the check of {what} could not run: {why}"))
        }
    }
}

/// The gate's `refused` of a peer check, as the check's error.
#[cfg(target_vendor = "apple")]
fn peer_refusal(refused: aterm_uds::deadline::Refused) -> CheckError {
    gate_refusal(refused, THE_PEERS_CODE).into()
}

/// A peer check [`Identity::start_check_named`] began, its wait on `trustd` left to the caller.
#[derive(Debug)]
pub struct PeerCheck(PeerCheckState);

#[derive(Debug)]
enum PeerCheckState {
    /// Settled without waiting on `trustd`.
    Settled(Result<(), CheckError>),
    /// Waiting on `trustd`.
    #[cfg(target_vendor = "apple")]
    Waiting(aterm_uds::deadline::Pending<Result<(), CheckError>>),
}

impl PeerCheck {
    const fn settled(answer: Result<(), CheckError>) -> Self {
        Self(PeerCheckState::Settled(answer))
    }

    /// `work`, a peer check, started through the process's `trustd` gate.
    #[cfg(target_vendor = "apple")]
    pub fn start(
        suspect: Duration,
        work: impl FnOnce() -> Result<(), CheckError> + Send + 'static,
    ) -> Self {
        Self::start_on(&aterm_uds::deadline::TRUST, suspect, work)
    }

    /// [`Self::start`] through `gate`.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn start_on(
        gate: &'static aterm_uds::deadline::Gate,
        suspect: Duration,
        work: impl FnOnce() -> Result<(), CheckError> + Send + 'static,
    ) -> Self {
        match gate.start(suspect, work) {
            Ok(pending) => Self(PeerCheckState::Waiting(pending)),
            Err(refused) => Self::settled(Err(peer_refusal(refused))),
        }
    }

    /// The check's answer, if it comes within `within`.
    /// # Errors
    /// The check itself, still waiting, while `trustd` has not answered it.
    pub fn wait(self, within: Duration) -> Result<Result<(), CheckError>, Self> {
        match self.0 {
            PeerCheckState::Settled(answer) => {
                let _ = within;
                Ok(answer)
            }
            #[cfg(target_vendor = "apple")]
            PeerCheckState::Waiting(pending) => match pending.wait(within) {
                Ok(answer) => Ok(answer.unwrap_or_else(|refused| Err(peer_refusal(refused)))),
                Err(still) => Err(Self(PeerCheckState::Waiting(still))),
            },
        }
    }

    /// What this check says while `trustd` has not answered it.
    #[must_use]
    pub fn unanswered(&self) -> String {
        unanswered(THE_PEERS_CODE)
    }
}

/// Which identity a peer must prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPolicy {
    /// Same uid only.
    SameUid,
    /// Same uid and code that satisfies this build's designated requirement.
    Designated,
}

/// Why a peer or an on-disk code check did not admit its subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckError {
    /// The code was checked and refused.
    Refused(String),
    /// The check could not run.
    Unchecked(String),
    /// The check could not run because `trustd` did not answer.
    Unanswered(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) | Self::Unchecked(why) | Self::Unanswered(why) => f.write_str(why),
        }
    }
}

/// The team identifier this process's code is signed by: `None` for an ad-hoc or unsigned image,
/// and off macOS.
/// # Errors
/// Security.framework could not read this process's signing information.
pub fn own_team() -> Result<Option<String>, String> {
    #[cfg(target_vendor = "apple")]
    {
        trust_resource_provenance::signing::own_team().map_err(|why| why.to_string())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        Ok(None)
    }
}

/// The signing identifier this process's code is signed with (`com.aterm.aterm` for the app):
/// `None` for unsigned code, and off macOS.
/// # Errors
/// Security.framework could not read this process's signing information.
pub fn own_identifier() -> Result<Option<String>, String> {
    #[cfg(target_vendor = "apple")]
    {
        trust_resource_provenance::signing::own_identifier().map_err(|why| why.to_string())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        Ok(None)
    }
}

/// The identity check in force.
#[derive(Debug)]
pub struct Identity {
    policy: IdentityPolicy,
    #[cfg(target_vendor = "apple")]
    requirement: Option<Arc<Requirement>>,
    description: String,
    /// Admit a peer of any uid whose code meets the requirement ([`Identity::regardless_of_uid`]).
    any_uid: bool,
}

impl Identity {
    /// The same-uid floor.
    #[must_use]
    pub fn same_uid() -> Self {
        Self {
            policy: IdentityPolicy::SameUid,
            #[cfg(target_vendor = "apple")]
            requirement: None,
            description: "same-uid".to_string(),
            any_uid: false,
        }
    }

    /// The identity `policy` asks for, derived from this process's own code.
    /// # Errors
    /// `Designated` on a platform without code signing, or when this process's own requirement
    /// cannot be read.
    pub fn for_self(policy: IdentityPolicy) -> Result<Self, String> {
        match policy {
            IdentityPolicy::SameUid => Ok(Self::same_uid()),
            IdentityPolicy::Designated => Self::designated(),
        }
    }

    #[cfg(target_vendor = "apple")]
    fn designated() -> Result<Self, String> {
        let (requirement, text) = Requirement::current().map_err(|why| why.to_string())?;
        Ok(Self {
            policy: IdentityPolicy::Designated,
            requirement: Some(Arc::new(requirement)),
            description: format!("designated requirement: {text}"),
            any_uid: false,
        })
    }

    #[cfg(not(target_vendor = "apple"))]
    fn designated() -> Result<Self, String> {
        Err("code identity is checked on macOS only".to_string())
    }

    /// Code that satisfies `requirement`.
    /// # Errors
    /// Off macOS, or when Security.framework does not compile the text.
    pub fn for_requirement(requirement: &str) -> Result<Self, String> {
        #[cfg(target_vendor = "apple")]
        {
            let (compiled, text) =
                Requirement::compile(requirement).map_err(|why| why.to_string())?;
            Ok(Self {
                policy: IdentityPolicy::Designated,
                requirement: Some(Arc::new(compiled)),
                description: format!("requirement: {text}"),
                any_uid: false,
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = requirement;
            Err("code identity is checked on macOS only".to_string())
        }
    }

    /// Code that satisfies the designated requirement of the code at `path`.
    /// # Errors
    /// Off macOS, or when the code at `path` is unsigned or unreadable.
    pub fn for_code_at(path: &Path) -> Result<Self, String> {
        #[cfg(target_vendor = "apple")]
        {
            let (compiled, text) = Requirement::at_path(path).map_err(|why| why.to_string())?;
            Ok(Self {
                policy: IdentityPolicy::Designated,
                requirement: Some(Arc::new(compiled)),
                description: format!("designated requirement of {}: {text}", path.display()),
                any_uid: false,
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = path;
            Err("code identity is checked on macOS only".to_string())
        }
    }

    /// This identity checked by code alone, a peer of any uid admitted when its code meets the
    /// requirement: a root service every login's processes may ask (the LAN broker). The same-uid
    /// floor ([`Self::same_uid`]) has no code to check, so it keeps its uid check.
    #[must_use]
    pub fn regardless_of_uid(mut self) -> Self {
        self.any_uid = self.policy == IdentityPolicy::Designated;
        self
    }

    /// Whether the code at `path` (a bundle) meets the requirement in force under the strictest
    /// static check (`kSecCSStrictValidate`, every architecture, nested code): a copy about to be
    /// adopted as a root service's image.
    /// # Errors
    /// A definite signature or requirement refusal is [`CheckError::Refused`]. A check
    /// that could not run, read the code, or interpret a framework failure is
    /// [`CheckError::Unchecked`]; a silent `trustd` is [`CheckError::Unanswered`].
    pub fn admits_code_at_strictly(&self, path: &Path) -> Result<(), CheckError> {
        #[cfg(target_vendor = "apple")]
        if let Some(req) = &self.requirement {
            let (owned, req) = (path.to_path_buf(), Arc::clone(req));
            return through_trust_gate(
                CODE_CHECK_HANG,
                CODE_CHECK_HANG,
                &path.display().to_string(),
                move || {
                    req.check_static(&owned, StaticValidation::Strict)
                        .map_err(CheckError::from)
                },
            )
            .map_err(CheckError::from)?;
        }
        let _ = path;
        Err(CheckError::Unchecked(
            "no code requirement is in force to check a copy against".to_string(),
        ))
    }

    /// The policy in force.
    #[must_use]
    pub const fn policy(&self) -> IdentityPolicy {
        self.policy
    }

    /// One line for `status`.
    #[must_use]
    pub fn describe(&self) -> &str {
        &self.description
    }

    /// Whether the code at `path` meets the requirement in force, with its signature intact.
    /// # Errors
    /// Why the code does not pass, or could not be read.
    pub fn admits_code_at(&self, path: &Path) -> Result<(), String> {
        #[cfg(target_vendor = "apple")]
        if let Some(req) = &self.requirement {
            let (owned, req) = (path.to_path_buf(), Arc::clone(req));
            return through_trust_gate(
                CODE_CHECK_HANG,
                CODE_CHECK_HANG,
                &path.display().to_string(),
                move || {
                    req.check_static(&owned, StaticValidation::Current)
                        .map_err(|why| why.to_string())
                },
            )?;
        }
        let _ = path;
        Ok(())
    }

    /// Check the peer on `stream`.
    /// # Errors
    /// Why the peer is refused, or why it could not be checked (a `trustd` that did not answer in
    /// time is [`CheckError::Unanswered`]).
    pub fn check(&self, stream: &CtlStream, within: Duration) -> Result<(), CheckError> {
        self.check_named(stream, aterm_uds::peer::peer_audit_token(stream), within)
    }

    /// [`Self::check`] using an earlier kernel token from this same connection.
    /// A mismatching process generation remains unchecked; no code lookup is attempted.
    /// # Errors
    /// As [`Self::check`].
    pub fn check_named(
        &self,
        stream: &CtlStream,
        token: Option<aterm_uds::peer::AuditToken>,
        within: Duration,
    ) -> Result<(), CheckError> {
        let hang = within.max(aterm_uds::peer::CHECK_HANG);
        self.begin_check(stream, token, hang)
            .wait(hang)
            .unwrap_or_else(|check| Err(CheckError::Unanswered(check.unanswered())))
    }

    /// [`Self::check_named`] started.
    #[must_use]
    pub fn start_check_named(
        &self,
        stream: &CtlStream,
        token: Option<aterm_uds::peer::AuditToken>,
    ) -> PeerCheck {
        self.begin_check(stream, token, aterm_uds::peer::CHECK_HANG)
    }

    fn begin_check(
        &self,
        stream: &CtlStream,
        token: Option<aterm_uds::peer::AuditToken>,
        suspect: Duration,
    ) -> PeerCheck {
        #[cfg(unix)]
        if !self.any_uid {
            let ours = aterm_uds::peer::our_uid();
            match aterm_uds::peer::peer_uid(stream) {
                Some(uid) if uid == ours => {}
                Some(uid) => {
                    return PeerCheck::settled(Err(CheckError::Refused(format!(
                        "peer uid {uid} is not ours"
                    ))));
                }
                None => {
                    return PeerCheck::settled(Err(CheckError::Unchecked(
                        "the kernel did not name the peer's uid".to_string(),
                    )));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (stream, token, suspect);
            return PeerCheck::settled(Err(CheckError::Unchecked(
                "the keeper is Unix-only".to_string(),
            )));
        }
        #[cfg(target_vendor = "apple")]
        if let Some(req) = &self.requirement {
            let Some(token) = token else {
                return PeerCheck::settled(Err(CheckError::Unchecked(
                    "the kernel did not give the peer's audit token".to_string(),
                )));
            };
            use std::os::fd::AsFd as _;
            let captured = match CapturedPeer::capture(stream.as_fd()) {
                Ok(peer) if peer.matches_native_token(&token.to_bytes()) => peer,
                Ok(_) => {
                    return PeerCheck::settled(Err(CheckError::Unchecked(
                        "the earlier audit token does not name this connected peer".into(),
                    )));
                }
                Err(why) => return PeerCheck::settled(Err(CheckError::from(why))),
            };
            let req = Arc::clone(req);
            return PeerCheck::start(suspect, move || {
                captured.check(&req).map_err(CheckError::from)
            });
        }
        #[cfg(all(unix, not(target_vendor = "apple")))]
        let _ = (suspect, token);
        #[allow(unreachable_code)]
        PeerCheck::settled(Ok(()))
    }
}

#[cfg(all(test, target_vendor = "apple"))]
#[path = "identity_tests.rs"]
mod tests;

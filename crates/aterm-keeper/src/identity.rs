// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who may talk to the keeper, both ways (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.7).

use std::path::Path;
#[cfg(target_vendor = "apple")]
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::CtlStream;

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
        sec::own_team()
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
        sec::own_identifier()
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
    requirement: Option<Arc<sec::Owned>>,
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
        let (requirement, text) = sec::own_designated_requirement()?;
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
            let (compiled, text) = sec::requirement_from_text(requirement)?;
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
            let (compiled, text) = sec::designated_requirement_at(path)?;
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
                move || sec::check_static_with(&owned, &req, sec::STRICT),
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
                move || sec::check_static(&owned, &req),
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

    /// [`Self::check`] of the process `token` names.
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
            let req = Arc::clone(req);
            return PeerCheck::start(suspect, move || sec::check_guest(&token, &req));
        }
        #[cfg(all(unix, not(target_vendor = "apple")))]
        let _ = (suspect, token);
        #[allow(unreachable_code)]
        PeerCheck::settled(Ok(()))
    }
}

#[cfg(target_vendor = "apple")]
mod sec {
    use core::ffi::c_void;

    type CFTypeRef = *const c_void;
    type OSStatus = i32;

    /// An owned CoreFoundation object, released on drop.
    #[derive(Debug)]
    pub struct Owned(CFTypeRef);

    // SAFETY: a SecRequirement is immutable once created and CoreFoundation's retain/release are
    // thread-safe.
    unsafe impl Send for Owned {}
    // SAFETY: see `Send`: only read (checked against) after creation.
    unsafe impl Sync for Owned {}

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own exactly one reference.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFDataCreate(alloc: CFTypeRef, bytes: *const u8, len: isize) -> CFTypeRef;
        fn CFDictionaryCreate(
            alloc: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            n: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, size: isize, encoding: u32) -> u8;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: [u8; 48];
        static kCFTypeDictionaryValueCallBacks: [u8; 40];
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecCodeCopySelf(flags: u32, out: *mut CFTypeRef) -> OSStatus;
        fn SecCodeCopyDesignatedRequirement(
            code: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecRequirementCopyString(req: CFTypeRef, flags: u32, out: *mut CFTypeRef) -> OSStatus;
        fn SecRequirementCreateWithString(
            text: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCopyGuestWithAttributes(
            host: CFTypeRef,
            attributes: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> OSStatus;
        fn SecStaticCodeCreateWithPath(
            path: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> OSStatus;
        fn SecCodeCopySigningInformation(
            code: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        static kSecGuestAttributeAudit: CFTypeRef;
        static kSecCodeInfoTeamIdentifier: CFTypeRef;
        static kSecCodeInfoIdentifier: CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: CFTypeRef,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> CFTypeRef;
    }

    fn owned(cf: CFTypeRef, what: &str) -> Result<Owned, String> {
        if cf.is_null() {
            Err(format!("{what}: no object"))
        } else {
            Ok(Owned(cf))
        }
    }

    fn status(rc: OSStatus, what: &str) -> Result<(), String> {
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("{what}: OSStatus {rc}"))
        }
    }

    /// This process's designated requirement, and its text.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn own_designated_requirement() -> Result<(Owned, String), String> {
        let mut me: CFTypeRef = std::ptr::null();
        // SAFETY: an out-parameter for one new reference.
        status(unsafe { SecCodeCopySelf(0, &mut me) }, "SecCodeCopySelf")?;
        let me = owned(me, "SecCodeCopySelf")?;
        designated_requirement_of(&me)
    }

    /// The designated requirement of the code on disk at `path` (an executable or a bundle).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn designated_requirement_at(path: &std::path::Path) -> Result<(Owned, String), String> {
        let code = static_code_at(path)?;
        designated_requirement_of(&code)
    }

    /// `requirement` (requirement-language text) compiled.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn requirement_from_text(requirement: &str) -> Result<(Owned, String), String> {
        let bytes = requirement.as_bytes();
        // SAFETY: `bytes` is `len` live bytes of UTF-8, copied by the call.
        let source = owned(
            unsafe {
                CFStringCreateWithBytes(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                    0,
                )
            },
            "CFStringCreateWithBytes",
        )?;
        let mut req: CFTypeRef = std::ptr::null();
        // SAFETY: `source` is a live CFString; an out-parameter for one reference.
        status(
            unsafe { SecRequirementCreateWithString(source.0, 0, &mut req) },
            "SecRequirementCreateWithString",
        )?;
        let req = owned(req, "SecRequirementCreateWithString")?;
        let text = requirement_text(&req)?;
        Ok((req, text))
    }

    /// The designated requirement of `code` (a static or a running code object), and its text.
    #[cfg_attr(trust_verify, trust::skip)]
    fn designated_requirement_of(code: &Owned) -> Result<(Owned, String), String> {
        let mut req: CFTypeRef = std::ptr::null();
        // SAFETY: `code` is a live code object; an out-parameter for one reference.
        status(
            unsafe { SecCodeCopyDesignatedRequirement(code.0, 0, &mut req) },
            "SecCodeCopyDesignatedRequirement",
        )?;
        let req = owned(req, "SecCodeCopyDesignatedRequirement")?;
        let text = requirement_text(&req)?;
        Ok((req, text))
    }

    /// A requirement's text, as Security.framework prints it.
    #[cfg_attr(trust_verify, trust::skip)]
    fn requirement_text(req: &Owned) -> Result<String, String> {
        let mut text: CFTypeRef = std::ptr::null();
        // SAFETY: `req` is a live requirement; an out-parameter for one reference.
        status(
            unsafe { SecRequirementCopyString(req.0, 0, &mut text) },
            "SecRequirementCopyString",
        )?;
        let text = owned(text, "SecRequirementCopyString")?;
        let mut buf = vec![0u8; 4096];
        // SAFETY: `text` is a live CFString; `buf` is its stated size.
        let ok = unsafe {
            CFStringGetCString(
                text.0,
                buf.as_mut_ptr(),
                buf.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        Ok(if ok != 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end]).into_owned()
        } else {
            "(unprintable)".to_string()
        })
    }

    /// The static code object for the code on disk at `path`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn static_code_at(path: &std::path::Path) -> Result<Owned, String> {
        use std::os::unix::ffi::OsStrExt as _;
        let bytes = path.as_os_str().as_bytes();
        let is_dir = u8::from(path.is_dir());
        // SAFETY: `bytes` is `len` live bytes, copied by the call.
        let url = owned(
            unsafe {
                CFURLCreateFromFileSystemRepresentation(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    is_dir,
                )
            },
            "CFURLCreateFromFileSystemRepresentation",
        )?;
        let mut code: CFTypeRef = std::ptr::null();
        // SAFETY: `url` is a live CFURL; an out-parameter for one reference.
        status(
            unsafe { SecStaticCodeCreateWithPath(url.0, 0, &mut code) },
            "SecStaticCodeCreateWithPath",
        )?;
        owned(code, "SecStaticCodeCreateWithPath")
    }

    /// Whether the process `token` names runs code satisfying `req`, its status classed by
    /// [`guest_validity`].
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_guest(
        token: &aterm_uds::peer::AuditToken,
        req: &Owned,
    ) -> Result<(), super::CheckError> {
        let guest = copy_guest(token).map_err(super::CheckError::Unchecked)?;
        // SAFETY: both objects are live.
        guest_validity(unsafe { SecCodeCheckValidity(guest.0, 0, req.0) })
    }

    /// Whether the code on disk at `path` (an executable or a bundle) is intact and satisfies
    /// `req`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_static(path: &std::path::Path, req: &Owned) -> Result<(), String> {
        check_static_with(path, req, 0).map_err(|why| why.to_string())
    }

    /// `kSecCSCheckAllArchitectures | kSecCSCheckNestedCode | kSecCSStrictValidate`.
    pub const STRICT: u32 = 1 | (1 << 3) | (1 << 4);

    /// [`check_static`] under the validity `flags`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_static_with(
        path: &std::path::Path,
        req: &Owned,
        flags: u32,
    ) -> Result<(), super::CheckError> {
        let code = static_code_at(path).map_err(super::CheckError::Unchecked)?;
        // SAFETY: both objects are live.
        static_validity(unsafe { SecStaticCodeCheckValidity(code.0, flags, req.0) })
    }

    /// Classify only definite code refusals; unknown framework and operational
    /// failures remain unchecked, including unreadable or incomplete resources.
    pub(super) fn static_validity(rc: OSStatus) -> Result<(), super::CheckError> {
        validity(rc, "the code", &[])
    }

    /// [`static_validity`]'s classes for a running guest, which also refuses one whose dynamic
    /// validity is lost (errSecCSGuestInvalid) or whose code on disk changed since it started
    /// (errSecCSStaticCodeChanged): neither comes back for that process. Any other failure, a
    /// framework or trust service one among them, judged nothing and remains unchecked.
    pub(super) fn guest_validity(rc: OSStatus) -> Result<(), super::CheckError> {
        const GUEST_INVALID: OSStatus = -67063;
        const STATIC_CODE_CHANGED: OSStatus = -67034;
        validity(rc, "the peer's code", &[GUEST_INVALID, STATIC_CODE_CHANGED])
    }

    /// A validity check's status `rc` for `what`: refused for a definite code refusal or one of
    /// `also`, unchecked for any other failure.
    fn validity(rc: OSStatus, what: &str, also: &[OSStatus]) -> Result<(), super::CheckError> {
        // Security.framework/Headers/CSCommon.h: errSecCSUnsigned,
        // errSecCSSignatureFailed, errSecCSReqFailed. Unsupported signature
        // formats (including errSecCSSignatureInvalid) remain unchecked.
        const UNSIGNED: OSStatus = -67062;
        const SIGNATURE_FAILED: OSStatus = -67061;
        const REQUIREMENT_FAILED: OSStatus = -67050;
        if rc == 0 {
            return Ok(());
        }
        if matches!(rc, UNSIGNED | SIGNATURE_FAILED | REQUIREMENT_FAILED) || also.contains(&rc) {
            Err(super::CheckError::Refused(format!(
                "{what} does not satisfy the requirement in force: OSStatus {rc}"
            )))
        } else {
            Err(super::CheckError::Unchecked(format!(
                "{what} could not be checked against the requirement in force: OSStatus {rc}"
            )))
        }
    }

    /// The team identifier this process's code is signed by, or `None` (ad hoc, or unsigned).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn own_team() -> Result<Option<String>, String> {
        // SAFETY: reading an immutable framework constant.
        own_signing_value(unsafe { kSecCodeInfoTeamIdentifier })
    }

    /// The signing identifier this process's code is signed with (`com.aterm.aterm`), or `None`
    /// (unsigned).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn own_identifier() -> Result<Option<String>, String> {
        // SAFETY: reading an immutable framework constant.
        own_signing_value(unsafe { kSecCodeInfoIdentifier })
    }

    /// One string of this process's signing information (`key`, a `kSecCodeInfo…` constant), or
    /// `None` when the information has none.
    #[cfg_attr(trust_verify, trust::skip)]
    fn own_signing_value(key: CFTypeRef) -> Result<Option<String>, String> {
        const SIGNING_INFORMATION: u32 = 1 << 1;
        let mut me: CFTypeRef = std::ptr::null();
        // SAFETY: an out-parameter for one new reference.
        status(unsafe { SecCodeCopySelf(0, &mut me) }, "SecCodeCopySelf")?;
        let me = owned(me, "SecCodeCopySelf")?;
        let mut info: CFTypeRef = std::ptr::null();
        // SAFETY: `me` is a live code object; an out-parameter for one reference.
        status(
            unsafe { SecCodeCopySigningInformation(me.0, SIGNING_INFORMATION, &mut info) },
            "SecCodeCopySigningInformation",
        )?;
        let info = owned(info, "SecCodeCopySigningInformation")?;
        // SAFETY: `key` is an immutable framework constant; `info` is a live dictionary, and the
        // value it answers is borrowed from it (the Get rule), read while `info` lives.
        let value = unsafe { CFDictionaryGetValue(info.0, key) };
        if value.is_null() {
            return Ok(None);
        }
        let mut buf = vec![0u8; 256];
        // SAFETY: `value` is a CFString `info` keeps alive; `buf` is its stated size.
        let ok = unsafe {
            CFStringGetCString(
                value,
                buf.as_mut_ptr(),
                buf.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return Err("a signing information string is unprintable".to_string());
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let value = String::from_utf8_lossy(&buf[..end]).into_owned();
        Ok((!value.is_empty()).then_some(value))
    }

    /// The code object of the process `token` names.
    #[cfg_attr(trust_verify, trust::skip)]
    fn copy_guest(token: &aterm_uds::peer::AuditToken) -> Result<Owned, String> {
        let bytes = token.to_bytes();
        // SAFETY: `bytes` is 32 live bytes, copied by CFDataCreate.
        let data = owned(
            unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) },
            "CFDataCreate",
        )?;
        // SAFETY: reading an immutable framework constant.
        let key = unsafe { kSecGuestAttributeAudit };
        let keys = [key];
        let values = [data.0];
        // SAFETY: one key and one value, both live; the standard CFType callbacks retain them for
        // the dictionary's life.
        let attrs = owned(
            unsafe {
                CFDictionaryCreate(
                    std::ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                    std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                    std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
                )
            },
            "CFDictionaryCreate",
        )?;
        let mut guest: CFTypeRef = std::ptr::null();
        // SAFETY: `attrs` is a live dictionary; an out-parameter for one reference.
        status(
            unsafe { SecCodeCopyGuestWithAttributes(std::ptr::null(), attrs.0, 0, &mut guest) },
            "the peer's code (SecCodeCopyGuestWithAttributes)",
        )?;
        owned(guest, "SecCodeCopyGuestWithAttributes")
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;

    const WITHIN: Duration = Duration::from_secs(60);

    const TRUSTD_SILENT: &str =
        "COULD NOT RUN: the process trustd gate was silent — the machine's, not the code's";

    fn verdict<T>(r: Result<T, CheckError>) -> Result<T, CheckError> {
        if let Err(CheckError::Unanswered(why)) = &r {
            panic!("{TRUSTD_SILENT}: {why}");
        }
        r
    }

    fn answered(r: Result<(), String>) -> Result<(), String> {
        if let Err(why) = &r
            && why.starts_with("macOS's code-signing service (trustd) is not answering")
        {
            panic!("{TRUSTD_SILENT}: {why}");
        }
        r
    }

    #[test]
    fn a_process_satisfies_its_own_requirement() {
        aterm_uds::deadline::require_trust_service();
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        assert!(
            id.describe().starts_with("designated requirement: "),
            "{}",
            id.describe()
        );
        let (a, _b) = CtlStream::pair().expect("pair");
        verdict(id.check(&a, WITHIN)).expect("self passes");
        assert_eq!(Identity::same_uid().describe(), "same-uid");
        Identity::same_uid()
            .check(&a, WITHIN)
            .expect("same uid passes");
    }

    #[test]
    fn a_check_regardless_of_uid_still_checks_code_and_the_floor_keeps_its_uid() {
        aterm_uds::deadline::require_trust_service();
        let id = Identity::for_self(IdentityPolicy::Designated)
            .expect("own requirement")
            .regardless_of_uid();
        assert!(id.any_uid);
        let (a, _b) = CtlStream::pair().expect("pair");
        verdict(id.check(&a, WITHIN)).expect("self passes by code");
        assert!(!Identity::same_uid().regardless_of_uid().any_uid);
        // A test binary is signed ad hoc by its linker: no team, and no strict check without a
        // requirement in force.
        assert_eq!(own_team(), Ok(None));
        assert!(matches!(
            Identity::same_uid().admits_code_at_strictly(Path::new("/bin/ls")),
            Err(CheckError::Unchecked(_))
        ));
        assert_eq!(sec::static_validity(0), Ok(()));
        for status in [-67062, -67061, -67050] {
            assert!(
                matches!(sec::static_validity(status), Err(CheckError::Refused(_))),
                "a definite code refusal: {status}"
            );
        }
        for status in [
            -67060, -67068, -67048, -67032, -67033, -67025, -67006, -67056, -67054, -67045, -1,
        ] {
            assert!(
                matches!(sec::static_validity(status), Err(CheckError::Unchecked(_))),
                "an operational, incomplete or unknown result is not a refusal: {status}"
            );
        }
        assert!(matches!(
            CheckError::from(GateRefusal::Unanswered("silent".into())),
            CheckError::Unanswered(_)
        ));
        assert!(matches!(
            CheckError::from(GateRefusal::Failed("could not run".into())),
            CheckError::Unchecked(_)
        ));
    }

    /// A running guest's status is refused only where it is a definite refusal, as a static
    /// check's is, or the guest's validity is gone for good; an operational failure is unchecked.
    #[test]
    fn a_guest_status_is_refused_only_where_its_code_was_judged() {
        assert_eq!(sec::guest_validity(0), Ok(()));
        for status in [-67062, -67061, -67050, -67063, -67034] {
            assert!(
                matches!(sec::guest_validity(status), Err(CheckError::Refused(_))),
                "a definite refusal of a running guest: {status}"
            );
        }
        for status in [
            -67060, -67068, -67048, -67032, -67033, -67056, -67054, -67045, -1,
        ] {
            assert!(
                matches!(sec::guest_validity(status), Err(CheckError::Unchecked(_))),
                "an operational or unknown result is not a refusal: {status}"
            );
        }
        for status in [-67063, -67034] {
            assert!(
                matches!(sec::static_validity(status), Err(CheckError::Unchecked(_))),
                "only a running guest's validity is gone for good: {status}"
            );
        }
    }

    #[test]
    fn a_held_token_is_checked_as_the_process_it_named() {
        aterm_uds::deadline::require_trust_service();
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let (a, _b) = CtlStream::pair().expect("pair");
        let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
        verdict(id.check_named(&a, Some(token), WITHIN))
            .expect("the token read from the connection names this process");
        let mut later = token;
        later.0[7] = later.0[7].wrapping_add(1);
        let stale = verdict(id.check_named(&a, Some(later), WITHIN));
        assert!(
            matches!(stale, Err(CheckError::Unchecked(_))),
            "a token for another generation of this pid was looked up as the live process: \
             {stale:?}"
        );
        let unnamed = verdict(id.check_named(&a, None, WITHIN));
        assert!(
            matches!(unnamed, Err(CheckError::Unchecked(_))),
            "a peer the kernel did not name was checked: {unnamed:?}"
        );
        Identity::same_uid()
            .check_named(&a, None, WITHIN)
            .expect("the floor reads no token");
    }

    #[test]
    fn code_on_disk_is_checked_against_the_requirement() {
        aterm_uds::deadline::require_trust_service();
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let exe = std::env::current_exe().expect("test binary");
        answered(id.admits_code_at(&exe)).expect("this binary meets its own requirement");
        let scratch = crate::test_scratch_dir("code");
        let copy = scratch.path().join("copy");
        std::fs::copy(&exe, &copy).expect("copy");
        answered(id.admits_code_at(&copy)).expect("a byte-for-byte copy is the same code");
        let other = answered(id.admits_code_at(Path::new("/usr/bin/true")));
        assert!(other.is_err(), "another program admitted: {other:?}");
        let mut bytes = std::fs::read(&copy).expect("read");
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&copy, &bytes).expect("write");
        let broken = answered(id.admits_code_at(&copy));
        assert!(broken.is_err(), "a changed copy admitted: {broken:?}");
        Identity::same_uid()
            .admits_code_at(Path::new("/usr/bin/true"))
            .expect("the floor checks no code");
    }

    #[test]
    fn a_stated_requirement_refuses_code_that_does_not_meet_it() {
        aterm_uds::deadline::require_trust_service();
        let id = Identity::for_requirement(
            "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and \
             certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
             leaf[subject.OU] = \"A66A9P66Z7\"",
        )
        .expect("the Developer ID requirement compiles");
        assert!(
            id.describe()
                .starts_with("requirement: anchor apple generic"),
            "{}",
            id.describe()
        );
        assert_eq!(id.policy(), IdentityPolicy::Designated);
        let (a, _b) = CtlStream::pair().expect("pair");
        let refused = verdict(id.check(&a, WITHIN));
        assert!(
            matches!(refused, Err(CheckError::Refused(_))),
            "an ad-hoc test binary met a Developer ID requirement: {refused:?}"
        );
        assert!(Identity::for_requirement("this is not a requirement").is_err());
    }

    #[test]
    fn the_code_at_a_path_admits_exactly_its_own_processes() {
        aterm_uds::deadline::require_trust_service();
        let exe = std::env::current_exe().expect("test binary");
        let own = Identity::for_code_at(&exe).expect("this binary's requirement");
        assert!(
            own.describe().starts_with("designated requirement of "),
            "{}",
            own.describe()
        );
        let (a, _b) = CtlStream::pair().expect("pair");
        verdict(own.check(&a, WITHIN)).expect("a process running the code at the path passes");
        let other = Identity::for_code_at(Path::new("/usr/bin/true")).expect("a platform binary");
        let refused = verdict(other.check(&a, WITHIN));
        assert!(
            matches!(refused, Err(CheckError::Refused(_))),
            "this test binary met /usr/bin/true's requirement: {refused:?}"
        );
        let scratch = crate::test_scratch_dir("unsigned");
        let unsigned = scratch.path().join("script");
        std::fs::write(&unsigned, "#!/bin/sh\nexit 0\n").expect("write");
        assert!(
            Identity::for_code_at(&unsigned).is_err(),
            "an unsigned file has no designated requirement"
        );
    }

    #[test]
    fn another_program_fails_the_designated_check() {
        aterm_uds::deadline::require_trust_service();
        let scratch = crate::test_scratch_dir("id");
        let path = scratch.path().join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let mut nc = aterm_uds::spawnfd::command("/usr/bin/nc")
            .arg("-U")
            .arg(&path)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("nc");
        let (stream, _) = listener.accept().expect("accept");
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let refused = verdict(id.check(&stream, WITHIN));
        assert!(
            matches!(refused, Err(CheckError::Refused(_))),
            "nc was not refused as other code: {refused:?}"
        );
        Identity::same_uid()
            .check(&stream, WITHIN)
            .expect("nc is our uid");
        let _ = nc.kill();
        let _ = nc.wait();
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who may talk to the keeper, both ways (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.7).
//!
//! The floor is the same uid, always: `getpeereid` on every connection, the socket in a 0700
//! directory. Above it, by default, code identity: the peer's audit token (`LOCAL_PEERTOKEN`,
//! never a recyclable pid) must name code that satisfies this build's own designated
//! requirement. The keeper and the window are the same binary, so:
//!
//! * the installed app carries its Developer ID requirement (identifier `com.aterm.aterm`, Team
//!   `A66A9P66Z7`; §11.1), which every future signed build also meets;
//! * a dev or ad-hoc build carries only its cdhash, so only the very same build passes;
//! * [`IdentityPolicy::SameUid`] is the explicit floor, for tests and by-hand runs. A keeper
//!   states its policy in its WELCOME, and a window that checks code registers nothing with one
//!   at the floor (`crate::client`): it would offer an orphaned master to any same-uid process.
//!
//! What the code check does not bind: on Darwin the audit token names the process that last
//! used the connection, read live, not the one that connected (`aterm_uds::peer`'s
//! `the_peer_is_the_connections_last_user_read_live`), and each side reads it once, at accept or
//! dial. A same-uid process can keep a copy of a connection in a second process and `exec` a
//! genuine aterm at its own pid before the check, and the copy then speaks for genuine code. So
//! against a same-uid process that knows this, the gate is the same-uid floor. The fix, a binding
//! to the connecting process no later use can move, is a transport decision shared with the
//! update's launched-lane rendezvous (`docs/AUDIT-update-robustness-r8-2026-09-30.md`, #904).
//!
//! That rendezvous checks its dialer with this machinery against a requirement that need not be
//! this build's: a release's ([`Identity::for_requirement`]) or the launched code's
//! ([`Identity::for_code_at`]). There a [`CheckError::Unanswered`] holds the dialer for another
//! check rather than turning it away.
//!
//! The calls are Security.framework's guest lookup (`SecCodeCopyGuestWithAttributes` with
//! `kSecGuestAttributeAudit`) and `SecCodeCheckValidity` against `SecCodeCopySelf`'s designated
//! requirement. The guest lookup re-opens the file the process was started from: with that file
//! deleted it fails `ENOENT` (OSStatus 100002), and with another build at the path
//! `errSecCSStaticCodeChanged` (-67034) or -67049; `kSecCSMatchGuestRequirementInKernel` does
//! not help (`errSecCSReqInvalid`). So a process that must stay checkable runs from code that
//! outlives it, as the keeper does (`crate::code`), and [`Identity::admits_code_at`] checks that
//! code before the keeper re-runs from it.
//!
//! Both checks wait on macOS's `trustd` for certificate-chained code and never return when it
//! stops answering. So each runs through the process's one `trustd` gate,
//! `aterm_uds::deadline::TRUST`, under a deadline: an unanswered check is
//! [`CheckError::Unanswered`], a refusal, never a pass, and while one is overdue every later check
//! is refused at once. It is its own variant because it is about the machine's moment, not the
//! peer. Reading this process's own requirement does not wait on `trustd`, so it runs inline.

use std::path::Path;
#[cfg(target_vendor = "apple")]
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::CtlStream;

/// How long the on-disk check of a copy of this build ([`Identity::admits_code_at`]) may take.
/// It hashes every page of a bundle, for many seconds on a loaded machine, so this is a hang
/// detector: no honest check is cut, and a `trustd` that never answers is named in a minute.
pub const CODE_CHECK_HANG: Duration = Duration::from_secs(60);

/// How long a peer's code check ([`Identity::check`]) may take, at least: a hang detector with
/// room for a loaded cold launch (one `SecCodeCheckValidity` answers in a quarter second), so an
/// honest slow check is never cut into a refused dial. It is also the check's suspect age, no
/// shorter than a TLS evaluation's (`aterm-http`'s five seconds), so a slow peer check never
/// makes the process's certificate checks refuse. Not a minute, because the keeper's accept loop
/// and the window's boot dial wait on it in line.
pub const PEER_CHECK_HANG: Duration = Duration::from_secs(10);

/// The words for a check `trustd` did not answer — at its deadline, or at once
/// because an earlier check (of any kind, sharing the gate) is overdue — so
/// they claim no wait.
#[cfg(target_vendor = "apple")]
fn unanswered(what: &str) -> String {
    format!("macOS's code-signing service (trustd) is not answering; {what} was not checked")
}

/// Why [`through_trust_gate`] has no answer, in words that name the cause.
#[cfg(target_vendor = "apple")]
enum GateRefusal {
    /// `trustd` did not answer: at the deadline, or at once because an earlier
    /// check sharing the gate is overdue.
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

/// `work`, a code check that waits on `trustd`, through the process's one
/// `trustd` gate: suspect after `suspect`, abandoned at `deadline`. Its own
/// answer, or why there is none.
#[cfg(target_vendor = "apple")]
fn through_trust_gate<T: Send + 'static>(
    suspect: Duration,
    deadline: Duration,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, GateRefusal> {
    use aterm_uds::deadline::{Refused, TRUST};
    TRUST
        .run(suspect, deadline, work)
        .map_err(|refused| match refused {
            Refused::Unanswered => GateRefusal::Unanswered(unanswered(what)),
            Refused::Failed(why) => {
                GateRefusal::Failed(format!("the check of {what} could not run: {why}"))
            }
        })
}

/// Which identity a peer must prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPolicy {
    /// Same uid only.
    SameUid,
    /// Same uid and code that satisfies this build's designated requirement.
    Designated,
}

/// Why [`Identity::check`] did not admit a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckError {
    /// The peer was checked and refused: another uid, or code that does not
    /// satisfy this build's requirement.
    Refused(String),
    /// The check could not run: the kernel did not name the peer, or its code
    /// could not be looked up (it may have exited, or the file it was started
    /// from was replaced).
    Unchecked(String),
    /// The check could not run because `trustd` did not answer (at the deadline, or at once while
    /// an earlier check through the gate is overdue). A refusal, never a pass, and about the
    /// machine's moment rather than the peer: the same check may pass once `trustd` answers.
    Unanswered(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) | Self::Unchecked(why) | Self::Unanswered(why) => f.write_str(why),
        }
    }
}

/// The identity check in force.
#[derive(Debug)]
pub struct Identity {
    policy: IdentityPolicy,
    #[cfg(target_vendor = "apple")]
    requirement: Option<Arc<sec::Owned>>,
    description: String,
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
        }
    }

    /// The identity `policy` asks for, derived from this process's own code.
    ///
    /// # Errors
    /// `Designated` on a platform without code signing, or when this process's
    /// own requirement cannot be read. The caller refuses to serve rather than
    /// falling to the floor silently.
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
        })
    }

    #[cfg(not(target_vendor = "apple"))]
    fn designated() -> Result<Self, String> {
        Err("code identity is checked on macOS only".to_string())
    }

    /// Code that satisfies `requirement`: requirement-language text, as
    /// `codesign -R` takes it after its `=` — for a peer whose code is not
    /// this build's but must meet a requirement this build states, as an update
    /// candidate meets an aterm release's (its signing identifier and the
    /// Developer ID requirement pinned to the release team).
    ///
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
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = requirement;
            Err("code identity is checked on macOS only".to_string())
        }
    }

    /// Code that satisfies the designated requirement of the code at `path` (an
    /// executable or an app bundle): a peer started from that code passes, and,
    /// for an ad-hoc signature (whose requirement is its cdhash), only one.
    /// Reading a requirement does not wait on `trustd`; checking a peer against
    /// it may, as every check does.
    ///
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
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = path;
            Err("code identity is checked on macOS only".to_string())
        }
    }

    /// The policy in force.
    #[must_use]
    pub fn policy(&self) -> IdentityPolicy {
        self.policy
    }

    /// One line for `status`.
    #[must_use]
    pub fn describe(&self) -> &str {
        &self.description
    }

    /// Whether the code at `path` — an executable, or an app bundle — meets the
    /// requirement in force, with its signature intact: what a peer checking a
    /// process started from it will find. Always `Ok` under the same-uid floor.
    ///
    /// # Errors
    /// Why the code does not pass, or could not be read — including a
    /// `trustd` that did not answer within [`CODE_CHECK_HANG`].
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

    /// Check the peer on `stream`: same uid, then (Designated) its code, which
    /// `trustd` must answer within `within` or [`PEER_CHECK_HANG`], whichever
    /// is longer.
    ///
    /// # Errors
    /// Why the peer is refused, or why it could not be checked (a `trustd`
    /// that did not answer in time is [`CheckError::Unanswered`]).
    pub fn check(&self, stream: &CtlStream, within: Duration) -> Result<(), CheckError> {
        self.check_named(stream, aterm_uds::peer::peer_audit_token(stream), within)
    }

    /// [`Self::check`] of the process `token` names, read from `stream` earlier
    /// ([`aterm_uds::peer::peer_audit_token`]): for a caller that must judge the same process
    /// across several checks (the update handoff's rendezvous). A token whose process has gone or
    /// been replaced at its pid fails the code lookup ([`CheckError::Unchecked`]); so does `None`
    /// (a peer the kernel did not name) where a requirement is in force.
    ///
    /// # Errors
    /// As [`Self::check`].
    pub fn check_named(
        &self,
        stream: &CtlStream,
        token: Option<aterm_uds::peer::AuditToken>,
        within: Duration,
    ) -> Result<(), CheckError> {
        #[cfg(unix)]
        {
            let ours = aterm_uds::peer::our_uid();
            match aterm_uds::peer::peer_uid(stream) {
                Some(uid) if uid == ours => {}
                Some(uid) => {
                    return Err(CheckError::Refused(format!("peer uid {uid} is not ours")));
                }
                None => {
                    return Err(CheckError::Unchecked(
                        "the kernel did not name the peer's uid".to_string(),
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (stream, token);
            return Err(CheckError::Unchecked("the keeper is Unix-only".to_string()));
        }
        #[cfg(target_vendor = "apple")]
        if let Some(req) = &self.requirement {
            let token = token.ok_or_else(|| {
                CheckError::Unchecked("the kernel did not give the peer's audit token".to_string())
            })?;
            let req = Arc::clone(req);
            let hang = within.max(PEER_CHECK_HANG);
            through_trust_gate(hang, hang, "the peer's code", move || {
                sec::check_guest(&token, &req)
            })
            .map_err(|refused| match refused {
                GateRefusal::Unanswered(why) => CheckError::Unanswered(why),
                GateRefusal::Failed(why) => CheckError::Unchecked(why),
            })??;
        }
        // Off Apple nothing judges the code, so neither the deadline nor the token is read.
        #[cfg(not(target_vendor = "apple"))]
        let _ = (within, token);
        #[allow(unreachable_code)]
        Ok(())
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

    // SAFETY: a SecRequirement is immutable once created and CoreFoundation's
    // retain/release are thread-safe.
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
        static kSecGuestAttributeAudit: CFTypeRef;
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
    // Skip: bottoms out at Security.framework FFI.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn own_designated_requirement() -> Result<(Owned, String), String> {
        let mut me: CFTypeRef = std::ptr::null();
        // SAFETY: an out-parameter for one new reference.
        status(unsafe { SecCodeCopySelf(0, &mut me) }, "SecCodeCopySelf")?;
        let me = owned(me, "SecCodeCopySelf")?;
        designated_requirement_of(&me)
    }

    /// The designated requirement of the code on disk at `path` (an executable
    /// or a bundle), and its text: the requirement a process started from that
    /// code satisfies. Unsigned code has none.
    // Skip: bottoms out at Security.framework FFI.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn designated_requirement_at(path: &std::path::Path) -> Result<(Owned, String), String> {
        let code = static_code_at(path)?;
        designated_requirement_of(&code)
    }

    /// `requirement` (requirement-language text) compiled, and its text as
    /// Security.framework prints it back.
    // Skip: bottoms out at Security.framework FFI.
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

    /// The designated requirement of `code` (a static or a running code
    /// object), and its text.
    // Skip: bottoms out at Security.framework FFI.
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
    // Skip: bottoms out at Security.framework FFI.
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
    // Skip: bottoms out at Security.framework FFI.
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

    /// Whether the process `token` names runs code satisfying `req`: a failed
    /// lookup of its code is [`CheckError::Unchecked`], code that fails `req`
    /// [`CheckError::Refused`].
    ///
    /// [`CheckError::Unchecked`]: super::CheckError::Unchecked
    /// [`CheckError::Refused`]: super::CheckError::Refused
    // Skip: bottoms out at Security.framework FFI.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_guest(
        token: &aterm_uds::peer::AuditToken,
        req: &Owned,
    ) -> Result<(), super::CheckError> {
        let guest = copy_guest(token).map_err(super::CheckError::Unchecked)?;
        // SAFETY: both objects are live.
        status(
            unsafe { SecCodeCheckValidity(guest.0, 0, req.0) },
            "the peer's code does not satisfy the requirement in force",
        )
        .map_err(super::CheckError::Refused)
    }

    /// Whether the code on disk at `path` (an executable or a bundle) is
    /// intact and satisfies `req`: `SecStaticCodeCheckValidity` with the
    /// default flags, which validate the signature, its resources and every
    /// page of the executable.
    // Skip: bottoms out at Security.framework FFI.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_static(path: &std::path::Path, req: &Owned) -> Result<(), String> {
        let code = static_code_at(path)?;
        // SAFETY: both objects are live.
        status(
            unsafe { SecStaticCodeCheckValidity(code.0, 0, req.0) },
            "the code does not satisfy the requirement in force",
        )
    }

    /// The code object of the process `token` names.
    // Skip: bottoms out at Security.framework FFI.
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
        // SAFETY: one key and one value, both live; the standard CFType
        // callbacks retain them for the dictionary's life.
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
    use aterm_uds::deadline::trust_service_silent;

    use super::*;

    /// A peer check's deadline here: a hang detector, not a budget.
    const WITHIN: Duration = Duration::from_secs(60);

    /// `trustd` going silent after the one probe above refuses every later
    /// check through the gate: the machine's moment, never the code's verdict,
    /// so it is neither a pass nor a refusal a negative control may count.
    const TRUSTD_SILENT: &str =
        "COULD NOT RUN: the process trustd gate was silent — the machine's, not the code's";

    /// `r`, unless the gate refused it unanswered (COULD NOT RUN).
    fn verdict<T>(r: Result<T, CheckError>) -> Result<T, CheckError> {
        if let Err(CheckError::Unanswered(why)) = &r {
            panic!("{TRUSTD_SILENT}: {why}");
        }
        r
    }

    /// [`Identity::admits_code_at`]'s `r`, unless the gate refused it
    /// unanswered (its words are [`unanswered`]'s).
    fn answered(r: Result<(), String>) -> Result<(), String> {
        if let Err(why) = &r
            && why.starts_with("macOS's code-signing service (trustd) is not answering")
        {
            panic!("{TRUSTD_SILENT}: {why}");
        }
        r
    }

    /// This process satisfies its own requirement: a socketpair's peer is
    /// ourselves, and the designated check passes. The floor names itself.
    #[test]
    fn a_process_satisfies_its_own_requirement() {
        // An ad-hoc build's check needs no `trustd`, but every check shares the gate.
        if trust_service_silent() {
            return;
        }
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

    /// A held token is judged as the process it names: the token read from the connection
    /// passes. Negative controls: the token with its pid version moved on (a later image at the
    /// same pid) and no token at all are `Unchecked`, never a pass, under a requirement.
    #[test]
    fn a_held_token_is_checked_as_the_process_it_named() {
        if trust_service_silent() {
            return;
        }
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

    /// The code a keeper re-runs from is checked as a peer would check it: this test binary's
    /// file and a byte-for-byte copy pass. Negative controls: `/usr/bin/true` and the copy with
    /// one byte changed do not; the same-uid floor admits any.
    #[test]
    fn code_on_disk_is_checked_against_the_requirement() {
        if trust_service_silent() {
            return;
        }
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let exe = std::env::current_exe().expect("test binary");
        answered(id.admits_code_at(&exe)).expect("this binary meets its own requirement");
        let dir = std::env::temp_dir().join(format!("akcode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let copy = dir.join("copy");
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stated requirement for another build's code (the Developer ID requirement pinned to a
    /// team, as an update candidate meets it): this ad-hoc test binary is refused, never
    /// unchecked, and text that is no requirement is refused when compiled.
    #[test]
    fn a_stated_requirement_refuses_code_that_does_not_meet_it() {
        if trust_service_silent() {
            return;
        }
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

    /// The code at a path, as its own designated requirement: this test binary passes, and the
    /// same process against `/usr/bin/true`'s code is refused. Unsigned code has no requirement.
    #[test]
    fn the_code_at_a_path_admits_exactly_its_own_processes() {
        if trust_service_silent() {
            return;
        }
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
        let dir = std::env::temp_dir().join(format!("akunsigned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let unsigned = dir.join("script");
        std::fs::write(&unsigned, "#!/bin/sh\nexit 0\n").expect("write");
        assert!(
            Identity::for_code_at(&unsigned).is_err(),
            "an unsigned file has no designated requirement"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `nc -U` dials a listener here: its audit token names `nc`, which fails the designated
    /// check and passes the floor.
    #[test]
    fn another_program_fails_the_designated_check() {
        if trust_service_silent() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("akid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s");
        let _ = std::fs::remove_file(&path);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}

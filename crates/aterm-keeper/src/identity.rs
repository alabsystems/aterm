// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO MAY TALK TO THE KEEPER, both ways (`docs/DESIGN-pty-keeper-2026-09-26.md`
//! §5.7, owner decision 7).
//!
//! The floor is the same uid, always: `getpeereid` on every connection, the
//! socket in a 0700 directory. Above it, by default, CODE IDENTITY: the peer's
//! audit token (`LOCAL_PEERTOKEN`, never a pid that may have been recycled)
//! must name code that satisfies THIS build's own designated requirement. The
//! keeper and the window are the same binary, so the requirement a build
//! carries is exactly the one its peers must meet:
//!
//! * the installed app carries its Developer ID requirement (identifier
//!   `com.aterm.aterm`, Team `A66A9P66Z7`; §11.1), which every future signed
//!   build also meets;
//! * a dev or ad-hoc build carries only its cdhash, so only the very same
//!   build passes — the strongest identity it has;
//! * [`IdentityPolicy::SameUid`] is the explicit floor, for tests and for a
//!   by-hand run of a build whose peers differ (a test harness talking to it).
//!
//! `aterm keeper status` says which one is in force. Security.framework is a C
//! API; the calls are the documented guest lookup
//! (`SecCodeCopyGuestWithAttributes` with `kSecGuestAttributeAudit`) and
//! `SecCodeCheckValidity` against `SecCodeCopySelf`'s designated requirement.

use aterm_uds::CtlStream;

/// Which identity a peer must prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPolicy {
    /// Same uid only.
    SameUid,
    /// Same uid AND code that satisfies this build's designated requirement.
    Designated,
}

/// The identity check in force.
#[derive(Debug)]
pub struct Identity {
    policy: IdentityPolicy,
    #[cfg(target_vendor = "apple")]
    requirement: Option<sec::Owned>,
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

    /// The identity `policy` asks for, derived from THIS process's own code.
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
            requirement: Some(requirement),
            description: format!("designated requirement: {text}"),
        })
    }

    #[cfg(not(target_vendor = "apple"))]
    fn designated() -> Result<Self, String> {
        Err("code identity is checked on macOS only; use the same-uid floor".to_string())
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

    /// Check the peer on `stream`: same uid, then (Designated) its code.
    ///
    /// # Errors
    /// Why the peer is refused.
    pub fn check(&self, stream: &CtlStream) -> Result<(), String> {
        #[cfg(unix)]
        {
            let ours = aterm_uds::peer::our_uid();
            match aterm_uds::peer::peer_uid(stream) {
                Some(uid) if uid == ours => {}
                Some(uid) => return Err(format!("peer uid {uid} is not ours")),
                None => return Err("the kernel did not name the peer's uid".to_string()),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = stream;
            return Err("the keeper is Unix-only".to_string());
        }
        #[cfg(target_vendor = "apple")]
        if let Some(req) = &self.requirement {
            let token = aterm_uds::peer::peer_audit_token(stream)
                .ok_or_else(|| "the kernel did not give the peer's audit token".to_string())?;
            sec::check_guest(&token, req)?;
        }
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
        fn SecCodeCopyGuestWithAttributes(
            host: CFTypeRef,
            attributes: CFTypeRef,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> OSStatus;
        static kSecGuestAttributeAudit: CFTypeRef;
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
        let mut req: CFTypeRef = std::ptr::null();
        // SAFETY: `me` is a live code object; an out-parameter for one reference.
        status(
            unsafe { SecCodeCopyDesignatedRequirement(me.0, 0, &mut req) },
            "SecCodeCopyDesignatedRequirement",
        )?;
        let req = owned(req, "SecCodeCopyDesignatedRequirement")?;
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
        let text = if ok != 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end]).into_owned()
        } else {
            "(unprintable)".to_string()
        };
        Ok((req, text))
    }

    /// Whether the process `token` names runs code satisfying `req`.
    // Skip: bottoms out at Security.framework FFI.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_guest(token: &aterm_uds::peer::AuditToken, req: &Owned) -> Result<(), String> {
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
        let guest = owned(guest, "SecCodeCopyGuestWithAttributes")?;
        // SAFETY: both objects are live.
        status(
            unsafe { SecCodeCheckValidity(guest.0, 0, req.0) },
            "the peer's code does not satisfy this build's requirement",
        )
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;

    /// This process satisfies its own requirement: a socketpair's peer is
    /// ourselves, and the designated check passes. The floor names itself.
    #[test]
    fn a_process_satisfies_its_own_requirement() {
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        assert!(
            id.describe().starts_with("designated requirement: "),
            "{}",
            id.describe()
        );
        let (a, _b) = CtlStream::pair().expect("pair");
        id.check(&a).expect("self passes");
        assert_eq!(Identity::same_uid().describe(), "same-uid");
        Identity::same_uid().check(&a).expect("same uid passes");
    }

    /// A different program's connection fails the designated check and passes
    /// the floor: `nc -U` dials a listener here, and its audit token names
    /// `nc`, whose code is not this test binary's.
    #[test]
    fn another_program_fails_the_designated_check() {
        let dir = std::env::temp_dir().join(format!("akid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let mut nc = std::process::Command::new("/usr/bin/nc")
            .arg("-U")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("nc");
        let (stream, _) = listener.accept().expect("accept");
        let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let refused = id.check(&stream);
        assert!(refused.is_err(), "nc passed this binary's requirement");
        Identity::same_uid().check(&stream).expect("nc is our uid");
        let _ = nc.kill();
        let _ = nc.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

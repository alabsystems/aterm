// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS / iOS: server-certificate verification straight over Security.framework.
//!
//! This is the whole of what aterm ever asked `rustls-platform-verifier` for on
//! Apple platforms: build a trust object over the presented chain, pin the
//! verification date to the instant `rustls` supplied, forbid network fetches,
//! evaluate, and translate the failure. Security.framework and CoreFoundation
//! are OS dylibs, so nothing is vendored and nothing is compiled — only the
//! entry points below are declared.
//!
//! # The call sequence, and why it is in this order
//!
//! 1. `SecCertificateCreateWithData` for the end-entity certificate FIRST, then
//!    each intermediate in the order the peer sent them. Apple requires the
//!    certificate being verified to be element 0 of the array.
//! 2. `SecPolicyCreateSSL(server: true, hostname)`. **The hostname argument is
//!    nullable and a NULL disables name checking entirely** — see
//!    [`super::PlatformVerifier`]'s module docs and `cf_string` below. Every
//!    path that cannot produce a CFString returns an error instead.
//! 3. `SecTrustCreateWithCertificates`, which is `CF_RETURNS_RETAINED`.
//! 4. `SecTrustSetVerifyDate` with `now`, so a fixture-driven test can pin the
//!    verdict and a certificate's validity window is judged against the same
//!    instant `rustls` used, not against whatever the clock says mid-handshake.
//! 5. `SecTrustSetNetworkFetchAllowed(false)`. **Deliberately stricter than
//!    `rustls-platform-verifier`, which does not call it.** Without it a chain
//!    that is missing its intermediate but carries an `authorityInfoAccess` URL
//!    can be REPAIRED by macOS downloading the issuer — turning a rejection into
//!    an acceptance, and adding a synchronous network round-trip inside a
//!    handshake that `stream.rs` has already put on a deadline.
//! 6. Extra anchors, test-only (see [`super::PlatformVerifier`]).
//! 7. The stapled OCSP response, if the peer sent one.
//! 8. `SecTrustEvaluateWithError`, which returns C `_Bool` — **not** an
//!    `OSStatus`. A verifier that followed the `OSStatus` habit of
//!    `crates/aterm-gui/src/net_connections/keychain.rs` and compared the result
//!    to `errSecSuccess` (0) would read a REJECTION (`false` == 0) as success.
//!    That mistake compiles, so the declaration below returns `bool` and the
//!    branch is written so acceptance needs an explicit `true`.
//!
//! # Memory discipline
//!
//! `SecTrustCreateWithCertificates` and `SecTrustEvaluateWithError`'s out-error
//! are both `CF_RETURNS_RETAINED` (+1). Leaking them would be an
//! attacker-triggerable leak — a hostile endpoint can drive the failure path as
//! often as it likes. Every +1 object here is wrapped in [`CfOwned`], which is
//! the only place `CFRelease` is called, exactly once, on drop. This mirrors
//! `crates/aterm-gui/src/net_connections/keychain.rs:218`.
//!
//! CHECKED, not asserted: `leaks --atExit` over the whole of `super::tests`
//! (every accept case, every reject case, both real-chain runs) reports **0
//! leaks for 0 total leaked bytes**. And the check is live rather than
//! vacuous — suppressing the single `CFRelease` of the `SecTrust` turns that
//! into 4,920 leaks / 755,488 bytes, which is the shape of the bug this
//! discipline exists to prevent.

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::{
    bad_encoding, eku_rejected, invalid_certificate, name_mismatch, revoked, unknown_issuer,
};

// ---------------------------------------------------------------------------
// Core Foundation / Security types. Every CF object is an opaque pointer; the
// only layout this module asserts is `CFArrayCallBacks`, whose ADDRESS (never
// its contents) is handed to `CFArrayCreate`.
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDataRef = *const c_void;
type CFDateRef = *const c_void;
type CFErrorRef = *const c_void;
type SecCertificateRef = *const c_void;
type SecPolicyRef = *const c_void;
type SecTrustRef = *const c_void;
type CFIndex = isize;
type CFStringEncoding = u32;
type CFAbsoluteTime = f64;
type CFTimeInterval = f64;
type OSStatus = i32;
type Boolean = u8;

/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;

// The four `OSStatus` values this module translates into a specific
// `rustls::CertificateError`. They are exactly the four that
// `rustls-platform-verifier` 0.6.2 maps (`src/verification/apple.rs`), so the
// differential oracle in `super::tests` can compare error KINDS and not only
// accept/reject. Every other code becomes an opaque rejection carrying the raw
// number — macOS reuses codes across unrelated causes (it returns
// `errSecCertificateExpired` for a NOT-YET-VALID certificate too), so inventing
// finer distinctions here would be inventing information.
/// `errSecHostNameMismatch`.
const ERR_SEC_HOST_NAME_MISMATCH: CFIndex = -67602;
/// `errSecCreateChainFailed`.
const ERR_SEC_CREATE_CHAIN_FAILED: CFIndex = -25318;
/// `errSecInvalidExtendedKeyUsage`.
const ERR_SEC_INVALID_EXTENDED_KEY_USAGE: CFIndex = -67609;
/// `errSecCertificateRevoked`.
const ERR_SEC_CERTIFICATE_REVOKED: CFIndex = -67820;

/// `CFArrayCallBacks`. Declared with real function-pointer fields rather than
/// opaque words so the type is `Sync` (an `extern` static of a
/// raw-pointer-bearing type is not) and so the layout claim is checkable against
/// `CFArray.h:78`. Only `&raw const` of the static is ever taken — the contents
/// are never read by this crate. Same reasoning, same shape, as the two
/// dictionary callback tables in `net_connections/keychain.rs:79`.
#[repr(C)]
struct CFArrayCallBacks {
    version: CFIndex,
    retain: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef) -> CFTypeRef>,
    release: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef)>,
    copy_description: Option<unsafe extern "C" fn(CFTypeRef) -> CFStringRef>,
    equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> Boolean>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// Retain/release callbacks — this is what makes a `CFArray` OWN the CF
    /// objects put into it, so the array keeps the certificates alive.
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
    /// `978307200.0` — seconds between the UNIX epoch and the CF epoch.
    static kCFAbsoluteTimeIntervalSince1970: CFTimeInterval;

    fn CFRelease(cf: CFTypeRef);

    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const CFTypeRef,
        num_values: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;

    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDateCreate(allocator: CFAllocatorRef, at: CFAbsoluteTime) -> CFDateRef;

    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: CFStringEncoding,
        is_external_representation: Boolean,
    ) -> CFStringRef;
    fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(len: CFIndex, encoding: CFStringEncoding) -> CFIndex;
    fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: CFStringEncoding,
    ) -> Boolean;

    fn CFErrorGetCode(err: CFErrorRef) -> CFIndex;
    fn CFErrorCopyDescription(err: CFErrorRef) -> CFStringRef;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCertificateCreateWithData(
        allocator: CFAllocatorRef,
        data: CFDataRef,
    ) -> SecCertificateRef;
    fn SecPolicyCreateSSL(server: Boolean, hostname: CFStringRef) -> SecPolicyRef;
    fn SecTrustCreateWithCertificates(
        certificates: CFTypeRef,
        policies: CFTypeRef,
        trust: *mut SecTrustRef,
    ) -> OSStatus;
    fn SecTrustSetVerifyDate(trust: SecTrustRef, verify_date: CFDateRef) -> OSStatus;
    fn SecTrustSetNetworkFetchAllowed(trust: SecTrustRef, allow_fetch: Boolean) -> OSStatus;
    fn SecTrustSetOCSPResponse(trust: SecTrustRef, response_data: CFTypeRef) -> OSStatus;
    fn SecTrustSetAnchorCertificates(trust: SecTrustRef, anchors: CFArrayRef) -> OSStatus;
    fn SecTrustSetAnchorCertificatesOnly(trust: SecTrustRef, only: Boolean) -> OSStatus;
    /// Returns C `_Bool`: `true` iff the chain is TRUSTED. **Not** an
    /// `OSStatus` — see this module's header. `error` is `CF_RETURNS_RETAINED`.
    fn SecTrustEvaluateWithError(trust: SecTrustRef, error: *mut CFErrorRef) -> bool;
}

// ---------------------------------------------------------------------------
// CF plumbing
// ---------------------------------------------------------------------------

/// An owned (+1) CF object, released exactly once on drop.
struct CfOwned(CFTypeRef);

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `CfOwned` is only ever constructed from a +1 reference
            // returned by a CF `Create`/`Copy` call, and this is the only
            // release of it.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Build a `CFString` from a Rust string.
///
/// `None` when CF refuses the bytes, which for well-formed UTF-8 means an
/// allocation failure. **A `None` here must never become a NULL hostname**: a
/// NULL hostname passed to `SecPolicyCreateSSL` switches name checking OFF
/// entirely and every certificate for every name starts being accepted. The
/// only caller propagates it as an error.
fn cf_string(s: &str) -> Option<CfOwned> {
    // SAFETY: the pointer/length pair describes `s`, which outlives the call;
    // `CFStringCreateWithBytes` copies. `is_external_representation` is false —
    // these bytes carry no BOM. A `&str` longer than `isize::MAX` cannot exist.
    let r = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    (!r.is_null()).then_some(CfOwned(r))
}

/// Copy a `CFString` out as a Rust `String`. Diagnostics only.
fn cf_string_to_string(s: CFStringRef) -> Option<String> {
    // SAFETY: `s` is a live CFString for the duration of these calls.
    let len = unsafe { CFStringGetLength(s) };
    let max = unsafe { CFStringGetMaximumSizeForEncoding(len, CF_STRING_ENCODING_UTF8) };
    if max <= 0 {
        return Some(String::new());
    }
    // +1 for the NUL `CFStringGetCString` always writes.
    let mut buf = vec![0u8; usize::try_from(max).ok()?.checked_add(1)?];
    // SAFETY: `buf` is valid for `buf.len()` bytes; the length is passed as the
    // buffer size so CF cannot write past the end.
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr().cast::<c_char>(),
            buf.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf.truncate(end);
    String::from_utf8(buf).ok()
}

/// A `CFArray` that OWNS (retains) the objects in `items`.
fn cf_array(items: &[CfOwned]) -> Option<CfOwned> {
    let raw: Vec<CFTypeRef> = items.iter().map(|item| item.0).collect();
    // SAFETY: `raw` is valid for `raw.len()` elements for the duration of the
    // call, every element is a live CF object, and `kCFTypeArrayCallBacks` is a
    // CF-owned immutable static whose address is all that is passed. The array
    // RETAINS each element, so it stays valid independently of `items`. A slice
    // longer than `isize::MAX` cannot exist.
    let r = unsafe {
        CFArrayCreate(
            ptr::null(),
            raw.as_ptr(),
            raw.len() as CFIndex,
            &raw const kCFTypeArrayCallBacks,
        )
    };
    (!r.is_null()).then_some(CfOwned(r))
}

/// Wrap one DER certificate as a `SecCertificate`.
///
/// `None` when the bytes are not a parseable certificate — attacker-controlled
/// input, so this is an ordinary outcome and never a panic.
fn sec_certificate(der: &[u8]) -> Option<CfOwned> {
    // SAFETY: the pointer/length pair describes `der`, which outlives the call;
    // `CFDataCreate` copies. A slice longer than `isize::MAX` cannot exist.
    let data = unsafe { CFDataCreate(ptr::null(), der.as_ptr(), der.len() as CFIndex) };
    if data.is_null() {
        return None;
    }
    let data = CfOwned(data);
    // SAFETY: `data` is a live CFData holding the DER for the duration of the
    // call; `SecCertificateCreateWithData` copies what it needs and returns
    // NULL (not a diagnostic) when the DER does not parse.
    let cert = unsafe { SecCertificateCreateWithData(ptr::null(), data.0) };
    (!cert.is_null()).then_some(CfOwned(cert))
}

// ---------------------------------------------------------------------------
// The verifier
// ---------------------------------------------------------------------------

/// Apple-platform chain verification.
///
/// `extra_roots` holds DER, not live `SecCertificateRef`s, on purpose: nothing
/// CF-allocated is shared between threads, so this type needs no `unsafe impl
/// Send`/`Sync`, and the shipped path (`extra_roots` empty) does no CF work for
/// it at all. The DER is validated once at construction so a bad anchor is
/// reported there rather than on the first handshake.
#[derive(Debug)]
pub(super) struct Verifier {
    extra_roots: Vec<Vec<u8>>,
}

impl Verifier {
    /// `extra_roots` is EMPTY on every shipped path; see [`super::PlatformVerifier`].
    ///
    /// `_provider` is unused here — Security.framework brings its own crypto —
    /// but every arm takes the same two arguments so `super::PlatformVerifier`
    /// has ONE constructor call rather than a `#[cfg]` fork per platform.
    pub(super) fn new(
        extra_roots: Vec<CertificateDer<'static>>,
        _provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        for root in &extra_roots {
            // Reject an unusable anchor at construction, like the incumbent does.
            if sec_certificate(root.as_ref()).is_none() {
                return Err(bad_encoding());
            }
        }
        Ok(Self {
            extra_roots: extra_roots.iter().map(|r| r.as_ref().to_vec()).collect(),
        })
    }

    /// The presented chain, judged by Security.framework.
    ///
    /// Returns `Ok(())` ONLY when `SecTrustEvaluateWithError` explicitly
    /// reported the chain trusted. Every other outcome — an FFI call that
    /// failed, a certificate CF would not parse, a hostname CF would not encode
    /// — returns `Err`.
    pub(super) fn verify(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        server_text: &str,
        ocsp_response: Option<&[u8]>,
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        // (1) End-entity FIRST, then the intermediates in the peer's order.
        let mut certs = Vec::with_capacity(1 + intermediates.len());
        for der in std::iter::once(end_entity.as_ref())
            .chain(intermediates.iter().map(CertificateDer::as_ref))
        {
            certs.push(sec_certificate(der).ok_or_else(bad_encoding)?);
        }
        let cert_array = cf_array(&certs)
            .ok_or_else(|| invalid_certificate("CFArrayCreate refused the certificate chain"))?;

        // (2) The SSL policy, carrying the name the certificate must match.
        // `cf_string` returning `None` is an ERROR, never a NULL hostname.
        let hostname = cf_string(server_text).ok_or_else(|| {
            invalid_certificate("the server name could not be encoded for the trust policy")
        })?;
        // SAFETY: `hostname` is a live, NON-NULL CFString for the duration of
        // the call; `SecPolicyCreateSSL` copies it. `server: true` selects the
        // server-authentication policy (we are the client).
        let policy = unsafe { SecPolicyCreateSSL(1, hostname.0) };
        if policy.is_null() {
            return Err(invalid_certificate("SecPolicyCreateSSL returned no policy"));
        }
        let policy = CfOwned(policy);
        let policy_array = cf_array(std::slice::from_ref(&policy))
            .ok_or_else(|| invalid_certificate("CFArrayCreate refused the trust policy"))?;

        // (3) The trust object. `CF_RETURNS_RETAINED`.
        let mut raw_trust: SecTrustRef = ptr::null();
        // SAFETY: both arrays are live CFArrays of the right element type, and
        // `raw_trust` is a valid place to write one `SecTrustRef`.
        let status =
            unsafe { SecTrustCreateWithCertificates(cert_array.0, policy_array.0, &mut raw_trust) };
        if status != 0 || raw_trust.is_null() {
            return Err(invalid_certificate(format!(
                "SecTrustCreateWithCertificates failed: {status}"
            )));
        }
        let trust = CfOwned(raw_trust);

        // (4) Pin the verification date to the instant rustls supplied.
        // SAFETY: an immutable global `double` defined by CoreFoundation.
        let unix_adjustment = unsafe { kCFAbsoluteTimeIntervalSince1970 } as u64;
        let cf_epoch = now
            .as_secs()
            .checked_sub(unix_adjustment)
            .ok_or(rustls::Error::FailedToGetCurrentTime)?;
        // SAFETY: `CFDateCreate` with the default allocator; any finite double
        // is a valid `CFAbsoluteTime`.
        let date = unsafe { CFDateCreate(ptr::null(), cf_epoch as CFAbsoluteTime) };
        if date.is_null() {
            return Err(invalid_certificate("CFDateCreate refused the verify date"));
        }
        let date = CfOwned(date);
        // SAFETY: `trust` and `date` are live objects of the declared types.
        let status = unsafe { SecTrustSetVerifyDate(trust.0, date.0) };
        if status != 0 {
            return Err(invalid_certificate(format!(
                "SecTrustSetVerifyDate failed: {status}"
            )));
        }

        // (5) No network, ever, from inside a handshake. See the header.
        // SAFETY: `trust` is a live SecTrust; the flag is a `Boolean`.
        let status = unsafe { SecTrustSetNetworkFetchAllowed(trust.0, 0) };
        if status != 0 {
            return Err(invalid_certificate(format!(
                "SecTrustSetNetworkFetchAllowed failed: {status}"
            )));
        }

        // (6) Test-only extra anchors. EMPTY on every shipped path, so this
        // whole branch is dead in a real build.
        //
        // `_anchors` and `_anchor_array` are bound at FUNCTION scope, not inside
        // the `if`. `SecTrustSetAnchorCertificates` is documented to retain what
        // it is given, and the incumbent drops its array immediately — but a
        // use-after-free here would be invisible in a passing test, so the
        // objects are simply kept alive until the evaluation is over. Same for
        // the OCSP data below. It costs nothing and removes the question.
        let mut _anchors: Vec<CfOwned> = Vec::new();
        let mut _anchor_array: Option<CfOwned> = None;
        let mut _ocsp_data: Option<CfOwned> = None;
        let mut _ocsp_array: Option<CfOwned> = None;
        if !self.extra_roots.is_empty() {
            let mut anchors = Vec::with_capacity(self.extra_roots.len());
            for der in &self.extra_roots {
                anchors.push(sec_certificate(der).ok_or_else(bad_encoding)?);
            }
            let anchor_array = cf_array(&anchors)
                .ok_or_else(|| invalid_certificate("CFArrayCreate refused the extra anchors"))?;
            // SAFETY: `trust` is live; `anchor_array` is a live CFArray of
            // SecCertificate, which is what this call requires.
            let status = unsafe { SecTrustSetAnchorCertificates(trust.0, anchor_array.0) };
            _anchors = anchors;
            _anchor_array = Some(anchor_array);
            if status != 0 {
                return Err(invalid_certificate(format!(
                    "SecTrustSetAnchorCertificates failed: {status}"
                )));
            }
            // `SecTrustSetAnchorCertificates` DISABLES every other anchor by
            // default. `false` restores the system roots alongside the extras —
            // matching `rustls-platform-verifier`, so the two implementations
            // agree on chains a real system root would have validated.
            // SAFETY: `trust` is live; the flag is a `Boolean`.
            let status = unsafe { SecTrustSetAnchorCertificatesOnly(trust.0, 0) };
            if status != 0 {
                return Err(invalid_certificate(format!(
                    "SecTrustSetAnchorCertificatesOnly failed: {status}"
                )));
            }
        }

        // (7) A stapled OCSP response, if the peer sent one. Passed as a
        // CFArray of CFData, which is the shape Security.framework documents
        // and the shape the incumbent used.
        if let Some(ocsp) = ocsp_response {
            // SAFETY: pointer/length describe `ocsp`, which outlives the call;
            // `CFDataCreate` copies. A slice longer than `isize::MAX` cannot exist.
            let data = unsafe { CFDataCreate(ptr::null(), ocsp.as_ptr(), ocsp.len() as CFIndex) };
            if data.is_null() {
                return Err(invalid_certificate(
                    "CFDataCreate refused the stapled OCSP response",
                ));
            }
            let data = CfOwned(data);
            let ocsp_array = cf_array(std::slice::from_ref(&data)).ok_or_else(|| {
                invalid_certificate("CFArrayCreate refused the stapled OCSP response")
            })?;
            // SAFETY: `trust` is live; `ocsp_array` is a live CFArray of CFData.
            let status = unsafe { SecTrustSetOCSPResponse(trust.0, ocsp_array.0) };
            _ocsp_data = Some(data);
            _ocsp_array = Some(ocsp_array);
            if status != 0 {
                return Err(invalid_certificate(format!(
                    "SecTrustSetOCSPResponse failed: {status}"
                )));
            }
        }

        // (8) Evaluate. `trusted` is C `_Bool`, NOT an OSStatus.
        let mut raw_error: CFErrorRef = ptr::null();
        // SAFETY: `trust` is a live, fully configured SecTrust and `raw_error`
        // is a valid place to write one `CFErrorRef`. The out-error is +1 when
        // written, which is why it is wrapped below before any early return.
        let trusted = unsafe { SecTrustEvaluateWithError(trust.0, &mut raw_error) };
        let error = (!raw_error.is_null()).then(|| CfOwned(raw_error));

        // ACCEPTANCE NEEDS AN EXPLICIT `true`. The extra `error.is_none()` is
        // defence in depth, not a documented case: Apple leaves the out-error
        // untouched on success, so a `true` alongside a populated CFError would
        // mean the contract is not what we think it is — and the safe reading of
        // "I do not understand this result" is to reject.
        if trusted && error.is_none() {
            return Ok(());
        }
        if trusted {
            return Err(invalid_certificate(
                "SecTrustEvaluateWithError reported success AND an error; refusing the chain",
            ));
        }

        let Some(error) = error else {
            // A rejection with no CFError. Still a rejection.
            return Err(invalid_certificate(
                "SecTrustEvaluateWithError refused the chain without a reason",
            ));
        };
        // SAFETY: `error` is a live CFError for the duration of both calls.
        let code = unsafe { CFErrorGetCode(error.0) };
        let description = {
            // SAFETY: as above; `CFErrorCopyDescription` returns a +1 CFString
            // or NULL.
            let raw = unsafe { CFErrorCopyDescription(error.0) };
            if raw.is_null() {
                None
            } else {
                // Named binding, not a temporary: the CFString must still be
                // alive while it is being copied out.
                let owned = CfOwned(raw);
                cf_string_to_string(owned.0)
            }
        };

        Err(match code {
            ERR_SEC_HOST_NAME_MISMATCH => name_mismatch(),
            ERR_SEC_CREATE_CHAIN_FAILED => unknown_issuer(),
            ERR_SEC_INVALID_EXTENDED_KEY_USAGE => eku_rejected(),
            ERR_SEC_CERTIFICATE_REVOKED => revoked(),
            other => invalid_certificate(match description {
                Some(text) => format!("{text}: {other}"),
                None => format!("certificate rejected by the system: {other}"),
            }),
        })
    }
}

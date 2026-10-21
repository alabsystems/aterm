// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS / iOS: server-certificate verification straight over Security.framework.
//! It and CoreFoundation are OS dylibs; only the entry points below are declared.
//!
//! The call sequence, in order:
//!
//! 1. `SecCertificateCreateWithData` for the end-entity certificate first, then
//!    each intermediate in the peer's order: Apple requires the certificate
//!    being verified at element 0.
//! 2. `SecPolicyCreateSSL(server: true, hostname)`. A NULL hostname disables
//!    name checking entirely, so every path that cannot produce a CFString
//!    (`cf_string`) returns an error instead.
//! 3. `SecTrustCreateWithCertificates` (`CF_RETURNS_RETAINED`).
//! 4. `SecTrustSetVerifyDate` with `now`, so validity is judged at the instant
//!    `rustls` used and a fixture-driven test can pin the verdict.
//! 5. `SecTrustSetNetworkFetchAllowed(false)`, stricter than
//!    `rustls-platform-verifier`: otherwise macOS can repair a chain missing its
//!    intermediate by downloading the issuer from `authorityInfoAccess`, turning
//!    a rejection into an acceptance and putting a network round-trip inside a
//!    deadlined handshake.
//! 6. Extra anchors, test-only (see [`super::PlatformVerifier`]).
//! 7. The stapled OCSP response, if the peer sent one.
//! 8. `SecTrustEvaluateWithError`, which returns C `_Bool` — **not** an
//!    `OSStatus`. A verifier that followed the `OSStatus` habit of the other
//!    `Sec*` calls and compared the result to `errSecSuccess` (0) would read a
//!    REJECTION (`false` == 0) as success.
//!    That mistake compiles, so the declaration below returns `bool` and the
//!    branch is written so acceptance needs an explicit `true`.
//!
//! # A deadline on the evaluation
//!
//! Step 8 is a synchronous round-trip to `trustd`, and when that agent stops
//! answering it never returns (observed), with every handshake in the process
//! parked behind it and nothing able to interrupt it. So steps 1-8
//! run on a helper thread, over an owned copy of the inputs, and the caller
//! waits at most [`EVALUATION_DEADLINE`]. Past it the chain is REFUSED with
//! [`super::is_service_unanswered`]'s error, and once an evaluation has been
//! running longer than [`SUSPECT_AFTER`] every later one is refused at once
//! with the same error instead of starting another thread that would stick
//! too. When `trustd` answers, the decision is exactly what it was before: the
//! same calls, in the same order, on another thread. [`aterm_uds::deadline`]
//! has the mechanism and why it is sound; its one [`TRUST`] gate is shared with
//! every other call in the process that waits on `trustd` (the PTY keeper's
//! code checks), so a wedge any of them finds refuses all of them at once.
//!
//! # Memory discipline
//!
//! `SecTrustCreateWithCertificates` and `SecTrustEvaluateWithError`'s out-error
//! are both `CF_RETURNS_RETAINED` (+1). Leaking them would be an
//! attacker-triggerable leak — a hostile endpoint can drive the failure path as
//! often as it likes. Every +1 object here is wrapped in [`CfOwned`], which is
//! the only place `CFRelease` is called, exactly once, on drop.
//!
//! `SecTrustCreateWithCertificates` and the out-error of
//! `SecTrustEvaluateWithError` are +1; a hostile endpoint can drive the failure
//! path at will, so a leak there is attacker-triggerable. Every +1 object is a
//! [`CfOwned`], the only place `CFRelease` is called, once, on drop.

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use std::time::Duration;

use super::{
    bad_encoding, eku_rejected, invalid_certificate, name_mismatch, revoked, service_unanswered,
    unknown_issuer,
};
use aterm_uds::deadline::{Refused, TRUST};

// Core Foundation / Security types: every CF object is an opaque pointer.

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

// The four `OSStatus` codes translated to a specific `rustls::CertificateError`:
// the four `rustls-platform-verifier` maps, so the tests can assert error kinds.
// Every other code is an opaque rejection carrying the number.
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
/// are never read by this crate.
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
    /// Retain/release callbacks: what makes a `CFArray` own the CF objects put
    /// into it, so the array keeps the certificates alive.
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
    /// Returns C `_Bool`: `true` iff the chain is trusted. Not an `OSStatus`
    /// (see the module docs). `error` is `CF_RETURNS_RETAINED`.
    fn SecTrustEvaluateWithError(trust: SecTrustRef, error: *mut CFErrorRef) -> bool;
}

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

/// Build a `CFString` from a Rust string; `None` when CF refuses the bytes.
///
/// A `None` must never become a NULL hostname: NULL passed to
/// `SecPolicyCreateSSL` switches name checking off entirely. The only caller
/// propagates it as an error.
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
    (!r.is_null()).then(|| CfOwned(r))
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

/// A `CFArray` that owns (retains) the objects in `items`.
fn cf_array(items: &[CfOwned]) -> Option<CfOwned> {
    let raw: Vec<CFTypeRef> = items.iter().map(|item| item.0).collect();
    // SAFETY: `raw` is valid for `raw.len()` elements for the duration of the
    // call, every element is a live CF object, and `kCFTypeArrayCallBacks` is a
    // CF-owned immutable static whose address is all that is passed. The array
    // retains each element, so it stays valid independently of `items`. A slice
    // longer than `isize::MAX` cannot exist.
    let r = unsafe {
        CFArrayCreate(
            ptr::null(),
            raw.as_ptr(),
            raw.len() as CFIndex,
            &raw const kCFTypeArrayCallBacks,
        )
    };
    (!r.is_null()).then(|| CfOwned(r))
}

/// Wrap one DER certificate as a `SecCertificate`; `None` when the
/// (attacker-controlled) bytes do not parse.
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
    (!cert.is_null()).then(|| CfOwned(cert))
}

/// How long one certificate evaluation may take before it is refused.
///
/// A healthy evaluation is local to `trustd` (no network fetch, step 5) and
/// takes milliseconds; the slowest honest case, `trustd` launched on demand on
/// a loaded machine, a second or two. Thirty seconds is ten times that and half
/// the minute past which a wait reads as a hang. Only calls started before
/// [`SUSPECT_AFTER`] ran out ever wait it.
const EVALUATION_DEADLINE: Duration = Duration::from_secs(30);

/// How long an evaluation may run before `trustd` is taken to be not
/// answering, and later evaluations are refused at once instead of each
/// starting a thread that would stick behind it. Over twice the slowest honest
/// evaluation [`EVALUATION_DEADLINE`] names; the calls already running keep
/// their full deadline.
const SUSPECT_AFTER: Duration = Duration::from_secs(5);

/// Apple-platform chain verification.
///
/// `extra_roots` holds DER, not live `SecCertificateRef`s, so nothing
/// CF-allocated crosses threads and the type needs no `unsafe impl Send`/`Sync`.
#[derive(Debug)]
pub(super) struct Verifier {
    extra_roots: Arc<[Vec<u8>]>,
}

/// One evaluation's inputs, owned, so the helper thread that evaluates them
/// borrows nothing from the caller and can outlive it (see
/// [`aterm_uds::deadline`]). Every CF object is created and released inside
/// [`evaluate`], on the helper; none crosses a thread.
struct Job {
    /// End-entity first, then the intermediates in the peer's order.
    chain: Vec<Vec<u8>>,
    server_text: String,
    ocsp_response: Option<Vec<u8>>,
    now: UnixTime,
    extra_roots: Arc<[Vec<u8>]>,
}

impl Verifier {
    /// `extra_roots` is empty on every shipped path. `_provider` is unused
    /// (Security.framework brings its own crypto); every arm takes the same
    /// arguments so `PlatformVerifier::build` needs no `#[cfg]` fork.
    pub(super) fn new(
        extra_roots: Vec<CertificateDer<'static>>,
        _provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        for root in &extra_roots {
            // Reject an unusable anchor at construction, not at first handshake.
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
    /// `Ok(())` only when `SecTrustEvaluateWithError` explicitly reported the
    /// chain trusted; a failed FFI call, an unparseable certificate, an
    /// unencodable hostname or an unanswered `trustd` is `Err`.
    pub(super) fn verify(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        server_text: &str,
        ocsp_response: Option<&[u8]>,
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        let job = Job {
            chain: std::iter::once(end_entity)
                .chain(intermediates)
                .map(|der| der.as_ref().to_vec())
                .collect(),
            server_text: server_text.to_owned(),
            ocsp_response: ocsp_response.map(<[u8]>::to_vec),
            now,
            extra_roots: Arc::clone(&self.extra_roots),
        };
        // The process's one `trustd` gate: a wedge is detected once, then every
        // later evaluation is refused at once.
        match TRUST.run(SUSPECT_AFTER, EVALUATION_DEADLINE, move || evaluate(&job)) {
            Ok(verdict) => verdict,
            Err(Refused::Unanswered) => Err(service_unanswered()),
            // `Other`, not `InvalidCertificate`: the check never ran, so nothing
            // is known about the certificate. Still a refusal.
            Err(Refused::Failed(why)) => Err(rustls::Error::Other(rustls::OtherError(Arc::new(
                std::io::Error::other(format!("the certificate check could not run: {why}")),
            )))),
        }
    }
}

/// The evaluation itself: steps 1-8 of this module's header, in order. Runs on
/// the helper thread [`Verifier::verify`] starts, over inputs it owns.
fn evaluate(job: &Job) -> Result<(), rustls::Error> {
    let Job {
        chain,
        server_text,
        ocsp_response,
        now,
        extra_roots,
    } = job;
    // (1) The chain, end-entity first.
    let mut certs = Vec::with_capacity(chain.len());
    for der in chain {
        certs.push(sec_certificate(der).ok_or_else(bad_encoding)?);
    }
    let cert_array = cf_array(&certs)
        .ok_or_else(|| invalid_certificate("CFArrayCreate refused the certificate chain"))?;

    // (2) The SSL policy, carrying the name the certificate must match.
    // `cf_string` returning `None` is an error, never a NULL hostname.
    let hostname = cf_string(server_text).ok_or_else(|| {
        invalid_certificate("the server name could not be encoded for the trust policy")
    })?;
    // SAFETY: `hostname` is a live, non-null CFString for the duration of
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

    // (6) Test-only extra anchors; empty on every shipped path. The anchor and
    // OCSP objects are bound at function scope so they outlive the evaluation
    // whatever the setters retain.
    let mut _anchors: Vec<CfOwned> = Vec::new();
    let mut _anchor_array: Option<CfOwned> = None;
    let mut _ocsp_data: Option<CfOwned> = None;
    let mut _ocsp_array: Option<CfOwned> = None;
    if !extra_roots.is_empty() {
        let mut anchors = Vec::with_capacity(extra_roots.len());
        for der in extra_roots.iter() {
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
        // `SecTrustSetAnchorCertificates` disables every other anchor by
        // default; `false` restores the system roots alongside the extras.
        // SAFETY: `trust` is live; the flag is a `Boolean`.
        let status = unsafe { SecTrustSetAnchorCertificatesOnly(trust.0, 0) };
        if status != 0 {
            return Err(invalid_certificate(format!(
                "SecTrustSetAnchorCertificatesOnly failed: {status}"
            )));
        }
    }

    // (7) A stapled OCSP response, if the peer sent one. Passed as a
    // CFArray of CFData, the shape Security.framework documents.
    if let Some(ocsp) = ocsp_response.as_deref() {
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

    // (8) Evaluate. `trusted` is C `_Bool`, not an OSStatus.
    let mut raw_error: CFErrorRef = ptr::null();
    // SAFETY: `trust` is a live, fully configured SecTrust and `raw_error`
    // is a valid place to write one `CFErrorRef`. The out-error is +1 when
    // written, which is why it is wrapped below before any early return.
    let trusted = unsafe { SecTrustEvaluateWithError(trust.0, &mut raw_error) };
    let error = (!raw_error.is_null()).then(|| CfOwned(raw_error));

    // Acceptance needs an explicit `true`. Apple leaves the out-error untouched
    // on success, so `true` with a CFError is a contract we do not understand:
    // reject.
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

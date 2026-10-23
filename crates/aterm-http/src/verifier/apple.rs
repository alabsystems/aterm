// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS / iOS: server-certificate verification straight over Security.framework.

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
/// `CFArray.h:78`.
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
    /// (see the module docs).
    fn SecTrustEvaluateWithError(trust: SecTrustRef, error: *mut CFErrorRef) -> bool;
}

/// An owned (+1) CF object, released exactly once on drop.
struct CfOwned(CFTypeRef);

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `CfOwned` is only ever constructed from a +1 reference returned by a CF `Create`/`Copy` call, and this is the only release of it.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Build a `CFString` from a Rust string; `None` when CF refuses the bytes.
fn cf_string(s: &str) -> Option<CfOwned> {
    // SAFETY: the pointer/length pair describes `s`, which outlives the call; `CFStringCreateWithBytes` copies.
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
    let mut buf = vec![0u8; usize::try_from(max).ok()?.checked_add(1)?];
    // SAFETY: `buf` is valid for `buf.len()` bytes; the length is passed as the buffer size so CF cannot write past the end.
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
    // SAFETY: `raw` is valid for `raw.len()` elements for the duration of the call, every element is a live CF object, and `kCFTypeArrayCallBacks` is a CF-owned immutable static whose address is all that is passed.
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
    // SAFETY: the pointer/length pair describes `der`, which outlives the call; `CFDataCreate` copies.
    let data = unsafe { CFDataCreate(ptr::null(), der.as_ptr(), der.len() as CFIndex) };
    if data.is_null() {
        return None;
    }
    let data = CfOwned(data);
    // SAFETY: `data` is a live CFData holding the DER for the duration of the call; `SecCertificateCreateWithData` copies what it needs and returns NULL (not a diagnostic) when the DER does not parse.
    let cert = unsafe { SecCertificateCreateWithData(ptr::null(), data.0) };
    (!cert.is_null()).then(|| CfOwned(cert))
}

/// How long one certificate evaluation may take before it is refused.
const EVALUATION_DEADLINE: Duration = Duration::from_secs(30);

/// How long an evaluation may run before `trustd` is taken to be not
/// answering, and later evaluations are refused at once instead of each
/// starting a thread that would stick behind it.
const SUSPECT_AFTER: Duration = Duration::from_secs(5);

/// Apple-platform chain verification.
#[derive(Debug)]
pub(super) struct Verifier {
    extra_roots: Arc<[Vec<u8>]>,
}

/// One evaluation's inputs, owned, so the helper thread that evaluates them
/// borrows nothing from the caller and can outlive it (see
/// [`aterm_uds::deadline`]).
struct Job {
    /// End-entity first, then the intermediates in the peer's order.
    chain: Vec<Vec<u8>>,
    server_text: String,
    ocsp_response: Option<Vec<u8>>,
    now: UnixTime,
    extra_roots: Arc<[Vec<u8>]>,
}

impl Verifier {
    /// `extra_roots` is empty on every shipped path.
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
        match TRUST.run(SUSPECT_AFTER, EVALUATION_DEADLINE, move || evaluate(&job)) {
            Ok(verdict) => verdict,
            Err(Refused::Unanswered) => Err(service_unanswered()),
            Err(Refused::Failed(why)) => Err(rustls::Error::Other(rustls::OtherError(Arc::new(
                std::io::Error::other(format!("the certificate check could not run: {why}")),
            )))),
        }
    }
}

/// The evaluation itself: steps 1-8 of this module's header, in order.
fn evaluate(job: &Job) -> Result<(), rustls::Error> {
    let Job {
        chain,
        server_text,
        ocsp_response,
        now,
        extra_roots,
    } = job;
    let mut certs = Vec::with_capacity(chain.len());
    for der in chain {
        certs.push(sec_certificate(der).ok_or_else(bad_encoding)?);
    }
    let cert_array = cf_array(&certs)
        .ok_or_else(|| invalid_certificate("CFArrayCreate refused the certificate chain"))?;

    let hostname = cf_string(server_text).ok_or_else(|| {
        invalid_certificate("the server name could not be encoded for the trust policy")
    })?;
    // SAFETY: `hostname` is a live, non-null CFString for the duration of the call; `SecPolicyCreateSSL` copies it.
    let policy = unsafe { SecPolicyCreateSSL(1, hostname.0) };
    if policy.is_null() {
        return Err(invalid_certificate("SecPolicyCreateSSL returned no policy"));
    }
    let policy = CfOwned(policy);
    let policy_array = cf_array(std::slice::from_ref(&policy))
        .ok_or_else(|| invalid_certificate("CFArrayCreate refused the trust policy"))?;

    let mut raw_trust: SecTrustRef = ptr::null();
    // SAFETY: both arrays are live CFArrays of the right element type, and `raw_trust` is a valid place to write one `SecTrustRef`.
    let status =
        unsafe { SecTrustCreateWithCertificates(cert_array.0, policy_array.0, &mut raw_trust) };
    if status != 0 || raw_trust.is_null() {
        return Err(invalid_certificate(format!(
            "SecTrustCreateWithCertificates failed: {status}"
        )));
    }
    let trust = CfOwned(raw_trust);

    // SAFETY: an immutable global `double` defined by CoreFoundation.
    let unix_adjustment = unsafe { kCFAbsoluteTimeIntervalSince1970 } as u64;
    let cf_epoch = now
        .as_secs()
        .checked_sub(unix_adjustment)
        .ok_or(rustls::Error::FailedToGetCurrentTime)?;
    // SAFETY: `CFDateCreate` with the default allocator; any finite double is a valid `CFAbsoluteTime`.
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

    // SAFETY: `trust` is a live SecTrust; the flag is a `Boolean`.
    let status = unsafe { SecTrustSetNetworkFetchAllowed(trust.0, 0) };
    if status != 0 {
        return Err(invalid_certificate(format!(
            "SecTrustSetNetworkFetchAllowed failed: {status}"
        )));
    }

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
        // SAFETY: `trust` is live; `anchor_array` is a live CFArray of SecCertificate, which is what this call requires.
        let status = unsafe { SecTrustSetAnchorCertificates(trust.0, anchor_array.0) };
        _anchors = anchors;
        _anchor_array = Some(anchor_array);
        if status != 0 {
            return Err(invalid_certificate(format!(
                "SecTrustSetAnchorCertificates failed: {status}"
            )));
        }
        // SAFETY: `trust` is live; the flag is a `Boolean`.
        let status = unsafe { SecTrustSetAnchorCertificatesOnly(trust.0, 0) };
        if status != 0 {
            return Err(invalid_certificate(format!(
                "SecTrustSetAnchorCertificatesOnly failed: {status}"
            )));
        }
    }

    if let Some(ocsp) = ocsp_response.as_deref() {
        // SAFETY: pointer/length describe `ocsp`, which outlives the call; `CFDataCreate` copies.
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

    let mut raw_error: CFErrorRef = ptr::null();
    // SAFETY: `trust` is a live, fully configured SecTrust and `raw_error` is a valid place to write one `CFErrorRef`.
    let trusted = unsafe { SecTrustEvaluateWithError(trust.0, &mut raw_error) };
    let error = (!raw_error.is_null()).then(|| CfOwned(raw_error));

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
        // SAFETY: as above; `CFErrorCopyDescription` returns a +1 CFString or NULL.
        let raw = unsafe { CFErrorCopyDescription(error.0) };
        if raw.is_null() {
            None
        } else {
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

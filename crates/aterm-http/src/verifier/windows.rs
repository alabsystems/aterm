// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows: chain building and SSL policy verification over `crypt32`.
//!
//! # NOT EXECUTED ANYWHERE YET — read this before trusting it
//!
//! Every claim in this file comes from binding and header inspection plus a
//! cross-target `cargo check`, done on macOS. **No Windows behaviour has been
//! observed.** The differential oracle in [`super::tests`] cannot reach this
//! arm; it only runs where the incumbent's Apple arm runs. Treat this as
//! reviewed-and-compiled, not as verified: the accept/reject matrix that the
//! Apple arm has been driven through has no counterpart here until someone runs
//! it on Windows.
//!
//! # The two traps this code is written around
//!
//! 1. **`BOOL` is not the verdict.** `CertGetCertificateChain` and
//!    `CertVerifyCertificateChainPolicy` both return `BOOL` meaning "the API
//!    call itself worked". The trust decision is
//!    `CERT_CHAIN_POLICY_STATUS.dwError`, and a chain that Windows rejects still
//!    comes back with `BOOL == TRUE`. A verifier that checks only the return
//!    value accepts every forged chain — and it compiles. Below, acceptance
//!    requires `status.dwError == 0` reached through an explicit `TRUE` check on
//!    both calls; every other path returns `Err`.
//! 2. **`szOID_PKIX_KP_SERVER_AUTH` must be NUL-terminated** when it goes into
//!    `CERT_USAGE_MATCH` (rustls-platform-verifier issue #126). In
//!    `windows-sys` 0.61.2 the constant is
//!    `windows_sys::core::s!("1.3.6.1.5.5.7.3.1")`, and that macro is
//!    `concat!($s, '\0').as_ptr()` — already terminated. Verified by reading
//!    `windows-sys-0.61.2/src/core/literals.rs:3`. If this crate's `windows-sys`
//!    requirement ever moves, re-check that line before anything else.
//!
//! # `CERT_CHAIN_PARA` is used as `windows-sys` declares it
//!
//! `rustls-platform-verifier` redefines this struct locally and pointer-casts
//! it in. That is not a workaround for a truncated binding: `windows-sys`
//! 0.61.2 already declares all nine fields through `dwStrongSignFlags`
//! (`Win32/Security/Cryptography/mod.rs:2106`). The redefinition exists so the
//! crate can drop the last two fields on `target_vendor = "win7"`. aterm does
//! not support Windows 7, so the upstream binding is used directly and `cbSize`
//! is `size_of::<CERT_CHAIN_PARA>()` — the value the OS reads to decide which
//! fields are present.
//!
//! # Revocation and network fetching: the incumbent's behaviour, preserved
//!
//! The chain flags below are exactly `rustls-platform-verifier`'s:
//! end-entity-only revocation, an accumulated retrieval timeout, and an
//! end-cert cache, with `dwUrlRetrievalTimeout` at 10 s. This arm therefore MAY
//! make a bounded network request during verification, which the Apple arm
//! explicitly forbids with `SecTrustSetNetworkFetchAllowed(false)`.
//!
//! That asymmetry is deliberate and is a decision, not an oversight. On Windows
//! the revocation checking IS the URL retrieval — `CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL`
//! would remove the fetch and the revocation checking together. Trading away
//! revocation on a platform nobody here can test, in the same change that
//! reimplements the verifier, is the wrong risk to take; behaviour-preserving is
//! the right default until a Windows-capable reviewer measures the alternative.
//! Recorded so the next reader sees a choice rather than an inconsistency.

use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use windows_sys::Win32::Foundation::{FILETIME, TRUE};
use windows_sys::Win32::Security::Cryptography::{
    AUTHTYPE_SERVER, CERT_CHAIN_CACHE_END_CERT, CERT_CHAIN_CONTEXT, CERT_CHAIN_ENGINE_CONFIG,
    CERT_CHAIN_PARA, CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS, CERT_CHAIN_POLICY_PARA,
    CERT_CHAIN_POLICY_SSL, CERT_CHAIN_POLICY_STATUS, CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT,
    CERT_CHAIN_REVOCATION_CHECK_END_CERT, CERT_CONTEXT, CERT_OCSP_RESPONSE_PROP_ID,
    CERT_SET_PROPERTY_IGNORE_PERSIST_ERROR_FLAG, CERT_STORE_ADD_ALWAYS,
    CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG, CERT_STORE_PROV_MEMORY,
    CERT_TRUST_IS_PARTIAL_CHAIN, CERT_USAGE_MATCH, CRYPT_INTEGER_BLOB, CTL_USAGE,
    CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateChainEngine,
    CertFreeCertificateChain, CertFreeCertificateChainEngine, CertFreeCertificateContext,
    CertGetCertificateChain, CertOpenStore, CertSetCertificateContextProperty,
    CertVerifyCertificateChainPolicy, HCERTCHAINENGINE, HCERTSTORE, HTTPSPolicyCallbackData,
    USAGE_MATCH_TYPE_AND, X509_ASN_ENCODING, szOID_PKIX_KP_SERVER_AUTH,
};
use windows_sys::core::{PCSTR, PSTR};

use super::{
    bad_encoding, eku_rejected, invalid_certificate, name_mismatch, revoked, unknown_issuer,
};

/// Seconds between 1601-01-01 (the Windows epoch) and 1970-01-01.
const UNIX_TO_WINDOWS_SECS: u64 = 11_644_473_600;
/// `FILETIME` counts 100-nanosecond intervals.
const INTERVALS_PER_SEC: u64 = 10_000_000;

// The five `dwError` values this module translates into a specific
// `rustls::CertificateError` — the same five `rustls-platform-verifier` maps.
const CERT_E_EXPIRED: i32 = -2146762495; // 0x800B0101
const CERT_E_UNTRUSTEDROOT: i32 = -2146762487; // 0x800B0109
const CERT_E_CN_NO_MATCH: i32 = -2146762481; // 0x800B010F
const CERT_E_WRONG_USAGE: i32 = -2146762480; // 0x800B0110
const CERT_E_INVALID_NAME: i32 = -2146762476; // 0x800B0114
const CRYPT_E_REVOKED: i32 = -2146885616; // 0x80092010

/// The last OS error, as a rustls error. Used only where a `crypt32` call
/// reported failure, so the resulting `Err` is a REJECTION either way.
fn last_os_error(what: &str) -> rustls::Error {
    invalid_certificate(format!("{what}: {}", std::io::Error::last_os_error()))
}

/// An in-memory certificate store, closed exactly once on drop.
///
/// Opened with `CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG`, which is what
/// makes it sound for a [`CertContext`] to outlive this handle.
struct Store(HCERTSTORE);

impl Store {
    fn new() -> Result<Self, rustls::Error> {
        // SAFETY: `CERT_STORE_PROV_MEMORY` takes no provider and no `pvPara`;
        // the two zeroed arguments are the documented values for it. The result
        // is checked for null before it is used.
        let handle = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_MEMORY,
                0,
                0,
                CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG,
                ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(last_os_error("CertOpenStore failed"));
        }
        Ok(Self(handle))
    }

    /// Add one DER certificate, returning its context.
    fn add(&self, der: &[u8]) -> Result<CertContext, rustls::Error> {
        let len = u32::try_from(der.len()).map_err(|_| bad_encoding())?;
        let mut context: *mut CERT_CONTEXT = ptr::null_mut();
        // SAFETY: `self.0` is a live store; `der` is valid for `len` bytes for
        // the duration of the call (crypt32 copies); `context` is a valid place
        // to write one pointer.
        let ok = unsafe {
            CertAddEncodedCertificateToStore(
                self.0,
                X509_ASN_ENCODING,
                der.as_ptr(),
                len,
                CERT_STORE_ADD_ALWAYS,
                &mut context,
            )
        };
        if ok != TRUE || context.is_null() {
            // Attacker-controlled bytes that crypt32 will not decode.
            return Err(bad_encoding());
        }
        Ok(CertContext(context))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live store handle and this is its only close.
        // Contexts handed out by `add` remain valid because of the
        // `CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG` this store was opened with.
        unsafe { CertCloseStore(self.0, 0) };
    }
}

/// A certificate context, freed exactly once on drop.
struct CertContext(*mut CERT_CONTEXT);

impl Drop for CertContext {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live, non-null context and this is its only free.
        unsafe { CertFreeCertificateContext(self.0) };
    }
}

/// A chain context, freed exactly once on drop.
struct ChainContext(*mut CERT_CHAIN_CONTEXT);

impl Drop for ChainContext {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live, non-null chain context and this is its only free.
        unsafe { CertFreeCertificateChain(self.0) };
    }
}

/// A chain engine, freed exactly once on drop. Only ever constructed on the
/// test-only extra-anchor path.
struct ChainEngine(HCERTCHAINENGINE);

impl Drop for ChainEngine {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live, non-null engine handle and this is its only free.
        unsafe { CertFreeCertificateChainEngine(self.0) };
    }
}

/// Build the chain for `cert` at `now`, optionally through a restricted engine.
fn build_chain(
    store: &Store,
    cert: &CertContext,
    now: UnixTime,
    engine: Option<&ChainEngine>,
) -> Result<ChainContext, rustls::Error> {
    // Saturating rather than wrapping: a clock far enough in the future to
    // overflow here would silently wrap into the past, which is the direction
    // that turns an expired certificate valid.
    let intervals = now
        .as_secs()
        .saturating_add(UNIX_TO_WINDOWS_SECS)
        .saturating_mul(INTERVALS_PER_SEC);
    let time = FILETIME {
        dwLowDateTime: (intervals & u64::from(u32::MAX)) as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    };

    // `PCSTR` and `PSTR` differ only in constness; `CTL_USAGE` wants the mutable
    // spelling but crypt32 only reads it. The OID is already NUL-terminated —
    // see this module's header.
    let mut ekus: [PCSTR; 1] = [szOID_PKIX_KP_SERVER_AUTH];
    // SAFETY: `CERT_CHAIN_PARA` is a plain integer/pointer aggregate; MSDN
    // requires every unused field to be zero, which is exactly what this gives.
    let mut parameters: CERT_CHAIN_PARA = unsafe { mem::zeroed() };
    parameters.cbSize = u32::try_from(mem::size_of::<CERT_CHAIN_PARA>())
        .map_err(|_| invalid_certificate("CERT_CHAIN_PARA is impossibly large"))?;
    parameters.RequestedUsage = CERT_USAGE_MATCH {
        dwType: USAGE_MATCH_TYPE_AND,
        Usage: CTL_USAGE {
            cUsageIdentifier: 1,
            rgpszUsageIdentifier: ekus.as_mut_ptr().cast::<PSTR>(),
        },
    };
    parameters.dwUrlRetrievalTimeout = 10 * 1000;

    const FLAGS: u32 = CERT_CHAIN_REVOCATION_CHECK_END_CERT
        | CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT
        | CERT_CHAIN_CACHE_END_CERT;

    let mut chain: *mut CERT_CHAIN_CONTEXT = ptr::null_mut();
    // SAFETY: `cert` and `store` are live; `time` and `parameters` are valid for
    // reads for the duration of the call, and `ekus` outlives `parameters`;
    // `chain` is a valid place to write one pointer. `pvReserved` must be null.
    let ok = unsafe {
        CertGetCertificateChain(
            engine.map_or(ptr::null_mut(), |e| e.0),
            cert.0,
            &time,
            store.0,
            &parameters,
            FLAGS,
            ptr::null(),
            &mut chain,
        )
    };
    if ok != TRUE || chain.is_null() {
        return Err(last_os_error("CertGetCertificateChain failed"));
    }
    Ok(ChainContext(chain))
}

/// Windows chain verification.
///
/// `extra_roots` holds DER rather than a live chain engine, so this type needs
/// no `unsafe impl Send`/`Sync` and the shipped path (`extra_roots` empty)
/// never creates an engine at all.
#[derive(Debug)]
pub(super) struct Verifier {
    extra_roots: Vec<Vec<u8>>,
}

impl Verifier {
    /// `extra_roots` is EMPTY on every shipped path; see [`super::PlatformVerifier`].
    ///
    /// `_provider` is unused here — crypt32 brings its own crypto — but every
    /// arm takes the same two arguments so `super::PlatformVerifier` has ONE
    /// constructor call rather than a `#[cfg]` fork per platform.
    pub(super) fn new(
        extra_roots: Vec<CertificateDer<'static>>,
        _provider: Arc<CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        // Reject an unusable anchor at construction, like the incumbent does.
        let store = Store::new()?;
        for root in &extra_roots {
            store.add(root.as_ref())?;
        }
        Ok(Self {
            extra_roots: extra_roots.iter().map(|r| r.as_ref().to_vec()).collect(),
        })
    }

    /// An engine whose ONLY roots are `self.extra_roots`, plus a store holding
    /// them. The store must outlive the engine, so both are returned.
    fn exclusive_engine(&self) -> Result<(Store, ChainEngine), rustls::Error> {
        let roots = Store::new()?;
        for der in &self.extra_roots {
            roots.add(der)?;
        }
        // SAFETY: plain integer/pointer aggregate; MSDN requires unused fields zeroed.
        let mut config: CERT_CHAIN_ENGINE_CONFIG = unsafe { mem::zeroed() };
        config.cbSize = u32::try_from(mem::size_of::<CERT_CHAIN_ENGINE_CONFIG>())
            .map_err(|_| invalid_certificate("CERT_CHAIN_ENGINE_CONFIG is impossibly large"))?;
        config.hExclusiveRoot = roots.0;
        let mut engine: HCERTCHAINENGINE = ptr::null_mut();
        // SAFETY: `config` is valid for reads for the duration of the call and
        // names a live store; `engine` is a valid place to write one handle.
        let ok = unsafe { CertCreateCertificateChainEngine(&config, &mut engine) };
        if ok != TRUE || engine.is_null() {
            return Err(last_os_error("CertCreateCertificateChainEngine failed"));
        }
        Ok((roots, ChainEngine(engine)))
    }

    /// The presented chain, judged by crypt32.
    ///
    /// Returns `Ok(())` ONLY when the SSL chain policy reported `dwError == 0`.
    pub(super) fn verify(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        server_text: &str,
        ocsp_response: Option<&[u8]>,
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        let store = Store::new()?;
        let leaf = store.add(end_entity.as_ref())?;
        // The intermediates go into the store so chain building can find them;
        // their contexts are dropped at the end of this scope, which is safe
        // because the store defers its close until the last free.
        let _intermediates = intermediates
            .iter()
            .map(|der| store.add(der.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;

        if let Some(ocsp) = ocsp_response {
            let blob = CRYPT_INTEGER_BLOB {
                cbData: u32::try_from(ocsp.len()).map_err(|_| {
                    invalid_certificate("stapled OCSP response is impossibly large")
                })?,
                pbData: ocsp.as_ptr().cast_mut(),
            };
            // SAFETY: `leaf` is a live context; `blob` matches the layout
            // `CERT_OCSP_RESPONSE_PROP_ID` expects and is valid for reads for
            // the duration of the call, as is the buffer it points at.
            let ok = unsafe {
                CertSetCertificateContextProperty(
                    leaf.0,
                    CERT_OCSP_RESPONSE_PROP_ID,
                    CERT_SET_PROPERTY_IGNORE_PERSIST_ERROR_FLAG,
                    (&raw const blob).cast::<c_void>(),
                )
            };
            if ok != TRUE {
                return Err(last_os_error("CertSetCertificateContextProperty failed"));
            }
        }

        let mut chain = build_chain(&store, &leaf, now, None)?;

        // Test-only extra anchors. EMPTY on every shipped path, so this whole
        // branch is dead in a real build. `TrustStatus` here has NOT been
        // through policy verification; it is read only to decide whether the
        // system roots failed to complete the chain.
        // A CHAIN CONTEXT MUST NOT OUTLIVE THE ENGINE THAT BUILT IT, so the
        // engine is bound HERE rather than inside the `if partial` block below.
        // Bound there, it and its store dropped — calling
        // CertFreeCertificateChainEngine — at that block's closing brace, while
        // `chain` stayed in use through the policy check further down: a
        // use-after-free that two independent reviews caught by reading. It has
        // never fired because the branch is test-only (`extra_roots` is empty on
        // every shipped path), which is exactly why it survived.
        let mut _engine_keepalive = None;
        if !self.extra_roots.is_empty() {
            // SAFETY: `chain.0` is a live, non-null chain context.
            let partial =
                unsafe { (*chain.0).TrustStatus }.dwErrorStatus & CERT_TRUST_IS_PARTIAL_CHAIN != 0;
            if partial {
                let (roots, engine) = self.exclusive_engine()?;
                chain = build_chain(&store, &leaf, now, Some(&engine))?;
                _engine_keepalive = Some((roots, engine));
            }
        }

        // UTF-16, NUL-terminated. `encode_utf16` (rather than widening bytes one
        // by one) is correct for any `&str`; a `ServerName` is ASCII in practice,
        // but this does not depend on that.
        let mut server: Vec<u16> = server_text.encode_utf16().chain(Some(0)).collect();

        // SAFETY: plain aggregate containing a union of two `u32`s; zeroing is
        // required by MSDN for the unused fields.
        let mut https: HTTPSPolicyCallbackData = unsafe { mem::zeroed() };
        https.Anonymous.cbSize = u32::try_from(mem::size_of::<HTTPSPolicyCallbackData>())
            .map_err(|_| invalid_certificate("HTTPSPolicyCallbackData is impossibly large"))?;
        https.dwAuthType = AUTHTYPE_SERVER;
        // `server` outlives `https`, which outlives `params`, which outlives the
        // call below.
        https.pwszServerName = server.as_mut_ptr();

        // SAFETY: plain integer/pointer aggregate; zeroing the unused fields is
        // required by MSDN.
        let mut params: CERT_CHAIN_POLICY_PARA = unsafe { mem::zeroed() };
        params.cbSize = u32::try_from(mem::size_of::<CERT_CHAIN_POLICY_PARA>())
            .map_err(|_| invalid_certificate("CERT_CHAIN_POLICY_PARA is impossibly large"))?;
        // Do not fail a chain merely because revocation status was UNKNOWN.
        // OpenSSL and Apple's Secure Transport behave the same way, and this is
        // the incumbent's setting. A definitively REVOKED certificate is still
        // rejected — that arrives as `CRYPT_E_REVOKED` below.
        params.dwFlags = CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS;
        params.pvExtraPolicyPara = (&raw mut https).cast::<c_void>();

        // SAFETY: plain aggregate; zeroed so a `dwError` that the call somehow
        // failed to write reads as 0 — which is why the `BOOL` is checked FIRST
        // and a non-`TRUE` return is an error before `dwError` is ever consulted.
        let mut status: CERT_CHAIN_POLICY_STATUS = unsafe { mem::zeroed() };
        status.cbSize = u32::try_from(mem::size_of::<CERT_CHAIN_POLICY_STATUS>())
            .map_err(|_| invalid_certificate("CERT_CHAIN_POLICY_STATUS is impossibly large"))?;

        // SAFETY: `chain.0` is a live chain context; `params` is valid for reads
        // (and everything it points at outlives the call); `status` is a valid
        // place to write one `CERT_CHAIN_POLICY_STATUS`.
        let ok = unsafe {
            CertVerifyCertificateChainPolicy(CERT_CHAIN_POLICY_SSL, chain.0, &params, &mut status)
        };
        // THE BOOL IS NOT THE VERDICT. It only says the policy engine ran.
        if ok != TRUE {
            return Err(invalid_certificate(
                "CertVerifyCertificateChainPolicy could not evaluate the chain",
            ));
        }
        // THE VERDICT. Acceptance requires an explicit zero.
        if status.dwError == 0 {
            return Ok(());
        }

        Err(match status.dwError as i32 {
            CERT_E_CN_NO_MATCH | CERT_E_INVALID_NAME => name_mismatch(),
            CERT_E_UNTRUSTEDROOT => unknown_issuer(),
            CERT_E_EXPIRED => rustls::Error::InvalidCertificate(rustls::CertificateError::Expired),
            CERT_E_WRONG_USAGE => eku_rejected(),
            CRYPT_E_REVOKED => revoked(),
            other => invalid_certificate(format!(
                "certificate rejected by the system: {}",
                std::io::Error::from_raw_os_error(other)
            )),
        })
    }
}

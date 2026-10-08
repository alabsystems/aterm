// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS Keychain generic passwords, straight over the system `SecItem*` API.
//!
//! This is the whole of what aterm ever asked the `security-framework` crate
//! for: read, write and delete ONE generic-password item keyed by
//! (service, account). That crate carried 10,503 lines to expose certificates,
//! identities, trust policies, `SecureTransport`, CMS and the authorization
//! database — none of which aterm links. Security.framework itself is an OS
//! dylib, so nothing is vendored and nothing is compiled: only the ~7 entry
//! points below are declared.
//!
//! The query is deliberately IDENTICAL to what that crate built, because
//! existing installs already have items in the login keychain and must keep
//! resolving:
//!
//! * `kSecClass` = `kSecClassGenericPassword`,
//! * `kSecAttrService` = the service string,
//! * `kSecAttrAccount` = the account string,
//! * and NOTHING else — in particular no `kSecUseDataProtectionKeychain`, so
//!   these stay file-based login-keychain items (the data-protection keychain
//!   is a separate store, and an item written to one is invisible to the
//!   other).
//!
//! Items written by that crate and by this module were cross-read in both
//! directions on a live keychain when it was retired.
//!
//! **Error discipline.** Every entry point returns the raw `OSStatus` in
//! [`Error`] rather than collapsing failures into an `Option`. The caller
//! ([`super::resolve_token`]) branches on exactly one code —
//! [`ERR_SEC_ITEM_NOT_FOUND`], "no such item, fall through to a token file" —
//! and must surface everything else (keychain locked, access denied, user
//! declined) as an error. Swallowing an `OSStatus` here would silently
//! downgrade "the keychain refused me" into "no token is provisioned".

use std::ffi::{c_char, c_void};
use std::fmt;
use std::ptr;

// ---------------------------------------------------------------------------
// Core Foundation / Security types. Every CF object is an opaque pointer; the
// only layout aterm asserts is the two dictionary callback-table structs,
// whose addresses (never their contents) are handed to CFDictionaryCreate.
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFIndex = isize;
type CFTypeID = usize;
type CFStringEncoding = u32;
type OSStatus = i32;
type Boolean = u8;

/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;

/// `errSecItemNotFound` — "no keychain item matched the query". The ONE status
/// a caller may treat as an ordinary absence rather than a failure.
pub(crate) const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
/// `errSecDuplicateItem` — `SecItemAdd` found an item with the same primary
/// key. Not an error here: it is the signal to UPDATE instead (see
/// [`set_generic_password`]).
const ERR_SEC_DUPLICATE_ITEM: OSStatus = -25299;
/// `errSecParam` — a bad/unusable parameter. Returned for the local failures
/// that never reach the OS (a string CF refuses to build, a match that comes
/// back as an unexpected CF type), so those look like every other failure to
/// the caller instead of needing a second error channel.
const ERR_SEC_PARAM: OSStatus = -50;

/// `CFDictionaryKeyCallBacks`. Declared with real function-pointer fields
/// rather than opaque words so the type is `Sync` (an `extern` static of a
/// raw-pointer-bearing type is not) and so the layout claim is checkable
/// against the header. Only `&raw const` of the static is ever taken.
#[repr(C)]
struct CFDictionaryKeyCallBacks {
    version: CFIndex,
    retain: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef) -> CFTypeRef>,
    release: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef)>,
    copy_description: Option<unsafe extern "C" fn(CFTypeRef) -> CFStringRef>,
    equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> Boolean>,
    hash: Option<unsafe extern "C" fn(CFTypeRef) -> CFIndex>,
}

/// `CFDictionaryValueCallBacks` — the key table minus `hash`.
#[repr(C)]
struct CFDictionaryValueCallBacks {
    version: CFIndex,
    retain: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef) -> CFTypeRef>,
    release: Option<unsafe extern "C" fn(CFAllocatorRef, CFTypeRef)>,
    copy_description: Option<unsafe extern "C" fn(CFTypeRef) -> CFStringRef>,
    equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> Boolean>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    static kCFBooleanTrue: CFTypeRef;

    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;

    fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
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

    fn CFDataGetTypeID() -> CFTypeID;
    fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFTypeRef;
    fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;

    fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num_values: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CFStringRef;
    static kSecClassGenericPassword: CFStringRef;
    static kSecAttrService: CFStringRef;
    static kSecAttrAccount: CFStringRef;
    static kSecReturnData: CFStringRef;
    static kSecValueData: CFStringRef;

    fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemUpdate(query: CFDictionaryRef, attributes_to_update: CFDictionaryRef) -> OSStatus;
    #[cfg(test)]
    fn SecItemDelete(query: CFDictionaryRef) -> OSStatus;
    fn SecCopyErrorMessageString(status: OSStatus, reserved: *mut c_void) -> CFStringRef;
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// A Security.framework failure, carried as its raw `OSStatus`.
///
/// The code is never zero: a zero handed to [`Error::from_code`] becomes `1`,
/// so an `Err` can never be mistaken for `errSecSuccess` by a caller that
/// compares codes.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) struct Error(OSStatus);

impl Error {
    /// Wrap an `OSStatus`. A zero status is not a failure, so it is remapped to
    /// `1` rather than producing an `Error` that claims success.
    fn from_code(code: OSStatus) -> Self {
        Self(if code == 0 { 1 } else { code })
    }

    /// The raw `OSStatus`. Compare against [`ERR_SEC_ITEM_NOT_FOUND`] to tell
    /// "not stored" from a real keychain failure.
    pub(crate) const fn code(self) -> OSStatus {
        self.0
    }

    /// The system's localized description of this status, when it has one.
    fn message(self) -> Option<String> {
        // SAFETY: `SecCopyErrorMessageString` takes any OSStatus and a reserved
        // pointer that must be null. It returns a +1 CFString or null.
        let s = unsafe { SecCopyErrorMessageString(self.0, ptr::null_mut()) };
        if s.is_null() {
            return None;
        }
        let owned = CfOwned(s);
        cf_string_to_string(owned.0)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message() {
            Some(message) => write!(f, "{message}"),
            None => write!(f, "error code {}", self.0),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = f.debug_struct("Error");
        builder.field("code", &self.0);
        if let Some(message) = self.message() {
            builder.field("message", &message);
        }
        builder.finish()
    }
}

impl std::error::Error for Error {}

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

/// Build a `CFString` from a Rust string. `None` when CF refuses the bytes,
/// which for well-formed UTF-8 means an allocation failure.
fn cf_string(s: &str) -> Option<CfOwned> {
    // SAFETY: the pointer/length pair describes `s`, which outlives the call;
    // CFStringCreateWithBytes copies. `is_external_representation` is false —
    // these bytes carry no BOM.
    let r = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            s.as_ptr(),
            // A `&str` longer than isize::MAX cannot exist.
            s.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    (!r.is_null()).then_some(CfOwned(r))
}

/// Copy a `CFString` out as a Rust `String`.
fn cf_string_to_string(s: CFStringRef) -> Option<String> {
    // SAFETY: `s` is a live CFString for the duration of these calls.
    let len = unsafe { CFStringGetLength(s) };
    let max = unsafe { CFStringGetMaximumSizeForEncoding(len, CF_STRING_ENCODING_UTF8) };
    if max <= 0 {
        return Some(String::new());
    }
    // +1 for the NUL CFStringGetCString always writes.
    let mut buf = vec![0u8; usize::try_from(max).ok()?.checked_add(1)?];
    // SAFETY: `buf` is exactly `buf.len()` writable bytes; CFStringGetCString
    // writes at most that many including the terminator.
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
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    String::from_utf8(buf).ok()
}

/// Build a `CFDictionary` over already-live CF objects, with the standard
/// CF-type key/value callbacks (so the dictionary retains what it holds and
/// hashes keys by value, which is what every `SecItem*` entry point expects).
///
/// Every pointer in `pairs` must be a live CF object for the duration of the
/// call; the returned dictionary holds its own references afterwards.
fn cf_dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Option<CfOwned> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|&(k, _)| k).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|&(_, v)| v).collect();
    // SAFETY: `keys`/`values` are two arrays of `pairs.len()` live CF objects,
    // and the callback tables are the CF-provided statics — address taken, never
    // read by Rust.
    let d = unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as CFIndex,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    };
    (!d.is_null()).then_some(CfOwned(d))
}

/// The three identifying attributes of a generic-password item, plus the two
/// `CFString`s they BORROW FROM.
///
/// The strings travel with the pairs because the pairs hold raw `CFTypeRef`s
/// into them: drop the strings and the array is dangling. Naming the triple
/// rather than spelling it at the return type is what keeps that relationship
/// legible — and `clippy::type_complexity` asks for the name anyway.
type GenericPasswordKey = (CfOwned, CfOwned, [(CFTypeRef, CFTypeRef); 3]);

/// The three attributes that IDENTIFY a generic-password item: class, service,
/// account. Returned alongside the two `CFString`s so the caller keeps them
/// alive while it appends more entries.
fn generic_password_key(service: &str, account: &str) -> Result<GenericPasswordKey, Error> {
    let service = cf_string(service).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    let account = cf_string(account).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    // SAFETY: reading the framework's CFString constants; they are immortal.
    let key = unsafe {
        [
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, service.0),
            (kSecAttrAccount, account.0),
        ]
    };
    Ok((service, account, key))
}

/// Take ownership of a `SecItemCopyMatching` result and return its bytes.
///
/// Anything other than a `CFData` (or a null result) is `errSecParam` — the
/// query asked for `kSecReturnData`, so any other type means the request was
/// not honoured and there is no password to hand back.
fn take_password_data(result: CFTypeRef) -> Result<Vec<u8>, Error> {
    if result.is_null() {
        return Err(Error::from_code(ERR_SEC_PARAM));
    }
    // Released on every path from here, including the wrong-type one.
    let owned = CfOwned(result);
    // SAFETY: `owned.0` is a live CF object.
    if unsafe { CFGetTypeID(owned.0) } != unsafe { CFDataGetTypeID() } {
        return Err(Error::from_code(ERR_SEC_PARAM));
    }
    // SAFETY: `owned.0` is a live CFData.
    let len = unsafe { CFDataGetLength(owned.0) };
    if len <= 0 {
        return Ok(Vec::new());
    }
    // SAFETY: same, and the pointer is valid for `len` bytes until release.
    let p = unsafe { CFDataGetBytePtr(owned.0) };
    if p.is_null() {
        return Ok(Vec::new());
    }
    let len = usize::try_from(len).map_err(|_| Error::from_code(ERR_SEC_PARAM))?;
    // SAFETY: CFData guarantees `len` readable bytes at `p`; copied out before
    // `owned` is dropped.
    Ok(unsafe { std::slice::from_raw_parts(p, len) }.to_vec())
}

// ---------------------------------------------------------------------------
// The three operations aterm needs
// ---------------------------------------------------------------------------

/// Read the generic password stored under (`service`, `account`).
///
/// `Err` with [`Error::code`] == [`ERR_SEC_ITEM_NOT_FOUND`] means "no such
/// item"; every other code is a real keychain failure (locked, denied, …) and
/// must not be treated as absence.
pub(crate) fn get_generic_password(service: &str, account: &str) -> Result<Vec<u8>, Error> {
    let (_service, _account, key) = generic_password_key(service, account)?;
    // SAFETY: framework constants.
    let return_data = unsafe { (kSecReturnData, kCFBooleanTrue) };
    let query = cf_dictionary(&[key[0], key[1], key[2], return_data])
        .ok_or(Error::from_code(ERR_SEC_PARAM))?;

    let mut result: CFTypeRef = ptr::null();
    // SAFETY: `query` is a live CFDictionary; `result` is a valid out-slot that
    // receives a +1 reference on success.
    let status = unsafe { SecItemCopyMatching(query.0, &raw mut result) };
    if status != 0 {
        return Err(Error::from_code(status));
    }
    take_password_data(result)
}

/// Store `password` under (`service`, `account`), creating the item or
/// replacing the password of the existing one.
///
/// `SecItemAdd` is tried first; `errSecDuplicateItem` means an item with this
/// primary key already exists, and only then is `SecItemUpdate` issued. Doing
/// it in that order (rather than "look up, then add or update") is what makes
/// the write atomic against a concurrent writer: there is no window in which
/// two processes both conclude the item is absent.
pub(crate) fn set_generic_password(
    service: &str,
    account: &str,
    password: &[u8],
) -> Result<(), Error> {
    let (_service, _account, key) = generic_password_key(service, account)?;

    // SAFETY: pointer/length pair over `password`, which outlives the call;
    // CFDataCreate copies the bytes.
    let data = unsafe { CFDataCreate(ptr::null(), password.as_ptr(), password.len() as CFIndex) };
    if data.is_null() {
        return Err(Error::from_code(ERR_SEC_PARAM));
    }
    let data = CfOwned(data);
    // SAFETY: framework constant.
    let value = unsafe { (kSecValueData, data.0) };

    let attributes =
        cf_dictionary(&[key[0], key[1], key[2], value]).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    let mut added: CFTypeRef = ptr::null();
    // SAFETY: `attributes` is a live CFDictionary. No `kSecReturn*` key is set,
    // so the out-slot is left null, but it is released below if the OS ever
    // hands something back.
    let status = unsafe { SecItemAdd(attributes.0, &raw mut added) };
    if !added.is_null() {
        drop(CfOwned(added));
    }
    if status != ERR_SEC_DUPLICATE_ITEM {
        return if status == 0 {
            Ok(())
        } else {
            Err(Error::from_code(status))
        };
    }

    // The item exists: replace just its password, matching on the same
    // primary key.
    let query = cf_dictionary(&key).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    let update = cf_dictionary(&[value]).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    // SAFETY: both are live CFDictionaries.
    let status = unsafe { SecItemUpdate(query.0, update.0) };
    if status == 0 {
        Ok(())
    } else {
        Err(Error::from_code(status))
    }
}

/// Remove the generic-password item under (`service`, `account`).
///
/// `Err` with [`Error::code`] == [`ERR_SEC_ITEM_NOT_FOUND`] when there was
/// nothing to remove. The live round trip's cleanup path, so test-only: aterm
/// itself never revokes a stored drive token.
#[cfg(test)]
pub(crate) fn delete_generic_password(service: &str, account: &str) -> Result<(), Error> {
    let (_service, _account, key) = generic_password_key(service, account)?;
    let query = cf_dictionary(&key).ok_or(Error::from_code(ERR_SEC_PARAM))?;
    // SAFETY: `query` is a live CFDictionary.
    let status = unsafe { SecItemDelete(query.0) };
    if status == 0 {
        Ok(())
    } else {
        Err(Error::from_code(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure keeps its exact `OSStatus`, and renders as something.
    #[test]
    fn error_codes_survive_and_render() {
        for code in [
            ERR_SEC_ITEM_NOT_FOUND,
            ERR_SEC_DUPLICATE_ITEM,
            ERR_SEC_PARAM,
            -25308,
            -25293,
            -128,
            -25291,
            1,
            123_456_789,
        ] {
            let e = Error::from_code(code);
            assert_eq!(e.code(), code);
            assert!(!e.to_string().is_empty(), "code {code}");
        }
    }

    /// A zero status is not a failure: it is never carried as one.
    #[test]
    fn zero_status_is_never_a_success_shaped_error() {
        assert_eq!(Error::from_code(0).code(), 1);
    }

    /// Add, update (on duplicate), read back, delete and the not-found statuses
    /// on the REAL login keychain, so `#[ignore]`d: a default `cargo test` must
    /// never touch a user's keychain. Run it deliberately when the FFI changes:
    ///     cargo test -p aterm-gui --lib net_connections::keychain -- --ignored --test-threads=1
    #[test]
    #[ignore = "writes to the real login Keychain; run with --ignored --test-threads=1"]
    fn live_round_trip() {
        let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos());
        let service = format!("aterm-keychain-test-{}-{nanos}", std::process::id());
        let account = "test-account";
        let code = |r: Result<Vec<u8>, Error>| r.map_err(Error::code);

        assert_eq!(
            code(get_generic_password(&service, account)),
            Err(ERR_SEC_ITEM_NOT_FOUND)
        );
        for payload in [
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_vec(),
            vec![0x00, 0xFF, 0x01, 0x00, 0x80, 0x7F],
            (0u16..=511).map(|b| (b & 0xFF) as u8).collect(),
            "ünïcøde password — ✅".as_bytes().to_vec(),
        ] {
            assert!(set_generic_password(&service, account, &payload).is_ok());
            assert_eq!(code(get_generic_password(&service, account)), Ok(payload));
        }
        assert!(delete_generic_password(&service, account).is_ok());
        assert_eq!(
            delete_generic_password(&service, account).map_err(Error::code),
            Err(ERR_SEC_ITEM_NOT_FOUND)
        );
        assert_eq!(
            code(get_generic_password(&service, account)),
            Err(ERR_SEC_ITEM_NOT_FOUND)
        );
    }
}

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
//! [`tests`] holds the differential test that pins that equality: the retired
//! crate is kept as a `[dev-dependencies]` ORACLE and every operation is
//! cross-checked against it, in both directions, on a live keychain.
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
/// nothing to remove.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the differential oracle's cleanup \
    path; aterm itself never revokes a stored drive token, but a delete that is \
    not exercised is a delete that is not known to work"
    )
)]
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

// ---------------------------------------------------------------------------
// The differential test: `security-framework` as the ORACLE
// ---------------------------------------------------------------------------

/// `security-framework` is kept as a `[dev-dependencies]` oracle (see this
/// crate's manifest) and every operation above is cross-checked against it.
///
/// Two shapes of check, because they catch different mistakes:
///
/// * **Parallel scripts** — the same sequence of operations is driven through
///   both implementations against two SEPARATE items, and every outcome
///   (returned bytes, or the exact `OSStatus`) must be equal. This is the check
///   that pins the add-then-update-on-duplicate branch, the not-found status,
///   and the empty/binary payload edges: whatever the OS does, the two must do
///   the same thing.
/// * **Cross reads** — one implementation writes and the OTHER reads the SAME
///   item. This is the check that pins the query itself: class, service and
///   account attributes, and (crucially) that both target the file-based login
///   keychain rather than one of them silently landing in the data-protection
///   keychain, where the other would never find it. That equality is what keeps
///   already-provisioned installs working across this change.
///
/// These tests touch the REAL login keychain, so: every item is created under a
/// service name unique to this process and this instant, the reads happen in the
/// process that created the item (so the creating application always has ACL
/// access and no interaction is required), and both items are deleted on every
/// exit path. If the keychain is unusable — no login session, locked keychain —
/// the ORACLE fails first and the test skips rather than reporting a defect in
/// the reimplementation.
#[cfg(test)]
mod tests {
    use super::*;

    /// One operation's result, in a shape both implementations can be reduced
    /// to, so that comparing them compares EVERYTHING: the bytes on success and
    /// the raw `OSStatus` on failure.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Bytes(Vec<u8>),
        Stored,
        Deleted,
        Failed(OSStatus),
    }

    fn mine_get(service: &str, account: &str) -> Outcome {
        match get_generic_password(service, account) {
            Ok(b) => Outcome::Bytes(b),
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    fn mine_set(service: &str, account: &str, password: &[u8]) -> Outcome {
        match set_generic_password(service, account, password) {
            Ok(()) => Outcome::Stored,
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    fn mine_delete(service: &str, account: &str) -> Outcome {
        match delete_generic_password(service, account) {
            Ok(()) => Outcome::Deleted,
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    fn oracle_get(service: &str, account: &str) -> Outcome {
        match security_framework::passwords::get_generic_password(service, account) {
            Ok(b) => Outcome::Bytes(b),
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    fn oracle_set(service: &str, account: &str, password: &[u8]) -> Outcome {
        match security_framework::passwords::set_generic_password(service, account, password) {
            Ok(()) => Outcome::Stored,
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    fn oracle_delete(service: &str, account: &str) -> Outcome {
        match security_framework::passwords::delete_generic_password(service, account) {
            Ok(()) => Outcome::Deleted,
            Err(e) => Outcome::Failed(e.code()),
        }
    }

    /// A service name no other run — or the user's real config — can collide
    /// with. `tag` separates the two items a parallel script needs.
    fn unique_service(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "aterm-keychain-difftest-{tag}-{}-{nanos}",
            std::process::id()
        )
    }

    /// Deletes its item on every exit path, including a panicking assert.
    struct Scratch(String, String);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = delete_generic_password(&self.0, &self.1);
        }
    }

    /// Whether this machine's keychain can be written at all, decided by the
    /// ORACLE — so a skip can never hide a defect in the reimplementation.
    /// Returns the failing status when it cannot.
    fn oracle_can_write(service: &str, account: &str) -> Result<(), OSStatus> {
        match oracle_set(service, account, b"probe") {
            Outcome::Stored => {
                let _ = oracle_delete(service, account);
                Ok(())
            }
            Outcome::Failed(code) => Err(code),
            other => panic!("oracle set returned {other:?}"),
        }
    }

    /// The payloads the sequential add-then-update script is run over: the
    /// ordinary 64-char hex token aterm actually stores, plus the edges
    /// (embedded NULs and high bytes, a value long enough to leave the
    /// small-item path, multi-byte UTF-8, and a single byte).
    ///
    /// The EMPTY payload is deliberately absent and tested on its own in
    /// [`empty_password_update_is_ignored_by_both`]: macOS does not honour it
    /// as an update, so it cannot take part in a "what went in comes out"
    /// script.
    fn payloads() -> Vec<Vec<u8>> {
        vec![
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_vec(),
            vec![0x00, 0xFF, 0x01, 0x00, 0x80, 0x7F],
            (0u16..=511).map(|b| (b & 0xFF) as u8).collect(),
            "ünïcøde password — ✅".as_bytes().to_vec(),
            b"0".to_vec(),
        ]
    }

    /// Drive both implementations through the SAME operation script against two
    /// separate items and require identical outcomes at every step.
    /// WRITES TO THE REAL LOGIN KEYCHAIN, so it is `#[ignore]`d and does not run
    /// in an ordinary `cargo test`.
    ///
    /// This is a differential test against `security-framework`, and the only way
    /// to compare two Keychain implementations is to have both talk to a real
    /// Keychain — there is no injectable backend behind `SecItemAdd`. On a
    /// developer's machine that means the default run was creating and deleting
    /// items in the user's own login Keychain, and macOS answered with a modal
    /// `Keychain "login" cannot be found to store "difftest-empty"` dialog
    /// offering to RESET IT TO DEFAULTS. A test suite must never put that
    /// dialog in front of anyone, and must never be one mis-click away from
    /// resetting a user's Keychain.
    ///
    /// Run it deliberately when the FFI changes:
    ///     cargo test -p aterm-gui --lib net_connections::keychain -- --ignored --test-threads=1
    ///
    /// `--test-threads=1` is not optional: these scripts share service names, so
    /// in parallel they race each other and fail for reasons that have nothing to
    /// do with the code under test.
    #[test]
    #[ignore = "writes to the real login Keychain; run with --ignored --test-threads=1"]
    fn parallel_script_outcomes_match_the_oracle() {
        let account = "difftest-account";
        let mine_service = unique_service("mine");
        let oracle_service = unique_service("oracle");
        let _mine_scratch = Scratch(mine_service.clone(), account.to_owned());
        let _oracle_scratch = Scratch(oracle_service.clone(), account.to_owned());

        if let Err(code) = oracle_can_write(&oracle_service, account) {
            eprintln!(
                "SKIP keychain differential: the oracle itself cannot write to this \
                 machine's keychain (OSStatus {code}) — no login session, or it is locked"
            );
            return;
        }

        // 1. Absent item: both must report errSecItemNotFound, not a generic
        //    failure — `resolve_token` branches on exactly this code.
        assert_eq!(
            mine_get(&mine_service, account),
            Outcome::Failed(ERR_SEC_ITEM_NOT_FOUND),
        );
        assert_eq!(
            mine_get(&mine_service, account),
            oracle_get(&oracle_service, account),
        );
        assert_eq!(
            mine_delete(&mine_service, account),
            oracle_delete(&oracle_service, account),
        );

        // 2. First write is an ADD; every later write hits errSecDuplicateItem
        //    inside and must become an UPDATE. Walking the payload list without
        //    deleting in between is what exercises that branch.
        for payload in payloads() {
            assert_eq!(
                mine_set(&mine_service, account, &payload),
                oracle_set(&oracle_service, account, &payload),
                "set disagreed on a {}-byte payload",
                payload.len(),
            );
            let mine = mine_get(&mine_service, account);
            assert_eq!(
                mine,
                oracle_get(&oracle_service, account),
                "get disagreed after a {}-byte payload",
                payload.len(),
            );
            // And the value that came back is the value that went in — an
            // agreement on the WRONG bytes would otherwise pass.
            assert_eq!(mine, Outcome::Bytes(payload));
        }

        // 3. Delete, then the absent-item statuses again.
        assert_eq!(
            mine_delete(&mine_service, account),
            oracle_delete(&oracle_service, account),
        );
        assert_eq!(
            mine_delete(&mine_service, account),
            Outcome::Failed(ERR_SEC_ITEM_NOT_FOUND),
        );
        assert_eq!(
            mine_delete(&mine_service, account),
            oracle_delete(&oracle_service, account),
        );
        assert_eq!(
            mine_get(&mine_service, account),
            oracle_get(&oracle_service, account),
        );
    }

    /// The compatibility check: an item written by one implementation must be
    /// readable by the other. This is what proves the query attributes and the
    /// keychain SELECTION are identical, so tokens provisioned by a build that
    /// used `security-framework` still resolve after this change.
    /// WRITES TO THE REAL LOGIN KEYCHAIN, so it is `#[ignore]`d and does not run
    /// in an ordinary `cargo test`.
    ///
    /// This is a differential test against `security-framework`, and the only way
    /// to compare two Keychain implementations is to have both talk to a real
    /// Keychain — there is no injectable backend behind `SecItemAdd`. On a
    /// developer's machine that means the default run was creating and deleting
    /// items in the user's own login Keychain, and macOS answered with a modal
    /// `Keychain "login" cannot be found to store "difftest-empty"` dialog
    /// offering to RESET IT TO DEFAULTS. A test suite must never put that
    /// dialog in front of anyone, and must never be one mis-click away from
    /// resetting a user's Keychain.
    ///
    /// Run it deliberately when the FFI changes:
    ///     cargo test -p aterm-gui --lib net_connections::keychain -- --ignored --test-threads=1
    ///
    /// `--test-threads=1` is not optional: these scripts share service names, so
    /// in parallel they race each other and fail for reasons that have nothing to
    /// do with the code under test.
    #[test]
    #[ignore = "writes to the real login Keychain; run with --ignored --test-threads=1"]
    fn each_implementation_reads_what_the_other_wrote() {
        let account = "difftest-cross";
        let service = unique_service("cross");
        let _scratch = Scratch(service.clone(), account.to_owned());

        if let Err(code) = oracle_can_write(&service, account) {
            eprintln!(
                "SKIP keychain cross-read differential: the oracle itself cannot write \
                 to this machine's keychain (OSStatus {code})"
            );
            return;
        }

        let first = b"aaaa1111bbbb2222cccc3333dddd4444".to_vec();
        let second = b"ffff9999eeee8888dddd7777cccc6666".to_vec();

        // Mine writes (an ADD) -> the oracle must find it.
        assert_eq!(mine_set(&service, account, &first), Outcome::Stored);
        assert_eq!(oracle_get(&service, account), Outcome::Bytes(first.clone()));

        // The oracle overwrites (its UPDATE branch) -> mine must see the new
        // value, not a stale or duplicated item.
        assert_eq!(oracle_set(&service, account, &second), Outcome::Stored);
        assert_eq!(mine_get(&service, account), Outcome::Bytes(second.clone()));

        // Mine overwrites (MY update branch) -> the oracle must see it, and
        // exactly one item must exist: if the update had instead added a second
        // item, the delete below would leave one behind for the final get to
        // find.
        assert_eq!(mine_set(&service, account, &first), Outcome::Stored);
        assert_eq!(oracle_get(&service, account), Outcome::Bytes(first));

        // The oracle deletes -> mine must report absence, not a stale hit.
        assert_eq!(oracle_delete(&service, account), Outcome::Deleted);
        assert_eq!(
            mine_get(&service, account),
            Outcome::Failed(ERR_SEC_ITEM_NOT_FOUND),
        );

        // And the reverse: mine deletes what the oracle wrote.
        assert_eq!(oracle_set(&service, account, &second), Outcome::Stored);
        assert_eq!(mine_delete(&service, account), Outcome::Deleted);
        assert_eq!(
            oracle_get(&service, account),
            Outcome::Failed(ERR_SEC_ITEM_NOT_FOUND),
        );
    }

    /// A macOS behaviour the oracle CONFIRMED rather than a divergence, pinned
    /// here so a future rewrite cannot quietly turn it into a difference:
    ///
    /// * `SecItemAdd` with an empty `kSecValueData` stores an empty item, and
    ///   reading it back yields zero bytes.
    /// * `SecItemUpdate` with an empty `kSecValueData` returns `errSecSuccess`
    ///   and leaves the PREVIOUS password in place. The write is silently
    ///   dropped — by the OS, in both implementations identically.
    ///
    /// aterm never reaches this: `store_token` validates 64-char hex before it
    /// calls [`set_generic_password`], so an empty drive token cannot be
    /// written. The test exists because the first cut of the differential
    /// script assumed "set then get returns what was set", and this is the one
    /// input for which the platform says otherwise.
    /// WRITES TO THE REAL LOGIN KEYCHAIN, so it is `#[ignore]`d and does not run
    /// in an ordinary `cargo test`.
    ///
    /// This is a differential test against `security-framework`, and the only way
    /// to compare two Keychain implementations is to have both talk to a real
    /// Keychain — there is no injectable backend behind `SecItemAdd`. On a
    /// developer's machine that means the default run was creating and deleting
    /// items in the user's own login Keychain, and macOS answered with a modal
    /// `Keychain "login" cannot be found to store "difftest-empty"` dialog
    /// offering to RESET IT TO DEFAULTS. A test suite must never put that
    /// dialog in front of anyone, and must never be one mis-click away from
    /// resetting a user's Keychain.
    ///
    /// Run it deliberately when the FFI changes:
    ///     cargo test -p aterm-gui --lib net_connections::keychain -- --ignored --test-threads=1
    ///
    /// `--test-threads=1` is not optional: these scripts share service names, so
    /// in parallel they race each other and fail for reasons that have nothing to
    /// do with the code under test.
    #[test]
    #[ignore = "writes to the real login Keychain; run with --ignored --test-threads=1"]
    fn empty_password_update_is_ignored_by_both() {
        let account = "difftest-empty";
        let mine_add = unique_service("empty-add-mine");
        let oracle_add = unique_service("empty-add-oracle");
        let mine_upd = unique_service("empty-upd-mine");
        let oracle_upd = unique_service("empty-upd-oracle");
        let _s1 = Scratch(mine_add.clone(), account.to_owned());
        let _s2 = Scratch(oracle_add.clone(), account.to_owned());
        let _s3 = Scratch(mine_upd.clone(), account.to_owned());
        let _s4 = Scratch(oracle_upd.clone(), account.to_owned());

        if let Err(code) = oracle_can_write(&oracle_add, account) {
            eprintln!(
                "SKIP keychain empty-password differential: the oracle itself cannot \
                 write to this machine's keychain (OSStatus {code})"
            );
            return;
        }

        // ADD with an empty value: stored, and it reads back empty.
        assert_eq!(mine_set(&mine_add, account, b""), Outcome::Stored);
        assert_eq!(oracle_set(&oracle_add, account, b""), Outcome::Stored);
        assert_eq!(mine_get(&mine_add, account), Outcome::Bytes(Vec::new()));
        assert_eq!(
            mine_get(&mine_add, account),
            oracle_get(&oracle_add, account),
        );

        // UPDATE with an empty value: reports success, changes nothing.
        assert_eq!(mine_set(&mine_upd, account, b"abc"), Outcome::Stored);
        assert_eq!(oracle_set(&oracle_upd, account, b"abc"), Outcome::Stored);
        assert_eq!(mine_set(&mine_upd, account, b""), Outcome::Stored);
        assert_eq!(oracle_set(&oracle_upd, account, b""), Outcome::Stored);
        assert_eq!(
            mine_get(&mine_upd, account),
            Outcome::Bytes(b"abc".to_vec())
        );
        assert_eq!(
            mine_get(&mine_upd, account),
            oracle_get(&oracle_upd, account),
        );
    }

    /// The `Display`/`Debug` text a keychain failure reaches the user through
    /// (`resolve_token` formats `{e}` into its hint) must be the same text the
    /// retired crate produced, for every status aterm can plausibly surface.
    /// This needs no keychain at all.
    #[test]
    fn error_rendering_matches_the_oracle() {
        // errSecItemNotFound, errSecDuplicateItem, errSecParam,
        // errSecInteractionNotAllowed, errSecAuthFailed, userCanceledErr,
        // errSecNotAvailable, plus a code the system has no message for.
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
            let mine = Error::from_code(code);
            let oracle = security_framework::base::Error::from_code(code);
            assert_eq!(mine.code(), oracle.code(), "code {code}");
            assert_eq!(mine.message(), oracle.message(), "message for {code}");
            assert_eq!(mine.to_string(), oracle.to_string(), "Display for {code}");
        }
    }

    /// A zero status is not a failure: both implementations refuse to build an
    /// `Error` that would compare equal to `errSecSuccess`.
    #[test]
    fn zero_status_is_never_a_success_shaped_error() {
        assert_eq!(
            Error::from_code(0).code(),
            security_framework::base::Error::from_code(0).code(),
        );
        assert_ne!(Error::from_code(0).code(), 0);
    }
}

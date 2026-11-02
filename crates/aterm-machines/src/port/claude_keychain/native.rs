// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Security.framework runs only in the disposable, no-UI child.

use super::{MAX_BYTES, ReadError, service, stage_text};
use aterm_secrets::SecretBytes;
use std::ffi::c_void;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

type Ref = *const c_void;

#[path = "provision.rs"]
mod provision;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFStringCreateMutableCopy(allocator: Ref, maximum: isize, value: Ref) -> Ref;
    fn CFStringNormalize(value: Ref, form: isize);
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        value: Ref,
        buffer: *mut std::ffi::c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFNumberCreate(allocator: Ref, kind: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        keys_callbacks: Ref,
        values_callbacks: Ref,
    ) -> Ref;
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFEqual(left: Ref, right: Ref) -> u8;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(value: Ref) -> isize;
    fn CFArrayGetValueAtIndex(value: Ref, index: isize) -> Ref;
    fn CFArrayCreate(allocator: Ref, values: *const Ref, count: isize, callbacks: Ref) -> Ref;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(value: Ref) -> isize;
    fn CFDataGetBytePtr(value: Ref) -> *const u8;
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: Ref;
    static kCFTypeArrayCallBacks: u8;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecItemCopyMatching(query: Ref, result: *mut Ref) -> i32;
    fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> i32;
    #[cfg(any(test, debug_assertions))]
    fn SecKeychainOpen(path: *const std::ffi::c_char, result: *mut Ref) -> i32;
    fn SecTrustedApplicationCreateFromPath(path: *const std::ffi::c_char, result: *mut Ref) -> i32;
    fn SecAccessCreate(descriptor: Ref, trusted: Ref, result: *mut Ref) -> i32;
    fn SecItemAdd(attributes: Ref, result: *mut Ref) -> i32;
    static kSecClass: Ref;
    static kSecClassGenericPassword: Ref;
    static kSecAttrService: Ref;
    static kSecAttrAccount: Ref;
    static kSecReturnData: Ref;
    static kSecMatchLimit: Ref;
    static kSecUseAuthenticationUI: Ref;
    static kSecUseAuthenticationUIFail: Ref;
    #[cfg(any(test, debug_assertions))]
    static kSecMatchSearchList: Ref;
    #[cfg(any(test, debug_assertions))]
    static kSecUseKeychain: Ref;
    static kSecAttrAccess: Ref;
    static kSecValueData: Ref;
    static kSecAttrGeneric: Ref;
    static kSecAttrComment: Ref;
    static kSecReturnAttributes: Ref;
}

struct Owned(Ref);

impl Owned {
    fn new(value: Ref) -> Result<Self, ReadError> {
        if value.is_null() {
            Err(ReadError::Unavailable)
        } else {
            Ok(Self(value))
        }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: Owned is constructed only for non-null +1 CoreFoundation results.
        unsafe { CFRelease(self.0) };
    }
}

struct Stage {
    path: PathBuf,
    parent: PathBuf,
    identity: [(u64, u64); 2],
}

fn identity(path: &Path) -> Result<(u64, u64), ReadError> {
    aterm_uds::private_file::check_private_dir(path).map_err(|_| ReadError::Refused)?;
    let meta = std::fs::symlink_metadata(path).map_err(|_| ReadError::Refused)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(ReadError::Refused);
    }
    Ok((meta.dev(), meta.ino()))
}

impl Stage {
    fn take(path: &Path) -> Result<Self, ReadError> {
        let parent = path.parent().ok_or(ReadError::Refused)?;
        Ok(Self {
            path: path.to_owned(),
            parent: parent.to_owned(),
            identity: [identity(parent)?, identity(path)?],
        })
    }

    fn current(&self) -> Result<(), ReadError> {
        if self.identity != [identity(&self.parent)?, identity(&self.path)?] {
            return Err(ReadError::Refused);
        }
        Ok(())
    }
}

pub(super) fn answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    aterm_uds::sudo::stdout_is_anon_pipe().map_err(|_| ReadError::Refused)?;
    super::super::secret::no_core_dumps();
    read_in_scope(path, query)
}

pub(super) fn prepare_answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    aterm_uds::sudo::stdout_is_anon_pipe().map_err(|_| ReadError::Refused)?;
    super::super::secret::no_core_dumps();
    // SAFETY: Only this disposable child's interaction policy changes.
    if unsafe { SecKeychainSetUserInteractionAllowed(0) } != 0 {
        return Err(ReadError::Unavailable);
    }
    read_in_scope(path, |service| {
        provision::prepare(path, service, None, None)
    })
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_prepare_answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    with_test_keychain(path, true, |service, keychain, search| {
        provision::prepare(path, service, Some(keychain), Some(search))
    })
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_vendor_scope(path: &Path) -> Result<(String, String), ReadError> {
    Ok((service(&nfc(stage_text(path)?)?), account()))
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    test_query(path, |_, _| {})
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_diagnostic_answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    let mut observation = None;
    let result = test_query(path, |status, value| {
        let (kind, count, element) = if status != 0 || value.is_null() {
            ("none", 0, "none")
        } else {
            // SAFETY: The observer borrows the live result returned by Security.
            unsafe {
                if CFGetTypeID(value) == CFArrayGetTypeID() {
                    let count = CFArrayGetCount(value);
                    let element = if count == 1 {
                        let first = CFArrayGetValueAtIndex(value, 0);
                        if first.is_null() {
                            "none"
                        } else if CFGetTypeID(first) == CFDataGetTypeID() {
                            "data"
                        } else {
                            "other"
                        }
                    } else {
                        "none"
                    };
                    ("array", count, element)
                } else if CFGetTypeID(value) == CFDataGetTypeID() {
                    ("data", 1, "none")
                } else {
                    ("other", 0, "none")
                }
            }
        };
        observation = Some(format!(
            "status={status} kind={kind} count={count} element={element}"
        ));
    });
    if matches!(result, Err(ReadError::Refused)) {
        return Err(ReadError::Refused);
    }
    let observation = observation.ok_or_else(|| result.err().unwrap_or(ReadError::Unavailable))?;
    Ok(Some(SecretBytes::new(observation.into_bytes())))
}

#[cfg(any(test, debug_assertions))]
fn test_query(
    path: &Path,
    observe: impl FnOnce(i32, Ref),
) -> Result<Option<SecretBytes>, ReadError> {
    with_test_keychain(path, false, |service, _keychain, search| {
        query_observed(service, Some(search), observe)
    })
}

#[cfg(any(test, debug_assertions))]
fn with_test_keychain(
    path: &Path,
    creates: bool,
    run: impl FnOnce(&str, Ref, Ref) -> Result<Option<SecretBytes>, ReadError>,
) -> Result<Option<SecretBytes>, ReadError> {
    use std::os::unix::ffi::OsStrExt as _;

    aterm_uds::sudo::stdout_is_anon_pipe().map_err(|_| ReadError::Refused)?;
    super::super::secret::no_core_dumps();
    // SAFETY: This disposable child owns no other work; refuse all legacy UI
    // before opening even the explicitly scoped synthetic keychain.
    if unsafe { SecKeychainSetUserInteractionAllowed(0) } != 0 {
        return Err(ReadError::Unavailable);
    }
    read_in_scope(path, |service| {
        let directory = path.join(super::TEST_KEYCHAIN_DIR);
        let (_file, before) =
            aterm_uds::private_file::open_private(&directory, super::TEST_KEYCHAIN)
                .map_err(|_| ReadError::Refused)?;
        let filename = directory.join(super::TEST_KEYCHAIN);
        let filename = std::ffi::CString::new(filename.as_os_str().as_bytes())
            .map_err(|_| ReadError::Refused)?;
        let mut keychain = std::ptr::null();
        // SAFETY: filename is a live NUL-terminated path; keychain is a writable
        // result slot. SecKeychainOpen neither creates nor changes the search list.
        if unsafe { SecKeychainOpen(filename.as_ptr(), &raw mut keychain) } != 0 {
            return Err(ReadError::Unavailable);
        }
        let keychain = Owned::new(keychain)?;
        // SAFETY: keychain is a live retained SecKeychainRef. Array creation retains
        // it using the native CF callbacks for the lifetime of the query.
        let search = Owned::new(unsafe {
            CFArrayCreate(
                std::ptr::null(),
                &raw const keychain.0,
                1,
                (&raw const kCFTypeArrayCallBacks).cast(),
            )
        })?;
        let result = run(service, keychain.0, search.0);
        let (_file, after) =
            aterm_uds::private_file::open_private(&directory, super::TEST_KEYCHAIN)
                .map_err(|_| ReadError::Refused)?;
        // A native create may atomically replace its own keychain database.
        // Read-only queries retain their exact file-identity requirement.
        if !creates && (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(ReadError::Refused);
        }
        result
    })
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_seed_answer(path: &Path) -> Result<Option<SecretBytes>, ReadError> {
    use std::os::unix::ffi::OsStrExt as _;
    with_test_keychain(path, true, |service, keychain, _| {
        let own = std::env::current_exe().map_err(|_| ReadError::Unavailable)?;
        let trusted = |path: &[u8]| -> Result<Owned, ReadError> {
            let path = std::ffi::CString::new(path).map_err(|_| ReadError::Refused)?;
            let mut trusted = std::ptr::null();
            // SAFETY: The path is a live NUL-terminated executable path and the
            // output slot is writable. The returned object is owned at +1.
            if unsafe { SecTrustedApplicationCreateFromPath(path.as_ptr(), &raw mut trusted) } != 0
            {
                return Err(ReadError::Unavailable);
            }
            Owned::new(trusted)
        };
        let own = trusted(own.as_os_str().as_bytes())?;
        let writer = trusted(b"/usr/bin/security")?;
        let applications = [own.0, writer.0];
        // SAFETY: Both +1 trusted-app objects remain live while CFArray retains
        // them using its native callbacks.
        let applications = Owned::new(unsafe {
            CFArrayCreate(
                std::ptr::null(),
                applications.as_ptr(),
                applications.len() as isize,
                (&raw const kCFTypeArrayCallBacks).cast(),
            )
        })?;
        let string = |value: &str| {
            Owned::new(unsafe {
                // SAFETY: Rust's UTF-8 bytes remain live for the synchronous copy.
                CFStringCreateWithBytes(
                    std::ptr::null(),
                    value.as_ptr(),
                    value.len() as isize,
                    0x0800_0100,
                    0,
                )
            })
        };
        let descriptor = string("aterm private synthetic authentication fixture")?;
        let mut access = std::ptr::null();
        // SAFETY: Descriptor and application array are live CF objects, output
        // is writable, and SecAccessCreate returns its own +1 reference.
        if unsafe { SecAccessCreate(descriptor.0, applications.0, &raw mut access) } != 0 {
            return Err(ReadError::Unavailable);
        }
        let access = Owned::new(access)?;
        let service = string(service)?;
        let account = string(&account())?;
        let placeholder = b"{}";
        // SAFETY: The fixed non-authentication bytes are copied into CFData.
        let data = Owned::new(unsafe {
            CFDataCreate(
                std::ptr::null(),
                placeholder.as_ptr(),
                placeholder.len() as isize,
            )
        })?;
        // SAFETY: All keys are immutable Security constants, all values are live
        // CF objects, and the explicit private keychain is the only target.
        let attributes = Owned::new(unsafe {
            let keys = [
                kSecClass,
                kSecAttrService,
                kSecAttrAccount,
                kSecValueData,
                kSecAttrAccess,
                kSecUseKeychain,
            ];
            let values = [
                kSecClassGenericPassword,
                service.0,
                account.0,
                data.0,
                access.0,
                keychain,
            ];
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as isize,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            )
        })?;
        // SAFETY: SecItemAdd copies the validated attributes. Duplicate items
        // refuse; this test helper never updates an existing item's access.
        if unsafe { SecItemAdd(attributes.0, std::ptr::null_mut()) } != 0 {
            return Err(ReadError::Unavailable);
        }
        Ok(Some(SecretBytes::from(b"created".as_slice())))
    })
}

fn read_in_scope(
    path: &Path,
    query: impl FnOnce(&str) -> Result<Option<SecretBytes>, ReadError>,
) -> Result<Option<SecretBytes>, ReadError> {
    let name = service(&nfc(stage_text(path)?)?);
    let stage = Stage::take(path)?;
    let result = query(&name);
    stage.current()?;
    result
}

fn query(service: &str) -> Result<Option<SecretBytes>, ReadError> {
    query_with_search(service, None)
}

fn query_with_search(service: &str, search: Option<Ref>) -> Result<Option<SecretBytes>, ReadError> {
    query_observed(service, search, |_, _| {})
}

fn query_observed(
    service: &str,
    search: Option<Ref>,
    observe: impl FnOnce(i32, Ref),
) -> Result<Option<SecretBytes>, ReadError> {
    // SAFETY: This process is the isolated helper. Disabling its legacy Keychain
    // interaction affects no caller or other process and no other work runs here.
    if unsafe { SecKeychainSetUserInteractionAllowed(0) } != 0 {
        return Err(ReadError::Unavailable);
    }
    // SAFETY: The borrowed UTF-8 bytes remain live for creation; encoding names UTF-8.
    let name = Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            service.as_ptr(),
            service.len() as isize,
            0x0800_0100,
            0,
        )
    })?;
    let account = account();
    // SAFETY: account contains bounded UTF-8 bytes, copied by CFString creation.
    let account = Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            account.as_ptr(),
            account.len() as isize,
            0x0800_0100,
            0,
        )
    })?;
    let maximum: i32 = 2;
    // SAFETY: kCFNumberSInt32Type is 3; maximum is a live i32 copied by creation.
    let limit =
        Owned::new(unsafe { CFNumberCreate(std::ptr::null(), 3, (&raw const maximum).cast()) })?;
    // SAFETY: These are Security/CoreFoundation's exported immutable object pointers.
    // The dictionary retains them with the framework's native callback tables.
    let query = unsafe {
        let keys = [
            kSecClass,
            kSecAttrService,
            kSecAttrAccount,
            kSecReturnData,
            kSecMatchLimit,
            kSecUseAuthenticationUI,
        ];
        let values = [
            kSecClassGenericPassword,
            name.0,
            account.0,
            kCFBooleanTrue,
            limit.0,
            kSecUseAuthenticationUIFail,
        ];
        #[cfg(any(test, debug_assertions))]
        let (keys, values) = {
            let (mut keys, mut values) = (keys.to_vec(), values.to_vec());
            if let Some(search) = search {
                keys.push(kSecMatchSearchList);
                values.push(search);
            }
            (keys, values)
        };
        #[cfg(not(any(test, debug_assertions)))]
        let _ = search;
        Owned::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        ))?
    };
    let mut result = std::ptr::null();
    // SAFETY: query is a live dictionary with documented SecItem query entries;
    // result is a writable pointer slot. CopyMatching transfers a +1 result on success.
    let status = unsafe { SecItemCopyMatching(query.0, &raw mut result) };
    observe(status, result);
    match status {
        -25300 => return Ok(None),                    // errSecItemNotFound
        -25308 => return Err(ReadError::Interaction), // errSecInteractionNotAllowed
        0 => {}
        _ => return Err(ReadError::Unavailable),
    }
    let result = Owned::new(result)?;
    // SAFETY: result is a live CF object. MatchLimit > 1 with ReturnData must produce
    // an array; checking the type precedes every array operation.
    if unsafe { CFGetTypeID(result.0) != CFArrayGetTypeID() } {
        return Err(ReadError::Unavailable);
    }
    // SAFETY: The type check above establishes the array contract.
    match unsafe { CFArrayGetCount(result.0) } {
        0 => return Err(ReadError::Unavailable),
        1 => {}
        _ => return Err(ReadError::Ambiguous),
    }
    // SAFETY: The array has exactly one entry, retained by result for this scope.
    let data = unsafe { CFArrayGetValueAtIndex(result.0, 0) };
    if data.is_null() || unsafe { CFGetTypeID(data) != CFDataGetTypeID() } {
        return Err(ReadError::Unavailable);
    }
    // SAFETY: data's checked CFData type establishes both accessor contracts.
    let len =
        usize::try_from(unsafe { CFDataGetLength(data) }).map_err(|_| ReadError::Unavailable)?;
    if len > MAX_BYTES {
        return Err(ReadError::TooLarge);
    }
    if len == 0 {
        return Ok(Some(SecretBytes::default()));
    }
    // SAFETY: CFData owns len live bytes for the duration of result's lifetime.
    let bytes = unsafe { CFDataGetBytePtr(data) };
    if bytes.is_null() {
        return Err(ReadError::Unavailable);
    }
    Ok(Some(SecretBytes::from(unsafe {
        std::slice::from_raw_parts(bytes, len)
    })))
}

fn account_value(
    user: Option<&str>,
    os_user: impl FnOnce() -> Result<String, ReadError>,
) -> String {
    let user = match user.filter(|s| !s.is_empty()) {
        Some(user) => user.to_owned(),
        None => os_user().unwrap_or_else(|_| "claude-code-user".into()),
    };
    if user
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && !user.is_empty()
    {
        user
    } else {
        "claude-code-user".to_owned()
    }
}

fn account() -> String {
    let user = std::env::var_os("USER");
    if user
        .as_ref()
        .is_some_and(|s| !s.is_empty() && s.to_str().is_none())
    {
        return "claude-code-user".into();
    }
    account_value(user.as_deref().and_then(|s| s.to_str()), os_username)
}

fn os_username() -> Result<String, ReadError> {
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: passwd, buffer and result are valid writable storage for the
        // reentrant lookup; pointers in a successful result borrow buffer.
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &raw mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 64 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return Err(ReadError::Unavailable);
        }
        // SAFETY: successful getpwuid_r initialized the result and its pw_name.
        let name = unsafe { (*result).pw_name };
        if name.is_null() {
            return Err(ReadError::Unavailable);
        }
        // SAFETY: name is the NUL-terminated string borrowing our live buffer.
        return Ok(unsafe { std::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned());
    }
}

fn nfc(text: &str) -> Result<String, ReadError> {
    const UTF8: u32 = 0x0800_0100;
    // SAFETY: The UTF-8 bytes remain live until creation has copied them.
    let original = Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            text.as_ptr(),
            text.len() as isize,
            UTF8,
            0,
        )
    })?;
    // SAFETY: original is a live CFString; maximum=0 permits its existing length.
    let normalized =
        Owned::new(unsafe { CFStringCreateMutableCopy(std::ptr::null(), 0, original.0) })?;
    // SAFETY: normalized is the uniquely owned mutable string; form2 is NFC.
    unsafe { CFStringNormalize(normalized.0, 2) };
    // SAFETY: The length/accessor accept this live CFString and UTF-8 encoding.
    let maximum =
        unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(normalized.0), UTF8) };
    let size = usize::try_from(maximum)
        .ok()
        .and_then(|v| v.checked_add(1))
        .filter(|v| *v <= 64 * 1024)
        .ok_or(ReadError::Refused)?;
    let mut output = vec![0_u8; size];
    // SAFETY: output is writable for size bytes, including the trailing NUL.
    if unsafe {
        CFStringGetCString(
            normalized.0,
            output.as_mut_ptr().cast(),
            size as isize,
            UTF8,
        )
    } == 0
    {
        return Err(ReadError::Unavailable);
    }
    let end = output
        .iter()
        .position(|b| *b == 0)
        .ok_or(ReadError::Unavailable)?;
    output.truncate(end);
    String::from_utf8(output).map_err(|_| ReadError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn vendor_account_uses_user_then_os_and_rejects_unsafe_names() {
        let not_called = || -> Result<String, ReadError> { panic!("USER should win") };
        assert_eq!(account_value(Some("person"), not_called), "person");
        assert_eq!(account_value(None, || Ok("os-person".into())), "os-person");
        assert_eq!(
            account_value(Some(""), || Ok("os-person".into())),
            "os-person"
        );
        assert_eq!(
            account_value(None, || Err(ReadError::Unavailable)),
            "claude-code-user"
        );
        for invalid in ["with space", "person@example", "é"] {
            assert_eq!(account_value(Some(invalid), not_called), "claude-code-user");
        }
    }

    #[test]
    fn vendor_service_normalizes_unicode_but_does_not_resolve_paths() {
        let composed = "/private/café/logins/person+1";
        let decomposed = "/private/cafe\u{301}/logins/person+1";
        assert_eq!(
            service(&nfc(composed).unwrap()),
            service(&nfc(decomposed).unwrap())
        );
        assert_ne!(
            service(&nfc(composed).unwrap()),
            service(&nfc("/private/a/../café/logins/person+1").unwrap())
        );
    }

    fn stage() -> (aterm_tempfile::TempDir, PathBuf) {
        let temp = aterm_tempfile::tempdir().unwrap();
        let parent = temp.path().join("logins");
        aterm_uds::private_file::create_private_dir(&parent).unwrap();
        let stage = parent.join("person+1");
        aterm_uds::private_file::create_private_dir(&stage).unwrap();
        (temp, stage)
    }

    #[test]
    fn scoped_read_accepts_private_stage_and_refuses_replacement_during_query() {
        let (_temp, path) = stage();
        let result = read_in_scope(&path, |_| {
            Ok(Some(SecretBytes::from(b"fixture".as_slice())))
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.as_slice(), b"fixture");
        let result = read_in_scope(&path, |_| {
            std::fs::rename(&path, path.with_file_name("kept-original")).unwrap();
            aterm_uds::private_file::create_private_dir(&path).unwrap();
            Ok(Some(SecretBytes::from(b"fixture".as_slice())))
        });
        assert!(matches!(result, Err(ReadError::Refused)));
    }

    #[test]
    fn loose_or_symlinked_stage_and_parent_never_reach_query() {
        let (_temp, path) = stage();
        let query = |_: &str| -> Result<Option<SecretBytes>, ReadError> {
            panic!("unsafe scope reached query")
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            read_in_scope(&path, query),
            Err(ReadError::Refused)
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let parent = path.parent().unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            read_in_scope(&path, query),
            Err(ReadError::Refused)
        ));
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let moved = path.with_file_name("kept-original");
        std::fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(matches!(
            read_in_scope(&path, query),
            Err(ReadError::Refused)
        ));
    }
}

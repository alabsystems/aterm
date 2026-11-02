// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Create-only access for an exact private staging directory. A durable random
//! marker is also held in the item's generic attribute; it grants no login.

use super::*;
use aterm_digest::Sha256;

const MARKER: &str = ".aterm-claude-keychain-owner";
const LOCK: &str = ".aterm-claude-keychain-owner.lock";
const MAGIC: &[u8; 5] = b"ATCK1";
const MARKER_LEN: usize = 69;
const COMMENT: &str = "aterm owned staged Claude authentication";

pub(super) fn prepare(
    path: &Path,
    service: &str,
    keychain: Option<Ref>,
    search: Option<Ref>,
) -> Result<Option<SecretBytes>, ReadError> {
    let stage = Stage::take(path)?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        path,
        LOCK,
        std::time::Duration::ZERO,
        "staged credential setup is busy",
    )
    .map_err(|_| ReadError::Unavailable)?;
    let _ = aterm_uds::private_file::open_private(path, LOCK).map_err(|_| ReadError::Refused)?;
    let account = account();
    let marker = marker(&stage, service, &account, keychain.is_some())?;
    if let Some(owned) = item(service, &account, &marker, search)? {
        if !owned {
            return Err(ReadError::Refused);
        }
    } else {
        if !matches!(std::fs::symlink_metadata(path.join(".credentials.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(ReadError::Refused);
        }
        create(service, &account, &marker, keychain)?;
        if item(service, &account, &marker, search)? != Some(true) {
            return Err(ReadError::Refused);
        }
    }
    // Existing matching items are observed, never overwritten. This also proves
    // that the no-UI reader still has access before a provider effect starts.
    if query_with_search(service, search)?.is_none() {
        return Err(ReadError::Unavailable);
    }
    stage.current()?;
    Ok(Some(SecretBytes::from(b"ready".as_slice())))
}

fn marker(
    stage: &Stage,
    service: &str,
    account: &str,
    fixture: bool,
) -> Result<Vec<u8>, ReadError> {
    let mut hash = Sha256::new();
    hash.update(b"aterm/claude-stage-keychain-owner/1\0");
    hash.update(service.as_bytes());
    hash.update([0]);
    hash.update(account.as_bytes());
    for (dev, ino) in stage.identity {
        hash.update(dev.to_le_bytes());
        hash.update(ino.to_le_bytes());
    }
    let binding = hash.finalize();
    match aterm_uds::private_file::read_private(&stage.path, MARKER, MARKER_LEN) {
        Ok(bytes) => {
            if bytes.len() != MARKER_LEN
                || &bytes[..5] != MAGIC
                || bytes[5..37] != binding
                || bytes[37..].iter().all(|byte| *byte == 0)
            {
                return Err(ReadError::Refused);
            }
            Ok(bytes)
        }
        Err(aterm_uds::private_file::PrivateFileError::Missing) => {
            for entry in std::fs::read_dir(&stage.path).map_err(|_| ReadError::Refused)? {
                let name = entry.map_err(|_| ReadError::Refused)?.file_name();
                let allowed = name == LOCK;
                #[cfg(any(test, debug_assertions))]
                let allowed = allowed || (fixture && name == super::super::TEST_KEYCHAIN_DIR);
                if !allowed {
                    return Err(ReadError::Refused);
                }
            }
            #[cfg(not(any(test, debug_assertions)))]
            let _ = fixture;
            let mut bytes = vec![0_u8; MARKER_LEN];
            bytes[..5].copy_from_slice(MAGIC);
            bytes[5..37].copy_from_slice(&binding);
            aterm_uds::rand::fill(&mut bytes[37..]).map_err(|_| ReadError::Unavailable)?;
            aterm_uds::private_file::write_private(&stage.path, MARKER, &bytes)
                .map_err(|_| ReadError::Refused)?;
            Ok(bytes)
        }
        Err(_) => Err(ReadError::Refused),
    }
}

fn string(value: &str) -> Result<Owned, ReadError> {
    // SAFETY: The UTF-8 slice is copied before this call returns.
    Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as isize,
            0x0800_0100,
            0,
        )
    })
}

fn data(value: &[u8]) -> Result<Owned, ReadError> {
    // SAFETY: CFDataCreate copies the live byte slice synchronously.
    Owned::new(unsafe { CFDataCreate(std::ptr::null(), value.as_ptr(), value.len() as isize) })
}

fn dictionary(keys: &[Ref], values: &[Ref]) -> Result<Owned, ReadError> {
    debug_assert_eq!(keys.len(), values.len());
    // SAFETY: Every caller retains all keys and values through this call. The
    // native callbacks retain them for the resulting dictionary's lifetime.
    Owned::new(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        )
    })
}

fn item(
    service: &str,
    account: &str,
    marker: &[u8],
    search: Option<Ref>,
) -> Result<Option<bool>, ReadError> {
    let service = string(service)?;
    let account = string(account)?;
    let expected = data(marker)?;
    let comment = string(COMMENT)?;
    let maximum: i32 = 2;
    // SAFETY: CFNumber copies this live i32; 3 denotes kCFNumberSInt32Type.
    let limit =
        Owned::new(unsafe { CFNumberCreate(std::ptr::null(), 3, (&raw const maximum).cast()) })?;
    // SAFETY: These are immutable framework objects.
    let (keys, values) = unsafe {
        (
            vec![
                kSecClass,
                kSecAttrService,
                kSecAttrAccount,
                kSecReturnAttributes,
                kSecMatchLimit,
                kSecUseAuthenticationUI,
            ],
            vec![
                kSecClassGenericPassword,
                service.0,
                account.0,
                kCFBooleanTrue,
                limit.0,
                kSecUseAuthenticationUIFail,
            ],
        )
    };
    #[cfg(any(test, debug_assertions))]
    let (keys, values) = {
        let (mut keys, mut values) = (keys, values);
        if let Some(search) = search {
            // SAFETY: The fixture holds its private search array for this call.
            keys.push(unsafe { kSecMatchSearchList });
            values.push(search);
        }
        (keys, values)
    };
    #[cfg(not(any(test, debug_assertions)))]
    let _ = search;
    let query = dictionary(&keys, &values)?;
    let mut result = std::ptr::null();
    // SAFETY: Query owns every attribute, and result is a writable output slot.
    match unsafe { SecItemCopyMatching(query.0, &raw mut result) } {
        -25300 => return Ok(None),
        -25308 => return Err(ReadError::Interaction),
        0 => {}
        _ => return Err(ReadError::Unavailable),
    }
    let result = Owned::new(result)?;
    // SAFETY: Type checks precede every CF collection accessor.
    unsafe {
        if CFGetTypeID(result.0) != CFArrayGetTypeID() {
            return Err(ReadError::Unavailable);
        }
        if CFArrayGetCount(result.0) != 1 {
            return Err(ReadError::Ambiguous);
        }
        let attributes = CFArrayGetValueAtIndex(result.0, 0);
        if attributes.is_null() || CFGetTypeID(attributes) != CFDictionaryGetTypeID() {
            return Err(ReadError::Unavailable);
        }
        let generic = CFDictionaryGetValue(attributes, kSecAttrGeneric);
        let found_comment = CFDictionaryGetValue(attributes, kSecAttrComment);
        Ok(Some(
            !generic.is_null()
                && !found_comment.is_null()
                && CFEqual(generic, expected.0) != 0
                && CFEqual(found_comment, comment.0) != 0,
        ))
    }
}

fn create(
    service: &str,
    account: &str,
    marker: &[u8],
    keychain: Option<Ref>,
) -> Result<(), ReadError> {
    use std::os::unix::ffi::OsStrExt as _;
    let trusted = |path: &[u8]| -> Result<Owned, ReadError> {
        let path = std::ffi::CString::new(path).map_err(|_| ReadError::Refused)?;
        let mut result = std::ptr::null();
        // SAFETY: The executable path is NUL-terminated and the output writable.
        if unsafe { SecTrustedApplicationCreateFromPath(path.as_ptr(), &raw mut result) } != 0 {
            return Err(ReadError::Unavailable);
        }
        Owned::new(result)
    };
    let program = std::env::current_exe().map_err(|_| ReadError::Unavailable)?;
    let program = trusted(program.as_os_str().as_bytes())?;
    let security = trusted(b"/usr/bin/security")?;
    let applications = [program.0, security.0];
    // SAFETY: The array retains two live trusted-application objects.
    let applications = Owned::new(unsafe {
        CFArrayCreate(
            std::ptr::null(),
            applications.as_ptr(),
            applications.len() as isize,
            (&raw const kCFTypeArrayCallBacks).cast(),
        )
    })?;
    let comment = string(COMMENT)?;
    let mut access = std::ptr::null();
    // SAFETY: The descriptor and trusted array are live; output is writable.
    if unsafe { SecAccessCreate(comment.0, applications.0, &raw mut access) } != 0 {
        return Err(ReadError::Unavailable);
    }
    let access = Owned::new(access)?;
    let service = string(service)?;
    let account = string(account)?;
    let marker = data(marker)?;
    let placeholder = data(b"{}")?;
    // SAFETY: Immutable framework keys name these retained values.
    let (keys, values) = unsafe {
        (
            vec![
                kSecClass,
                kSecAttrService,
                kSecAttrAccount,
                kSecAttrGeneric,
                kSecAttrComment,
                kSecAttrAccess,
                kSecValueData,
            ],
            vec![
                kSecClassGenericPassword,
                service.0,
                account.0,
                marker.0,
                comment.0,
                access.0,
                placeholder.0,
            ],
        )
    };
    #[cfg(any(test, debug_assertions))]
    let (keys, values) = {
        let (mut keys, mut values) = (keys, values);
        if let Some(keychain) = keychain {
            // SAFETY: The fixture holds the exact private keychain reference.
            keys.push(unsafe { kSecUseKeychain });
            values.push(keychain);
        }
        (keys, values)
    };
    #[cfg(not(any(test, debug_assertions)))]
    let _ = keychain;
    let attributes = dictionary(&keys, &values)?;
    // SAFETY: Add copies the attributes. It cannot update an existing item;
    // duplicate service/account is a refusal, never an ACL replacement.
    match unsafe { SecItemAdd(attributes.0, std::ptr::null_mut()) } {
        0 => Ok(()),
        -25299 => Err(ReadError::Refused),
        -25308 => Err(ReadError::Interaction),
        _ => Err(ReadError::Unavailable),
    }
}

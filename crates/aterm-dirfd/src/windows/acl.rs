// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! WINDOWS SECURITY DESCRIPTORS: this process token's accounts ([`Sid`]), a protected access list
//! built from the accounts a caller names ([`protected_dacl`]), files and folders made with a
//! descriptor at their creation ([`create_file`], [`create_dir`]: `SECURITY_ATTRIBUTES`, never a
//! file made open and narrowed after), and the owner-only verdict on an open handle
//! ([`owner_only`]), which `aterm_uds::private_file`'s Windows arm holds every file and folder to
//! (aterm-uds re-exports this module as `aterm_uds::acl`).
//!
//! This crate's own handle-relative creates make a private file or folder with
//! [`owner_only_descriptor`] (`openat`, `mkdirat` with a mode that grants the group and others
//! nothing), and `fchmod` writes it in place.
//!
//! The window's control folder builds its access lists here too, each with its own set of
//! accounts: its shared posture names Administrators beside the user and SYSTEM, and its
//! elevated posture is its own descriptor text.

#![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

use std::ffi::c_void;
use std::fmt;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsHandle, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr::null_mut;

use super::ffi;

/// `SE_OBJECT_TYPE::SE_FILE_OBJECT`.
const SE_FILE_OBJECT: u32 = 1;
/// `SDDL_REVISION_1`.
const SDDL_REVISION_1: u32 = 1;
/// `TOKEN_QUERY`.
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`.
const TOKEN_USER_CLASS: i32 = 1;
/// `TOKEN_INFORMATION_CLASS::TokenOwner`.
const TOKEN_OWNER_CLASS: i32 = 4;
/// `ACL_INFORMATION_CLASS::AclSizeInformation`.
const ACL_SIZE_INFORMATION_CLASS: u32 = 2;
/// `SECURITY_DESCRIPTOR_CONTROL`: a DACL is present.
const SE_DACL_PRESENT: u16 = 0x0004;
/// `SECURITY_DESCRIPTOR_CONTROL`: the DACL takes no entry from its parent.
const SE_DACL_PROTECTED: u16 = 0x1000;
/// `ACCESS_ALLOWED_ACE_TYPE`.
pub const ACCESS_ALLOWED: u8 = 0;
/// `ACCESS_DENIED_ACE_TYPE`.
pub const ACCESS_DENIED: u8 = 1;
/// `INHERITED_ACE`: the entry came from the object's parent.
pub const INHERITED: u8 = 0x10;
/// `FILE_ALL_ACCESS`, the `FA` of descriptor text.
pub const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// `ERROR_SUCCESS`.
const ERROR_SUCCESS: u32 = 0;
/// `GENERIC_READ`.
const GENERIC_READ: u32 = 0x8000_0000;
/// `GENERIC_WRITE`.
const GENERIC_WRITE: u32 = 0x4000_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`, std's default sharing.
const SHARE_ALL: u32 = 0x0000_0007;
/// `CREATE_NEW`: fails on any name that exists, a link or a junction included.
const CREATE_NEW: u32 = 1;
/// `FILE_ATTRIBUTE_NORMAL`.
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: a reparse point is opened itself, never followed.
pub const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `FILE_FLAG_BACKUP_SEMANTICS`, which a folder's open needs.
pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// `FILE_ATTRIBUTE_REPARSE_POINT`: a symlink, a junction, a mount point, a placeholder.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
/// `WELL_KNOWN_SID_TYPE::WinLocalSystemSid`.
const WIN_LOCAL_SYSTEM_SID: u32 = 22;
/// `WELL_KNOWN_SID_TYPE::WinBuiltinAdministratorsSid`.
const WIN_BUILTIN_ADMINISTRATORS_SID: u32 = 26;

/// Which parts of a descriptor a read or a write takes (`SECURITY_INFORMATION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts(u32);

impl Parts {
    /// The owner (`OWNER_SECURITY_INFORMATION`).
    pub const OWNER: Self = Self(0x0000_0001);
    /// The access list (`DACL_SECURITY_INFORMATION`).
    pub const DACL: Self = Self(0x0000_0004);
    /// The mandatory label (`LABEL_SECURITY_INFORMATION`).
    pub const LABEL: Self = Self(0x0000_0010);
    /// With [`Self::DACL`] in a write: the list takes no entry from its parent
    /// (`PROTECTED_DACL_SECURITY_INFORMATION`).
    pub const PROTECTED_DACL: Self = Self(0x8000_0000);

    /// The `SECURITY_INFORMATION` bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    const fn has(self, part: Self) -> bool {
        self.0 & part.0 != 0
    }
}

impl std::ops::BitOr for Parts {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// An account's security identifier, copied out of wherever it was read.
#[derive(Clone, PartialEq, Eq)]
pub struct Sid {
    /// The SID's bytes in `u32` words, so the copy has the alignment its sub-authorities need;
    /// the tail past [`Self::len`] is zero.
    words: Vec<u32>,
    len: usize,
}

/// The accounts Windows names for itself that this module's access lists use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WellKnown {
    /// `NT AUTHORITY\SYSTEM` (`SY`).
    LocalSystem,
    /// `BUILTIN\Administrators` (`BA`).
    Administrators,
}

impl WellKnown {
    const fn kind(self) -> u32 {
        match self {
            Self::LocalSystem => WIN_LOCAL_SYSTEM_SID,
            Self::Administrators => WIN_BUILTIN_ADMINISTRATORS_SID,
        }
    }

    const fn sddl(self) -> &'static str {
        match self {
            Self::LocalSystem => "SY",
            Self::Administrators => "BA",
        }
    }
}

impl Sid {
    /// A copy of the SID at `sid`.
    ///
    /// # Safety
    /// `sid` is null or points at a whole SID, live for this call.
    unsafe fn copied(sid: *const c_void) -> io::Result<Self> {
        // SAFETY: the caller's contract.
        unsafe { Self::copied_within(sid, usize::MAX) }
    }

    /// A copy of the SID at `sid`, refused where it is longer than `room` bytes.
    ///
    /// # Safety
    /// `sid` is null or points at memory that holds a SID's eight-byte header and the
    /// sub-authorities that header counts, or `room` bytes, whichever is fewer, live for this
    /// call.
    unsafe fn copied_within(sid: *const c_void, room: usize) -> io::Result<Self> {
        // SAFETY: the caller's contract; a null pointer is refused first. `IsValidSid` reads the
        // header and checks its revision and sub-authority count.
        if sid.is_null() || room < 8 || unsafe { ffi::IsValidSid(sid) } == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no valid SID"));
        }
        // SAFETY: `sid`'s header is a valid SID's (just checked); the length is computed from it.
        let len = unsafe { ffi::GetLengthSid(sid) } as usize;
        if len > room {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a SID runs past the bytes that hold it",
            ));
        }
        let mut words = vec![0u32; len.div_ceil(4)];
        // SAFETY: `sid` holds `len` bytes (GetLengthSid of a valid SID) and `words` at least as
        // many; the two do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(sid.cast::<u8>(), words.as_mut_ptr().cast(), len) };
        Ok(Self { words, len })
    }

    /// The SID's own bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `words` holds at least `len` initialized bytes, as `copied` and `parse` made it.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) }
    }

    /// The SID for a Win32 call that only reads it (a `PSID` is never const in their headers).
    #[must_use]
    pub fn as_ptr(&self) -> *mut c_void {
        self.words.as_ptr().cast_mut().cast()
    }

    /// This process token's user (`TokenUser`): the account the process runs as, the same
    /// whether or not the token is elevated.
    ///
    /// # Errors
    /// The token could not be opened or read.
    pub fn token_user() -> io::Result<Self> {
        token_sid(TOKEN_USER_CLASS)
    }

    /// The account this process token makes the owner of what it creates with no owner named
    /// (`TokenOwner`): the user unelevated, and in an administrator's elevated token usually
    /// `BUILTIN\Administrators`.
    ///
    /// # Errors
    /// The token could not be opened or read.
    pub fn token_owner() -> io::Result<Self> {
        token_sid(TOKEN_OWNER_CLASS)
    }

    /// The SID `text` spells (`S-1-5-…`, or a two-letter SDDL alias such as `BA`).
    ///
    /// # Errors
    /// `text` is no SID (`ConvertStringSidToSidW`'s error).
    pub fn parse(text: &str) -> io::Result<Self> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sid: *mut c_void = null_mut();
        // SAFETY: `wide` is NUL-terminated; on success `sid` is a LocalAlloc'd SID we free below.
        if unsafe { ffi::ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call above made `sid` a valid SID.
        let copied = unsafe { Self::copied(sid) };
        // SAFETY: `sid` was LocalAlloc'd by ConvertStringSidToSidW; freed once.
        unsafe { ffi::LocalFree(sid) };
        copied
    }

    /// Whether this SID is the well-known account `which`.
    #[must_use]
    pub fn is(&self, which: WellKnown) -> bool {
        // SAFETY: `self` holds a valid SID for the call's duration.
        unsafe { ffi::IsWellKnownSid(self.as_ptr(), which.kind()) != 0 }
    }
}

impl fmt::Display for Sid {
    /// The SID's string form (`S-1-5-…`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text: *mut u16 = null_mut();
        // SAFETY: `self` holds a valid SID; on success `text` is a LocalAlloc'd string.
        if unsafe { ffi::ConvertSidToStringSidW(self.as_ptr(), &mut text) } == 0 || text.is_null() {
            return f.write_str("(an unreadable SID)");
        }
        // SAFETY: on success `text` is NUL-terminated and LocalAlloc'd; read once, freed once.
        let spelled = unsafe { take_local_wide(text) };
        f.write_str(&spelled)
    }
}

impl fmt::Debug for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A NUL-terminated UTF-16 string a Win32 call LocalAlloc'd, copied out and freed.
///
/// # Safety
/// `text` is non-null, NUL-terminated and LocalAlloc'd, and nothing else frees it.
unsafe fn take_local_wide(text: *mut u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a NUL terminator within the allocation.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units precede the NUL in the same allocation.
    let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    // SAFETY: LocalAlloc'd by the caller's API; freed once.
    unsafe { ffi::LocalFree(text.cast()) };
    out
}

/// One SID of this process's token, by information class (`TokenUser` or `TokenOwner`, each a
/// structure that starts with the SID's pointer).
fn token_sid(class: i32) -> io::Result<Sid> {
    let mut raw: ffi::HANDLE = null_mut();
    // SAFETY: the current-process pseudo-handle is always valid; `raw` is a live local.
    if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful OpenProcessToken returned a fresh handle this function now owns
    // exclusively; dropping it closes it once.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let token = token.as_raw_handle();
    let mut len: u32 = 0;
    // SAFETY: a null buffer of length 0 asks for the length only.
    unsafe { ffi::GetTokenInformation(token, class, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: `buf` holds at least `len` bytes, pointer-aligned, and `len` is a live local.
    let ok =
        unsafe { ffi::GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `TOKEN_USER` and `TOKEN_OWNER` each start with the SID's pointer, which points
    // into `buf`, live for this call.
    unsafe { Sid::copied(buf[0] as *const c_void) }
}

/// Who an access-list entry [`protected_dacl`] writes names.
#[derive(Clone, Copy, Debug)]
pub enum Grantee<'a> {
    /// An account by its SID.
    Sid(&'a Sid),
    /// A well-known account.
    WellKnown(WellKnown),
}

/// Whether the entries [`protected_dacl`] writes pass on to what is made inside a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inherit {
    /// Each entry is the object's own alone: a file's.
    No,
    /// Each entry is also given to the files and folders made inside (`OICI`).
    ToChildren,
}

/// The descriptor text of a PROTECTED access list that allows each of `grantees` full access
/// (`FILE_ALL_ACCESS`) and nobody anything else: `D:P(A;<inherit>;FA;;;<grantee>)…`.
#[must_use]
pub fn protected_dacl(grantees: &[Grantee<'_>], inherit: Inherit) -> String {
    let flags = match inherit {
        Inherit::No => "",
        Inherit::ToChildren => "OICI",
    };
    let mut text = String::from("D:P");
    for grantee in grantees {
        let who = match grantee {
            Grantee::Sid(sid) => sid.to_string(),
            Grantee::WellKnown(which) => which.sddl().to_owned(),
        };
        text.push_str(&format!("(A;{flags};FA;;;{who})"));
    }
    text
}

/// An owner-only security descriptor: owned by `owner`, with the protected access list
/// [`protected_dacl`] writes for `owner` and SYSTEM. A file is made with `Inherit::No` and a
/// folder with `Inherit::ToChildren`.
///
/// # Errors
/// As [`Descriptor::from_sddl`].
pub fn owner_only_descriptor(owner: &Sid, inherit: Inherit) -> io::Result<Descriptor> {
    let dacl = protected_dacl(
        &[
            Grantee::Sid(owner),
            Grantee::WellKnown(WellKnown::LocalSystem),
        ],
        inherit,
    );
    Descriptor::from_sddl(&format!("O:{owner}{dacl}"))
}

/// A security descriptor the system allocated (self-relative), freed on drop.
pub struct Descriptor(*mut c_void);

impl Descriptor {
    /// `sddl` as a security descriptor (`ConvertStringSecurityDescriptorToSecurityDescriptorW`).
    ///
    /// # Errors
    /// `sddl` does not parse.
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `wide` is NUL-terminated; on success `psd` is LocalAlloc'd and owned by `Self`.
        let ok = unsafe {
            ffi::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                null_mut(),
            )
        };
        if ok == 0 || psd.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(psd))
    }

    /// The `parts` of the descriptor of the object `file` is open on (`GetSecurityInfo`; the
    /// handle needs `READ_CONTROL`, which every read open has).
    ///
    /// # Errors
    /// `GetSecurityInfo`'s.
    pub fn of(file: impl AsHandle, parts: Parts) -> io::Result<Self> {
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `file` is a live handle; every part's out pointer is null and only the whole
        // descriptor is asked for, which on success is LocalAlloc'd and owned by `Self`.
        let rc = unsafe {
            ffi::GetSecurityInfo(
                file.as_handle().as_raw_handle(),
                SE_FILE_OBJECT,
                parts.bits(),
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut psd,
            )
        };
        Self::read(rc, psd)
    }

    /// The `parts` of the descriptor of the file or folder at `path`, read by its name
    /// (`GetNamedSecurityInfoW`), which follows a link.
    ///
    /// # Errors
    /// `GetNamedSecurityInfoW`'s.
    pub fn of_path(path: &Path, parts: Parts) -> io::Result<Self> {
        let wide = wide(path);
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `wide` is NUL-terminated; as in `of`.
        let rc = unsafe {
            ffi::GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                parts.bits(),
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut psd,
            )
        };
        Self::read(rc, psd)
    }

    fn read(rc: u32, psd: *mut c_void) -> io::Result<Self> {
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        if psd.is_null() {
            return Err(io::Error::other("no security descriptor"));
        }
        Ok(Self(psd))
    }

    /// The descriptor itself, live while `self` is (for a Win32 call that reads one).
    #[must_use]
    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    /// Its owner, or `None` where it names none.
    ///
    /// # Errors
    /// `GetSecurityDescriptorOwner`'s.
    pub fn owner(&self) -> io::Result<Option<Sid>> {
        let mut owner: *mut c_void = null_mut();
        let mut defaulted: i32 = 0;
        // SAFETY: `self.0` is a live descriptor; the out pointers are locals.
        if unsafe { ffi::GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if owner.is_null() {
            return Ok(None);
        }
        // SAFETY: a non-null owner points into `self`, live for this call.
        unsafe { Sid::copied(owner) }.map(Some)
    }

    /// Its control bits (`SECURITY_DESCRIPTOR_CONTROL`).
    fn control(&self) -> io::Result<u16> {
        let (mut control, mut revision): (u16, u32) = (0, 0);
        // SAFETY: `self.0` is a live descriptor; the out pointers are locals.
        if unsafe { ffi::GetSecurityDescriptorControl(self.0, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(control)
    }

    /// Whether its access list is protected: it takes no entry from its parent.
    ///
    /// # Errors
    /// `GetSecurityDescriptorControl`'s.
    pub fn dacl_protected(&self) -> io::Result<bool> {
        Ok(self.control()? & SE_DACL_PROTECTED != 0)
    }

    /// Its access list, or null where it has none.
    fn dacl(&self) -> io::Result<*mut c_void> {
        let (mut present, mut defaulted): (i32, i32) = (0, 0);
        let mut dacl: *mut c_void = null_mut();
        // SAFETY: `self.0` is a live descriptor; the out pointers are locals.
        let ok = unsafe {
            ffi::GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(if present == 0 { null_mut() } else { dacl })
    }

    /// Its mandatory-label list, or null where it has none.
    fn sacl(&self) -> io::Result<*mut c_void> {
        let (mut present, mut defaulted): (i32, i32) = (0, 0);
        let mut sacl: *mut c_void = null_mut();
        // SAFETY: `self.0` is a live descriptor; the out pointers are locals.
        let ok = unsafe {
            ffi::GetSecurityDescriptorSacl(self.0, &mut present, &mut sacl, &mut defaulted)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(if present == 0 { null_mut() } else { sacl })
    }

    /// The entries of its access list in order, or `None` where it has no list at all (a NULL
    /// DACL, which lets everyone do everything; a list with no entries lets nobody do anything
    /// and is `Some(vec![])`).
    ///
    /// # Errors
    /// The list or one of its entries could not be read.
    pub fn entries(&self) -> io::Result<Option<Vec<Entry>>> {
        if self.control()? & SE_DACL_PRESENT == 0 {
            return Ok(None);
        }
        let dacl = self.dacl()?;
        if dacl.is_null() {
            return Ok(None);
        }
        let mut size = [0u32; 3];
        // SAFETY: `dacl` points into `self`; `size` is `ACL_SIZE_INFORMATION`'s three DWORDs.
        let ok = unsafe {
            ffi::GetAclInformation(
                dacl,
                size.as_mut_ptr().cast(),
                std::mem::size_of_val(&size) as u32,
                ACL_SIZE_INFORMATION_CLASS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        (0..size[0])
            .map(|index| {
                let mut ace: *mut c_void = null_mut();
                // SAFETY: `index` is below the list's entry count; `ace` is a local.
                if unsafe { ffi::GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `ace` points at a whole entry inside `self` (GetAce checked it is
                // within the list).
                unsafe { Entry::read(ace.cast::<u8>()) }
            })
            .collect::<io::Result<Vec<_>>>()
            .map(Some)
    }

    /// The descriptor's `parts` as descriptor text (`O:…D:…`).
    ///
    /// # Errors
    /// `ConvertSecurityDescriptorToStringSecurityDescriptorW`'s.
    pub fn to_sddl(&self, parts: Parts) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        // SAFETY: `self.0` is a live descriptor; on success `text` is LocalAlloc'd.
        let ok = unsafe {
            ffi::ConvertSecurityDescriptorToStringSecurityDescriptorW(
                self.0,
                SDDL_REVISION_1,
                parts.bits(),
                &mut text,
                null_mut(),
            )
        };
        if ok == 0 || text.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success `text` is NUL-terminated and LocalAlloc'd; read once, freed once.
        Ok(unsafe { take_local_wide(text) })
    }

    /// The owner, access list and label pointers `parts` asks to write, each pointing into
    /// `self` (null for a part not asked for).
    fn parts_to_write(&self, parts: Parts) -> io::Result<[*mut c_void; 3]> {
        let owner = if parts.has(Parts::OWNER) {
            let mut owner: *mut c_void = null_mut();
            let mut defaulted: i32 = 0;
            // SAFETY: `self.0` is a live descriptor; the out pointers are locals.
            if unsafe { ffi::GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) } == 0 {
                return Err(io::Error::last_os_error());
            }
            owner
        } else {
            null_mut()
        };
        let dacl = if parts.has(Parts::DACL) {
            self.dacl()?
        } else {
            null_mut()
        };
        let sacl = if parts.has(Parts::LABEL) {
            self.sacl()?
        } else {
            null_mut()
        };
        Ok([owner, dacl, sacl])
    }

    /// Write this descriptor's `parts` onto the object `file` is open on (`SetSecurityInfo`; the
    /// handle needs `WRITE_DAC`, and `WRITE_OWNER` for the owner).
    ///
    /// # Errors
    /// `SetSecurityInfo`'s.
    pub fn apply(&self, file: impl AsHandle, parts: Parts) -> io::Result<()> {
        let [owner, dacl, sacl] = self.parts_to_write(parts)?;
        // SAFETY: `file` is a live handle; the SID and the lists point into `self`, which
        // outlives the call.
        let rc = unsafe {
            ffi::SetSecurityInfo(
                file.as_handle().as_raw_handle(),
                SE_FILE_OBJECT,
                parts.bits(),
                owner,
                null_mut(),
                dacl,
                sacl,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }

    /// Write this descriptor's `parts` onto the file or folder at `path`, by its name
    /// (`SetNamedSecurityInfoW`, which follows a link and passes a folder's inheritable entries
    /// on to what is inside).
    ///
    /// # Errors
    /// `SetNamedSecurityInfoW`'s.
    pub fn apply_to_path(&self, path: &Path, parts: Parts) -> io::Result<()> {
        let [owner, dacl, sacl] = self.parts_to_write(parts)?;
        let mut wide = wide(path);
        // SAFETY: `wide` is NUL-terminated; the SID and the lists point into `self`, which
        // outlives the call.
        let rc = unsafe {
            ffi::SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                parts.bits(),
                owner,
                null_mut(),
                dacl,
                sacl,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system LocalAlloc'd the descriptor (a convert or a security read); freed
        // once.
        unsafe { ffi::LocalFree(self.0) };
    }
}

impl fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_sddl(Parts::OWNER | Parts::DACL) {
            Ok(text) => f.write_str(&text),
            Err(e) => write!(f, "(a descriptor: {e})"),
        }
    }
}

/// One access-list entry, as [`Descriptor::entries`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its type (`ACE_HEADER.AceType`): [`ACCESS_ALLOWED`], [`ACCESS_DENIED`], or another.
    pub kind: u8,
    /// Its flags (`ACE_HEADER.AceFlags`), [`INHERITED`] among them.
    pub flags: u8,
    /// The rights it names, for an allow or a deny entry (0 for another type).
    pub mask: u32,
    /// The account it names, for an allow or a deny entry (`None` for another type, whose
    /// layout this module does not read).
    pub sid: Option<Sid>,
}

impl Entry {
    /// The entry at `ace`.
    ///
    /// # Safety
    /// `ace` points at a whole access-list entry (its `AceSize` bytes), live for this call.
    unsafe fn read(ace: *const u8) -> io::Result<Self> {
        // SAFETY: an entry starts with its four-byte header.
        let header = unsafe { std::slice::from_raw_parts(ace, 4) };
        let (kind, flags) = (header[0], header[1]);
        let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if !matches!(kind, ACCESS_ALLOWED | ACCESS_DENIED) {
            return Ok(Self {
                kind,
                flags,
                mask: 0,
                sid: None,
            });
        }
        // An allow or deny entry: the header, a four-byte mask, then the SID, all within `size`.
        if size < 8 + 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an access-list entry too short for its SID",
            ));
        }
        // SAFETY: the entry holds `size` bytes, at least 16 (checked), the mask at 4..8.
        let mask = unsafe { std::ptr::read_unaligned(ace.add(4).cast::<u32>()) };
        // SAFETY: the SID starts at offset 8 within the entry, whose `size - 8` remaining bytes
        // bound what is read of it.
        let sid = unsafe { Sid::copied_within(ace.add(8).cast(), size - 8) }?;
        Ok(Self {
            kind,
            flags,
            mask,
            sid: Some(sid),
        })
    }
}

/// Why an object is not its owner's alone ([`owner_only`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotOwnerOnly {
    /// Another account owns it (`None`: its descriptor names no owner).
    Owner(Option<Sid>),
    /// It has no access list at all, which lets everyone do everything.
    NoDacl,
    /// Its access list is not protected: it takes entries from its folder.
    Unprotected,
    /// An entry of its access list was inherited from its folder.
    Inherited,
    /// An entry allows this account something.
    Grants(Sid),
    /// An entry of a type this rule does not read (an object, callback or compound entry), which
    /// may allow someone something.
    Unread(u8),
}

impl NotOwnerOnly {
    /// What the clause says of the object, with no path or account.
    #[must_use]
    pub const fn why(&self) -> &'static str {
        match self {
            Self::Owner(_) => "it is another user's",
            Self::NoDacl => "it has no access list, which lets everyone in",
            Self::Unprotected => "its access list takes entries from its folder",
            Self::Inherited => "its access list holds an entry inherited from its folder",
            Self::Grants(_) => "its access list lets another account in",
            Self::Unread(_) => "its access list holds an entry of a kind this rule does not read",
        }
    }
}

impl fmt::Display for NotOwnerOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner(Some(owner)) => write!(f, "it is owned by {owner}, not this user"),
            Self::Grants(sid) => write!(f, "its access list lets {sid} in"),
            Self::Unread(kind) => write!(f, "{} (type {kind})", self.why()),
            _ => f.write_str(self.why()),
        }
    }
}

/// The owner-only rule over the owner and access list of `descriptor`: owned by `owner`, a
/// protected access list (present: a NULL list lets everyone in), no inherited entry, and every
/// entry that allows anything an allow entry for `owner` or for one of `also`. A deny entry for
/// anyone takes nothing away and passes; an entry of any other type is refused unread. The
/// clauses are judged in that order, and the first one broken is the answer.
///
/// The outer `Err` is a read that failed; `Ok(Err(..))` is the clause broken.
///
/// # Errors
/// The owner, the control bits or an entry could not be read.
pub fn judge(
    descriptor: &Descriptor,
    owner: &Sid,
    also: &[WellKnown],
) -> io::Result<Result<(), NotOwnerOnly>> {
    match descriptor.owner()? {
        Some(found) if found == *owner => {}
        found => return Ok(Err(NotOwnerOnly::Owner(found))),
    }
    let Some(entries) = descriptor.entries()? else {
        return Ok(Err(NotOwnerOnly::NoDacl));
    };
    if !descriptor.dacl_protected()? {
        return Ok(Err(NotOwnerOnly::Unprotected));
    }
    for entry in entries {
        if entry.flags & INHERITED != 0 {
            return Ok(Err(NotOwnerOnly::Inherited));
        }
        match (entry.kind, entry.sid) {
            (ACCESS_DENIED, _) => {}
            (ACCESS_ALLOWED, Some(sid)) => {
                if sid != *owner && !also.iter().any(|which| sid.is(*which)) {
                    return Ok(Err(NotOwnerOnly::Grants(sid)));
                }
            }
            (kind, _) => return Ok(Err(NotOwnerOnly::Unread(kind))),
        }
    }
    Ok(Ok(()))
}

/// [`judge`] of the object `file` is open on, read off the handle (`GetSecurityInfo`), never by
/// path: owned by `owner`, with no allow entry but `owner`'s and SYSTEM's.
///
/// # Errors
/// As [`judge`], and `GetSecurityInfo`'s.
pub fn owner_only(file: impl AsHandle, owner: &Sid) -> io::Result<Result<(), NotOwnerOnly>> {
    let descriptor = Descriptor::of(file, Parts::OWNER | Parts::DACL)?;
    judge(&descriptor, owner, &[WellKnown::LocalSystem])
}

/// `path` as a NUL-terminated wide string.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Win32 `SECURITY_ATTRIBUTES`.
#[repr(C)]
struct SecurityAttributes {
    nLength: u32,
    lpSecurityDescriptor: *mut c_void,
    bInheritHandle: i32,
}

impl SecurityAttributes {
    fn of(descriptor: &Descriptor) -> Self {
        Self {
            nLength: std::mem::size_of::<Self>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr(),
            bInheritHandle: 0,
        }
    }
}

/// Make the folder `path` with `descriptor` from its creation (`CreateDirectoryW` with
/// `SECURITY_ATTRIBUTES`): it is never there without it.
///
/// # Errors
/// `CreateDirectoryW`'s, `AlreadyExists` for a name that exists (a link or a junction included).
pub fn create_dir(path: &Path, descriptor: &Descriptor) -> io::Result<()> {
    let wide = wide(path);
    let attributes = SecurityAttributes::of(descriptor);
    // SAFETY: `wide` is NUL-terminated; `attributes` and the descriptor it names outlive the
    // call.
    if unsafe { ffi::CreateDirectoryW(wide.as_ptr(), (&raw const attributes).cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Make the NEW file `path` with `descriptor` from its creation, open to read and write
/// (`CreateFileW`, `CREATE_NEW` with `SECURITY_ATTRIBUTES`, a reparse point opened itself): it is
/// never there without it, and a name that exists, a link or a junction included, is refused.
/// The handle shares reading, writing and deletion, as std's opens do.
///
/// # Errors
/// `CreateFileW`'s, `AlreadyExists` for a name that exists.
pub fn create_file(path: &Path, descriptor: &Descriptor) -> io::Result<File> {
    let wide = wide(path);
    let attributes = SecurityAttributes::of(descriptor);
    // SAFETY: `wide` is NUL-terminated; `attributes` and the descriptor it names outlive the
    // call; no template.
    let handle = unsafe {
        ffi::CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            SHARE_ALL,
            (&raw const attributes).cast(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle.is_null() || handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is the live file handle CreateFileW just returned, owned by the `File`.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// What [`by_handle`] reads of an open file: its attributes and its link count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleFacts {
    /// `dwFileAttributes`.
    pub attributes: u32,
    /// `nNumberOfLinks`: the names the file has (0 once its last was removed while open).
    pub links: u32,
}

/// The attributes and link count of the file `file` is open on (`GetFileInformationByHandle`).
///
/// # Errors
/// `GetFileInformationByHandle`'s.
pub fn by_handle(file: impl AsHandle) -> io::Result<HandleFacts> {
    let mut facts = [0u32; 13];
    // SAFETY: `file` is a live handle; `facts` is `BY_HANDLE_FILE_INFORMATION`'s thirteen
    // DWORDs.
    let ok = unsafe {
        ffi::GetFileInformationByHandle(file.as_handle().as_raw_handle(), facts.as_mut_ptr())
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(HandleFacts {
        attributes: facts[0],
        links: facts[10],
    })
}

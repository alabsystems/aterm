// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process image basename and kernel code-signature inspection (`proc_pidpath`, `csops`).

/// `CS_VALID`: the signature is valid now.
pub const CS_VALID: u32 = 0x0000_0001;
/// `CS_ADHOC`: signed with no identity.
pub const CS_ADHOC: u32 = 0x0000_0002;
/// `CS_LINKER_SIGNED`: signed ad hoc by the linker.
pub const CS_LINKER_SIGNED: u32 = 0x0002_0000;
/// `CS_PLATFORM_BINARY`: the kernel holds the image as macOS's own.
pub const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
/// `CS_DEBUGGED`: debugged and allowed to run with invalid pages.
pub const CS_DEBUGGED: u32 = 0x1000_0000;
/// `CS_SIGNED`: the process has a signature, valid or not.
pub const CS_SIGNED: u32 = 0x2000_0000;

/// `CS_VALIDATION_CATEGORY_PLATFORM`: macOS's own.
pub const CATEGORY_PLATFORM: u32 = 1;
/// `CS_VALIDATION_CATEGORY_TESTFLIGHT`.
pub const CATEGORY_TESTFLIGHT: u32 = 2;
/// `CS_VALIDATION_CATEGORY_DEVELOPMENT`: a development certificate.
pub const CATEGORY_DEVELOPMENT: u32 = 3;
/// `CS_VALIDATION_CATEGORY_APP_STORE`.
pub const CATEGORY_APP_STORE: u32 = 4;
/// `CS_VALIDATION_CATEGORY_ENTERPRISE`.
pub const CATEGORY_ENTERPRISE: u32 = 5;
/// `CS_VALIDATION_CATEGORY_DEVELOPER_ID`.
pub const CATEGORY_DEVELOPER_ID: u32 = 6;
/// `CS_VALIDATION_CATEGORY_LOCAL_SIGNING`.
pub const CATEGORY_LOCAL_SIGNING: u32 = 7;
/// `CS_VALIDATION_CATEGORY_NONE`: a signature in no category (an ad-hoc one).
pub const CATEGORY_NONE: u32 = 10;

/// C constant names and values checked against the macOS SDK.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("CS_VALID", CS_VALID as i64),
    ("CS_ADHOC", CS_ADHOC as i64),
    ("CS_LINKER_SIGNED", CS_LINKER_SIGNED as i64),
    ("CS_PLATFORM_BINARY", CS_PLATFORM_BINARY as i64),
    ("CS_DEBUGGED", CS_DEBUGGED as i64),
    ("CS_SIGNED", CS_SIGNED as i64),
    ("CS_VALIDATION_CATEGORY_PLATFORM", CATEGORY_PLATFORM as i64),
    (
        "CS_VALIDATION_CATEGORY_TESTFLIGHT",
        CATEGORY_TESTFLIGHT as i64,
    ),
    (
        "CS_VALIDATION_CATEGORY_DEVELOPMENT",
        CATEGORY_DEVELOPMENT as i64,
    ),
    (
        "CS_VALIDATION_CATEGORY_APP_STORE",
        CATEGORY_APP_STORE as i64,
    ),
    (
        "CS_VALIDATION_CATEGORY_ENTERPRISE",
        CATEGORY_ENTERPRISE as i64,
    ),
    (
        "CS_VALIDATION_CATEGORY_DEVELOPER_ID",
        CATEGORY_DEVELOPER_ID as i64,
    ),
    (
        "CS_VALIDATION_CATEGORY_LOCAL_SIGNING",
        CATEGORY_LOCAL_SIGNING as i64,
    ),
    ("CS_VALIDATION_CATEGORY_NONE", CATEGORY_NONE as i64),
];

/// Buffer size for `CS_OPS_IDENTITY` or `CS_OPS_TEAMID` responses.
pub const BLOB_CAP: usize = 256;

/// 8-byte header written by xnu before blob text.
pub const BLOB_HEADER: usize = 8;

/// One process's identity, read now ([`identity`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// Its image's basename (`proc_pidpath`), never `p_comm`.
    pub exe: String,
    /// Its code-signing status flags (`CS_OPS_STATUS`); `None` when refused.
    pub cs_status: Option<u32>,
    /// Validation category (`CS_OPS_VALIDATION_CATEGORY`), or `None` when refused.
    pub category: Option<u32>,
    /// Signing identifier (`CS_OPS_IDENTITY`), or `None` when absent or invalid.
    pub signing_id: Option<String>,
    /// Its team id (`CS_OPS_TEAMID`), as [`team_of`] keeps it.
    pub team: Option<String>,
}

impl Identity {
    /// What its signature vouches for ([`voucher`]).
    #[must_use]
    pub const fn voucher(&self) -> Voucher {
        voucher(self.cs_status, self.category)
    }
}

/// What a process's signature vouches for ([`voucher`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Voucher {
    /// macOS's own: the platform flag and category 1.
    Apple,
    /// The App Store's, or TestFlight's (categories 4 and 2).
    AppStore,
    /// A developer's Developer ID (category 6).
    DeveloperId,
    /// A developer's development certificate (category 3).
    Development,
    /// An enterprise certificate (category 5).
    Enterprise,
    /// Signed with no identity (category 7 or ad-hoc/linker category 10).
    AdHoc,
    /// No signature (`CS_SIGNED` and `CS_VALID` both unset).
    Unsigned,
    /// Anything else, or a refused read.
    Unknown,
}

impl Voucher {
    /// Returns the wire word for this voucher.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Apple => "apple",
            Self::AppStore => "app_store",
            Self::DeveloperId => "developer_id",
            Self::Development => "development",
            Self::Enterprise => "enterprise",
            Self::AdHoc => "ad_hoc",
            Self::Unsigned => "unsigned",
            Self::Unknown => "unknown",
        }
    }
}

/// Classifies code-signing `status` flags and validation `category` into a [`Voucher`].
#[must_use]
pub const fn voucher(status: Option<u32>, category: Option<u32>) -> Voucher {
    let Some(status) = status else {
        return Voucher::Unknown;
    };
    if status & (CS_SIGNED | CS_VALID) == 0 {
        return Voucher::Unsigned;
    }
    let Some(category) = category else {
        return Voucher::Unknown;
    };
    match category {
        CATEGORY_PLATFORM if status & CS_PLATFORM_BINARY != 0 => Voucher::Apple,
        CATEGORY_TESTFLIGHT | CATEGORY_APP_STORE => Voucher::AppStore,
        CATEGORY_DEVELOPMENT => Voucher::Development,
        CATEGORY_ENTERPRISE => Voucher::Enterprise,
        CATEGORY_DEVELOPER_ID => Voucher::DeveloperId,
        CATEGORY_LOCAL_SIGNING => Voucher::AdHoc,
        CATEGORY_NONE if status & (CS_ADHOC | CS_LINKER_SIGNED) != 0 => Voucher::AdHoc,
        _ => Voucher::Unknown,
    }
}

/// Returns the validated team ID for a developer signature category (3–6).
#[must_use]
pub fn team_of(category: Option<u32>, read: impl FnOnce() -> Option<String>) -> Option<String> {
    if !matches!(category, Some(CATEGORY_DEVELOPMENT..=CATEGORY_DEVELOPER_ID)) {
        return None;
    }
    read().filter(|t| is_team_id(t))
}

/// Whether `text` reads as a team id: ten capital letters or digits. PURE.
#[must_use]
pub fn is_team_id(text: &str) -> bool {
    text.len() == 10
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Extracts the NUL-terminated UTF-8 string from a `CS_OPS_IDENTITY` or `CS_OPS_TEAMID` blob.
#[must_use]
pub fn blob_text(blob: &[u8]) -> Option<String> {
    let (header, body) = blob.split_at_checked(BLOB_HEADER)?;
    let end = body.iter().position(|b| *b == 0).filter(|n| *n > 0)?;
    let length = u32::from_be_bytes(header[4..].try_into().ok()?);
    let whole = u32::try_from(BLOB_HEADER + end + 1).ok()?;
    if header[..4] != [0; 4] || length != whole {
        return None;
    }
    String::from_utf8(body[..end].to_vec()).ok()
}

/// Reads `pid`'s executable basename and code-signing identity from the kernel.
#[must_use]
pub fn identity(pid: u32) -> Option<Identity> {
    if pid == 0 {
        return None;
    }
    imp::identity(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{BLOB_CAP, Identity};

    /// `CS_OPS_STATUS` (xnu `bsd/sys/codesign.h`): the status flags, a `u32`.
    const CS_OPS_STATUS: u32 = 0;
    /// `CS_OPS_IDENTITY`: the signing identifier, as a blob.
    const CS_OPS_IDENTITY: u32 = 11;
    /// `CS_OPS_TEAMID`: the team id, as a blob.
    const CS_OPS_TEAMID: u32 = 14;
    /// `CS_OPS_VALIDATION_CATEGORY`: the validation category, a `u32`.
    const CS_OPS_VALIDATION_CATEGORY: u32 = 17;

    unsafe extern "C" {
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`
        /// — xnu `bsd/sys/codesign.h`; no SDK header declares it.
        fn csops(
            pid: libc::pid_t,
            ops: u32,
            useraddr: *mut libc::c_void,
            usersize: libc::size_t,
        ) -> libc::c_int;
    }

    /// One `u32` answer of `ops` for `pid` (above 0); `None` when refused.
    fn word(pid: libc::pid_t, ops: u32) -> Option<u32> {
        let mut out = 0u32;
        // SAFETY: `out` is a live `u32` and the size passed is its own; `ops`
        // is a read operation, which writes at most that many bytes.
        let rc = unsafe { csops(pid, ops, (&raw mut out).cast(), std::mem::size_of::<u32>()) };
        (rc == 0).then_some(out)
    }

    /// The text of one blob answer of `ops` for `pid` (above 0); `None` when
    /// refused, longer than [`BLOB_CAP`] or not decoded.
    fn text(pid: libc::pid_t, ops: u32) -> Option<String> {
        let mut blob = [0u8; BLOB_CAP];
        // SAFETY: `blob` is `BLOB_CAP` writable bytes, the size passed, and
        // the kernel copies no more than that (`ERANGE` past it).
        let rc = unsafe { csops(pid, ops, blob.as_mut_ptr().cast(), blob.len()) };
        if rc != 0 {
            return None;
        }
        super::blob_text(&blob)
    }

    /// `pid` is above 0 ([`super::identity`]).
    pub(super) fn identity(pid: u32) -> Option<Identity> {
        let cs_pid = libc::pid_t::try_from(pid).ok()?;
        let path = crate::process_path(pid)?;
        let exe = path.file_name()?.to_string_lossy().into_owned();
        let category = word(cs_pid, CS_OPS_VALIDATION_CATEGORY);
        let team = super::team_of(category, || text(cs_pid, CS_OPS_TEAMID));
        let read = Identity {
            exe,
            cs_status: word(cs_pid, CS_OPS_STATUS),
            category,
            signing_id: text(cs_pid, CS_OPS_IDENTITY),
            team,
        };
        (crate::process_path(pid)? == path).then_some(read)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Identity;

    pub(super) fn identity(_pid: u32) -> Option<Identity> {
        None
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;

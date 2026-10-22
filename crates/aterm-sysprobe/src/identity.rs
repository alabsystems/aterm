// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO A PROCESS IS, BY THE KERNEL'S WORD (macOS): what the cause engine
//! names a coalition's lead by (`docs/DESIGN-resource-details-2026-10-20.md`
//! §2.3).
//!
//! * [`identity`] — one process's image basename (`proc_pidpath`) and its
//!   code signature as the kernel holds it (`csops`): the status flags, the
//!   validation category, the signing identifier and, for a developer's
//!   signature, the team id. Every read answers for every user's process
//!   unprivileged, opens no file and asks no daemon.
//! * [`voucher`] — what the status and category vouch for, in the closed
//!   words the wire prints ([`Voucher::word`]). PURE.
//! * [`team_of`] — the team id a signature's category lets a reader keep.
//! * [`blob_text`] — the text of a `CS_OPS_IDENTITY` or `CS_OPS_TEAMID`
//!   answer. PURE.
//!
//! A signing identifier is a claim the signer chose: only [`voucher`] says
//! whose it is (a fresh binary signed ad hoc as `com.apple.mds` reads that
//! identifier and category 10, RD §2.3 rule 3).
//!
//! `csops` on pid 0 reads the CALLER, not the kernel (RD §2.3 rule 4): pid 0
//! is never read. Every other platform answers `None`.

// The status flags and validation categories are the SDK's
// (`Kernel.framework/Headers/kern/cs_blobs.h`), held to it by
// `tests/proc_info_sdk.rs` through `SDK_ROWS`; `csops` and its operations
// have no SDK header (`imp`).

/// `CS_VALID`: the signature is valid now.
pub const CS_VALID: u32 = 0x0000_0001;
/// `CS_ADHOC`: signed with no identity.
pub const CS_ADHOC: u32 = 0x0000_0002;
/// `CS_LINKER_SIGNED`: signed ad hoc by the linker. The kernel keeps this
/// bit and not [`CS_ADHOC`] for such an image (status `0x22020201`, category
/// 10, for a `cc`-built probe whose code directory reads
/// `adhoc,linker-signed`: `answers/sign/q1-out-1.txt` under
/// `~/.cache/aterm-design-evidence/details-2026-10-20`).
pub const CS_LINKER_SIGNED: u32 = 0x0002_0000;
/// `CS_PLATFORM_BINARY`: the kernel holds the image as macOS's own.
pub const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
/// `CS_DEBUGGED`: debugged and let run with invalid pages. The kernel answers
/// the category and the signing identifier only with it or [`CS_VALID`].
pub const CS_DEBUGGED: u32 = 0x1000_0000;
/// `CS_SIGNED`: the process has a signature, valid or not now. With neither
/// it nor [`CS_VALID`] the process is unsigned, and the kernel then refuses
/// its category (`EINVAL`).
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

/// The flags and categories above as the C names the SDK must agree with:
/// `tests/proc_info_sdk.rs` asserts each on `arm64` and `x86_64`.
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

/// The bytes a `CS_OPS_IDENTITY` or `CS_OPS_TEAMID` answer is read into: the
/// [`BLOB_HEADER`] and at most 247 bytes of text and its NUL. The kernel
/// refuses a longer one (`ERANGE`).
pub const BLOB_CAP: usize = 256;

/// The header xnu writes before the text: four zero bytes, then the whole
/// answer's length (header, text and NUL), big-endian.
pub const BLOB_HEADER: usize = 8;

/// One process's identity, read now ([`identity`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// Its image's basename (`proc_pidpath`), never `p_comm`.
    pub exe: String,
    /// Its code-signing status flags (`CS_OPS_STATUS`); `None` when refused.
    pub cs_status: Option<u32>,
    /// Its validation category (`CS_OPS_VALIDATION_CATEGORY`: 1 platform, 2
    /// TestFlight, 3 development, 4 App Store, 5 enterprise, 6 Developer ID,
    /// 7 local signing, 10 none); `None` when refused.
    pub category: Option<u32>,
    /// Its signing identifier (`CS_OPS_IDENTITY`); `None` when the kernel
    /// holds none or the signature is not valid.
    pub signing_id: Option<String>,
    /// Its team id (`CS_OPS_TEAMID`), as [`team_of`] keeps it.
    pub team: Option<String>,
}

impl Identity {
    /// What its signature vouches for ([`voucher`]).
    #[must_use]
    pub fn voucher(&self) -> Voucher {
        voucher(self.cs_status, self.category)
    }
}

/// What a process's signature vouches for, by the kernel's status and
/// validation category ([`voucher`]).
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
    /// Signed with no identity: local signing (category 7), or category 10
    /// with an ad-hoc or linker signature.
    AdHoc,
    /// No signature: neither [`CS_SIGNED`] nor [`CS_VALID`] (the kernel then
    /// refuses the category).
    Unsigned,
    /// Anything else, or a refused read.
    Unknown,
}

impl Voucher {
    /// The wire word: `apple`, `app_store`, `developer_id`, `development`,
    /// `enterprise`, `ad_hoc`, `unsigned`, `unknown`.
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

/// What a signature with `status` flags and validation `category` vouches
/// for. Unsigned is the status's alone, since the kernel refuses an unsigned
/// process's category; Apple needs both the platform flag and category 1. A
/// refused status, or a signed process's refused category, is
/// [`Voucher::Unknown`]. PURE.
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

/// The team id of a signature of validation `category`: `read` (the
/// `CS_OPS_TEAMID` answer) is asked only for a developer's signature
/// (categories 3–6), and its text kept only when [`is_team_id`]. PURE but
/// for `read`.
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

/// The text of a `CS_OPS_IDENTITY` or `CS_OPS_TEAMID` answer `blob`: the
/// bytes after the [`BLOB_HEADER`] up to the first NUL, whose header holds
/// zeros and the answer's length. `None` for an empty text, one with no NUL
/// in `blob`, a header that does not hold its length (a layout this decode
/// does not read), or text that is not UTF-8. PURE.
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

/// `pid`'s identity, read now; `None` for pid 0 (`csops` would read the
/// caller), when its image path cannot be read (the process is gone) or is
/// another after the signature's reads (an exec between them, which keeps
/// the pid and the birth: launchd's `xpcproxy` becoming its service), and
/// off macOS. A birth read again after guards only against an exit and the
/// pid's reuse: a caller that keeps an identity reads it again when the
/// process's image changes (its name in the next sweep).
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

    // Declared here rather than in `crates/aterm-libc`, whose cells are
    // generated from the crates.io `libc` and held to it by `libc-oracle`
    // item for item: upstream does not declare `csops`. It is a libSystem
    // export (`libsystem_kernel`) and takes no `$`-suffixed link name on
    // either Darwin arch. Only its read operations are passed here.
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
        // An exec between the reads would mix two images' answers: they
        // stand only when the image read after them is the one read before.
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

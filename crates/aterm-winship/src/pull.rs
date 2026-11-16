// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pull lane's wire: how a release cut on the Mac and `release-serve` on a Windows host
//! meet through the private dev repository (origin), which both reach and neither of them
//! reaches the other through.
//!
//! One definition serves both ends: this crate writes these records on Windows
//! (`crate::serve`), and the release cutter, which links this crate, writes the request and
//! reads the rest on macOS, so the two agree by compilation.
//!
//! * The request, [`REQUEST_REF`]: a commit whose one parent is the reserved release commit C
//!   and whose tree holds [`REQUEST_FILE`] alone ([`Request`]). The cutter rewrites it at each
//!   (re)reservation; its parent makes C reachable on origin.
//! * The handoff, [`handoff_ref`]`(version, build, C)`: a parentless commit whose tree holds
//!   the zip split into parts of at most [`PART_MAX_BYTES`] ([`part_name`]), [`PARTS_FILE`]
//!   ([`Parts`]), the receipt (`crate::handoff::receipt_name`) and its attestation
//!   ([`attestation_name`], `crate::attest`).
//! * The heartbeat, [`ALIVE_REF`]: a parentless commit holding [`ALIVE_FILE`] ([`Alive`]),
//!   pushed at every poll of a builder that is serving: which builder, when, the request
//!   commit it read and, when it will not build that request, why ([`PassedOver`]).
//!
//! On the Mac, a handoff commit is written out by [`extract`], put back together by
//! [`assemble`] into a directory that holds the zip, the receipt and the attestation and
//! nothing else, and admitted by [`crate::handoff::admit`] and
//! [`crate::attest::verify_beside`] against the builders pinned in the release commit
//! ([`BUILDERS_FILE`]).

use std::io::{Read, Seek, SeekFrom, Write as _};
use std::path::Path;

use aterm_update_core::windows::WINDOWS_ZIP_MAX_BYTES;
use serde::{Deserialize, Serialize};

use crate::handoff::{self, TARGET};

/// The request ref: the cut asks for a Windows zip of its reservation.
pub const REQUEST_REF: &str = "refs/aterm-release/windows/request";
/// The one file of the request commit's tree.
pub const REQUEST_FILE: &str = "windows-request.toml";
/// The prefix of every handoff ref ([`handoff_ref`]).
pub const HANDOFF_REF_PREFIX: &str = "refs/aterm-release/windows/handoff/";
/// The heartbeat ref: a builder is serving.
pub const ALIVE_REF: &str = "refs/aterm-release/windows/alive";
/// The one file of the heartbeat commit's tree.
pub const ALIVE_FILE: &str = "alive.toml";
/// The handoff's table of parts.
pub const PARTS_FILE: &str = "parts.toml";
/// The largest part of a split zip: under the 50 MiB above which GitHub warns of a blob.
pub const PART_MAX_BYTES: u64 = 45 * 1024 * 1024;
/// The builders whose attestation a release admits, in the release commit's tree.
pub const BUILDERS_FILE: &str = "apps/aterm-win/release-builders.toml";
/// What an attestation signs ahead of the receipt file's exact bytes.
pub const ATTESTATION_DOMAIN: &[u8] = b"aterm-windows-handoff-attestation/1\n";
/// The parts table's schema.
pub const PARTS_SCHEMA: u32 = 1;
/// The largest request, heartbeat, parts table or attestation read.
pub const RECORD_MAX_BYTES: u64 = handoff::RECEIPT_MAX_BYTES;

/// The attestation's file name beside the zip `asset`.
#[must_use]
pub fn attestation_name(asset: &str) -> String {
    format!("{asset}.handoff.sig")
}

/// Part `index` (from 0) of the zip `asset`.
#[must_use]
pub fn part_name(asset: &str, index: usize) -> String {
    format!("{asset}.part{index:03}")
}

/// The most parts a zip of at most `WINDOWS_ZIP_MAX_BYTES` splits into.
#[must_use]
pub const fn max_parts() -> u64 {
    WINDOWS_ZIP_MAX_BYTES.div_ceil(PART_MAX_BYTES)
}

/// A handoff's key: `<version>-<build>-<first 12 hex digits of C>`.
#[must_use]
pub fn handoff_key(version: &str, build: u64, commit: &str) -> String {
    format!("{version}-{build}-{}", commit.get(..12).unwrap_or(commit))
}

/// The handoff ref of release `version` build `build` at the release commit `commit`.
#[must_use]
pub fn handoff_ref(version: &str, build: u64, commit: &str) -> String {
    format!(
        "{HANDOFF_REF_PREFIX}{}",
        handoff_key(version, build, commit)
    )
}

/// `secs` after the Unix epoch as the UTC date, `YYYY-MM-DD`.
#[must_use]
pub fn date_of(secs: u64) -> String {
    crate::pointer::rfc3339(secs)
        .get(..10)
        .unwrap_or_default()
        .to_string()
}

/// Now, in seconds since the Unix epoch.
#[must_use]
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Today's UTC date, `YYYY-MM-DD`: what a builder's `not_after` is held to.
#[must_use]
pub fn today() -> String {
    date_of(now_secs())
}

fn digits(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Days since the epoch of the civil date `year-month-day`, when it is a real date at or after
/// 1970-01-01.
fn days(year: u64, month: u64, day: u64) -> Option<u64> {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=length).contains(&day) || year < 1970 {
        return None;
    }
    // The inverse of `crate::pointer::rfc3339`'s civil date, eras counted from 0000-03-01.
    let y = if month <= 2 { year - 1 } else { year };
    let (era, yoe) = (y / 400, y % 400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe).checked_sub(719_468)
}

/// The days since the epoch a `YYYY-MM-DD` date names, when it names a real one.
#[must_use]
pub fn date_days(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    days(
        digits(&text[..4])?,
        digits(&text[5..7])?,
        digits(&text[8..])?,
    )
}

/// The Unix second an RFC 3339 UTC time `YYYY-MM-DDTHH:MM:SSZ` names (what
/// `crate::pointer::rfc3339` writes), when it names a real one.
#[must_use]
pub fn rfc3339_secs(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 20 || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let day = date_days(&text[..10])?;
    let (h, m, s) = (
        digits(&text[11..13])?,
        digits(&text[14..16])?,
        digits(&text[17..19])?,
    );
    (h < 24 && m < 60 && s < 60).then_some(day * 86_400 + h * 3_600 + m * 60 + s)
}

fn parse_record<T: for<'de> Deserialize<'de>>(text: &str, what: &str) -> Result<T, String> {
    if text.len() as u64 > RECORD_MAX_BYTES {
        return Err(format!("the {what} is over {RECORD_MAX_BYTES} bytes"));
    }
    aterm_toml::from_str(text).map_err(|e| format!("the {what} does not read: {e}"))
}

fn to_record<T: Serialize>(record: &T, what: &str) -> Result<String, String> {
    aterm_toml::to_string(record).map_err(|e| format!("the {what}: {e}"))
}

/// The request: the cut's reservation, as [`REQUEST_FILE`] in the [`REQUEST_REF`] commit.
///
/// A key it does not know is refused: a request of another shape is another tool's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The release version the cut froze.
    pub version: String,
    /// The build number it reserved.
    pub build: u64,
    /// The release commit C, forty lowercase hex digits: the request commit's one parent.
    pub commit: String,
    /// C's root tree.
    pub tree: String,
    /// When the cut reserved it, RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
    pub reserved_at: String,
}

impl Request {
    /// The request [`REQUEST_FILE`]'s `text` says, checked ([`Request::check`]).
    ///
    /// # Errors
    /// Text that is not a request, or one [`Request::check`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let request: Self = parse_record(text, "Windows build request")?;
        request.check()?;
        Ok(request)
    }

    /// It names a release: a version, a positive build, a full lowercase commit and tree, and
    /// a reservation time.
    ///
    /// # Errors
    /// Which of those it does not.
    pub fn check(&self) -> Result<(), String> {
        if !crate::release::is_version(&self.version) {
            return Err(format!(
                "the Windows build request names {:?}, no release version",
                self.version
            ));
        }
        if self.build == 0 {
            return Err("the Windows build request names build 0".into());
        }
        if !aterm_codec::hex::is_lower(&self.commit, 40) {
            return Err(format!(
                "the Windows build request's commit {:?} is not a full lowercase commit",
                self.commit
            ));
        }
        if !aterm_codec::hex::is_lower(&self.tree, 40)
            && !aterm_codec::hex::is_lower(&self.tree, 64)
        {
            return Err(format!(
                "the Windows build request's tree {:?} is no git object id",
                self.tree
            ));
        }
        if self.reserved_secs().is_none() {
            return Err(format!(
                "the Windows build request's reserved_at {:?} is not an RFC 3339 UTC time \
                 (YYYY-MM-DDTHH:MM:SSZ)",
                self.reserved_at
            ));
        }
        Ok(())
    }

    /// When it was reserved, in seconds since the epoch.
    #[must_use]
    pub fn reserved_secs(&self) -> Option<u64> {
        rfc3339_secs(&self.reserved_at)
    }

    /// Its handoff's key ([`handoff_key`]).
    #[must_use]
    pub fn key(&self) -> String {
        handoff_key(&self.version, self.build, &self.commit)
    }

    /// Its handoff's ref ([`handoff_ref`]).
    #[must_use]
    pub fn handoff_ref(&self) -> String {
        handoff_ref(&self.version, self.build, &self.commit)
    }

    /// The request as [`REQUEST_FILE`]'s TOML.
    ///
    /// # Errors
    /// One the serializer refuses.
    pub fn to_toml(&self) -> Result<String, String> {
        to_record(self, "Windows build request")
    }
}

/// Why a builder will not build the request it read ([`Alive::passed_over`]). A cut that finds
/// its own request passed over waits on that builder no longer. Its words are fixed: the
/// builder's log says the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PassedOver {
    /// The request, or the release commit it names, is not of the shape a release is.
    Refused,
    /// The release commit does not pin this builder with its key, unexpired.
    Unpinned,
    /// It was built `crate::serve::MAX_ATTEMPTS` times without a handoff.
    Spent,
}

impl PassedOver {
    /// The reason, for a person.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Refused => "the request or its release commit is not of a release's shape",
            Self::Unpinned => "the release commit does not pin this builder, unexpired",
            Self::Spent => "its builds failed as many times as a request is built",
        }
    }
}

/// The heartbeat, [`ALIVE_FILE`] in the [`ALIVE_REF`] commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alive {
    /// The builder that is serving, as `crate::attest` names it (`crate::attest::check_id`).
    pub builder: String,
    /// When it last polled, RFC 3339 UTC.
    pub at: String,
    /// The request commit it read from origin at that poll (the commit [`REQUEST_REF`] named);
    /// absent when origin held none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// Why it will not build that request; absent while it builds it, will, or handed it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed_over: Option<PassedOver>,
}

impl Alive {
    /// The heartbeat `text` says: a builder id ([`crate::attest::check_id`]), an RFC 3339 UTC
    /// time and, when it names one, a request commit that is a git object id. Anyone who can
    /// write origin's refs can write a heartbeat, and the cut prints its builder: nothing else
    /// reaches a terminal from it.
    ///
    /// # Errors
    /// Text that is not one, or a field that is not what it says.
    pub fn parse(text: &str) -> Result<Self, String> {
        let alive: Self = parse_record(text, "Windows builder heartbeat")?;
        crate::attest::check_id(&alive.builder)
            .map_err(|e| format!("the Windows builder heartbeat: {e}"))?;
        if alive.at_secs().is_none() {
            return Err(format!(
                "the Windows builder heartbeat's at {:?} is not an RFC 3339 UTC time",
                alive.at
            ));
        }
        if let Some(request) = &alive.request
            && !aterm_codec::hex::is_lower(request, 40)
            && !aterm_codec::hex::is_lower(request, 64)
        {
            return Err(format!(
                "the Windows builder heartbeat's request {request:?} is no git object id"
            ));
        }
        Ok(alive)
    }

    /// Why the builder passed over the request commit `request`: `None` when it did not, or
    /// its heartbeat names another request (or none).
    #[must_use]
    pub fn passed_over(&self, request: &str) -> Option<PassedOver> {
        self.passed_over
            .filter(|_| self.request.as_deref() == Some(request))
    }

    /// Its time, in seconds since the epoch.
    #[must_use]
    pub fn at_secs(&self) -> Option<u64> {
        rfc3339_secs(&self.at)
    }

    /// Whether it was written within `bound` seconds of `now`, either way: a clock that runs
    /// ahead does not make a heartbeat fresh for ever.
    #[must_use]
    pub fn fresh(&self, now: u64, bound: u64) -> bool {
        self.at_secs().is_some_and(|at| now.abs_diff(at) <= bound)
    }

    /// The heartbeat as [`ALIVE_FILE`]'s TOML.
    ///
    /// # Errors
    /// One the serializer refuses.
    pub fn to_toml(&self) -> Result<String, String> {
        to_record(self, "Windows builder heartbeat")
    }
}

/// One part of a split zip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Its name, [`part_name`] of its index.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
}

/// [`PARTS_FILE`]: the zip a handoff carries in parts, and the parts in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parts {
    /// [`PARTS_SCHEMA`].
    pub schema: u32,
    /// The zip's name, the target's asset name.
    pub zip: String,
    /// The zip's size in bytes.
    pub size: u64,
    /// The zip's SHA-256, lowercase hex.
    pub sha256: String,
    /// The parts, in order.
    pub part: Vec<Part>,
}

impl Parts {
    /// The table `text` says, checked ([`Parts::check`]).
    ///
    /// # Errors
    /// Text that is not one, or one [`Parts::check`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Self = parse_record(text, "handoff's parts table")?;
        parts.check()?;
        Ok(parts)
    }

    /// The table is whole: its schema, a zip named as [`TARGET`]'s release zip, a size in 64
    /// bytes to `WINDOWS_ZIP_MAX_BYTES`, a digest, and parts named in order, none empty or
    /// over [`PART_MAX_BYTES`], none but the last short, that add up to the size.
    ///
    /// # Errors
    /// The first of those it is not.
    pub fn check(&self) -> Result<(), String> {
        if self.schema != PARTS_SCHEMA {
            return Err(format!(
                "the handoff's parts table is schema {}, and this reads schema {PARTS_SCHEMA}",
                self.schema
            ));
        }
        let well_named = self
            .zip
            .strip_prefix("aterm-")
            .and_then(|rest| rest.strip_suffix(&format!("-windows-{}.zip", TARGET.arch())))
            .is_some_and(|version| {
                crate::release::is_version(version) && TARGET.asset_name(version) == self.zip
            });
        if !well_named {
            return Err(format!(
                "the handoff's parts table names {:?}, no Windows release zip",
                self.zip
            ));
        }
        if !(64..=WINDOWS_ZIP_MAX_BYTES).contains(&self.size) {
            return Err(format!(
                "the handoff's zip is {} bytes, outside 64..={WINDOWS_ZIP_MAX_BYTES}",
                self.size
            ));
        }
        if !aterm_codec::hex::is_lower(&self.sha256, 64) {
            return Err(format!(
                "the handoff's zip SHA-256 {:?} is not 64 lowercase hex digits",
                self.sha256
            ));
        }
        if self.part.is_empty() || self.part.len() as u64 > max_parts() {
            return Err(format!(
                "the handoff's zip comes in {} parts, not 1..={}",
                self.part.len(),
                max_parts()
            ));
        }
        let mut total = 0_u64;
        let last = self.part.len() - 1;
        for (index, part) in self.part.iter().enumerate() {
            let want = part_name(&self.zip, index);
            if part.name != want {
                return Err(format!(
                    "the handoff's part {index} is named {:?}, not {want:?}",
                    part.name
                ));
            }
            if part.size == 0
                || part.size > PART_MAX_BYTES
                || (index < last && part.size != PART_MAX_BYTES)
            {
                return Err(format!(
                    "the handoff's part {} is {} bytes: every part but the last is \
                     {PART_MAX_BYTES} bytes, and none is empty",
                    part.name, part.size
                ));
            }
            total += part.size;
        }
        if total != self.size {
            return Err(format!(
                "the handoff's parts add up to {total} bytes, not the zip's {}",
                self.size
            ));
        }
        Ok(())
    }

    /// The table as [`PARTS_FILE`]'s TOML.
    ///
    /// # Errors
    /// One the serializer refuses.
    pub fn to_toml(&self) -> Result<String, String> {
        to_record(self, "handoff's parts table")
    }

    /// Every name the handoff's tree holds: the parts in order, then [`PARTS_FILE`], the
    /// receipt and the attestation.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.part.iter().map(|p| p.name.clone()).collect();
        names.push(PARTS_FILE.to_string());
        names.push(handoff::receipt_name(&self.zip));
        names.push(attestation_name(&self.zip));
        names
    }
}

fn create_new(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Splits the zip at `zip` into `into` (an existing directory) as parts of at most
/// `part_max` bytes, [`part_name`]d, and writes [`PARTS_FILE`] beside them. The table it wrote.
/// `part_max` is [`PART_MAX_BYTES`] for a handoff; another size makes a table [`Parts::check`]
/// refuses, which only a test of the split itself wants.
///
/// # Errors
/// A zip that is not a regular file of at most `WINDOWS_ZIP_MAX_BYTES`, or a write that fails
/// (a name already there included).
pub fn split(zip: &Path, into: &Path, part_max: u64) -> Result<Parts, String> {
    let name = zip
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", zip.display()))?
        .to_string();
    if part_max == 0 {
        return Err("a part holds at least one byte".into());
    }
    let (sha256, size, mut file) = handoff::hash_regular(zip, WINDOWS_ZIP_MAX_BYTES)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut parts = Vec::new();
    let mut left = size;
    let mut buf = vec![0_u8; 1 << 20];
    while left > 0 {
        let take = left.min(part_max);
        let part = part_name(&name, parts.len());
        let path = into.join(&part);
        let mut out = create_new(&path)?;
        let mut copied = 0_u64;
        while copied < take {
            let want = usize::try_from((take - copied).min(buf.len() as u64))
                .map_err(|e| e.to_string())?;
            file.read_exact(&mut buf[..want])
                .map_err(|e| format!("{}: {e}", zip.display()))?;
            out.write_all(&buf[..want])
                .map_err(|e| format!("{}: {e}", path.display()))?;
            copied += want as u64;
        }
        parts.push(Part {
            name: part,
            size: take,
        });
        left -= take;
    }
    let table = Parts {
        schema: PARTS_SCHEMA,
        zip: name,
        size,
        sha256,
        part: parts,
    };
    let path = into.join(PARTS_FILE);
    create_new(&path)?
        .write_all(table.to_toml()?.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(table)
}

/// The exact bytes of the regular file at `path`, of at most [`RECORD_MAX_BYTES`].
///
/// # Errors
/// A path that is not such a file, or one that cannot be read.
pub fn read_record(path: &Path) -> Result<Vec<u8>, String> {
    let (_, _, mut file) = handoff::hash_regular(path, RECORD_MAX_BYTES)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

fn read_small(path: &Path) -> Result<String, String> {
    String::from_utf8(read_record(path)?).map_err(|_| format!("{} is not UTF-8", path.display()))
}

/// Puts the handoff written out in `parts_dir` ([`extract`]) back together in `carry`, an
/// existing empty directory: the zip its parts make, held to [`PARTS_FILE`] (every part a
/// regular file of its listed size, the whole of the listed size and SHA-256), and beside it
/// the receipt and the attestation, copied. `parts_dir` holds those names and no other.
/// `carry` then holds exactly what [`crate::handoff::admit`] and
/// [`crate::attest::verify_beside`] read. The table.
///
/// # Errors
/// A table that does not read or check, a name missing or another one there, a part or a
/// whole that is not what the table says, or a write that fails.
pub fn assemble(parts_dir: &Path, carry: &Path) -> Result<Parts, String> {
    let parts = Parts::parse(&read_small(&parts_dir.join(PARTS_FILE))?)?;
    let names = parts.names();
    let entries =
        std::fs::read_dir(parts_dir).map_err(|e| format!("{}: {e}", parts_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", parts_dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !names.contains(&name) {
            return Err(format!(
                "the Windows handoff holds {name:?}, which is no part of it ({})",
                names.join(", ")
            ));
        }
    }
    let zip = carry.join(&parts.zip);
    let mut out = create_new(&zip)?;
    let mut digest = aterm_digest::Sha256::new();
    let mut total = 0_u64;
    let mut buf = vec![0_u8; 1 << 20];
    for part in &parts.part {
        let path = parts_dir.join(&part.name);
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !meta.file_type().is_file() || meta.len() != part.size {
            return Err(format!(
                "{} is not a regular file of the table's {} bytes",
                path.display(),
                part.size
            ));
        }
        let mut file =
            std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut read = 0_u64;
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > part.size {
                return Err(format!("{} grew as it was read", path.display()));
            }
            digest.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|e| format!("{}: {e}", zip.display()))?;
        }
        if read != part.size {
            return Err(format!(
                "{} is {read} bytes as read, not the table's {}",
                path.display(),
                part.size
            ));
        }
        total += read;
    }
    drop(out);
    let sha256 = aterm_codec::hex::encode_lower(&digest.finalize());
    if total != parts.size || sha256 != parts.sha256 {
        return Err(format!(
            "the Windows handoff's parts make {total} bytes of SHA-256 {sha256}, not the \
             table's {} bytes of {}",
            parts.size, parts.sha256
        ));
    }
    for name in [
        handoff::receipt_name(&parts.zip),
        attestation_name(&parts.zip),
    ] {
        let bytes = read_record(&parts_dir.join(&name))?;
        create_new(&carry.join(&name))?
            .write_all(&bytes)
            .map_err(|e| format!("{}: {e}", carry.join(&name).display()))?;
    }
    Ok(parts)
}

/// Runs git with these arguments and returns its standard output's bytes, or why it failed.
pub type GitBytes<'a> = dyn FnMut(&[&str]) -> Result<Vec<u8>, String> + 'a;

/// Whether `name` may be a file of a handoff's tree: a flat name of the handoff's own shape.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Writes out the handoff commit `commit` into `into`, an existing empty directory, through
/// `git` (in a repository that holds the commit): the commit must be parentless and its tree
/// flat, of regular non-executable files with plain names, at most [`max_parts`] plus three,
/// each of at most [`PART_MAX_BYTES`] (the size git lists is checked before a blob is read and
/// again after). Each blob's exact bytes are written, filters never applied. The names written.
///
/// # Errors
/// A commit of another shape, a git failure, or a write that fails.
pub fn extract(git: &mut GitBytes<'_>, commit: &str, into: &Path) -> Result<Vec<String>, String> {
    let text =
        |bytes: Vec<u8>| String::from_utf8(bytes).map_err(|_| "git wrote no UTF-8".to_string());
    let parents = text(git(&["rev-list", "--parents", "-n", "1", commit])?)?;
    if parents.split_whitespace().count() != 1 {
        return Err(format!(
            "the Windows handoff commit {commit} has parents: a handoff commit is parentless"
        ));
    }
    let listing = text(git(&["ls-tree", "-l", "-z", "--full-tree", commit])?)?;
    let mut entries = Vec::new();
    for line in listing.split('\0').filter(|line| !line.is_empty()) {
        let (head, name) = line
            .split_once('\t')
            .ok_or_else(|| format!("git ls-tree wrote {line:?}"))?;
        let fields: Vec<&str> = head.split_whitespace().collect();
        let [mode, kind, oid, size] = fields[..] else {
            return Err(format!("git ls-tree wrote {line:?}"));
        };
        if mode != "100644" || kind != "blob" || !plain_name(name) {
            return Err(format!(
                "the Windows handoff holds {name:?} ({mode} {kind}): a handoff is regular \
                 files with plain names"
            ));
        }
        let size: u64 = size
            .parse()
            .map_err(|_| format!("git ls-tree gave {name:?} the size {size:?}"))?;
        if size > PART_MAX_BYTES {
            return Err(format!(
                "the Windows handoff's {name} is {size} bytes, over {PART_MAX_BYTES}"
            ));
        }
        entries.push((name.to_string(), oid.to_string(), size));
    }
    if entries.len() as u64 > max_parts() + 3 {
        return Err(format!(
            "the Windows handoff holds {} files, more than a handoff can",
            entries.len()
        ));
    }
    let mut names = Vec::new();
    for (name, oid, size) in entries {
        let bytes = git(&["cat-file", "blob", &oid])?;
        if bytes.len() as u64 != size {
            return Err(format!(
                "git gave {} bytes for the Windows handoff's {name}, which it listed at {size}",
                bytes.len()
            ));
        }
        let path = into.join(&name);
        create_new(&path)?
            .write_all(&bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        names.push(name);
    }
    Ok(names)
}

#[cfg(test)]
#[path = "pull_tests.rs"]
mod tests;

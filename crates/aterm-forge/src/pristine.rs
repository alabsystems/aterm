// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `[OB-7]`'s pristine copies. A fork's upstream is the `.crate` crates.io published for it:
//! attest fetches it over aterm-http, holds it to the sha256 the crates.io index records for
//! that version (the check cargo makes of each download), and unpacks it into a cache outside
//! the checkout. Cargo never fetches it: a patched-out package has a source-less lock entry.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Hang detector for the required registry connection within its original request.
const CONNECT_HANG: Duration = Duration::from_secs(60);

use aterm_codec::inflate::stream::GzipReader;

/// The crates.io sparse index.
const INDEX: &str = "https://index.crates.io";
/// Where crates.io serves each published `.crate` (its index's `dl`).
const DOWNLOAD: &str = "https://static.crates.io/crates";
/// The largest index file read (winit's is 0.8 MB).
const INDEX_LIMIT: usize = 16 << 20;
/// The largest `.crate` read: crates.io refuses an upload over 10 MB unless it raised that crate's limit.
const CRATE_LIMIT: usize = 64 << 20;
/// The most file bytes one unpack writes.
const UNPACKED_LIMIT: u64 = 1 << 30;
/// The most members one unpack takes.
const MEMBER_LIMIT: usize = 100_000;
/// The largest GNU long-name or PAX body read.
const EXTENSION_LIMIT: u64 = 1 << 20;
/// One tar block.
const BLOCK: usize = 512;
/// A cache entry's unpacked package root.
const PACKAGE: &str = "package";
/// A cache entry's record of the sha256 its `.crate` was held to, written before the entry is
/// renamed into place, so an entry is whole or absent.
const STAMP: &str = "sha256";

/// How crates.io is read: [`CratesIo`], or a test's own bytes.
pub(crate) trait Registry {
    /// The sparse index's file for `name`: one JSON object per published version.
    ///
    /// # Errors
    /// It could not be read: why, naming the URL.
    fn index(&self, name: &str) -> Result<Vec<u8>, String>;

    /// The `.crate` crates.io published for `name` `version`.
    ///
    /// # Errors
    /// It could not be read: why, naming the URL.
    fn archive(&self, name: &str, version: &str) -> Result<Vec<u8>, String>;
}

/// crates.io over aterm-http, through the environment's proxy.
pub(crate) struct CratesIo {
    client: aterm_http::Client,
}

impl CratesIo {
    pub(crate) fn new() -> Self {
        Self {
            client: aterm_http::Client::new(
                aterm_http::Trust::PlatformVerifier,
                aterm_http::ProxyMode::Environment,
                Duration::from_secs(300),
            ),
        }
    }

    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        self.get_started(url, limit, Instant::now())
    }

    fn get_started(&self, url: &str, limit: usize, started: Instant) -> Result<Vec<u8>, String> {
        let connect = (started + CONNECT_HANG).saturating_duration_since(Instant::now());
        if connect.is_zero() {
            return Err(format!("GET {url} connect deadline expired"));
        }
        let response = self
            .client
            .get(url)
            .header("User-Agent", "aterm-forge")
            .limit(limit)
            .connect_within(connect)
            .idle(Duration::from_secs(60))
            .send(b"")
            .map_err(|e| format!("GET {url} failed: {e}"))?;
        if !response.is_success() {
            return Err(format!("GET {url} answered HTTP {}", response.status()));
        }
        Ok(response.body().to_vec())
    }
}

#[cfg(test)]
#[path = "pristine/native_connect_tests.rs"]
mod native_connect_tests;

impl Registry for CratesIo {
    fn index(&self, name: &str) -> Result<Vec<u8>, String> {
        self.get(&index_url(name), INDEX_LIMIT)
    }

    fn archive(&self, name: &str, version: &str) -> Result<Vec<u8>, String> {
        self.get(&crate_url(name, version), CRATE_LIMIT)
    }
}

/// The sparse index's URL for `name` (cargo's sharding: `1/a`, `2/ab`, `3/a/abc`, `ab/cd/abcd…`).
fn index_url(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let shard = match lower.len() {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => format!("3/{}", &lower[..1]),
        _ => format!("{}/{}", &lower[..2], &lower[2..4]),
    };
    format!("{INDEX}/{shard}/{lower}")
}

/// Where crates.io serves `name` `version`'s `.crate`.
fn crate_url(name: &str, version: &str) -> String {
    format!("{DOWNLOAD}/{name}/{name}-{version}.crate")
}

/// Where `[OB-7]` takes each pristine copy from: a cache of copies already held to the index's
/// sha256, filled from a [`Registry`].
pub(crate) struct Pristine {
    /// `None` where this host names no user cache folder.
    cache: Option<PathBuf>,
    registry: Box<dyn Registry>,
}

/// A pristine copy to diff against.
pub(crate) struct Held {
    /// The unpacked package root (its `Cargo.toml` at the top).
    pub(crate) dir: PathBuf,
    /// `<name>-<version>`.
    id: String,
    /// The sha256 of the `.crate` it was unpacked from, which the crates.io index records.
    sha256: String,
    /// This run fetched it, rather than finding it in the cache.
    fetched: bool,
}

impl Held {
    /// Where this copy came from, for the transcript.
    pub(crate) fn provenance(&self) -> String {
        let how = if self.fetched {
            "fetched from crates.io by this run"
        } else {
            "cached by an earlier run"
        };
        format!(
            "`{}.crate`, {how}, held to the sha256 the crates.io index records ({}), unpacked \
             at {}",
            self.id,
            self.sha256,
            self.dir.display()
        )
    }
}

impl Pristine {
    /// crates.io, cached in this user's `forge` cache (`~/Library/Caches/aterm/forge.noindex`
    /// on macOS, `~/.cache/aterm/forge` elsewhere).
    pub(crate) fn host() -> Self {
        Self::at(
            aterm_uds::host::host_user_cache("forge"),
            Box::new(CratesIo::new()),
        )
    }

    /// `registry`, cached under `cache`.
    pub(crate) fn at(cache: Option<PathBuf>, registry: Box<dyn Registry>) -> Self {
        Self { cache, registry }
    }

    /// The pristine copy of `name` `version`: the cached one, else the published `.crate`,
    /// fetched, held to the index's sha256 and unpacked into the cache.
    ///
    /// # Errors
    /// Why there is none: no cache folder, a name or version no `.crate` file can carry, the
    /// index or the download could not be read, the index records no sha256 for it, the
    /// download hashes to another (refused, nothing unpacked), or it did not unpack.
    pub(crate) fn copy(&self, name: &str, version: &str) -> Result<Held, String> {
        let cache = self.cache.as_deref().ok_or(
            "this host names no user cache folder (HOME is unset), so there is nowhere to \
             unpack one",
        )?;
        if std::fs::symlink_metadata(cache).is_ok_and(|m| !m.file_type().is_dir()) {
            return Err(format!("{} is not a cache directory", cache.display()));
        }
        let crates_dir = cache.join("crates");
        if std::fs::symlink_metadata(&crates_dir).is_ok_and(|m| !m.file_type().is_dir()) {
            return Err(format!("{} is not a cache directory", crates_dir.display()));
        }
        let id = package_id(name, version)?;
        let entry = crates_dir.join(&id);
        if let Some(copy) = cached(&entry, &id) {
            return Ok(copy);
        }
        if let Ok(meta) = std::fs::symlink_metadata(&entry) {
            let removed = if meta.file_type().is_dir() {
                std::fs::remove_dir_all(&entry)
            } else {
                std::fs::remove_file(&entry)
            };
            removed.map_err(|e| {
                format!(
                    "{} is an incomplete cache entry and could not be removed: {e}",
                    entry.display()
                )
            })?;
        }
        let want = recorded_sha256(&self.registry.index(name)?, name, version)?;
        let archive = self.registry.archive(name, version)?;
        let got = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&archive));
        if got != want {
            return Err(format!(
                "`{id}.crate` from crates.io hashes to {got}, but the crates.io index records \
                 {want} for it: the download is refused and nothing was unpacked"
            ));
        }
        place(&entry, &id, &archive, &want)?;
        let copy = cached(&entry, &id).ok_or_else(|| {
            format!(
                "{} holds no whole copy after unpacking into it",
                entry.display()
            )
        })?;
        Ok(Held {
            fetched: true,
            ..copy
        })
    }
}

/// `<name>-<version>`, the folder a `.crate` holds, once both are shown to be what crates.io
/// publishes: a name of ASCII letters, digits, `-` and `_`, and a version of letters, digits,
/// `.`, `-` and `+`.
fn package_id(name: &str, version: &str) -> Result<String, String> {
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let version_ok = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b));
    if name_ok && version_ok {
        Ok(format!("{name}-{version}"))
    } else {
        Err(format!(
            "`{name} {version}` is not a crates.io package and version (read from its manifest)"
        ))
    }
}

/// The whole cache entry at `entry`: its package root and the sha256 it was held to.
fn cached(entry: &Path, id: &str) -> Option<Held> {
    const MAX_STAMP_BYTES: u64 = 128;
    if !std::fs::symlink_metadata(entry).ok()?.file_type().is_dir() {
        return None;
    }
    let dir = entry.join(PACKAGE);
    if !std::fs::symlink_metadata(&dir).ok()?.file_type().is_dir() {
        return None;
    }
    let stamp =
        aterm_uds::atomic_file::read_regular_text(&entry.join(STAMP), MAX_STAMP_BYTES).ok()?;
    let sha256 = stamp.trim();
    aterm_codec::hex::is_lower(sha256, 64).then(|| Held {
        dir,
        id: id.to_string(),
        sha256: sha256.to_string(),
        fetched: false,
    })
}

/// The sha256 the crates.io index records for `name` `version` (its row's `cksum`).
fn recorded_sha256(index: &[u8], name: &str, version: &str) -> Result<String, String> {
    for line in index.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let row: aterm_json::Value = aterm_json::from_slice(line).map_err(|e| {
            format!("the crates.io index for `{name}` holds a line that is not JSON: {e}")
        })?;
        if row.get("vers").and_then(aterm_json::Value::as_str) != Some(version) {
            continue;
        }
        let cksum = row
            .get("cksum")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or_default();
        return if aterm_codec::hex::is_lower(cksum, 64) {
            Ok(cksum.to_string())
        } else {
            Err(format!(
                "the crates.io index's row for `{name} {version}` records no sha256 (its \
                 `cksum` is `{cksum}`)"
            ))
        };
    }
    Err(format!(
        "the crates.io index records no published `{name} {version}`"
    ))
}

/// Unpack `archive` into a fresh folder beside `entry`, stamp it with `sha256`, and rename it
/// into place. A run that loses the rename to another keeps the other's entry.
fn place(entry: &Path, id: &str, archive: &[u8], sha256: &str) -> Result<(), String> {
    let parent = entry
        .parent()
        .ok_or_else(|| format!("{} has no parent", entry.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    if !std::fs::symlink_metadata(parent)
        .map_err(|e| format!("could not inspect {}: {e}", parent.display()))?
        .file_type()
        .is_dir()
    {
        return Err(format!("{} is not a cache directory", parent.display()));
    }
    let staging = aterm_tempfile::Builder::new()
        .prefix(&format!(".unpack-{id}-"))
        .tempdir_in(parent)
        .map_err(|e| format!("could not create a folder under {}: {e}", parent.display()))?;
    unpack(archive, id, &staging.path().join(PACKAGE))
        .map_err(|e| format!("`{id}.crate` did not unpack: {e}"))?;
    let stamp = staging.path().join(STAMP);
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        opts.open(&stamp)
            .and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(format!("{sha256}\n").as_bytes())
            })
            .map_err(|e| format!("could not write {}: {e}", stamp.display()))?;
    }
    match std::fs::rename(staging.path(), entry) {
        Ok(()) => {
            let _ = staging.keep();
            Ok(())
        }
        Err(_) if cached(entry, id).is_some() => Ok(()),
        Err(e) => Err(format!(
            "could not rename {} to {}: {e}",
            staging.path().display(),
            entry.display()
        )),
    }
}

/// A PAX extended header's records this reader honours, for the member after it.
#[derive(Default)]
struct Pax {
    path: Option<String>,
    size: Option<u64>,
}

/// Unpack the gzip'd tar `archive` (a `.crate`) into the new folder `into`, taking only what a
/// published crate holds: regular files and folders under `top/`. A member outside `top/`, a
/// `..`, a link or device, a second member at one path, a damaged header, or more than the
/// limits allow refuses the whole unpack. No link is ever laid, so no member can be written
/// through one. Returns the number of members.
fn unpack(archive: &[u8], top: &str, into: &Path) -> Result<usize, String> {
    std::fs::create_dir(into).map_err(|e| format!("could not create {}: {e}", into.display()))?;
    let mut tar = GzipReader::new(archive);
    let mut members = 0usize;
    let mut extensions = 0usize;
    let mut written = 0u64;
    let mut long_name: Option<String> = None;
    let mut pax = Pax::default();
    let mut pending_ext = false;
    let mut header = [0u8; BLOCK];
    while read_block(&mut tar, &mut header)? {
        if header.iter().all(|b| *b == 0) {
            break;
        }
        check_sum(&header)?;
        let kind = header[156];
        let field_size = octal(&header[124..136])?;
        if matches!(kind, b'L' | b'x' | b'g') {
            extensions += 1;
            if extensions > MEMBER_LIMIT {
                return Err(format!("more than {MEMBER_LIMIT} extension headers"));
            }
            if field_size > EXTENSION_LIMIT {
                return Err(format!(
                    "an extension header of {field_size} bytes, over the {EXTENSION_LIMIT} read"
                ));
            }
            let mut body = Vec::new();
            copy_exact(&mut tar, field_size, &mut body)?;
            skip(&mut tar, padding(field_size))?;
            match kind {
                b'L' => {
                    if long_name.is_some() || pax.path.is_some() {
                        return Err(
                            "duplicate path override in member extension headers".to_string()
                        );
                    }
                    let name = body.split(|b| *b == 0).next().unwrap_or_default();
                    long_name = Some(utf8(name)?);
                    pending_ext = true;
                }
                b'x' => {
                    parse_pax_into(&body, &mut pax, long_name.is_some())?;
                    pending_ext = true;
                }
                b'g' => {
                    if pending_ext {
                        return Err(
                            "a global PAX header appears inside a member's extension headers"
                                .to_string(),
                        );
                    }
                    let mut global = Pax::default();
                    parse_pax_into(&body, &mut global, false)?;
                    if global.path.is_some() || global.size.is_some() {
                        return Err("a global PAX header overrides member path or size".to_string());
                    }
                }
                _ => unreachable!(),
            }
            continue;
        }
        pending_ext = false;
        let size = pax.size.take().unwrap_or(field_size);
        let path = match long_name.take().or_else(|| pax.path.take()) {
            Some(path) => path,
            None => header_path(&header)?,
        };
        pax = Pax::default();
        members += 1;
        if members > MEMBER_LIMIT {
            return Err(format!("more than {MEMBER_LIMIT} members"));
        }
        let rel = member(&path, top)?;
        match (kind, rel) {
            (b'5', rel) => {
                if let Some(rel) = rel {
                    let dir = into.join(rel);
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| format!("`{path}` could not be made a folder: {e}"))?;
                }
                skip(&mut tar, size.saturating_add(padding(size)))?;
            }
            (b'0' | 0 | b'7', Some(rel)) => {
                written = written.saturating_add(size);
                if written > UNPACKED_LIMIT {
                    return Err(format!("more than {UNPACKED_LIMIT} bytes of files"));
                }
                let dest = into.join(rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("`{path}`'s folder could not be made: {e}"))?;
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
                }
                let mut file = opts.open(&dest).map_err(|e| {
                    format!(
                        "`{path}` could not be written ({e}): a second member at one \
                         path is refused"
                    )
                })?;
                copy_exact(&mut tar, size, &mut file)?;
                skip(&mut tar, padding(size))?;
            }
            (b'0' | 0 | b'7', None) => {
                return Err(format!("`{path}` is a file where `{top}/` should be"));
            }
            (other, _) => {
                return Err(format!(
                    "`{path}` is a member of type `{}`, which a published crate does not hold",
                    char::from(other).escape_default()
                ));
            }
        }
    }
    if pending_ext {
        return Err("an extension header has no member after it".to_string());
    }
    drain_zero_trailer(&mut tar)?;
    Ok(members)
}

/// Read to the end of `tar`, checking its gzip CRC-32 and length while refusing non-zero
/// trailing bytes after the end-of-archive zero block.
fn drain_zero_trailer(tar: &mut impl Read) -> Result<(), String> {
    const TRAILER_LIMIT: u64 = 1 << 20;
    let mut seen = 0u64;
    let mut buf = [0u8; 4096];
    loop {
        match tar.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if buf[..n].iter().any(|b| *b != 0) {
                    return Err(
                        "non-zero bytes trail the archive's end-of-archive zero block".to_string(),
                    );
                }
                seen = seen.saturating_add(n as u64);
                if seen > TRAILER_LIMIT {
                    return Err(format!(
                        "more than {TRAILER_LIMIT} trailing zero bytes after the archive"
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("the gzip stream: {e}")),
        }
    }
}

/// Fill `block`, or `false` at a clean end of the stream.
fn read_block(r: &mut impl Read, block: &mut [u8; BLOCK]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < BLOCK {
        match r.read(&mut block[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err("the archive ends inside a header".to_string()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("the gzip stream: {e}")),
        }
    }
    Ok(true)
}

/// Copy exactly `len` bytes of `r` into `out`.
fn copy_exact(r: &mut impl Read, len: u64, out: &mut impl io::Write) -> Result<(), String> {
    let copied = io::copy(&mut r.take(len), out).map_err(|e| format!("a member's body: {e}"))?;
    if copied == len {
        Ok(())
    } else {
        Err("the archive ends inside a member".to_string())
    }
}

fn skip(r: &mut impl Read, len: u64) -> Result<(), String> {
    copy_exact(r, len, &mut io::sink())
}

/// The zero bytes after a body of `size` that end its last block.
const fn padding(size: u64) -> u64 {
    (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64
}

/// The header's checksum is the unsigned sum of its bytes with the checksum field as spaces.
fn check_sum(header: &[u8; BLOCK]) -> Result<(), String> {
    let recorded = octal(&header[148..156])?;
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, b)| u64::from(if (148..156).contains(&i) { b' ' } else { *b }))
        .sum();
    if sum == recorded {
        Ok(())
    } else {
        Err("a member header's checksum does not match its bytes".to_string())
    }
}

/// An octal header field: leading spaces, digits, then only NULs and spaces.
fn octal(field: &[u8]) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err("a base-256 number field (a member of 8 GiB or more)".to_string());
    }
    let mut n = 0u64;
    let mut rest = field.iter().skip_while(|b| **b == b' ').peekable();
    while let Some(d) = rest.next_if(|b| (b'0'..=b'7').contains(*b)) {
        n = n
            .checked_mul(8)
            .and_then(|n| n.checked_add(u64::from(d - b'0')))
            .ok_or("a number field overflows")?;
    }
    if rest.all(|b| *b == 0 || *b == b' ') {
        Ok(n)
    } else {
        Err(format!(
            "a number field is not octal: `{}`",
            field.escape_ascii()
        ))
    }
}

/// The member path a ustar header names: its `prefix/name` under POSIX ustar, else `name`.
fn header_path(header: &[u8; BLOCK]) -> Result<String, String> {
    let field = |range: std::ops::Range<usize>| {
        let raw = &header[range];
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        utf8(&raw[..end])
    };
    let name = field(0..100)?;
    if &header[257..263] == b"ustar\0" {
        let prefix = field(345..500)?;
        if !prefix.is_empty() {
            return Ok(format!("{prefix}/{name}"));
        }
    }
    Ok(name)
}

fn utf8(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| format!("a member path is not UTF-8: `{}`", bytes.escape_ascii()))
}

/// Merge the `path` and `size` records of a PAX body (`<len> <key>=<value>\n` each) into `pax`,
/// refusing duplicate `path` or `size` overrides.
fn parse_pax_into(body: &[u8], pax: &mut Pax, has_long_name: bool) -> Result<(), String> {
    let bad = || "a PAX record is malformed".to_string();
    let mut rest = body;
    while !rest.is_empty() {
        let space = rest.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
            .ok_or_else(bad)?;
        if len <= space + 1 || len > rest.len() {
            return Err(bad());
        }
        let record = rest[space + 1..len].strip_suffix(b"\n").ok_or_else(bad)?;
        let eq = record.iter().position(|b| *b == b'=').ok_or_else(bad)?;
        let (key, value) = (&record[..eq], &record[eq + 1..]);
        match key {
            b"path" => {
                if has_long_name || pax.path.is_some() {
                    return Err("duplicate path override in member extension headers".to_string());
                }
                pax.path = Some(utf8(value)?);
            }
            b"size" => {
                if pax.size.is_some() {
                    return Err("duplicate size override in member PAX headers".to_string());
                }
                pax.size = Some(
                    std::str::from_utf8(value)
                        .ok()
                        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(bad)?,
                );
            }
            b"linkpath" => {
                return Err(
                    "a PAX linkpath record, which a published crate does not hold".to_string(),
                );
            }
            k if k.starts_with(b"GNU.sparse.") => {
                return Err(
                    "a PAX sparse record, which a published crate does not hold".to_string()
                );
            }
            _ => {}
        }
        rest = &rest[len..];
    }
    Ok(())
}

/// `path` below `top/`, or `None` for `top/` itself. Refuses an absolute path, one outside
/// `top/`, and a `..`, `\`, `:` or NUL in any part.
fn member(path: &str, top: &str) -> Result<Option<PathBuf>, String> {
    let outside =
        || format!("`{path}` is outside `{top}/`, the one folder a published crate holds: refused");
    if path.starts_with('/') {
        return Err(outside());
    }
    let mut parts = path.split('/').filter(|p| !p.is_empty() && *p != ".");
    if parts.next() != Some(top) {
        return Err(outside());
    }
    let mut rel = PathBuf::new();
    for part in parts {
        if part == ".." || part.contains(['\\', ':', '\0']) {
            return Err(outside());
        }
        rel.push(part);
    }
    Ok((!rel.as_os_str().is_empty()).then_some(rel))
}

#[cfg(test)]
#[path = "pristine_tests.rs"]
pub(crate) mod tests;

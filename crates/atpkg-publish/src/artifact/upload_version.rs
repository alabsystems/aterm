// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable, create-only assignment of one staged build to an exact version.

use super::*;
use std::io::Read as _;

/// Independent metadata, never a ready package manifest or a replaceable payload.
pub const VERSION_RESERVATION: &str = "atpkg-stage-version.v1";
const MAGIC: &[u8] = b"atpkg staged version 1\n";
const VERSION_BYTES: usize = 4096;
const RECORD_BYTES: usize = MAGIC.len() + 4 + VERSION_BYTES;
const MANIFEST_BYTES: usize = 16 * 1024 * 1024;

fn bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(maximum as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| Error::io(format!("read {}", path.display()), &error))?;
    if bytes.len() > maximum {
        return Err(Error::refused(format!(
            "{} exceeds its {maximum}-byte bound",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(super) fn read_manifest(path: &Path) -> Result<Vec<u8>> {
    bounded(path, MANIFEST_BYTES)
}

fn version_of(record: &[u8]) -> Option<&str> {
    let body = record.strip_prefix(MAGIC)?;
    let length = u32::from_le_bytes(body.get(..4)?.try_into().ok()?) as usize;
    let version = body.get(4..)?;
    (length > 0 && length <= VERSION_BYTES && version.len() == length)
        .then(|| std::str::from_utf8(version).ok())?
}

pub(super) fn check_reservation(path: &Path, version: &str) -> Result<()> {
    let record = bounded(path, RECORD_BYTES)?;
    if version_of(&record) != Some(version) {
        return Err(Error::refused(
            "the version reservation is malformed or differs from the signed manifest",
        ));
    }
    Ok(())
}

pub(super) fn check_managed_manifest(path: &Path, program: &str, build: u64) -> Result<()> {
    let expected = crate::artifact::pkgdoc::manifest_name(program, build);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
        return Err(Error::refused(
            "the supplied manifest name differs from the managed release tag",
        ));
    }
    let bytes = read_manifest(path)?;
    let document: aterm_toml::Value = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| Error::refused("the supplied managed pack manifest is not readable TOML"))?;
    if document.get("program").and_then(aterm_toml::Value::as_str) != Some(program)
        || document
            .get("build_number")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|number| u64::try_from(number).ok())
            != Some(build)
    {
        return Err(Error::refused(
            "the supplied manifest's program/build differs from the managed release tag",
        ));
    }
    Ok(())
}

/// Copies the small manifest whose version was admitted, so a later local rewrite
/// cannot upload a different manifest under the reservation just acquired.
pub(super) struct Prepared {
    _scratch: aterm_tempfile::TempDir,
    pub(super) manifest: PathBuf,
    pub(super) reservation: PathBuf,
    pub(super) files: Vec<PathBuf>,
    record: Vec<u8>,
}

impl Prepared {
    pub(super) fn new(manifest: &Path, files: &[PathBuf]) -> Result<Self> {
        let bytes = read_manifest(manifest)?;
        let version = manifest_version(&bytes)
            .filter(|version| !version.is_empty() && version.len() <= VERSION_BYTES)
            .ok_or_else(|| {
                Error::refused(
                    "the pack manifest needs a nonempty version of at most 4096 UTF-8 bytes",
                )
            })?;
        let name = manifest
            .file_name()
            .ok_or_else(|| Error::refused("the pack manifest has no file name"))?;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-version-")
            .tempdir()
            .map_err(|error| Error::io("create version reservation scratch", &error))?;
        let copied = scratch.path().join(name);
        let reservation = scratch.path().join(VERSION_RESERVATION);
        let mut ordered = Vec::with_capacity(files.len());
        let mut names = std::collections::BTreeSet::new();
        let mut found = false;
        for file in files {
            let file_name = file
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::refused("a pack asset has no UTF-8 file name"))?;
            if file_name == VERSION_RESERVATION
                || file_name == format!("{VERSION_RESERVATION}{STAGED}")
                || !names.insert(file_name.to_owned())
            {
                return Err(Error::refused(
                    "a pack payload cannot replace its version reservation or repeat an asset name",
                ));
            }
            if file.file_name() == Some(name) {
                if read_manifest(file)? != bytes {
                    return Err(Error::refused(
                        "the uploaded manifest differs from the version being reserved",
                    ));
                }
                found = true;
            } else {
                ordered.push(file.clone());
            }
        }
        if !found {
            return Err(Error::refused(
                "the uploaded files do not contain the exact pack manifest",
            ));
        }
        std::fs::write(&copied, bytes)
            .map_err(|error| Error::io("copy the admitted manifest", &error))?;
        let mut record = MAGIC.to_vec();
        record.extend_from_slice(&(version.len() as u32).to_le_bytes());
        record.extend_from_slice(version.as_bytes());
        std::fs::write(&reservation, &record)
            .map_err(|error| Error::io("write version reservation", &error))?;
        ordered.push(copied.clone());
        Ok(Self {
            _scratch: scratch,
            manifest: copied,
            reservation,
            files: ordered,
            record,
        })
    }

    /// A lost upload ACK is not an absence proof: only the exact immutable winner
    /// read back under this fixed name authorizes a payload clobber.
    pub(super) fn acquire(&self, host: &dyn ReleaseHost, slug: &Slug, tag: &str) -> Result<()> {
        let sent = host.upload(
            slug,
            tag,
            std::slice::from_ref(&self.reservation),
            Replace::Never,
        );
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-version-winner-")
            .tempdir()
            .map_err(|error| Error::io("create reservation read scratch", &error))?;
        host.download(slug, tag, &[VERSION_RESERVATION], scratch.path()).map_err(|error| {
            Error::cannot_run(format!("could not read {slug} {tag}'s immutable version winner ({error}; reservation upload: {sent:?}) — no payload uploaded"))
        })?;
        let winner = bounded(&scratch.path().join(VERSION_RESERVATION), RECORD_BYTES)?;
        let version = version_of(&winner).ok_or_else(|| {
            Error::refused(format!(
                "{slug} {tag} has an unknown or malformed version reservation — no payload uploaded"
            ))
        })?;
        if winner != self.record {
            return Err(Error::refused(format!(
                "{slug} {tag} reserves version {version:?}, not {:?} — one build number names one version (D51)",
                version_of(&self.record).expect("locally encoded reservation")
            )));
        }
        Ok(())
    }
}

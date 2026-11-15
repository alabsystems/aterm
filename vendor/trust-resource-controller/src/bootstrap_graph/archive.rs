// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Closed local archive-copy DATA. No frame creates input/SDK custody, a GNU
//! loan, native child, returned producer, extracted seed or completion owner.

use std::io;

use super::query::OperationKey;
use super::{GnuIdentity, absolute};
use crate::wire::{Encoder, header, invalid};
use crate::{FileIdentity, MetadataFile, MetadataPrepared};

pub const PURPOSE: &str = "native-bootstrap-local-archive-bundle-v1";
pub const START_ARGUMENT: &str = "--trust-bootstrap-local-archives-v1";
pub const PREPARED_POLICY: &str = "bootstrap-local-archive-bundle-v1";
pub const PREPARED_SCOPE: &str = "native-no-fork-target-v1";
pub const HOST: &str = "aarch64-apple-darwin";
pub const MAX_WORK_NS: u64 = 900_000_000_000;
pub const MAX_DECLARATION_BYTES: u64 = 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_TOTAL_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const ARCHIVE_COUNT: usize = 9;
pub const COMPONENTS: [&str; ARCHIVE_COUNT] = [
    "targo",
    "targo-trust",
    "tippy",
    "trust-analyzer",
    "trust-src",
    "trust-std",
    "trustc",
    "trustc-dev",
    "trustfmt",
];
const DOMAIN: &[u8] = b"trust-bootstrap-local-archives-v1\0";

/// Software support DATA in the SAME held Targo image, never launch, lifetime,
/// source authentication, seed, SDK compatibility or proof authority.
pub const PROTOCOL_MARKER: [u8; 64] = marker();
const fn marker() -> [u8; 64] {
    let mut bytes = [0; 64];
    let magic = *b"TRUSTBARCH000002";
    let mut i = 0;
    while i < magic.len() {
        bytes[i] = magic[i];
        i += 1;
    }
    let fields = [2_u64, 118, 119, 120, 121, 122];
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index].to_be_bytes();
        let mut offset = 0;
        while offset < field.len() {
            bytes[16 + index * 8 + offset] = field[offset];
            offset += 1;
        }
        index += 1;
    }
    bytes
}
fn join(root: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut out = root.to_vec();
    out.push(b'/');
    out.extend(tail);
    out
}
fn date(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}
fn version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && value != "."
        && value != ".."
}
fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
pub fn relative_archive(date: &str, version: &str, index: usize) -> io::Result<Vec<u8>> {
    if !self::date(date) || !self::version(version) {
        return Err(invalid());
    }
    let component = COMPONENTS.get(index).ok_or_else(invalid)?;
    let suffix = if *component == "trust-src" {
        String::new()
    } else {
        format!("-{HOST}")
    };
    Ok(format!("dist/{date}/{component}-{version}{suffix}.tar.xz").into_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivePin {
    pub relative: Vec<u8>,
    pub sha256: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub key: OperationKey,
    pub gnu: GnuIdentity,
    pub sdk_root: Vec<u8>,
    pub driver: MetadataFile,
    /// Actual retained context identities; these are not target triples.
    pub host: String,
    pub boot: String,
    pub source_root: Vec<u8>,
    pub source_directory: FileIdentity,
    /// Exactly src/stage0, seed-ledger.toml and this date's channel manifest.
    pub declarations: [MetadataFile; 3],
    pub date: String,
    pub version: String,
    /// Exact COMPONENTS order; no arbitrary destination or payload selector.
    pub archives: Vec<ArchivePin>,
    pub contents_directory: FileIdentity,
    pub timeout_ns: u64,
}
impl Request {
    /// Fixed direct leaf under the actual original contents descriptor. No
    /// directory is created/adopted and no caller output pathname is accepted.
    pub fn output_basename(&self, index: usize) -> io::Result<Vec<u8>> {
        self.validate()?;
        let relative = relative_archive(&self.date, &self.version, index)?;
        Ok(relative
            .rsplit(|byte| *byte == b'/')
            .next()
            .ok_or_else(invalid)?
            .to_vec())
    }
    pub fn validate(&self) -> io::Result<()> {
        self.key.validate()?;
        self.gnu.write(&mut Encoder::new(0))?;
        self.driver.validate(&self.sdk_root)?;
        if !absolute(&self.sdk_root)
            || !absolute(&self.source_root)
            || self.sdk_root == self.source_root
            || self.driver.path != join(&self.sdk_root, b"bin/targo")
            || !identity(&self.host)
            || !identity(&self.boot)
            || self.source_directory.inode == 0
            || self.contents_directory.inode == 0
            || self.source_directory == self.contents_directory
            || !date(&self.date)
            || !version(&self.version)
            || self.archives.len() != ARCHIVE_COUNT
            || !matches!(self.timeout_ns, 1..=MAX_WORK_NS)
        {
            return Err(invalid());
        }
        let expected = [
            b"src/stage0".to_vec(),
            b"bootstrap/trust-stage0/seed-ledger.toml".to_vec(),
            format!(
                "bootstrap/trust-stage0/dist/{}/channel-rust-trust.toml",
                self.date
            )
            .into_bytes(),
        ];
        for (file, relative) in self.declarations.iter().zip(expected) {
            file.validate(&self.source_root)?;
            if file.path != join(&self.source_root, &relative) || file.bytes > MAX_DECLARATION_BYTES
            {
                return Err(invalid());
            }
        }
        for (index, pin) in self.archives.iter().enumerate() {
            if pin.relative != relative_archive(&self.date, &self.version, index)? {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// One distinct native service. No aliases, flags, command, extraction,
    /// network source, output pathname or compiler may be supplied by a caller.
    pub fn arguments(&self) -> io::Result<[&'static str; 1]> {
        self.validate()?;
        Ok([START_ARGUMENT])
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut e = Encoder::new(118);
        e.number(2);
        self.key.write(&mut e)?;
        self.gnu.write(&mut e)?;
        e.bytes(&self.sdk_root)?;
        self.driver.encode(&mut e)?;
        e.bytes(self.host.as_bytes())?;
        e.bytes(self.boot.as_bytes())?;
        e.bytes(&self.source_root)?;
        super::write_file(&mut e, self.source_directory)?;
        for declaration in &self.declarations {
            declaration.encode(&mut e)?;
        }
        e.bytes(self.date.as_bytes())?;
        e.bytes(self.version.as_bytes())?;
        e.number(self.archives.len() as u64);
        for archive in &self.archives {
            e.bytes(&archive.relative)?;
            e.0.extend(archive.sha256);
        }
        super::write_file(&mut e, self.contents_directory)?;
        e.number(self.timeout_ns);
        e.finish()
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut d = header(bytes, 118)?;
        if d.number()? != 2 {
            return Err(invalid());
        }
        let key = OperationKey::read(&mut d)?;
        let gnu = GnuIdentity::read(&mut d)?;
        let sdk_root = d.bytes()?;
        let driver = MetadataFile::decode(&mut d)?;
        let host = d.text()?;
        let boot = d.text()?;
        let source_root = d.bytes()?;
        let source_directory = super::read_file(&mut d)?;
        let declarations = [
            MetadataFile::decode(&mut d)?,
            MetadataFile::decode(&mut d)?,
            MetadataFile::decode(&mut d)?,
        ];
        let date = d.text()?;
        let version = d.text()?;
        let count = d.count(ARCHIVE_COUNT)?;
        let mut archives = Vec::with_capacity(count);
        for _ in 0..count {
            archives.push(ArchivePin {
                relative: d.bytes()?,
                sha256: d.array()?,
            });
        }
        let contents_directory = super::read_file(&mut d)?;
        let timeout_ns = d.number()?;
        d.done()?;
        let value = Self {
            key,
            gnu,
            sdk_root,
            driver,
            host,
            boot,
            source_root,
            source_directory,
            declarations,
            date,
            version,
            archives,
            contents_directory,
            timeout_ns,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn hello(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        self.key.graph.hello(PURPOSE)
    }
    pub fn binding_preimage(&self) -> io::Result<Vec<u8>> {
        let frame = self.encode()?;
        let mut bytes = Vec::from(DOMAIN);
        bytes.extend((frame.len() as u64).to_be_bytes());
        bytes.extend(frame);
        Ok(bytes)
    }
    pub fn validate_prepared(&self, prepared: &MetadataPrepared) -> io::Result<()> {
        self.validate()?;
        prepared.encode()?;
        if prepared.root_attempt != self.key.graph.query.binding.root_attempt
            || prepared.member_attempt != self.key.operation
            || prepared.root != self.sdk_root
            || prepared.executable != self.driver
            || prepared.policy != PREPARED_POLICY
            || prepared.scope != PREPARED_SCOPE
            || prepared.host != self.host
            || prepared.boot != self.boot
        {
            return Err(invalid());
        }
        Ok(())
    }
}

mod protocol;
pub use protocol::{Ack, Event, MAX_CHUNK, MAX_OUTPUT, Stream, Terminal};

#[cfg(test)]
mod tests;

/// The child-side pre-read exchange. A matching frame is not native funding,
/// loaded-image proof, child exit, cohort settlement or returned availability.
pub mod startup;

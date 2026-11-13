// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private, resumable package-publication observations. Receipts never authorize
//! signing, publication or installation: the controller rechecks those native
//! gates and the actual external state on every invocation.

use std::fs::File;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_uds::private_file::{self, PrivateLock};
use serde::{Deserialize, Serialize};

use crate::artifact::package::{self, Package};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

const MAX_BYTES: usize = 64 * 1024;
const MAX_RECEIPTS: usize = 256;

/// Frozen inputs for one regular package release, derived from its actual tree
/// and the native publisher's configured destinations.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    schema: u32,
    program: String,
    build: u64,
    source: String,
    version: String,
    target: String,
    package_tree: String,
    packed_tree: String,
    repository: String,
    staging_owner: String,
    public_owner: String,
}

impl Intent {
    /// Freeze a regular package. This is an identity observation, not package
    /// qualification or permission to upload anything.
    /// # Errors
    /// An unsupported package, development identity, or unreadable tree.
    pub fn from_package(ctx: &Ctx, program: &str, build: u64, pkg: &Package) -> Result<Self> {
        let (definition, row, target) = package::package_row(program)?;
        package::check_build(program, build)?;
        if package::read_package(&pkg.root, row.exposes[0], target)? != *pkg {
            return Err(Error::refused(
                "the package changed before its campaign was frozen",
            ));
        }
        if pkg.count.is_some()
            || !atpkg::alab::AlabVersion::parse(&pkg.version)
                .is_some_and(|version| version.dev.is_none() && version.baseline.is_none())
            || !hex(&pkg.source, 40)
        {
            return Err(Error::refused(
                "a package campaign requires a regular release identity",
            ));
        }
        let trees = atpkg::tree::tree_roots_for_pack(&pkg.root)
            .map_err(|error| Error::io("freeze source and payload trees", &error))?;
        Ok(Self {
            schema: 2,
            program: program.into(),
            build,
            source: pkg.source.clone(),
            version: pkg.version.clone(),
            target: target.to_string(),
            package_tree: trees.source,
            packed_tree: trees.payload,
            repository: definition.repo.into(),
            staging_owner: ctx.accounts.staging.clone(),
            public_owner: ctx.accounts.public.clone(),
        })
    }
}

/// Exact signed files observed for a package. Every digest is SHA-256.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageFiles {
    /// Signed package manifest bytes.
    pub manifest: String,
    /// Detached signature bytes.
    pub signature: String,
    /// Downloaded archive bytes.
    pub archive: String,
    /// The client's installed-tree digest.
    pub tree_root: String,
}

/// An observation made by native gates or actual readback. In particular,
/// preparing an index does not mean that its publication failed or succeeded.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Observation {
    /// Package qualification and signature verification completed.
    Qualified {
        /// Files admitted by the native package verifier.
        files: PackageFiles,
    },
    /// The staging host serves these exact signed package files.
    Staged {
        /// Exact files actually read from staging.
        files: PackageFiles,
    },
    /// Retained before attempting publication; the outcome can be unknown.
    IndexPrepared {
        /// The reserved index number.
        build: u64,
        /// Digests in the native QUAD_FILES order.
        quad: [String; 4],
        /// The exact public predecessor used to qualify this attempt.
        predecessor_build: u64,
        /// Predecessor digests in QUAD_FILES order.
        predecessor: [String; 4],
    },
    /// All four public files were read and verified, not merely uploaded.
    IndexPublished {
        /// The index actually observed publicly.
        build: u64,
        /// Digests in the native QUAD_FILES order.
        quad: [String; 4],
    },
    /// The client installed and ran the intended package entrypoint.
    Installed {
        /// The client's installed-tree digest.
        tree_root: String,
        /// Executable bytes actually invoked by the installed selection.
        entrypoint: String,
    },
}

impl Observation {
    fn valid(&self) -> bool {
        match self {
            Self::Qualified { files } | Self::Staged { files } => [
                &files.manifest,
                &files.signature,
                &files.archive,
                &files.tree_root,
            ]
            .into_iter()
            .all(|digest| hex(digest, 64)),
            Self::IndexPrepared {
                build,
                quad,
                predecessor_build,
                predecessor,
            } => {
                *predecessor_build != 0
                    && predecessor_build.checked_add(1) == Some(*build)
                    && quad.iter().chain(predecessor).all(|digest| hex(digest, 64))
            }
            Self::IndexPublished { build, quad } => {
                *build != 0 && quad.iter().all(|digest| hex(digest, 64))
            }
            Self::Installed {
                tree_root,
                entrypoint,
            } => hex(tree_root, 64) && hex(entrypoint, 64),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    intent: String,
    sequence: usize,
    previous: Option<String>,
    observation: Observation,
}

struct Entry {
    digest: String,
    observation: Observation,
}

/// One exact private writer. A failed write remains uncertain until the same
/// observation is durably reconciled; a retry never overwrites an earlier file.
pub struct Campaign {
    inputs: Intent,
    root: PathBuf,
    directory: [u64; 2],
    lock_identity: [u64; 2],
    _lock: PrivateLock,
    intent: String,
    entries: Vec<Entry>,
    attempted: Option<Observation>,
}

impl Campaign {
    /// Open or create a campaign in an existing parent directory. An existing
    /// campaign must match every frozen input, including destinations and bytes.
    /// # Errors
    /// Nonprivate state, concurrent ownership, changed inputs, or damaged receipts.
    pub fn open(root: &Path, intent: &Intent) -> Result<Self> {
        if intent.schema != 2
            || !hex(&intent.source, 40)
            || !hex(&intent.package_tree, 64)
            || !hex(&intent.packed_tree, 64)
            || package::check_build(&intent.program, intent.build).is_err()
            || package::package_row(&intent.program)?.2.as_str() != intent.target
            || !atpkg::alab::AlabVersion::parse(&intent.version)
                .is_some_and(|version| version.dev.is_none() && version.baseline.is_none())
            || agh::Slug::new(&intent.public_owner, &intent.repository).is_err()
            || agh::Slug::new(&intent.staging_owner, &intent.repository).is_err()
        {
            return Err(Error::refused("invalid frozen package campaign"));
        }
        private_file::create_private_dir(root)
            .map_err(|error| Error::io("create package campaign", &error))?;
        let root = root
            .canonicalize()
            .map_err(|error| Error::io("resolve campaign", &error))?;
        let lock = PrivateLock::take(
            &root,
            "campaign.lock",
            Duration::ZERO,
            "campaign is already running",
        )
        .map_err(|error| Error::io("lease package campaign", &error))?;
        let lock_file = File::from(
            lock.as_fd()
                .try_clone_to_owned()
                .map_err(|error| Error::io("inspect campaign lease", &error))?,
        );
        let directory = identity(
            &std::fs::metadata(&root)
                .map_err(|error| Error::io("inspect campaign directory", &error))?,
        );
        let lock_identity = identity(
            &lock_file
                .metadata()
                .map_err(|error| Error::io("inspect campaign lease", &error))?,
        );
        let bytes =
            aterm_json::to_vec(intent).map_err(|error| Error::refused(error.to_string()))?;
        let mut campaign = Self {
            inputs: intent.clone(),
            root,
            directory,
            lock_identity,
            _lock: lock,
            intent: atpkg::sha256_bytes(&bytes),
            entries: Vec::new(),
            attempted: None,
        };
        campaign.keep("intent.json", &bytes)?;
        campaign.entries = campaign.read_entries()?;
        Ok(campaign)
    }

    /// Ordered observations; callers must reverify the actual signed files and
    /// external state rather than treating a receipt as authority.
    pub fn observations(&self) -> impl Iterator<Item = &Observation> {
        self.entries.iter().map(|entry| &entry.observation)
    }

    /// Durably retain one observation. Repeating the last observation is
    /// idempotent, including after an uncertain file/directory flush.
    /// # Errors
    /// Malformed evidence, changed files, a different uncertain retry, or I/O failure.
    pub fn record(&mut self, observation: Observation) -> Result<()> {
        if !observation.valid()
            || self
                .attempted
                .as_ref()
                .is_some_and(|attempted| attempted != &observation)
        {
            return Err(Error::refused(
                "invalid observation or unreconciled campaign write",
            ));
        }
        let entries = self.read_entries()?;
        if entries.len() < self.entries.len()
            || self
                .entries
                .iter()
                .zip(&entries)
                .any(|(old, now)| old.digest != now.digest)
            || (entries.len() > self.entries.len()
                && (entries.len() != self.entries.len() + 1
                    || self.attempted.as_ref() != entries.last().map(|entry| &entry.observation)))
        {
            return Err(Error::refused("package campaign history changed"));
        }
        self.entries = entries;
        if self
            .entries
            .last()
            .is_some_and(|entry| entry.observation == observation)
        {
            self.attempted = Some(observation);
            self.flush(&receipt_name(self.entries.len() - 1))?;
            self.attempted = None;
            return Ok(());
        }
        if self.entries.len() >= MAX_RECEIPTS {
            return Err(Error::refused("package campaign receipt bound reached"));
        }
        let receipt = Receipt {
            schema: 1,
            intent: self.intent.clone(),
            sequence: self.entries.len(),
            previous: self.entries.last().map(|entry| entry.digest.clone()),
            observation: observation.clone(),
        };
        let bytes =
            aterm_json::to_vec(&receipt).map_err(|error| Error::refused(error.to_string()))?;
        self.attempted = Some(observation.clone());
        self.keep(&receipt_name(receipt.sequence), &bytes)?;
        self.entries.push(Entry {
            digest: atpkg::sha256_bytes(&bytes),
            observation,
        });
        self.attempted = None;
        Ok(())
    }

    fn check(&self) -> Result<()> {
        private_file::check_private_dir(&self.root)
            .map_err(|error| Error::io("check campaign directory", &error))?;
        let directory = std::fs::metadata(&self.root)
            .map_err(|error| Error::io("inspect campaign directory", &error))?;
        let (_, lock) = private_file::open_private(&self.root, "campaign.lock")
            .map_err(|error| Error::refused(format!("campaign lease changed: {error}")))?;
        if identity(&directory) != self.directory || identity(&lock) != self.lock_identity {
            return Err(Error::refused(
                "package campaign directory or lease was replaced",
            ));
        }
        Ok(())
    }

    fn keep(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::refused("package campaign receipt exceeds its bound"));
        }
        self.check()?;
        private_file::write_private_new(&self.root, name, bytes, |_| Ok(()))
            .map_err(|error| Error::io("retain immutable campaign receipt", &error))?;
        let actual = private_file::read_private(&self.root, name, MAX_BYTES)
            .map_err(|error| Error::refused(format!("read campaign receipt: {error}")))?;
        if actual != bytes {
            return Err(Error::refused(
                "package campaign receipt belongs to different inputs",
            ));
        }
        self.flush(name)
    }

    fn flush(&self, name: &str) -> Result<()> {
        let (file, _) = private_file::open_private(&self.root, name)
            .map_err(|error| Error::refused(format!("open campaign receipt: {error}")))?;
        aterm_durable::Flush::Durable
            .file(&file)
            .and_then(|()| aterm_durable::Flush::Durable.dir(&self.root))
            .map_err(|error| Error::io("finish durable campaign receipt", &error))?;
        self.check()
    }

    fn read_entries(&self) -> Result<Vec<Entry>> {
        self.check()?;
        let intent = private_file::read_private(&self.root, "intent.json", MAX_BYTES)
            .map_err(|error| Error::refused(format!("read frozen campaign: {error}")))?;
        if atpkg::sha256_bytes(&intent) != self.intent {
            return Err(Error::refused("frozen package campaign changed"));
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .map_err(|error| Error::io("read campaign history", &error))?
        {
            let entry = entry.map_err(|error| Error::io("read campaign entry", &error))?;
            let name = entry.file_name();
            if name.as_encoded_bytes().starts_with(b"receipt-") {
                names.push(name);
                if names.len() > MAX_RECEIPTS {
                    return Err(Error::refused("package campaign receipt bound exceeded"));
                }
            }
        }
        names.sort();
        if names.len() > MAX_RECEIPTS {
            return Err(Error::refused("package campaign receipt bound exceeded"));
        }
        let mut entries: Vec<Entry> = Vec::new();
        for (sequence, name) in names.into_iter().enumerate() {
            if name != receipt_name(sequence).as_str() {
                return Err(Error::refused(
                    "package campaign has a missing or foreign receipt",
                ));
            }
            let bytes = private_file::read_private(&self.root, &receipt_name(sequence), MAX_BYTES)
                .map_err(|error| Error::refused(format!("read campaign history: {error}")))?;
            let receipt: Receipt = aterm_json::from_slice(&bytes)
                .map_err(|error| Error::refused(format!("decode campaign history: {error}")))?;
            if receipt.schema != 1
                || receipt.intent != self.intent
                || receipt.sequence != sequence
                || receipt.previous != entries.last().map(|entry| entry.digest.clone())
                || !receipt.observation.valid()
            {
                return Err(Error::refused(
                    "package campaign receipt chain does not match its intent",
                ));
            }
            entries.push(Entry {
                digest: atpkg::sha256_bytes(&bytes),
                observation: receipt.observation,
            });
        }
        self.check()?;
        Ok(entries)
    }
}

fn identity(meta: &std::fs::Metadata) -> [u64; 2] {
    [meta.dev(), meta.ino()]
}

fn receipt_name(sequence: usize) -> String {
    format!("receipt-{sequence:08}.json")
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[path = "package_campaign_index.rs"]
mod index_snapshot;
pub use index_snapshot::{RetainedIndex, Retention};

#[path = "package_campaign_package.rs"]
mod qualified_package;

#[path = "package_campaign_cli.rs"]
mod cli;
pub use cli::{SYNOPSIS, run};

#[cfg(test)]
#[path = "package_campaign_tests.rs"]
mod tests;

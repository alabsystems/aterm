// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private stable configuration, local status and independent lifetime/source locks.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use aterm_uds::private_file::{self, PrivateDir, PrivateFileError};

use super::{Config, Status};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

const MAX_RECORD: usize = 64 << 10;

/// Judge every component below the exact canonical home; never normalize a linked path.
pub(super) fn ancestry(home: &Path, path: &Path, missing: bool) -> Result<()> {
    if !home.is_absolute() || home.canonicalize().ok().as_deref() != Some(home) {
        return Err(Error::refused(
            "local manager home must be exact and canonical",
        ));
    }
    let relative = path
        .strip_prefix(home)
        .map_err(|_| Error::refused("local manager paths must be inside its owned home"))?;
    let mut current = home.to_path_buf();
    for part in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(part) = part {
            let Component::Normal(name) = part else {
                return Err(Error::refused(
                    "local manager paths require ordinary absolute components",
                ));
            };
            current.push(name);
        }
        match private_file::judge_private_dir(&current)
            .map_err(|e| Error::io("judge local manager ancestry", &e))?
        {
            PrivateDir::Private => {}
            PrivateDir::Open(mode) if mode & 0o022 == 0 => {}
            PrivateDir::Missing if missing => {}
            _ => {
                return Err(Error::refused(format!(
                    "local manager ancestry is missing, foreign or writable by others: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn create(home: &Path, path: &Path) -> Result<()> {
    ancestry(home, path, true)?;
    let mut current = home.to_path_buf();
    for component in path
        .strip_prefix(home)
        .map_err(|_| Error::refused("private manager path escaped home"))?
        .components()
    {
        let Component::Normal(name) = component else {
            return Err(Error::refused("nonordinary private manager path"));
        };
        current.push(name);
        match private_file::judge_private_dir(&current)
            .map_err(|e| Error::io("judge manager directory", &e))?
        {
            PrivateDir::Private => {}
            PrivateDir::Open(mode) if mode & 0o022 == 0 => {}
            PrivateDir::Missing => private_file::create_private_dir(&current)
                .map_err(|e| Error::io("create private manager directory", &e))?,
            _ => {
                return Err(Error::refused(
                    "manager directory changed or became writable by others",
                ));
            }
        }
    }
    ancestry(home, path, false)
}

pub(super) fn root(prefix: &Path, program: &str) -> Result<PathBuf> {
    if atpkg::store::ToolName::new(program).is_none() || !atpkg::harness::is_harness(program) {
        return Err(Error::usage("invalid local harness package"));
    }
    Ok(prefix.join(".local-harness-manager").join(program))
}

/// The lock's open inode is compared to its exact name before every state write.
struct ExactLock {
    file: std::fs::File,
    dir: PathBuf,
}

impl ExactLock {
    fn take(dir: &Path) -> Result<Self> {
        let file: std::fs::File =
            aterm_uds::kept_out(|| private_file::open_private_rw(dir, "manager.lock"))
                .map_err(|e| Error::refused(format!("private manager lock: {e}")))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this lifetime guard releases by LOCK_UN in Drop"
        )]
        file.try_lock().map_err(|e| {
            Error::refused(format!(
                "another local manager holds the exact program or source lock: {e}"
            ))
        })?;
        let lock = Self {
            file,
            dir: dir.into(),
        };
        lock.check()?;
        Ok(lock)
    }
    fn check(&self) -> Result<()> {
        let (_, named) = private_file::open_private(&self.dir, "manager.lock")
            .map_err(|e| Error::refused(format!("manager lock name changed: {e}")))?;
        let held = self
            .file
            .metadata()
            .map_err(|e| Error::io("read held manager lock", &e))?;
        if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
            return Err(Error::refused("manager lock was replaced"));
        }
        Ok(())
    }
}

impl Drop for ExactLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) struct Session {
    home: PathBuf,
    config: Config,
    root: PathBuf,
    program_lock: ExactLock,
    source_lock: ExactLock,
    source_home: PathBuf,
    source_identity: [u64; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LeaseProof {
    program: [u64; 2],
    source: [u64; 2],
}

fn source_root(home: &Path, checkout: &Path) -> Result<(PathBuf, PathBuf, [u64; 2])> {
    #[cfg(test)]
    let anchor = home.to_path_buf();
    #[cfg(not(test))]
    let anchor = aterm_uds::account::home()
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| {
            Error::cannot_run("account home is unavailable for the shared source lease")
        })?;
    #[cfg(not(test))]
    let _ = home;
    let meta =
        std::fs::symlink_metadata(checkout).map_err(|e| Error::io("source lease identity", &e))?;
    if !meta.is_dir() {
        return Err(Error::refused(
            "source lease requires its actual directory inode",
        ));
    }
    let identity = [meta.dev(), meta.ino()];
    let digest =
        aterm_digest::Sha256::digest(format!("{}:{}", identity[0], identity[1]).as_bytes());
    let key = aterm_codec::hex::encode_lower(&digest);
    let path = anchor
        .join(".local/state/aterm-local-harness/sources")
        .join(key);
    Ok((anchor, path, identity))
}

pub(super) fn verify_lease(home: &Path, config: &Config, proof: &LeaseProof) -> Result<()> {
    let program = root(&config.prefix, &config.program)?;
    let (source_home, source, _) = source_root(home, &config.checkout)?;
    for (anchor, dir, expected) in [
        (home, &program, proof.program),
        (source_home.as_path(), &source, proof.source),
    ] {
        ancestry(anchor, dir, false)?;
        let (file, meta) = private_file::open_private(dir, "manager.lock")
            .map_err(|e| Error::refused(format!("worker lease: {e}")))?;
        if [meta.dev(), meta.ino()] != expected {
            return Err(Error::refused("worker lease inode changed"));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "read-only lock probe; a successful take is explicitly unlocked before refusal"
        )]
        let held = file.try_lock();
        match held {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(()) => {
                let _ = file.unlock();
                return Err(Error::refused("recorded guardian lease is no longer held"));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(Error::io("inspect worker lease", &error));
            }
        }
    }
    if read::<Config>(&program, "config.json")?.as_ref() != Some(config) {
        return Err(Error::refused("worker's stable configuration changed"));
    }
    Ok(())
}

pub(super) fn source_record<T: serde::de::DeserializeOwned>(
    home: &Path,
    checkout: &Path,
) -> Result<Option<T>> {
    let (anchor, dir, _) = source_root(home, checkout)?;
    ancestry(&anchor, &dir, false)?;
    read(&dir, "active.json")
}

pub(super) fn read<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<T>> {
    let bytes = match private_file::read_private(dir, name, MAX_RECORD) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(error) => return Err(Error::refused(format!("private manager {name}: {error}"))),
    };
    aterm_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::refused(format!("manager {name}: {e}")))
}

pub(super) fn bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes =
        aterm_json::to_vec(value).map_err(|e| Error::refused(format!("manager record: {e}")))?;
    if bytes.len() > MAX_RECORD {
        return Err(Error::refused("manager record exceeds its bound"));
    }
    Ok(bytes)
}

impl Session {
    pub(super) fn source_identity(&self) -> [u64; 2] {
        self.source_identity
    }
    #[cfg(any(test, debug_assertions))]
    pub(super) fn source_record_path(&self) -> PathBuf {
        self.source_lock.dir.join("active.json")
    }
    pub(super) fn read_source<T: serde::de::DeserializeOwned>(&self) -> Result<Option<T>> {
        self.check()?;
        read(&self.source_lock.dir, "active.json")
    }
    pub(super) fn save_source<T: serde::Serialize>(&self, value: &T) -> Result<()> {
        self.check()?;
        private_file::write_private(&self.source_lock.dir, "active.json", &bytes(value)?)
            .map_err(|e| Error::io("save shared source attempt", &e))?;
        self.check()
    }
    pub(super) fn home(&self) -> &Path {
        &self.home
    }
    pub(super) fn lease(&self) -> Result<LeaseProof> {
        self.check()?;
        let of = |lock: &ExactLock| {
            lock.file
                .metadata()
                .map(|meta| [meta.dev(), meta.ino()])
                .map_err(|e| Error::io("capture manager lease", &e))
        };
        Ok(LeaseProof {
            program: of(&self.program_lock)?,
            source: of(&self.source_lock)?,
        })
    }
    pub(super) fn root(&self) -> &Path {
        &self.root
    }
    pub(super) fn check_owned(&self) -> Result<()> {
        self.check()
    }
    pub(super) fn take(home: &Path, config: &Config) -> Result<Self> {
        config.validate(home)?;
        create(home, &config.prefix)?;
        create(home, &config.out)?;
        let root = root(&config.prefix, &config.program)?;
        create(home, &root)?;
        private_file::check_private_dir(&root)
            .map_err(|e| Error::io("check private program state", &e))?;
        let program_lock = ExactLock::take(&root)?;
        // The source lock is shared by every prefix using this checkout. Two managers
        // cannot fetch/merge/build the same source under different program locks.
        let (source_home, source_root, source_identity) = source_root(home, &config.checkout)?;
        create(&source_home, &source_root)?;
        private_file::check_private_dir(&source_root)
            .map_err(|e| Error::io("check private source state", &e))?;
        let source_lock = ExactLock::take(&source_root)?;
        let session = Self {
            home: home.into(),
            config: config.clone(),
            root,
            program_lock,
            source_lock,
            source_home,
            source_identity,
        };
        session.check()?;
        match read::<Config>(&session.root, "config.json")? {
            Some(old) if old != *config => {
                return Err(Error::refused(
                    "this program/prefix has a different stable local configuration; its files were preserved",
                ));
            }
            Some(_) => {}
            None => private_file::write_private(&session.root, "config.json", &bytes(config)?)
                .map_err(|e| Error::io("save stable local configuration", &e))?,
        }
        session.check()?;
        Ok(session)
    }
    fn check(&self) -> Result<()> {
        ancestry(&self.home, &self.root, false)?;
        ancestry(&self.source_home, &self.source_lock.dir, false)?;
        let source = std::fs::symlink_metadata(&self.config.checkout)
            .map_err(|e| Error::io("reprove source inode", &e))?;
        if !source.is_dir() || [source.dev(), source.ino()] != self.source_identity {
            return Err(Error::refused(
                "the source directory inode changed under its lease",
            ));
        }
        self.program_lock.check()?;
        self.source_lock.check()?;
        Ok(())
    }
    pub(super) fn status(&self) -> Result<Status> {
        self.check()?;
        let status = read::<Status>(&self.root, "status.json")?.unwrap_or_default();
        if status.schema != 1 || status.live_adoption {
            return Err(Error::refused(
                "unsupported or false live-adoption local status",
            ));
        }
        self.check()?;
        Ok(status)
    }
    pub(super) fn save(&self, status: &Status) -> Result<()> {
        self.check()?;
        if read::<Config>(&self.root, "config.json")?.as_ref() != Some(&self.config) {
            return Err(Error::refused(
                "stable configuration changed under the manager",
            ));
        }
        private_file::write_private(&self.root, "status.json", &bytes(status)?)
            .map_err(|e| Error::io("save local manager status", &e))?;
        self.check()
    }
}

pub(super) fn show_status(ctx: &Ctx, args: &[String]) -> Result<()> {
    let [program, flag, prefix] = args else {
        return Err(Error::usage("harness-local status <program> --prefix DIR"));
    };
    if flag != "--prefix" {
        return Err(Error::usage("harness-local status requires --prefix DIR"));
    }
    let prefix = PathBuf::from(prefix);
    let root = root(&prefix, program)?;
    ancestry(&ctx.home, &root, false)?;
    let config = read::<Config>(&root, "config.json")?
        .ok_or_else(|| Error::refused("no local manager configuration at this prefix"))?;
    config.validate(&ctx.home)?;
    if config.program != *program || config.prefix != prefix {
        return Err(Error::refused(
            "local configuration belongs to another program/prefix",
        ));
    }
    let mut status = read::<Status>(&root, "status.json")?.unwrap_or_default();
    if status.schema != 1 || status.live_adoption {
        return Err(Error::refused(
            "unsupported or false live-adoption local status",
        ));
    }
    let lock = atpkg::lock::try_lock_store(&atpkg::store::Layout { prefix })
        .map_err(|e| Error::refused(format!("local current is unknown: {e}")))?;
    status.selected = match atpkg::local_harness::observe_current(&lock, program)
        .map_err(|e| Error::io("checked local current is unknown", &e))?
    {
        atpkg::local_harness::CurrentLocal::Absent => None,
        atpkg::local_harness::CurrentLocal::Selected(identity) => Some(identity.into()),
    };
    drop(lock);
    println!(
        "{}",
        aterm_json::to_string(&status).map_err(|e| Error::refused(e.to_string()))?
    );
    println!("selection=local-current-only live_adoption=unimplemented execution=guarded");
    Ok(())
}

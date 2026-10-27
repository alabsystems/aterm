// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk layers (`docs/DESIGN-vz-guest-2026-10-23.md` §4).
//!
//! | Layer | File | Written? |
//! | --- | --- | --- |
//! | base | the root member in the tooling cache ([`crate::pins::base`]) | never (0444, never attached) |
//! | baked | `baked/<base sha>-<seed fingerprint>.img`, a clone of base grown to 60 GiB | only during its bake, as `*.partial.img`; then 0444 |
//! | live | `live/aterm-linux.img`, beside `live/aterm-linux.toml` naming its baked disk | yes: the lane's disk, provisioned in place |
//!
//! [`Layers::prepare`] brings the disks up. A changed provisioning plan is no input here: it
//! is provisioned in place on the live disk and never bakes.
//!
//! This module starts no VM. The caller boots a [`Prepared::Bake`] disk through the helper
//! with [`Layers::seed_iso`] and calls [`PartialBake::seal`] once that VM has stopped. It calls
//! [`Layers::prepare`], which may replace the live disk, only when no VM runs: a running VM
//! keeps the disks it booted on.

use std::io;
use std::path::{Path, PathBuf};

use crate::seed::Seed;
use crate::space::Free;
use crate::{Error, io};

/// The lane's one VM: its live disk's and its record's name.
pub const VM: &str = "aterm-linux";

/// The disk the guest sees: 60 GiB, sparse, the lane's size on every host.
pub const DISK_BYTES: u64 = 60 << 30;

/// A partial file's suffix: only a bake, or the clone of a live disk, writes one.
pub const PARTIAL: &str = ".partial.img";

/// What the layers ask of the machine, injected so the tests run over a scratch directory.
pub trait Host {
    /// The free bytes of the volume holding `path`.
    ///
    /// # Errors
    /// The volume cannot be read.
    fn free(&self, path: &Path) -> Result<Free, String>;

    /// Clone `src` to the new name `dst` ([`crate::clone::clone_file`]).
    ///
    /// # Errors
    /// The clone failed.
    fn clone_file(&self, src: &Path, dst: &Path) -> io::Result<()>;

    /// Write the NoCloud ISO of the directory `files` at `iso` ([`crate::seed::make_iso`]).
    ///
    /// # Errors
    /// It was not made.
    fn make_iso(&self, files: &Path, iso: &Path) -> Result<(), String>;
}

/// This machine: aterm-sysprobe's volume reading, `clonefile(2)`, and `hdiutil`.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Host for System {
    fn free(&self, path: &Path) -> Result<Free, String> {
        crate::space::read(path)
    }

    fn clone_file(&self, src: &Path, dst: &Path) -> io::Result<()> {
        crate::clone::clone_file(src, dst)
    }

    fn make_iso(&self, files: &Path, iso: &Path) -> Result<(), String> {
        crate::seed::make_iso(files, iso)
    }
}

/// The base a bring-up clones from: the placed root member and the pin it was held to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    /// The root member ([`crate::pins::base`]).
    pub path: PathBuf,
    /// The sha256 of the archive it came from ([`crate::pins::ROOT_ARCHIVE`]), which names
    /// every baked disk made from it.
    pub sha256: String,
}

impl Base {
    /// The base the tree pins, as `lookup` finds it.
    ///
    /// # Errors
    /// As [`crate::pins::base`].
    pub fn pinned(lookup: &aterm_uds::host::Lookup) -> Result<Self, Error> {
        Ok(Self {
            path: crate::pins::base(lookup)?,
            sha256: crate::pins::ROOT_ARCHIVE.sha256.to_string(),
        })
    }
}

/// The live record: which baked disk the live disk was cloned from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The baked disk's file name.
    pub baked: String,
    /// The base's pin ([`Base::sha256`]).
    pub base_sha256: String,
    /// The seed's fingerprint.
    pub seed: String,
    /// When the clone was made (Unix seconds).
    pub cloned: u64,
}

impl Record {
    /// The record as `live/aterm-linux.toml` holds it.
    #[must_use]
    pub fn text(&self) -> String {
        format!(
            "# The baked disk {VM}.img was cloned from (aterm-vz; \
             docs/DESIGN-vz-guest-2026-10-23.md §4).\n\
             baked = \"{}\"\nbase_sha256 = \"{}\"\nseed = \"{}\"\ncloned_unix = {}\n",
            self.baked, self.base_sha256, self.seed, self.cloned
        )
    }

    /// The record `text` holds, where it is whole: a name of the baked layer's shape, two
    /// digests and a time.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let table: aterm_toml::Table = text.parse().ok()?;
        let field = |key: &str| table.get(key).and_then(aterm_toml::Value::as_str);
        let record = Self {
            baked: field("baked")?.to_string(),
            base_sha256: field("base_sha256")?.to_string(),
            seed: field("seed")?.to_string(),
            cloned: u64::try_from(table.get("cloned_unix")?.as_integer()?).ok()?,
        };
        (crate::is_hex(&record.base_sha256, 64)
            && crate::is_hex(&record.seed, 16)
            && record.baked == baked_name(&record.base_sha256, &record.seed))
        .then_some(record)
    }
}

/// A baked disk's file name: its base's pin and its seed's fingerprint.
#[must_use]
pub fn baked_name(base_sha256: &str, seed: &str) -> String {
    format!("{base_sha256}-{seed}.img")
}

/// Why a bring-up clones a new live disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reclone {
    /// There is no live disk.
    NoLiveDisk,
    /// A live disk without a whole record: what it was cloned from is unknown.
    NoRecord,
    /// The record names another base: the pinned kernel's modules are not on it (§3).
    Base {
        /// The base the live disk came from.
        was: String,
    },
}

impl std::fmt::Display for Reclone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLiveDisk => f.write_str("there was no live disk"),
            Self::NoRecord => f.write_str("the live disk had no whole record"),
            Self::Base { was } => write!(f, "the live disk was cloned from base {was}"),
        }
    }
}

/// What a bring-up found, and what its caller does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Prepared {
    /// The live disk to attach: kept, or cloned now for `cloned`'s reason.
    Live {
        /// `live/aterm-linux.img`.
        disk: PathBuf,
        /// Why it was cloned now, or `None` where the disk was kept.
        cloned: Option<Reclone>,
    },
    /// No sealed baked disk exists for this base and seed: bake this one (boot it with the
    /// seed, provision, finish), [`PartialBake::seal`] it, and bring up again.
    Bake(PartialBake),
}

/// A baked disk being made: `baked/<name>.partial.img`, the only file a bake writes.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a bake that is not sealed is removed at the next bring-up"]
pub struct PartialBake {
    partial: PathBuf,
    sealed: PathBuf,
}

impl PartialBake {
    /// The disk the bake's VM attaches writable.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.partial
    }

    /// The name it takes once sealed.
    #[must_use]
    pub fn sealed_path(&self) -> &Path {
        &self.sealed
    }

    /// Seal the bake once it finished (§4's step 4): its VM was `Ready`, the lab provisioned
    /// it, and the helper reported the VM stopped with no detector asking. Flush the file to
    /// stable media, make it 0444, and rename it to its final name, so a baked disk under its
    /// final name is always whole. A bake a detector stopped, or one whose VM never reached
    /// `Ready`, is dropped unsealed (`VzLifecycle`'s `SealedOnlyWhenFinished`): that leaves
    /// the partial file for the next bring-up to remove, as a killed lab does, and a VM may
    /// still hold it.
    ///
    /// # Errors
    /// A flush, the mode change or the rename failed; the partial file stays.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "vz_lifecycle",
            action = "Seal",
            project = "conformance_lifecycle::project"
        )
    )]
    pub fn seal(self) -> Result<PathBuf, Error> {
        let file = std::fs::File::open(&self.partial).map_err(|e| io("open", &self.partial, &e))?;
        aterm_durable::durable(&file).map_err(|e| io("flush", &self.partial, &e))?;
        drop(file);
        crate::read_only(&self.partial)?;
        std::fs::rename(&self.partial, &self.sealed).map_err(|e| io("seal", &self.partial, &e))?;
        if let Some(dir) = self.sealed.parent() {
            aterm_durable::durable_dir(dir).map_err(|e| io("flush", dir, &e))?;
        }
        Ok(self.sealed)
    }
}

/// The layers under one state directory (`~/Library/Caches/aterm/vz.noindex`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layers {
    root: PathBuf,
    disk_bytes: u64,
}

impl Layers {
    /// The layers under `root`, each disk [`DISK_BYTES`].
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            disk_bytes: DISK_BYTES,
        }
    }

    /// This user's: `vz.noindex` in aterm's cache, on the volume of the tooling cache that
    /// holds base, so a clone of base works. `None` where no `$HOME` names it.
    #[must_use]
    pub fn host() -> Option<Self> {
        aterm_uds::host::host_user_cache("vz").map(Self::at)
    }

    /// The layers with disks of `bytes` (the tests' small disks).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn with_disk_bytes(mut self, bytes: u64) -> Self {
        self.disk_bytes = bytes;
        self
    }

    /// The state directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `baked/`.
    #[must_use]
    pub fn baked_dir(&self) -> PathBuf {
        self.root.join("baked")
    }

    /// `live/`.
    #[must_use]
    pub fn live_dir(&self) -> PathBuf {
        self.root.join("live")
    }

    /// `kernel/`, where [`crate::kernel::lay`] lays the pinned kernel.
    #[must_use]
    pub fn kernel_dir(&self) -> PathBuf {
        self.root.join("kernel")
    }

    /// `seed/`, where [`Layers::seed_iso`] keeps the seed ISO.
    #[must_use]
    pub fn seed_dir(&self) -> PathBuf {
        self.root.join("seed")
    }

    /// `live/aterm-linux.img`.
    #[must_use]
    pub fn live_disk(&self) -> PathBuf {
        self.live_dir().join(format!("{VM}.img"))
    }

    /// `live/aterm-linux.toml`.
    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        self.live_dir().join(format!("{VM}.toml"))
    }

    /// The live record, where one is whole.
    #[must_use]
    pub fn record(&self) -> Option<Record> {
        Record::parse(&std::fs::read_to_string(self.record_path()).ok()?)
    }

    /// Bring the disks up for `base` and the seed whose fingerprint is `seed`, under the
    /// lane lock. In order:
    /// 1. every `*.partial.img` in `baked/` and `live/` is removed unread, and so is every
    ///    baked disk that is neither this one nor the live record's, so leftovers cannot hold
    ///    the volume under its floor;
    /// 2. the volume is held to [`crate::space::BRING_UP_FLOOR`] (else [`Error::Floor`], and
    ///    nothing is cloned or baked);
    /// 3. the live disk is kept where its record names `base` ([`Prepared::Live`],
    ///    `cloned: None`);
    /// 4. otherwise the sealed baked disk for `base` and `seed` is cloned to live
    ///    (`cloned: Some(why)`);
    /// 5. and where there is none, a bake begins ([`Prepared::Bake`]).
    ///
    /// The baked disk a replaced record named is then removed too, and `say` hears what was
    /// removed and cloned.
    ///
    /// # Errors
    /// [`Error::Floor`] below the floor, [`Error::Input`] for a base that cannot be read or is
    /// larger than the disk, [`Error::Io`] for a failed volume read, removal, clone or write.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "vz_lifecycle",
            action = "Prepare",
            project = "conformance_lifecycle::project"
        )
    )]
    pub fn prepare(
        &self,
        host: &dyn Host,
        base: &Base,
        seed: &str,
        say: &mut dyn FnMut(&str),
    ) -> Result<Prepared, Error> {
        for dir in [self.baked_dir(), self.live_dir()] {
            std::fs::create_dir_all(&dir).map_err(|e| io("create", &dir, &e))?;
        }
        #[cfg(any(test, debug_assertions))]
        if let Some(planted) = self.planted(host, base, seed)? {
            return Ok(planted);
        }
        for dir in [self.baked_dir(), self.live_dir()] {
            for partial in files(&dir, |name| name.ends_with(PARTIAL)) {
                std::fs::remove_file(&partial).map_err(|e| io("remove", &partial, &e))?;
                say(&format!(
                    "removed {}: a bake or a clone that did not finish",
                    partial.display()
                ));
            }
        }
        let want = baked_name(&base.sha256, seed);
        self.reclaim_baked(&want, say)?;
        self.floor(host)?;
        let live = self.live_disk();
        let record = self.record();
        let reclone = match (&record, live.is_file()) {
            (_, false) => Some(Reclone::NoLiveDisk),
            (None, true) => Some(Reclone::NoRecord),
            (Some(r), true) if r.base_sha256 != base.sha256 => Some(Reclone::Base {
                was: r.base_sha256.clone(),
            }),
            (Some(_), true) => None,
        };
        let next = match reclone {
            None => Prepared::Live {
                disk: live,
                cloned: None,
            },
            Some(why) => {
                let baked = self.baked_dir().join(&want);
                if baked.is_file() {
                    self.clone_live(host, &baked, base, seed)?;
                    say(&format!(
                        "cloned {} from {}: {why}",
                        live.display(),
                        baked.display()
                    ));
                    Prepared::Live {
                        disk: live,
                        cloned: Some(why),
                    }
                } else {
                    Prepared::Bake(self.begin_bake(host, base, &want)?)
                }
            }
        };
        self.reclaim_baked(&want, say)?;
        Ok(next)
    }

    /// The negative controls' defective bring-ups ([`crate::fault`]), where no live disk is
    /// kept: the live disk cloned from a bake's partial file, or the sealed baked disk handed
    /// out to attach writable.
    #[cfg(any(test, debug_assertions))]
    fn planted(&self, host: &dyn Host, base: &Base, seed: &str) -> Result<Option<Prepared>, Error> {
        use crate::fault::{Fault, armed};
        let live = self.live_disk();
        if live.is_file() {
            return Ok(None);
        }
        let want = baked_name(&base.sha256, seed);
        let stem = want.strip_suffix(".img").unwrap_or(&want);
        let partial = self.baked_dir().join(format!("{stem}{PARTIAL}"));
        if armed(Fault::ClonePartial) && partial.is_file() {
            self.clone_live(host, &partial, base, seed)?;
            return Ok(Some(Prepared::Live {
                disk: live,
                cloned: Some(Reclone::NoLiveDisk),
            }));
        }
        let baked = self.baked_dir().join(&want);
        if armed(Fault::AttachBaked) && baked.is_file() {
            return Ok(Some(Prepared::Live {
                disk: baked,
                cloned: None,
            }));
        }
        Ok(None)
    }

    /// Remove every sealed baked disk that is neither `want` nor the one the live record
    /// names.
    fn reclaim_baked(&self, want: &str, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        let named = self.record().map(|r| r.baked);
        for old in files(&self.baked_dir(), |name| {
            name.ends_with(".img")
                && !name.ends_with(PARTIAL)
                && name != want
                && Some(name) != named.as_deref()
        }) {
            std::fs::remove_file(&old).map_err(|e| io("remove", &old, &e))?;
            say(&format!(
                "removed {}: no live record names it, and it is not this pin's",
                old.display()
            ));
        }
        Ok(())
    }

    /// Refuse below [`crate::space::BRING_UP_FLOOR`], naming the figure and the volume.
    fn floor(&self, host: &dyn Host) -> Result<(), Error> {
        let free = host.free(&self.root).map_err(Error::Io)?;
        match crate::space::below_bring_up(&free) {
            Some(why) => Err(Error::Floor(why)),
            None => Ok(()),
        }
    }

    /// Clone `base` to `baked/<want minus .img>.partial.img`, writable, grown sparse to the
    /// disk size.
    fn begin_bake(&self, host: &dyn Host, base: &Base, want: &str) -> Result<PartialBake, Error> {
        let len = std::fs::metadata(&base.path)
            .map_err(|e| Error::Input(format!("{}: {e}", base.path.display())))?
            .len();
        if len > self.disk_bytes {
            return Err(Error::Input(format!(
                "{} is {len} bytes, more than the {}-byte disk it would grow to",
                base.path.display(),
                self.disk_bytes
            )));
        }
        let stem = want.strip_suffix(".img").unwrap_or(want);
        let partial = self.baked_dir().join(format!("{stem}{PARTIAL}"));
        host.clone_file(&base.path, &partial)
            .map_err(|e| io("clone the base to", &partial, &e))?;
        crate::writable(&partial)?;
        std::fs::File::options()
            .write(true)
            .open(&partial)
            .and_then(|f| f.set_len(self.disk_bytes))
            .map_err(|e| io("grow", &partial, &e))?;
        Ok(PartialBake {
            partial,
            sealed: self.baked_dir().join(want),
        })
    }

    /// Clone the sealed `baked` to `live/aterm-linux.partial.img`, writable, rename it over
    /// the live disk, then write its record by rename.
    fn clone_live(
        &self,
        host: &dyn Host,
        baked: &Path,
        base: &Base,
        seed: &str,
    ) -> Result<(), Error> {
        let live = self.live_disk();
        let partial = self.live_dir().join(format!("{VM}{PARTIAL}"));
        host.clone_file(baked, &partial)
            .map_err(|e| io("clone the baked disk to", &partial, &e))?;
        crate::writable(&partial)?;
        std::fs::rename(&partial, &live).map_err(|e| io("place", &live, &e))?;
        let record = Record {
            baked: baked_name(&base.sha256, seed),
            base_sha256: base.sha256.clone(),
            seed: seed.to_string(),
            cloned: aterm_time::unix_secs().unwrap_or(0),
        };
        let path = self.record_path();
        let staged = self.live_dir().join(format!(".{VM}.toml.partial"));
        write_durably(&staged, &record.text())?;
        std::fs::rename(&staged, &path).map_err(|e| io("place", &path, &e))?;
        aterm_durable::durable_dir(&self.live_dir()).map_err(|e| io("flush", &self.live_dir(), &e))
    }

    /// The seed ISO `seed/<fingerprint>.iso`, made in a scratch directory and renamed into
    /// place where it is not there yet. Every other entry under `seed/` is removed.
    ///
    /// # Errors
    /// [`Error::Io`]: the files or the ISO could not be made or placed.
    pub fn seed_iso(&self, host: &dyn Host, seed: &Seed) -> Result<PathBuf, Error> {
        let dir = self.seed_dir();
        std::fs::create_dir_all(&dir).map_err(|e| io("create", &dir, &e))?;
        let name = format!("{}.iso", seed.fingerprint());
        let iso = dir.join(&name);
        if !iso.is_file() {
            let scratch = aterm_tempfile::Builder::new()
                .prefix(".partial-")
                .tempdir_in(&dir)
                .map_err(|e| io("create a scratch in", &dir, &e))?;
            let files = scratch.path().join("files");
            std::fs::create_dir(&files).map_err(|e| io("create", &files, &e))?;
            seed.write_to(&files)
                .map_err(|e| io("write the seed in", &files, &e))?;
            let made = scratch.path().join("seed.iso");
            host.make_iso(&files, &made).map_err(Error::Io)?;
            std::fs::rename(&made, &iso).map_err(|e| io("place", &iso, &e))?;
        }
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_name() != name.as_str() {
                let path = entry.path();
                let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            }
        }
        Ok(iso)
    }
}

/// Write `text` to the new file `path`, flushed to stable media.
fn write_durably(path: &Path, text: &str) -> Result<(), Error> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io("create", path, &e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| io("write", path, &e))?;
    aterm_durable::durable(&file).map_err(|e| io("flush", path, &e))
}

/// The regular files in `dir` whose names `keep` takes, sorted; none where it cannot be read.
fn files(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.file_name().to_str().is_some_and(&keep))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

#[cfg(all(test, unix))]
mod tests;

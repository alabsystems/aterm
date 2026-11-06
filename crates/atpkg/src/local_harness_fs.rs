// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Descriptor custody for local publication. Failed attempts retain their scratch; no cleanup
//! resolves a potentially replaced ancestor, and no executable is run here.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use aterm_dirfd::{AtFlags, FileType, Mode, OFlags};

use super::{Executable, ExpectedCurrent, invalid};

pub(super) fn directory(parent: &File, name: &OsStr) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        aterm_dirfd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}
pub(super) fn private(file: &File) -> io::Result<()> {
    owned_verdict(file, astream_sys::owner_private_dir(file)?).map(|_| ())
}
fn owned_verdict(
    file: &File,
    verdict: Result<std::fs::Metadata, astream_sys::NotPrivate>,
) -> io::Result<std::fs::Metadata> {
    match verdict {
        Ok(meta) => Ok(meta),
        // The descriptor judge checks ACL grants before Mode: read/search bits are compatible.
        Err(astream_sys::NotPrivate::Mode(mode)) if mode & 0o022 == 0 => file.metadata(),
        Err(why) => Err(invalid(format!("local custody {why}"))),
    }
}
pub(crate) fn owned_file(file: &File) -> io::Result<std::fs::Metadata> {
    owned_verdict(file, astream_sys::owner_private(file)?)
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino()))
}
pub(super) fn regular_with_meta(
    parent: &File,
    name: &OsStr,
) -> io::Result<(File, std::fs::Metadata)> {
    let f = File::from(
        aterm_dirfd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let meta = owned_file(&f)?;
    Ok((f, meta))
}
pub(super) fn regular(parent: &File, name: &OsStr) -> io::Result<File> {
    regular_with_meta(parent, name).map(|(f, _)| f)
}
fn sync_dir(file: &File) -> io::Result<()> {
    aterm_durable::refusal_accepted(aterm_durable::primitive::fsync(file))
}

/// The prefix chain, store and program are retained independently of their names.
pub(crate) struct Parent {
    chain: Vec<(OsString, File)>,
    prefix: usize,
    lock_identity: (u64, u64),
    program: String,
}

impl Parent {
    pub(crate) fn open(
        lock: &crate::lock::StoreLock,
        program: &str,
        create: bool,
    ) -> io::Result<Self> {
        Self::open_optional(lock, program, create)?.ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    /// A missing store/program component is absence only after proving its held parent.
    pub(crate) fn open_optional(
        lock: &crate::lock::StoreLock,
        program: &str,
        create: bool,
    ) -> io::Result<Option<Self>> {
        let layout = lock.checked_layout()?;
        if crate::store::ToolName::new(program).is_none()
            || !layout.prefix.is_absolute()
            || layout.prefix.components().count() > 56
        {
            return Err(invalid("local store prefix must be absolute and bounded"));
        }
        let root = aterm_uds::kept_out(|| {
            aterm_dirfd::openat(
                aterm_dirfd::CWD,
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(io::Error::from)
        })?;
        let mut chain = vec![(OsString::from("/"), root)];
        for component in layout.prefix.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let next = directory(&chain.last().unwrap().1, name)?;
                    chain.push((name.into(), next));
                }
                _ => {
                    return Err(invalid(
                        "local store prefix contains noncanonical components",
                    ));
                }
            }
        }
        let prefix = chain.len() - 1;
        private(&chain[prefix].1)?;
        lock.checked_layout_at(&chain[prefix].1)?;
        let lock_identity = identity(&regular(&chain[prefix].1, OsStr::new("store.lock"))?)?;
        let mut parent = Self {
            chain,
            prefix,
            lock_identity,
            program: program.into(),
        };
        for name in ["store", program] {
            parent.reprove()?;
            let held = &parent.chain.last().unwrap().1;
            if create {
                match aterm_dirfd::mkdirat(held, name, Mode::from_bits(0o755))
                    .map_err(io::Error::from)
                {
                    Ok(()) => sync_dir(held)?,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
            }
            let next = match directory(held, OsStr::new(name)) {
                Err(e) if !create && e.kind() == io::ErrorKind::NotFound => {
                    parent.reprove()?;
                    return Ok(None);
                }
                other => other?,
            };
            private(&next)?;
            parent.chain.push((name.into(), next));
        }
        parent.reprove()?;
        Ok(Some(parent))
    }
    pub(crate) fn held(&self) -> &File {
        &self.chain.last().unwrap().1
    }
    pub(crate) fn reprove(&self) -> io::Result<()> {
        for pair in self.chain.windows(2) {
            let named = aterm_dirfd::fstatat(&pair[0].1, &pair[1].0, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            if !FileType::from_raw_mode(named.st_mode).is_dir()
                || (named.st_dev, named.st_ino) != identity(&pair[1].1)?
            {
                return Err(invalid("local store ancestor changed"));
            }
        }
        for (_, file) in &self.chain[self.prefix..] {
            private(file)?;
        }
        if identity(&regular(
            &self.chain[self.prefix].1,
            OsStr::new("store.lock"),
        )?)? != self.lock_identity
        {
            return Err(invalid("local store lock name changed"));
        }
        Ok(())
    }
    pub(crate) fn sync(&self) -> io::Result<()> {
        sync_dir(self.held())
    }
    pub(super) fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        let (mut file, meta) = regular_with_meta(self.held(), OsStr::new(name))?;
        if meta.len() > max as u64 {
            return Err(invalid("local metadata is oversized"));
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
        std::io::Read::by_ref(&mut file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > max || owned_file(&file)?.len() != bytes.len() as u64 {
            return Err(invalid("local metadata grew past its bound"));
        }
        Ok(bytes)
    }
    pub(crate) fn current(&self, layout: &crate::store::Layout) -> io::Result<ExpectedCurrent> {
        let stat = match aterm_dirfd::fstatat(self.held(), "current", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io::Error::from)
        {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ExpectedCurrent::Absent),
            other => other?,
        };
        // POSIX file-type mode bits, kept independent of each platform's mode_t width.
        if stat.st_mode & 0o170000 != 0o120000 {
            return Err(invalid("current is not a managed link"));
        }
        let name = cstr(OsStr::new("current"))?;
        let mut bytes = vec![0u8; 4097];
        // SAFETY: the held directory, NUL-terminated name and writable bounded buffer outlive readlinkat.
        let n = unsafe {
            libc::readlinkat(
                self.held().as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = usize::try_from(n).map_err(|_| invalid("current target length"))?;
        if n >= bytes.len() {
            return Err(invalid("current target is oversized"));
        }
        bytes.truncate(n);
        let target = PathBuf::from(OsString::from_vec(bytes));
        let (owner, build) = crate::ops::store_build_of(&layout.prefix, &target)
            .ok_or_else(|| invalid("current is not a store build"))?;
        if owner != self.program || target != layout.build_dir(&owner, build) {
            return Err(invalid("current is not this program's exact store build"));
        }
        Ok(ExpectedCurrent::Build(build))
    }
    pub(crate) fn select(&self, target: &Path) -> io::Result<()> {
        self.reprove()?;
        let tmp = unique("current");
        let target = cstr(target.as_os_str())?;
        let name = cstr(OsStr::new(&tmp))?;
        // SAFETY: the target and relative name are NUL-terminated and the retained parent stays open.
        if unsafe { libc::symlinkat(target.as_ptr(), self.held().as_raw_fd(), name.as_ptr()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.reprove()?;
        checkpoint("select-rename");
        aterm_dirfd::renameat(self.held(), &tmp, self.held(), "current")
            .map_err(io::Error::from)?;
        self.sync()?;
        self.reprove()
    }
}

fn cstr(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| invalid("local name contains NUL"))
}
fn unique(stem: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        ".{stem}.local-tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Incoming payload and all metadata publication retain the very same program directory.
pub(crate) struct Stage<'a> {
    parent: &'a Parent,
    name: String,
    incoming: String,
    root: File,
    bin: File,
    pub(crate) file: File,
    command: String,
    expected: Executable,
    published: std::cell::Cell<bool>,
}
impl<'a> Stage<'a> {
    pub(crate) fn new(
        parent: &'a Parent,
        build_dir: &Path,
        artifact: &crate::Artifact,
    ) -> io::Result<Self> {
        if crate::store::ToolName::new(&artifact.entry).is_none() {
            return Err(invalid("invalid local executable name"));
        }
        let name = build_dir
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("local build has no name"))?
            .to_owned();
        let incoming = format!("{name}.incoming-{}", std::process::id());
        checkpoint("create");
        parent.reprove()?;
        aterm_dirfd::mkdirat(parent.held(), &incoming, Mode::from_bits(0o700))
            .map_err(io::Error::from)?;
        let root = directory(parent.held(), OsStr::new(&incoming))?;
        private(&root)?;
        aterm_dirfd::mkdirat(&root, "bin", Mode::from_bits(0o755)).map_err(io::Error::from)?;
        let bin = directory(&root, OsStr::new("bin"))?;
        private(&bin)?;
        let file = File::from(
            aterm_dirfd::openat(
                &bin,
                &artifact.entry,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::from_bits(0o755),
            )
            .map_err(io::Error::from)?,
        );
        owned_file(&file)?;
        Ok(Self {
            parent,
            name,
            incoming,
            root,
            bin,
            file,
            command: artifact.entry.clone(),
            expected: Executable {
                path: format!("bin/{}", artifact.entry),
                size: artifact.size,
                sha256: artifact.sha256.clone(),
            },
            published: std::cell::Cell::new(false),
        })
    }
    fn reprove(&self) -> io::Result<()> {
        self.parent.reprove()?;
        private(&self.root)?;
        private(&self.bin)?;
        owned_file(&self.file)?;
        let name = if self.published.get() {
            &self.name
        } else {
            &self.incoming
        };
        for (parent, name, file) in [
            (self.parent.held(), name.as_str(), &self.root),
            (&self.root, "bin", &self.bin),
            (&self.bin, self.command.as_str(), &self.file),
        ] {
            let named = aterm_dirfd::fstatat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            if (named.st_dev, named.st_ino) != identity(file)? {
                return Err(invalid("local payload name changed"));
            }
        }
        Ok(())
    }
    pub(crate) fn seal_sync(&self) -> io::Result<()> {
        self.reprove()?;
        super::matches_open_file(&self.file, &self.expected)?;
        let mut head = [0u8; 8];
        let mut file = self.file.try_clone()?;
        file.rewind()?;
        let n = file.read(&mut head)?;
        if !crate::macho::is_native_executable(&head[..n]) {
            return Err(invalid("local harness is not a native executable format"));
        }
        aterm_dirfd::fchmod(&self.root, Mode::from_bits(0o755)).map_err(io::Error::from)?;
        aterm_dirfd::fchmod(&self.bin, Mode::from_bits(0o555)).map_err(io::Error::from)?;
        crate::store::sync_contents_or_accept_refusal(&self.file)?;
        sync_dir(&self.bin)?;
        sync_dir(&self.root)
    }
    pub(crate) fn publish(&self) -> io::Result<()> {
        checkpoint("publish");
        self.reprove()?;
        super::matches_open_file(&self.file, &self.expected)?;
        let named = directory(self.parent.held(), OsStr::new(&self.incoming))?;
        if identity(&named)? != identity(&self.root)? {
            return Err(invalid("incoming payload changed"));
        }
        checkpoint("publish-rename");
        aterm_dirfd::renameat_with(
            self.parent.held(),
            &self.incoming,
            self.parent.held(),
            &self.name,
            aterm_dirfd::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        self.published.set(true);
        Ok(())
    }
    pub(crate) fn sidecar(&self, suffix: &str, bytes: &[u8]) -> io::Result<()> {
        self.reprove()?;
        let dest = format!("{}{suffix}", self.name);
        let tmp = unique(&dest);
        let mut file = File::from(
            aterm_dirfd::openat(
                self.parent.held(),
                &tmp,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::from_bits(0o644),
            )
            .map_err(io::Error::from)?,
        );
        owned_file(&file)?;
        file.write_all(bytes)?;
        crate::store::sync_contents_or_accept_refusal(&file)?;
        owned_file(&file)?;
        self.reprove()?;
        aterm_dirfd::renameat_with(
            self.parent.held(),
            &tmp,
            self.parent.held(),
            &dest,
            aterm_dirfd::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
    }
    pub(crate) fn metadata_checkpoint(&self) {
        checkpoint("metadata");
    }
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.parent.sync()
    }
    pub(crate) fn ready(&self) -> io::Result<()> {
        let body = crate::store::ready_body_with_inventory(Some(&format!("bin:{}", self.command)));
        self.sidecar(".ready", body.as_bytes())?;
        self.parent.sync()?;
        self.parent.reprove()
    }
}

pub(super) fn checkpoint(phase: &str) {
    #[cfg(test)]
    let hook = CHECKPOINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(at, _)| *at == phase) {
            slot.take().map(|(_, hook)| hook)
        } else {
            None
        }
    });
    #[cfg(test)]
    if let Some(hook) = hook {
        hook();
    }
    #[cfg(not(test))]
    let _ = phase;
}
#[cfg(test)]
type CheckpointHook = Option<(&'static str, Box<dyn FnOnce()>)>;
#[cfg(test)]
thread_local! {
    pub(super) static CHECKPOINT:std::cell::RefCell<CheckpointHook>=const{std::cell::RefCell::new(None)};
}

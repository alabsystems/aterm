// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable executable provenance for native resource hosts. Records confer no admission authority.

use aterm_uds::{
    CtlListener, private_file,
    process::{self, Birth, LoadedImage},
};
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{MetadataExt, PermissionsExt},
    io::AsRawFd,
};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use trust_resource_provenance::schema::{Book, FileId, Record};

const MAX_JSON: usize = 1024 * 1024;
const MAX_IMAGE: u64 = 1024 * 1024 * 1024;
const MAX_INSTANCES: usize = 128;

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}
fn private_error(e: private_file::PrivateFileError) -> io::Error {
    io::Error::other(e)
}

/// A store opened at an explicit, already authenticated account path.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    identity: FileId,
    images: FileId,
    instances: FileId,
    version: FileId,
    lock_id: FileId,
}

/// A read-only, SHA-256-addressed executable. Its pathname alone is never provenance.
#[derive(Clone, Debug)]
pub struct Program {
    store: Store,
    path: PathBuf,
    digest: String,
    identity: FileId,
    directory: FileId,
}

#[derive(Clone, Copy)]
enum Lifetime {
    Absent,
    Stable(Birth),
    Unknown,
}
fn native_lifetime(pid: u32) -> Lifetime {
    let Ok(pid) = i32::try_from(pid) else {
        return Lifetime::Unknown;
    };
    let before = process::read_identity(pid);
    let after = process::read_identity(pid);
    if let Some(identity) = before.filter(|identity| Some(*identity) == after) {
        return Lifetime::Stable(identity.birth);
    }
    if process::process_presence(pid) == process::ProcessPresence::Absent {
        Lifetime::Absent
    } else {
        Lifetime::Unknown
    }
}

// Provenance garbage collection only. This neither settles resource debt nor proves
// descendant/group closure, and never deletes executable images or endpoint paths.
fn prune_records(
    records: &mut Vec<Record>,
    host: &str,
    boot: &str,
    mut observe: impl FnMut(u32) -> Lifetime,
) {
    records.retain(|r| {
        if r.host != host {
            return true;
        }
        if r.boot != boot {
            return false;
        }
        match observe(r.pid) {
            Lifetime::Absent => false,
            Lifetime::Stable(birth) => birth.0 == r.birth,
            Lifetime::Unknown => true,
        }
    });
}

/// A native process, immutable image and bound socket observed together.
#[derive(Clone, Debug)]
pub struct Instance {
    program: Program,
    socket_path: PathBuf,
    record: Record,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}
fn unhex(text: &str) -> io::Result<Vec<u8>> {
    if text.len() > 32768 || !text.len().is_multiple_of(2) {
        return Err(refused("invalid bounded path hex"));
    }
    fn nibble(b: u8) -> io::Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(refused("noncanonical hex")),
        }
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| Ok(nibble(b[0])? * 16 + nibble(b[1])?))
        .collect()
}
fn digest_shaped(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Refuse symlinked ancestors too: final-component nofollow alone would admit redirected stores.
fn plain_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(refused("absolute path required"));
    }
    let mut walked = PathBuf::new();
    for part in path.components() {
        if !matches!(part, Component::RootDir | Component::Normal(_)) {
            return Err(refused("noncanonical path"));
        }
        walked.push(part);
        let m = fs::symlink_metadata(&walked)?;
        if m.file_type().is_symlink() {
            return Err(refused("symlinked path"));
        }
    }
    Ok(())
}
fn directory(path: &Path) -> io::Result<FileId> {
    plain_path(path)?;
    private_file::check_private_dir(path)?;
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.mode() & 0o7000 != 0 {
        return Err(refused("invalid private directory"));
    }
    Ok(FileId::of_metadata(&m))
}
fn stamp(m: &Metadata) -> (FileId, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        FileId::of_metadata(m),
        m.len(),
        m.nlink(),
        m.mode(),
        u64::from(m.uid()),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn hash_file(file: &mut File, before: &Metadata) -> io::Result<String> {
    if !before.is_file() || before.len() == 0 || before.len() > MAX_IMAGE {
        return Err(refused("invalid executable size or type"));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut buf = [0; 65536];
    let mut total = 0_u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| refused("image overflow"))?;
        if total > MAX_IMAGE {
            return Err(refused("image exceeds bound"));
        }
        hash.update(&buf[..n]);
    }
    if total != before.len() || stamp(before) != stamp(&file.metadata()?) {
        return Err(refused("image changed during read"));
    }
    Ok(hex(&hash.finalize()))
}
fn host_boot() -> io::Result<(String, String)> {
    let host = process::host_id()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| refused("native host identity unavailable"))?;
    let boot = process::boot_id()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| refused("native boot identity unavailable"))?;
    Ok((host, boot))
}
fn own_identity() -> io::Result<process::Identity> {
    process::read_identity(std::process::id() as i32)
        .ok_or_else(|| refused("native own identity unavailable"))
}
fn loaded(pid: u32, birth: Birth) -> io::Result<LoadedImage> {
    let native = i32::try_from(pid).map_err(|_| refused("invalid pid"))?;
    let node = process::read_lineage(native)
        .ok_or_else(|| refused("same-user live process unavailable"))?;
    if node.identity.birth != birth || birth.0 == 0 {
        return Err(refused("process birth changed"));
    }
    let image = LoadedImage::read(native).ok_or_else(|| refused("kernel image unavailable"))?;
    if process::read_identity(native) != Some(node.identity) {
        return Err(refused("process changed during kernel image read"));
    }
    Ok(image)
}

fn cached_image(pid: u32, birth: Birth, path: &Path) -> io::Result<bool> {
    let image = loaded(pid, birth)?;
    // Darwin's public loaded-image witness supplies an inode without its
    // device. Require the kernel pathname as well so equal inode numbers on
    // different volumes cannot impersonate the immutable cached executable.
    Ok(image.matches_file(path) == Some(true)
        && image.launch_path().canonicalize()? == path.canonicalize()?)
}

impl Store {
    /// Account-database home; environment variables supply no authority.
    /// # Errors
    /// The native account database supplies no home directory.
    pub fn account_path() -> io::Result<PathBuf> {
        let home = aterm_uds::account::home().ok_or_else(|| refused("account home unavailable"))?;
        #[cfg(target_os = "macos")]
        let suffix = "Library/Caches/aterm/resource-host-v3.noindex";
        #[cfg(not(target_os = "macos"))]
        let suffix = ".cache/aterm/resource-host-v3.noindex";
        Ok(home.join(suffix))
    }

    /// Explicit bootstrap only. Existing partial or malformed state is never repaired.
    /// # Errors
    /// Parent, private-state, atomic publication or durability validation fails.
    pub fn prepare_at(path: &Path) -> io::Result<Self> {
        if fs::symlink_metadata(path).is_ok() {
            return Self::open_at(path);
        }
        let parent = path
            .parent()
            .ok_or_else(|| refused("store parent missing"))?;
        plain_path(parent)?;
        let staged = aterm_tempfile::Builder::new()
            .prefix("host-store-")
            .tempdir_in(parent)?;
        fs::set_permissions(staged.path(), fs::Permissions::from_mode(0o700))?;
        for name in ["images", "instances"] {
            private_file::ensure_private_dir(&staged.path().join(name))?;
        }
        aterm_uds::atomic_file::write(
            &staged.path().join("version.json"),
            b"{\"version\":1}",
            0o400,
        )?;
        aterm_uds::atomic_file::write(
            &staged.path().join("instances/book.json"),
            b"{\"version\":1,\"instances\":[]}",
            0o600,
        )?;
        aterm_uds::atomic_file::write(&staged.path().join("lock"), b"", 0o600)?;
        aterm_durable::durable_dir(staged.path())?;
        aterm_dirfd::renameat_with(
            aterm_dirfd::CWD,
            staged.path(),
            aterm_dirfd::CWD,
            path,
            aterm_dirfd::RenameFlags::NOREPLACE,
        )?;
        aterm_durable::durable_dir(parent)?;
        Self::open_at(path)
    }

    /// Runtime read: creates no directories, files or repairs.
    /// # Errors
    /// State is absent, partial, malformed, inaccessible or fails private-path validation.
    pub fn open_at(path: &Path) -> io::Result<Self> {
        let store = Self {
            root: path.to_path_buf(),
            identity: directory(path)?,
            images: directory(&path.join("images"))?,
            instances: directory(&path.join("instances"))?,
            version: FileId::of_metadata(&fs::symlink_metadata(path.join("version.json"))?),
            lock_id: FileId::of_metadata(&fs::symlink_metadata(path.join("lock"))?),
        };
        store.check()?;
        store.read_book()?;
        Ok(store)
    }
    fn check(&self) -> io::Result<()> {
        if directory(&self.root)? != self.identity {
            return Err(refused("store directory replaced"));
        }
        if directory(&self.root.join("images"))? != self.images
            || directory(&self.root.join("instances"))? != self.instances
            || FileId::of_metadata(&fs::symlink_metadata(self.root.join("version.json"))?)
                != self.version
        {
            return Err(refused("store structure replaced"));
        }
        if private_file::read_private(&self.root, "version.json", 64).map_err(private_error)?
            != b"{\"version\":1}"
        {
            return Err(refused("unsupported store version"));
        }
        let (lock, meta) = private_file::open_private(&self.root, "lock").map_err(private_error)?;
        if FileId::of_metadata(&meta) != self.lock_id || meta.nlink() != 1 || meta.len() != 0 {
            return Err(refused("invalid store lock"));
        }
        drop(lock);
        Ok(())
    }
    fn read_book(&self) -> io::Result<Book> {
        self.check()?;
        let bytes = private_file::read_private(&self.root.join("instances"), "book.json", MAX_JSON)
            .map_err(private_error)?;
        Book::decode(&bytes)
    }
    fn lock(&self) -> io::Result<aterm_uds::file_lock::LockFile> {
        self.check()?;
        let lock = aterm_uds::file_lock::LockFile::take_existing(
            &self.root.join("lock"),
            Instant::now() + Duration::from_secs(2),
            "host executable store busy",
        )?;
        self.check()?;
        Ok(lock)
    }

    /// Cache the actual current kernel image, retaining no authority from argv or PATH.
    /// # Errors
    /// The native image is unavailable or changes, the cache is invalid, or I/O fails.
    pub fn cache_current(&self) -> io::Result<Program> {
        let _lock = self.lock()?;
        let identity = own_identity()?;
        let image = loaded(std::process::id(), identity.birth)?;
        let source = image.launch_path();
        plain_path(source)?;
        // Installed images may be world-readable; they must still be owned and not writable by others.
        let (mut file, before) = aterm_uds::regular_file::open(source)?;
        if !image.matches_metadata(&before)
            || before.mode() & 0o6022 != 0
            || before.mode() & 0o111 == 0
            || before.nlink() != 1
            || (before.uid() != aterm_uds::peer::our_uid() && before.uid() != 0)
        {
            return Err(refused("unsafe loaded executable"));
        }
        let digest = hash_file(&mut file, &before)?;
        if !loaded(std::process::id(), identity.birth)?.matches_metadata(&before)
            || stamp(&before) != stamp(&fs::symlink_metadata(source)?)
        {
            return Err(refused("loaded executable replaced"));
        }
        let destination = self.root.join("images").join(&digest);
        if fs::symlink_metadata(&destination).is_err() {
            let stage = aterm_tempfile::Builder::new()
                .prefix("image-")
                .tempdir_in(self.root.join("images"))?;
            fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
            use std::io::{Seek as _, SeekFrom};
            let staged_image = stage.path().join("aterm");
            if aterm_uds::regular_file::clone_held(&file, &staged_image)? {
                let (cloned, _) = aterm_uds::regular_file::open(&staged_image)?;
                cloned.set_permissions(fs::Permissions::from_mode(0o500))?;
                aterm_durable::durable(&cloned)?;
            } else {
                file.seek(SeekFrom::Start(0))?;
                aterm_uds::atomic_file::write_with(&staged_image, 0o500, |out| {
                    // A writer extending the source cannot turn this second pass into an
                    // unbounded copy, even before the subsequent stat/hash checks reject it.
                    let copied = io::copy(&mut (&mut file).take(before.len() + 1), out)?;
                    if copied != before.len() {
                        return Err(refused("image length changed during copy"));
                    }
                    Ok(())
                })?;
            }
            let (mut copied, meta) =
                private_file::open_private(stage.path(), "aterm").map_err(private_error)?;
            if hash_file(&mut copied, &meta)? != digest
                || stamp(&before) != stamp(&file.metadata()?)
                || !loaded(std::process::id(), identity.birth)?.matches_metadata(&before)
                || stamp(&before) != stamp(&fs::symlink_metadata(source)?)
            {
                return Err(refused("image changed during cache"));
            }
            fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o500))?;
            aterm_durable::durable_dir(stage.path())?;
            aterm_dirfd::renameat_with(
                aterm_dirfd::CWD,
                stage.path(),
                aterm_dirfd::CWD,
                &destination,
                aterm_dirfd::RenameFlags::NOREPLACE,
            )?;
            aterm_durable::durable_dir(&self.root.join("images"))?;
        }
        let path = destination.join("aterm");
        let program = Program {
            store: self.clone(),
            digest,
            identity: FileId::of_metadata(&fs::symlink_metadata(&path)?),
            directory: directory(&destination)?,
            path,
        };
        program.validate()?;
        Ok(program)
    }

    /// Publish only this process executing the exact cached inode, with its actual bound listener.
    /// Call before installing an accept loop; the listener must have no pending clients.
    /// # Errors
    /// Image, process, endpoint, bounded self-connection proof or durable publication fails.
    pub fn publish(&self, program: &Program, listener: &CtlListener) -> io::Result<Instance> {
        let _lock = self.lock()?;
        if self.root != program.store.root || self.identity != program.store.identity {
            return Err(refused("program belongs to another store"));
        }
        program.validate()?;
        let identity = own_identity()?;
        if !cached_image(std::process::id(), identity.birth, program.path())? {
            return Err(refused("host is not executing cached inode"));
        }
        let path = listener
            .local_addr()?
            .as_pathname()
            .ok_or_else(|| refused("unnamed host listener"))?
            .to_path_buf();
        let socket = socket_identity(&path)?;
        prove_listener(listener, &path, identity.birth)?;
        let (host, boot) = host_boot()?;
        let record = Record {
            pid: std::process::id(),
            birth: identity.birth.0,
            host,
            boot,
            digest: program.digest.clone(),
            image: program.identity,
            directory: directory(
                program
                    .path
                    .parent()
                    .ok_or_else(|| refused("image directory missing"))?,
            )?,
            socket_hex: hex(path.as_os_str().as_bytes()),
            socket,
        };
        let instance = Instance {
            program: program.clone(),
            socket_path: path,
            record,
        };
        instance.validate()?;
        let mut book = self.read_book()?;
        prune_records(
            &mut book.instances,
            &instance.record.host,
            &instance.record.boot,
            native_lifetime,
        );
        book.instances.retain(|r| {
            !(r.pid == instance.record.pid
                && r.birth == instance.record.birth
                && r.host == instance.record.host
                && r.boot == instance.record.boot)
        });
        if book.instances.len() >= MAX_INSTANCES {
            return Err(refused("instance book full"));
        }
        book.instances.push(instance.record.clone());
        let bytes = book.encode()?;
        if bytes.len() > MAX_JSON {
            return Err(refused("instance book exceeds bound"));
        }
        aterm_uds::atomic_file::write_checked(
            &self.root.join("instances/book.json"),
            &bytes,
            0o600,
            || instance.validate(),
        )?;
        instance.validate()?;
        Ok(instance)
    }

    /// Decode provenance. Call `Instance::validate` before using a live endpoint.
    /// # Errors
    /// Store or bounded versioned records fail validation.
    pub fn instances(&self) -> io::Result<Vec<Instance>> {
        self.read_book()?
            .instances
            .into_iter()
            .map(|r| {
                let socket_path =
                    PathBuf::from(std::ffi::OsString::from_vec(unhex(&r.socket_hex)?));
                let path = self.root.join("images").join(&r.digest).join("aterm");
                let program = Program {
                    store: self.clone(),
                    path,
                    digest: r.digest.clone(),
                    identity: r.image,
                    directory: r.directory,
                };
                Ok(Instance {
                    program,
                    socket_path,
                    record: r,
                })
            })
            .collect()
    }
}

impl Program {
    /// The cached executable path; call [`Self::validate`] before relying on its identity.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Verify private directories, read-only executable, stable identity and full SHA-256.
    /// # Errors
    /// A private path, identity, mode, ACL, link count, digest or read fails validation.
    pub fn validate(&self) -> io::Result<()> {
        self.store.check()?;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| refused("image directory missing"))?;
        if directory(dir)? != self.directory {
            return Err(refused("image directory replaced"));
        }
        if fs::symlink_metadata(dir)?.mode() & 0o7777 != 0o500
            || !digest_shaped(&self.digest)
            || self.path
                != self
                    .store
                    .root
                    .join("images")
                    .join(&self.digest)
                    .join("aterm")
        {
            return Err(refused("invalid immutable image directory"));
        }
        let (mut file, meta) = private_file::open_private(dir, "aterm").map_err(private_error)?;
        if FileId::of_metadata(&meta) != self.identity
            || meta.nlink() != 1
            || meta.mode() & 0o7777 != 0o500
            || !meta.permissions().readonly()
            || hash_file(&mut file, &meta)? != self.digest
            || stamp(&meta) != stamp(&fs::symlink_metadata(&self.path)?)
            || directory(dir)? != self.directory
        {
            return Err(refused("cached executable changed"));
        }
        self.store.check()
    }
    /// Content equality for an authenticated GUI still executing its original installed inode.
    /// # Errors
    /// Native same-user process identity, pinned image content or cached program differs.
    pub fn validate_process(&self, pid: u32, birth: Birth) -> io::Result<()> {
        self.validate()?;
        let image = loaded(pid, birth)?;
        plain_path(image.launch_path())?;
        let (mut file, meta) = aterm_uds::regular_file::open(image.launch_path())?;
        if !image.matches_metadata(&meta)
            || meta.mode() & 0o6022 != 0
            || meta.nlink() != 1
            || (meta.uid() != aterm_uds::peer::our_uid() && meta.uid() != 0)
            || hash_file(&mut file, &meta)? != self.digest
            || stamp(&meta) != stamp(&fs::symlink_metadata(image.launch_path())?)
            || !loaded(pid, birth)?.matches_metadata(&meta)
        {
            return Err(refused("process image content or identity differs"));
        }
        self.validate()
    }
}
fn socket_identity(path: &Path) -> io::Result<FileId> {
    use std::os::unix::fs::FileTypeExt as _;
    plain_path(path)?;
    let m = fs::symlink_metadata(path)?;
    if !m.file_type().is_socket()
        || m.uid() != aterm_uds::peer::our_uid()
        || m.nlink() != 1
        || m.mode() & 0o7077 != 0
    {
        return Err(refused("unsafe listener endpoint"));
    }
    directory(
        path.parent()
            .ok_or_else(|| refused("socket parent missing"))?,
    )?;
    Ok(FileId::of_metadata(&m))
}

// This publication-only proof consumes one private self-connection, never a client command.
// The caller must publish before installing an accept loop. Restore descriptor flags on every path.
fn prove_listener(listener: &CtlListener, path: &Path, birth: Birth) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut client = aterm_uds::connect::until(path, deadline)?;
    if aterm_uds::fdpass::peer_pid(&client)? != std::process::id()
        || aterm_uds::peer::peer_uid(&client) != Some(aterm_uds::peer::our_uid())
    {
        return Err(refused("listener is not owned by this process"));
    }
    let fd = listener.as_raw_fd();
    // SAFETY: the borrowed listener remains live; F_GETFL neither changes nor takes its ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    struct Restore(i32, i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: the listener outlives this guard; restore the original status flags.
            unsafe {
                libc::fcntl(self.0, libc::F_SETFL, self.1);
            }
        }
    }
    // SAFETY: F_SETFL changes this live descriptor's status, restored by the guard.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let _restore = Restore(fd, flags);
    let (mut accepted, _) = aterm_uds::accept(
        listener,
        aterm_uds::AcceptWait::Ready,
        aterm_uds::AcceptedMode::Nonblocking,
    )?;
    if aterm_uds::fdpass::peer_pid(&accepted)? != std::process::id()
        || aterm_uds::peer::peer_uid(&accepted) != Some(aterm_uds::peer::our_uid())
    {
        return Err(refused("unexpected pending connection"));
    }
    let nonce = aterm_uds::rand::hex_token::<32>()?;
    use std::io::Write as _;
    // Both private endpoints exist before sending the fixed-size challenge. Refuse a
    // blocked/partial transfer immediately rather than renewing a relative socket timeout.
    client.set_nonblocking(true)?;
    if Instant::now() >= deadline {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    client.write_all(nonce.as_bytes())?;
    let mut seen = [0; 64];
    accepted.read_exact(&mut seen)?;
    if Instant::now() >= deadline
        || seen != nonce.as_bytes()
        || process::read_birth(std::process::id() as i32) != Some(birth)
    {
        return Err(refused("listener proof changed"));
    }
    // SAFETY: the borrowed listener is still live. Report a failed explicit restoration.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl Instance {
    /// The recorded native owner PID, subject to [`Self::validate`].
    pub fn pid(&self) -> u32 {
        self.record.pid
    }
    /// The recorded platform-native process birth, subject to [`Self::validate`].
    pub fn birth(&self) -> Birth {
        Birth(self.record.birth)
    }
    /// The instance's cached executable path, subject to [`Self::validate`].
    pub fn program_path(&self) -> &Path {
        self.program.path()
    }
    /// The recorded control socket path, subject to [`Self::validate`].
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
    /// Native identity, loaded cached inode and socket ownership must all remain current.
    /// # Errors
    /// Any native identity or private provenance read differs or is unavailable.
    pub fn validate(&self) -> io::Result<()> {
        self.program.validate()?;
        if host_boot()? != (self.record.host.clone(), self.record.boot.clone())
            || directory(
                self.program
                    .path
                    .parent()
                    .ok_or_else(|| refused("image parent missing"))?,
            )? != self.record.directory
            || socket_identity(&self.socket_path)? != self.record.socket
        {
            return Err(refused("instance provenance changed"));
        }
        if !cached_image(self.pid(), self.birth(), self.program.path())? {
            return Err(refused("instance no longer executes cached inode"));
        }
        let scan = aterm_uds::holders::scan_listener_holders(&self.socket_path, &[])?;
        if !scan.holds(self.pid())
            || socket_identity(&self.socket_path)? != self.record.socket
            || !cached_image(self.pid(), self.birth(), self.program.path())?
        {
            return Err(refused("native listener owner changed"));
        }
        Ok(())
    }
    /// Bounded, reread native ancestry; this observation does not grant resource authority.
    /// # Errors
    /// Instance provenance or the current process identity fails validation.
    pub fn is_ancestor_of_current(&self) -> io::Result<bool> {
        self.validate()?;
        let own = own_identity()?;
        let yes = process::stable_descendant(
            std::process::id(),
            own.birth,
            self.pid(),
            self.birth(),
            Instant::now() + Duration::from_secs(2),
        );
        self.validate()?;
        Ok(yes)
    }
}

/// Test seam: takes group and other write off this process's own image, once, before a test
/// caches it. [`Store::cache_current`] refuses an image others may write, and a linker writes a
/// test binary with the umask's mode: group-writable under a user-private-group login's 002,
/// where an installed image is 0755. The cache stamps the image with its mode and change time,
/// so this runs before any cache in the process and leaves an image already narrow untouched.
///
/// # Panics
/// Panics if this process's image cannot be read or narrowed.
#[cfg(any(test, debug_assertions))]
pub fn narrow_test_image() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let exe = std::env::current_exe().expect("this test binary's path");
        let mode = fs::metadata(&exe).expect("this test binary's mode").mode();
        if mode & 0o022 != 0 {
            fs::set_permissions(&exe, fs::Permissions::from_mode(mode & 0o7777 & !0o022))
                .expect("narrow this test binary's mode");
        }
    });
}

#[cfg(test)]
#[path = "host_executable/tests.rs"]
mod tests;

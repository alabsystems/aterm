// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Physical isolation of cohort claims from readers that reap v1 files before decoding.
//!
//! The private constructor prepares a new domain, exchanges its empty old ledger directory
//! with a regular-file fence, then publishes readiness only after checking the retained old
//! inode. It never moves an existing user ledger or authorizes a global frontend migration.
//! Every validation checks physical identities and retained predecessor entries again: an old
//! reader holding the directory across the exchange can still write to that retained inode.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use aterm_dirfd::{self as fd, AsFd, FileType, Mode, OFlags, RenameFlags};
use aterm_json::Value;

const OLD: &str = "capacity";
const BACKEND: &str = "capacity-v4.noindex";
const ACTIVE: &str = "active.noindex";
const RETIRED: &str = "retired.noindex";
const PRIOR: &str = "prior";
const HEADER: &str = ".capacity-namespace.json";
const PENDING: &str = ".ready.pending";
const READY: &str = ".ready";
const MAX: u64 = 8192;

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inode(u64, u64);

impl Inode {
    fn read(file: &File, directory: bool) -> io::Result<Self> {
        let stat = fd::fstat(file)?;
        let kind = FileType::from_raw_mode(stat.st_mode);
        // SAFETY: geteuid takes no arguments and reads this process's effective identity.
        let uid = unsafe { libc::geteuid() };
        if stat.st_uid != uid
            || stat.st_mode & 0o077 != 0
            || (directory && !kind.is_dir())
            || (!directory && (!kind.is_file() || stat.st_nlink != 1))
        {
            return Err(refused(
                "cohort namespace entry is not private, owned and of the expected kind",
            ));
        }
        Ok(Self(stat.st_dev, stat.st_ino))
    }

    fn encode(&self) -> Value {
        aterm_json::json!([self.0, self.1])
    }

    fn decode(value: &Value) -> Option<Self> {
        let pair = value.as_array()?;
        (pair.len() == 2).then_some(())?;
        Some(Self(pair[0].as_u64()?, pair[1].as_u64()?))
    }

    fn check(&self, file: &File, directory: bool) -> io::Result<()> {
        if &Self::read(file, directory)? != self {
            return Err(refused("cohort namespace entry changed physical identity"));
        }
        Ok(())
    }
}

/// An explicitly created private cohort domain. Decoding its files supplies observations,
/// never authority to migrate the account's normal ledger or to release a reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    root: PathBuf,
    home: PathBuf,
    host: String,
    nonce: String,
    // Ledger identities followed by the original host-lock inode. Only leadership
    // validates that last inode: losing leadership must not invalidate child custody.
    identities: Vec<Inode>,
}

impl Namespace {
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }

    pub(crate) fn open_backend(&self) -> io::Result<File> {
        self.validate()?;
        let backend = directory(&self.backend())?;
        self.identities[5].check(&backend, true)?;
        Ok(backend)
    }

    pub(crate) fn open_active(&self) -> io::Result<File> {
        let backend = self.open_backend()?;
        let active = child_dir(&backend, ACTIVE)?;
        self.identities[8].check(&active, true)?;
        Ok(active)
    }

    pub(crate) fn open_retired(&self) -> io::Result<File> {
        let backend = self.open_backend()?;
        let retired = child_dir(&backend, RETIRED)?;
        self.identities[9].check(&retired, true)?;
        Ok(retired)
    }

    pub(crate) fn open_host_lock_in(&self, backend: &File) -> io::Result<(File, (u64, u64))> {
        let (lock, inode) = regular_inode(backend, crate::leadership::LOCK)?;
        if inode != self.identities[10] {
            return Err(refused("cohort namespace entry changed physical identity"));
        }
        Ok((lock, (inode.0, inode.1)))
    }
    /// Prepare a fresh domain under an existing private home inside an existing private root.
    /// Neither path may be the account home or contain it. This does not redirect `Ledger::user`.
    /// A partial preparation remains on disk on failure; it is never adopted as an empty ledger.
    ///
    /// # Errors
    /// Paths are not confined, owned and private; scope is unknown; a write, flush, exchange or
    /// identity check fails; or the retained old inode contains any predecessor entry.
    pub fn create_private(private_root: &Path, private_home: &Path) -> io::Result<Self> {
        Self::prepare_private(private_root, private_home, |_| Ok(()))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "PublishNamespaceFence",
            project = "aterm_capacity::ledger::failure_tests::namespace_project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "ProvePredecessorDrain",
            project = "aterm_capacity::ledger::failure_tests::namespace_project"
        )
    )]
    fn prepare_private(
        private_root: &Path,
        private_home: &Path,
        before_exchange: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<Self> {
        let root = private_root.canonicalize()?;
        let home = private_home.canonicalize()?;
        confined(&root, &home)?;
        let root_fd = directory(&root)?;
        let home_fd = directory(&home)?;
        let nonce = aterm_uds::rand::hex_token::<16>()?;
        let name = format!(".capacity-cohort-{nonce}.noindex");
        fd::mkdirat(&home_fd, name.as_str(), Mode::RWXU)?;
        let container = child_dir(&home_fd, &name)?;
        fd::mkdirat(&container, OLD, Mode::RWXU)?;
        fd::mkdirat(&container, BACKEND, Mode::RWXU)?;
        let old = child_dir(&container, OLD)?;
        let backend = child_dir(&container, BACKEND)?;
        fd::mkdirat(&backend, ACTIVE, Mode::RWXU)?;
        fd::mkdirat(&backend, RETIRED, Mode::RWXU)?;
        let active = child_dir(&backend, ACTIVE)?;
        let retired = child_dir(&backend, RETIRED)?;
        let mut fence = create(&container, PRIOR)?;
        let mut header = create(&container, HEADER)?;
        let mut ready = create(&backend, PENDING)?;
        let host_lock = create(&backend, crate::leadership::LOCK)?;
        crate::leadership::prepare(&backend, &nonce)?;
        aterm_durable::durable(&host_lock)?;
        let namespace = Self {
            root: home.join(&name),
            home,
            host: aterm_uds::process::host_id()
                .ok_or_else(|| refused("host identity is unknown"))?,
            nonce,
            identities: vec![
                Inode::read(&root_fd, true)?,
                Inode::read(&home_fd, true)?,
                Inode::read(&container, true)?,
                Inode::read(&old, true)?,
                Inode::read(&fence, false)?,
                Inode::read(&backend, true)?,
                Inode::read(&header, false)?,
                Inode::read(&ready, false)?,
                Inode::read(&active, true)?,
                Inode::read(&retired, true)?,
                Inode::read(&host_lock, false)?,
            ],
        };
        let text = namespace.encode(&root)?;
        for file in [&mut fence, &mut header, &mut ready] {
            file.write_all(text.as_bytes())?;
            aterm_durable::durable(file)?;
        }
        aterm_durable::durable(&old)?;
        aterm_durable::durable(&active)?;
        aterm_durable::durable(&retired)?;
        aterm_durable::durable(&backend)?;
        aterm_durable::durable(&container)?;
        aterm_durable::durable(&home_fd)?;
        before_exchange(&old)?;
        fd::renameat_with(&container, OLD, &container, PRIOR, RenameFlags::EXCHANGE)?;
        aterm_durable::durable(&container)?;
        // Check after the exchange. Holding a turn lock cannot exclude an old force-admit.
        namespace.identities[3].check(&child_dir(&container, PRIOR)?, true)?;
        empty(&old)?;
        fd::renameat_with(&backend, PENDING, &backend, READY, RenameFlags::NOREPLACE)?;
        aterm_durable::durable(&backend)?;
        aterm_durable::durable(&container)?;
        namespace.validate()?;
        Ok(namespace)
    }

    /// The regular-file fence at the former v1 directory name.
    #[must_use]
    pub fn fence(&self) -> PathBuf {
        self.root.join(OLD)
    }

    /// The isolated backend. Only a cohort-aware ledger may read or write claims here.
    #[must_use]
    pub fn backend(&self) -> PathBuf {
        self.root.join(BACKEND)
    }

    pub(crate) fn active(&self) -> PathBuf {
        self.backend().join(ACTIVE)
    }

    #[cfg(test)]
    pub(crate) fn retired(&self) -> PathBuf {
        self.backend().join(RETIRED)
    }

    /// The retained old directory, including writes by readers that held its inode open.
    #[must_use]
    pub fn predecessors(&self) -> PathBuf {
        self.root.join(PRIOR)
    }

    fn encode(&self, private_root: &Path) -> io::Result<String> {
        aterm_json::to_string(&aterm_json::json!({
            "v": 4,
            "kind": "private-cohort",
            "root": path_text(private_root)?,
            "home": path_text(&self.home)?,
            "container": path_text(&self.root)?,
            "host": self.host,
            "nonce": self.nonce,
            "identities": self.identities.iter().map(Inode::encode).collect::<Vec<_>>(),
        }))
        .map_err(|error| refused(&error.to_string()))
    }

    /// Resolve a declared fence or backend. A partial, replaced or unreadable namespace is an
    /// error, never permission to fall back to a fresh v1 ledger. Unmarked ordinary paths return
    /// `None`; this method does not change any process's global ledger.
    ///
    /// # Errors
    /// Protocol evidence is incomplete, malformed, replaced, foreign, or has predecessors.
    pub fn resolve(dir: &Path) -> io::Result<Option<Self>> {
        let canonical = canonical_candidate(dir)?;
        let dir = canonical.as_path();
        let Some(parent) = dir.parent() else {
            return Ok(None);
        };
        // Nested active claims are an explicit shape. A missing/old header beneath the
        // reserved container is never an ordinary legacy ledger, even before record creation.
        let mut container = None;
        for (depth, path) in dir.ancestors().enumerate() {
            if depth >= 128 {
                return Err(refused(
                    "ledger ancestry exceeds the explicit namespace bound",
                ));
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".capacity-cohort-"))
            {
                container = Some(path);
                break;
            }
        }
        let container = container.unwrap_or(parent);
        let header_path = container.join(HEADER);
        let text = match std::fs::symlink_metadata(&header_path) {
            Ok(_) => read(&directory(container)?, HEADER)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let named_backend = dir.file_name().is_some_and(|name| {
                    name == BACKEND || name == "capacity-v3.noindex" || name == "capacity-v2"
                });
                let reserved_parent = container
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".capacity-cohort-"));
                // The fence is a file, so looking for readiness *under* it only gets ENOTDIR.
                // Protocol evidence also survives a lost fence and an interrupted preparation.
                let sibling_evidence = exists(&container.join(BACKEND))?
                    || exists(&container.join("capacity-v3.noindex"))?
                    || exists(&container.join("capacity-v2"))?
                    || exists(&container.join(PRIOR))?;
                if named_backend || reserved_parent || sibling_evidence {
                    return Err(refused("cohort namespace header is missing"));
                }
                match std::fs::symlink_metadata(dir) {
                    Ok(meta) if !meta.file_type().is_dir() => {
                        return Err(refused("ledger path is not an ordinary directory"));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                if exists(&dir.join(READY))? || exists(&dir.join(PENDING))? {
                    return Err(refused("cohort namespace header is missing"));
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let (namespace, _) = Self::decode(&text)?;
        let active = namespace.active();
        let active_claim = dir.parent() == Some(active.as_path())
            && dir.file_name().is_some_and(valid_claim_directory);
        if namespace.root != container
            || (dir != namespace.fence()
                && dir != namespace.backend()
                && dir != active
                && !active_claim)
        {
            return Err(refused(
                "path is not this cohort namespace's declared active ledger shape",
            ));
        }
        namespace.validate()?;
        Ok(Some(namespace))
    }

    fn decode(text: &str) -> io::Result<(Self, PathBuf)> {
        let value: Value =
            aterm_json::from_str(text).map_err(|error| refused(&error.to_string()))?;
        let get = |name| {
            value
                .get(name)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        let build = || {
            (value.as_object()?.len() == 8).then_some(())?;
            (value.get("v")?.as_u64()? == 4 && get("kind")? == "private-cohort").then_some(())?;
            let identities = value
                .get("identities")?
                .as_array()?
                .iter()
                .map(Inode::decode)
                .collect::<Option<Vec<_>>>()?;
            (identities.len() == 11).then_some(())?;
            let nonce = get("nonce")?.to_owned();
            (nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())).then_some(())?;
            let namespace = Self {
                root: PathBuf::from(get("container")?),
                home: PathBuf::from(get("home")?),
                host: get("host")?.to_owned(),
                nonce,
                identities,
            };
            let root = PathBuf::from(get("root")?);
            (root.is_absolute()
                && namespace.home.starts_with(&root)
                && namespace.home != root
                && namespace.root.parent() == Some(namespace.home.as_path())
                && namespace.root.file_name()?.to_str()?
                    == format!(".capacity-cohort-{}.noindex", namespace.nonce))
            .then_some((namespace, root))
        };
        build().ok_or_else(|| refused("invalid cohort namespace header"))
    }

    /// Recheck scope, ledger physical identities, all three matching headers and the
    /// retained predecessor inode. A single predecessor entry prevents new admission.
    /// The separately anchored host-lock identity is checked only by leadership;
    /// its loss must not invalidate the custody of already-owned native children.
    ///
    /// # Errors
    /// Any scope, identity, type, privacy, header, readiness or predecessor check fails.
    pub fn validate(&self) -> io::Result<()> {
        if aterm_uds::process::host_id().as_deref() != Some(self.host.as_str()) {
            return Err(refused(
                "cohort namespace belongs to an unknown or foreign host",
            ));
        }
        let container = directory(&self.root)?;
        self.identities[2].check(&container, true)?;
        let (header, header_inode) = regular_inode(&container, HEADER)?;
        let text = read_open(&header)?;
        let (record, private_root) = Self::decode(&text)?;
        confined(&private_root, &self.home)?;
        if &record != self {
            return Err(refused("cohort namespace header changed"));
        }
        self.identities[0].check(&directory(&private_root)?, true)?;
        self.identities[1].check(&directory(&self.home)?, true)?;
        let old = child_dir(&container, PRIOR)?;
        let (fence, fence_inode) = regular_inode(&container, OLD)?;
        let backend = child_dir(&container, BACKEND)?;
        let (ready, ready_inode) = regular_inode(&backend, READY)?;
        let active = child_dir(&backend, ACTIVE)?;
        let retired = child_dir(&backend, RETIRED)?;
        self.identities[3].check(&old, true)?;
        if fence_inode != self.identities[4]
            || header_inode != self.identities[6]
            || ready_inode != self.identities[7]
        {
            return Err(refused("cohort namespace entry changed physical identity"));
        }
        self.identities[5].check(&backend, true)?;
        self.identities[8].check(&active, true)?;
        self.identities[9].check(&retired, true)?;
        if read_open(&fence)? != text || read_open(&ready)? != text {
            return Err(refused("cohort namespace fence and readiness disagree"));
        }
        empty(&old)
    }
}

/// Resolve existing directory aliases before deciding whether a ledger is ordinary. For
/// an absent leaf retain its spelling beneath the nearest actual canonical ancestor; a
/// hidden namespace must not become v1 merely because its offered path used an alias.
fn canonical_candidate(path: &Path) -> io::Result<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(refused("ledger spelling contains parent traversal"));
    }
    let mut suffix = Vec::new();
    let mut probe = path;
    for _ in 0..128 {
        match probe.canonicalize() {
            Ok(mut root) => {
                for part in suffix.iter().rev() {
                    root.push(part)
                }
                return Ok(root);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(probe) {
                    Ok(_) => {
                        return Err(refused(
                            "an existing ledger candidate has an unresolved physical spelling",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                let part = probe
                    .file_name()
                    .ok_or_else(|| refused("ledger has no resolvable canonical ancestor"))?;
                suffix.push(part.to_owned());
                probe = probe
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
            }
            Err(error) => return Err(error),
        }
    }
    Err(refused("ledger canonical ancestry exceeds its bound"))
}

pub(crate) fn valid_claim_directory(name: &std::ffi::OsStr) -> bool {
    let Some((marker, epoch)) = name
        .to_str()
        .and_then(|name| name.strip_suffix(".noindex"))
        .and_then(|name| name.rsplit_once('.'))
    else {
        return false;
    };
    crate::ledger::Record::marker_holder(marker).is_some()
        && epoch.len() == 32
        && epoch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn confined(root: &Path, home: &Path) -> io::Result<()> {
    let account = crate::account_home()
        .ok_or_else(|| refused("account home is unknown"))?
        .canonicalize()?;
    if !root.is_absolute()
        || !home.is_absolute()
        || !home.starts_with(root)
        || home == root
        || root == account
        || account.starts_with(root)
        || home == account
    {
        return Err(refused(
            "private cohort roots cannot replace or contain the account home",
        ));
    }
    Ok(())
}

fn exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_text(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| refused("namespace path is not UTF-8"))
}

fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(refused("cohort namespace path must be absolute"));
    }
    let mut dir = aterm_uds::kept_out(|| {
        fd::openat(fd::CWD, "/", directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(io::Error::from)
    })?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => dir = child_dir(&dir, name)?,
            _ => return Err(refused("cohort namespace path is not canonical")),
        }
    }
    Ok(dir)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn child_dir(parent: &File, name: impl fd::Arg) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        fd::openat(parent, name, directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(io::Error::from)
    })
}

pub(crate) fn regular(parent: &File, name: &str) -> io::Result<(File, (u64, u64))> {
    regular_inode(parent, name).map(|(file, inode)| (file, (inode.0, inode.1)))
}

fn regular_inode(parent: &File, name: &str) -> io::Result<(File, Inode)> {
    let file = aterm_uds::kept_out(|| {
        fd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })?;
    let inode = Inode::read(&file, false)?;
    Ok((file, inode))
}

pub(crate) fn create(parent: &File, name: &str) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        fd::openat(
            parent,
            name,
            OFlags::RDWR
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}

fn read(parent: &File, name: &str) -> io::Result<String> {
    let (file, _) = regular_inode(parent, name)?;
    read_open(&file)
}

fn read_open(mut file: &File) -> io::Result<String> {
    let mut text = String::with_capacity(256);
    (&mut file).take(MAX + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX {
        return Err(refused("cohort namespace header is too large"));
    }
    Ok(text)
}

fn empty(directory: &File) -> io::Result<()> {
    for entry in fd::Dir::read_from(directory.as_fd())? {
        let entry = entry?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "retained predecessor inode is not empty; its work is not proven drained",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare() -> (aterm_tempfile::TempDir, Namespace) {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir(&home).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        (scratch, namespace)
    }

    #[test]
    fn old_reader_hits_a_file_and_late_predecessor_writes_block() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        fd::mkdirat(
            directory(&scratch.path().canonicalize().unwrap()).unwrap(),
            "home",
            Mode::RWXU,
        )
        .unwrap();
        let mut held_old = None;
        let namespace = Namespace::prepare_private(scratch.path(), &home, |old| {
            held_old = Some(old.try_clone()?);
            Ok(())
        })
        .unwrap();
        assert!(std::fs::read_dir(namespace.fence()).is_err());
        assert_eq!(
            Namespace::resolve(&namespace.fence()).unwrap(),
            Some(namespace.clone())
        );
        assert_eq!(
            Namespace::resolve(&namespace.backend()).unwrap(),
            Some(namespace.clone())
        );
        let old = held_old.unwrap();
        let held_identity = Inode::read(&old, true).unwrap();
        assert_eq!(held_identity, namespace.identities[3]);
        // This descriptor still reaches the old inode after the directory-name fence.
        let _late = create(&old, "late.claim").unwrap();
        assert_eq!(
            namespace.validate().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        fd::unlinkat(&old, "late.claim", fd::AtFlags::empty()).unwrap();
        namespace.validate().unwrap();
    }

    #[test]
    fn replacement_missing_header_and_incomplete_ready_never_mean_empty() {
        let (_scratch, namespace) = prepare();
        let backend = directory(&namespace.backend()).unwrap();
        fd::renameat_with(&backend, READY, &backend, PENDING, RenameFlags::NOREPLACE).unwrap();
        assert!(Namespace::resolve(&namespace.backend()).is_err());
        fd::renameat_with(&backend, PENDING, &backend, READY, RenameFlags::NOREPLACE).unwrap();
        namespace.validate().unwrap();
        let container = directory(&namespace.root).unwrap();
        fd::renameat_with(
            &container,
            HEADER,
            &container,
            "saved-header",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        assert!(Namespace::resolve(&namespace.backend()).is_err());
        fd::renameat_with(
            &container,
            "saved-header",
            &container,
            HEADER,
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        namespace.validate().unwrap();
        fd::renameat_with(
            &container,
            BACKEND,
            &container,
            "saved-backend",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        fd::mkdirat(&container, BACKEND, Mode::RWXU).unwrap();
        assert!(Namespace::resolve(&namespace.backend()).is_err());
    }

    #[test]
    fn headerless_fence_and_pre_header_preparation_cannot_fall_back() {
        let (scratch, namespace) = prepare();
        let container = directory(&namespace.root).unwrap();
        fd::renameat_with(
            &container,
            HEADER,
            &container,
            "saved-header",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        assert!(
            Namespace::resolve(&namespace.fence()).is_err(),
            "a missing header must not return the ordinary-ledger result for the fence"
        );
        fd::renameat_with(
            &container,
            OLD,
            &container,
            "saved-fence",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        assert!(
            Namespace::resolve(&namespace.fence()).is_err(),
            "a missing fence cannot authorize an empty replacement beside the old backend"
        );

        let partial = namespace
            .home
            .join(".capacity-cohort-00000000000000000000000000000001.noindex");
        fd::mkdirat(
            directory(&namespace.home).unwrap(),
            partial.file_name().unwrap(),
            Mode::RWXU,
        )
        .unwrap();
        let partial_fd = directory(&partial).unwrap();
        fd::mkdirat(&partial_fd, OLD, Mode::RWXU).unwrap();
        fd::mkdirat(&partial_fd, BACKEND, Mode::RWXU).unwrap();
        assert!(
            Namespace::resolve(&partial.join(OLD)).is_err(),
            "pre-header interruption is protocol evidence, not an empty v1 ledger"
        );
        assert_eq!(
            Namespace::resolve(&scratch.path().join("ordinary")).unwrap(),
            None
        );
    }

    #[test]
    fn nested_and_aliased_v3_paths_cannot_become_ordinary_ledgers() {
        let (scratch, namespace) = prepare();
        let key = "0000000000001-1-1.00000000000000000000000000000001.noindex";
        let claim = namespace.active().join(key);
        std::fs::create_dir(&claim).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&claim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = scratch.path().join("ordinary-name");
        std::os::unix::fs::symlink(&claim, &alias).unwrap();
        assert_eq!(Namespace::resolve(&alias).unwrap(), Some(namespace.clone()));
        assert!(Namespace::resolve(&alias.join("unknown/one/two/three/four/five/six")).is_err());
        assert!(Namespace::resolve(&namespace.root).is_err());
        assert!(Namespace::resolve(&namespace.retired().join(key).join("claim.noindex")).is_err());
        let dangling = scratch.path().join("arbitrary-dangling-name");
        std::os::unix::fs::symlink(claim.join("absent"), &dangling).unwrap();
        assert!(
            Namespace::resolve(&dangling).is_err(),
            "an unresolved alias cannot authorize a later v1 walk"
        );
        let container = directory(&namespace.root).unwrap();
        fd::renameat_with(
            &container,
            HEADER,
            &container,
            "saved-header",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        assert!(
            Namespace::resolve(&alias).is_err(),
            "an alias must retain the missing-header refusal"
        );
    }
}

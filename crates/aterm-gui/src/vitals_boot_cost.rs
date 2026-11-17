// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded boot checkpoints persisted above the statistics model. One worker owns each
//! lock/read/merge/replace transaction; a failed write retains the original pending cuts.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_vitals::boot_cost::Book;
#[cfg(unix)]
use aterm_vitals::boot_cost::{MAX_BYTES, decode, encode};

pub(crate) const FILE: &str = "vitals-boot-cost.bin";
#[cfg(unix)]
const LOCK: &str = "vitals-boot-cost.lock";
const LOCK_WAIT: Duration = Duration::from_millis(250);

/// Static operation names keep native error details out of the report projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) operation: &'static str,
    pub(crate) kind: io::ErrorKind,
}

impl Failure {
    fn at(operation: &'static str, error: &io::Error) -> Self {
        Self {
            operation,
            kind: error.kind(),
        }
    }

    fn invalid(operation: &'static str) -> Self {
        Self {
            operation,
            kind: io::ErrorKind::InvalidData,
        }
    }
}

/// Publication and durability are separate: a directory flush may fail after rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "off Unix no worker loads or saves the record, so only its first states are reached"
    )
)]
pub(crate) enum Persistence {
    Unavailable,
    Loading,
    Missing,
    Restored,
    Saved,
    PublishedDurabilityUnknown(Failure),
    Failed(Failure),
}

impl Persistence {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Loading => "loading",
            Self::Missing => "missing",
            Self::Restored => "restored",
            Self::Saved => "saved",
            Self::PublishedDurabilityUnknown(_) => "published_durability_unknown",
            Self::Failed(_) => "failed",
        }
    }
}

/// Known disk records and current native persistence coverage, without an I/O capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreView {
    pub(crate) book: Book,
    pub(crate) persistence: Persistence,
    pub(crate) busy: bool,
    pub(crate) dirty: bool,
}

struct Completed {
    book: Book,
    persistence: Persistence,
}

struct Active {
    receiver: Receiver<Result<Completed, Failure>>,
    sent: Option<Book>,
}

#[derive(Default)]
struct Notice {
    ready: AtomicBool,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Notice {
    fn notify(&self) {
        self.ready.store(true, Ordering::Release);
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// The host polls this outside its sample path. `None` is inert, including in Reader tests.
pub(crate) struct BootStore {
    path: Option<PathBuf>,
    probes: Probes,
    active: Option<Active>,
    pending: Option<Book>,
    notice: Arc<Notice>,
    view: StoreView,
}

impl BootStore {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self::with_probes(path, Probes::NATIVE)
    }

    fn with_probes(path: Option<PathBuf>, probes: Probes) -> Self {
        let mut store = Self {
            path,
            probes,
            active: None,
            pending: None,
            notice: Arc::default(),
            view: StoreView {
                book: Book::default(),
                persistence: Persistence::Unavailable,
                busy: false,
                dirty: false,
            },
        };
        if store.path.is_some() {
            store.view.persistence = Persistence::Loading;
            store.start(None);
        }
        store
    }

    pub(crate) const fn view(&self) -> &StoreView {
        &self.view
    }

    /// Notify the host after a result is queued, including one completed before installation.
    /// The callback supplies only a host wake; this store never reaches a UI platform.
    pub(crate) fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .notice
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&wake));
        if self.notice.ready.load(Ordering::Acquire) {
            wake();
        }
    }

    /// Retain an already captured book. Retries never take a new observation or alter a cut.
    pub(crate) fn submit(&mut self, book: Book) {
        if self.path.is_none() {
            return;
        }
        let mut pending = self.pending.clone().unwrap_or_default();
        if pending.merge(&book).is_err() {
            self.view.persistence = Persistence::Failed(Failure::invalid("merge_pending"));
            return;
        }
        self.pending = Some(pending);
        self.view.dirty = true;
        self.retry();
    }

    /// Retry retained bytes when the host next schedules optional persistence work.
    pub(crate) fn retry(&mut self) {
        if self.active.is_none()
            && let Some(pending) = self.pending.clone()
        {
            self.start(Some(pending));
        }
    }

    /// Consume a completed worker without waiting. Errors leave pending observations intact.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        let result = match active.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(Failure {
                operation: "worker",
                kind: io::ErrorKind::BrokenPipe,
            }),
        };
        let sent = self.active.take().and_then(|active| active.sent);
        self.notice.ready.store(false, Ordering::Release);
        self.view.busy = false;
        match result {
            Ok(done) => {
                let durable =
                    !matches!(done.persistence, Persistence::PublishedDurabilityUnknown(_));
                self.view.book = done.book;
                self.view.persistence = done.persistence;
                if durable && sent.is_some() && self.pending == sent {
                    self.pending = None;
                }
                self.view.dirty = self.pending.is_some();
                // A capture made while the restore/read was running still needs its first save.
                // Failed writes and uncertain durability wait for an explicit host retry.
                if durable && self.pending.is_some() && self.pending != sent {
                    self.retry();
                }
            }
            Err(error) => {
                self.view.persistence = Persistence::Failed(error);
                self.view.dirty = self.pending.is_some();
            }
        }
        true
    }

    fn start(&mut self, incoming: Option<Book>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let probes = self.probes;
        let sent = incoming.clone();
        let notice = Arc::clone(&self.notice);
        self.notice.ready.store(false, Ordering::Release);
        let (sender, receiver) = mpsc::sync_channel(1);
        match crate::qos::spawn_lane_thread(
            "aterm-boot-cost-store",
            crate::qos::Role::Background,
            move || {
                let result = transact(&path, incoming.as_ref(), probes);
                if sender.send(result).is_ok() {
                    notice.notify();
                }
            },
        ) {
            Ok(_worker) => {
                self.active = Some(Active { receiver, sent });
                self.view.busy = true;
            }
            Err(error) => {
                self.view.persistence = Persistence::Failed(Failure::at("start_worker", &error));
                self.view.busy = false;
                self.notice.notify();
            }
        }
    }
}

/// Native observations are injected as fields; headless tests never discover a platform host.
#[derive(Clone, Copy)]
struct Probes {
    file: fn(&std::fs::File) -> io::Result<()>,
    directory: fn(&std::fs::File) -> io::Result<()>,
    lock: fn(std::fs::File) -> io::Result<crate::native_document_host::HeldAdvisoryLock>,
    publish: fn(Publication<'_>) -> io::Result<io::Result<()>>,
}

struct Publication<'a> {
    path: &'a Path,
    bytes: &'a [u8],
    check: &'a mut dyn FnMut() -> io::Result<()>,
    #[cfg(unix)]
    directory: &'a aterm_uds::private_file::PrivateDirectory,
}

impl Probes {
    const NATIVE: Self = Self {
        file: private_file,
        directory: private_directory,
        lock: take_lock,
        publish,
    };
}

fn take_lock(file: std::fs::File) -> io::Result<crate::native_document_host::HeldAdvisoryLock> {
    use crate::native_document_host::{AdvisoryLockRefusal, acquire_advisory_lock_within};
    acquire_advisory_lock_within(file, LOCK_WAIT).map_err(|error| match error {
        AdvisoryLockRefusal::Busy => io::Error::from(io::ErrorKind::WouldBlock),
        AdvisoryLockRefusal::Failed(error) => error,
    })
}

fn publish(publication: Publication<'_>) -> io::Result<io::Result<()>> {
    #[cfg(unix)]
    {
        publication.directory.view()?.write_entry_landed(
            record_name(publication.path)?,
            publication.bytes,
            publication.check,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (publication.path, publication.bytes, publication.check);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(unix)]
fn record_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}

fn private_file(file: &std::fs::File) -> io::Result<()> {
    match astream_sys::owner_private(file)? {
        Ok(_) => Ok(()),
        Err(_) => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
    }
}

fn private_directory(file: &std::fs::File) -> io::Result<()> {
    match astream_sys::owner_private_dir(file)? {
        Ok(_) => Ok(()),
        Err(_) => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
    }
}

fn transact(path: &Path, incoming: Option<&Book>, probes: Probes) -> Result<Completed, Failure> {
    #[cfg(unix)]
    {
        native::transact(path, incoming, probes)
    }
    #[cfg(not(unix))]
    {
        let _ = (
            path,
            incoming,
            probes.file,
            probes.directory,
            probes.lock,
            probes.publish,
        );
        Err(Failure {
            operation: "platform",
            kind: io::ErrorKind::Unsupported,
        })
    }
}

#[cfg(unix)]
mod native {
    use std::fs::{File, Metadata, OpenOptions};
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Component;

    use super::*;

    const MAX_ANCESTORS: usize = 64;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Identity {
        dev: u64,
        ino: u64,
    }

    impl Identity {
        fn of(metadata: &Metadata) -> Self {
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
    }

    pub(super) struct Admission {
        ancestors: Vec<(PathBuf, Identity)>,
        parent: File,
        directory: aterm_uds::private_file::PrivateDirectory,
        lock: crate::native_document_host::HeldAdvisoryLock,
        lock_identity: Identity,
    }

    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    pub(super) fn private_error(error: aterm_uds::private_file::PrivateFileError) -> io::Error {
        use aterm_uds::private_file::PrivateFileError;
        let kind = match error {
            PrivateFileError::Io(error) => return error,
            PrivateFileError::Missing => io::ErrorKind::NotFound,
            PrivateFileError::Malformed(_) => io::ErrorKind::InvalidData,
            PrivateFileError::NotPrivate(_)
            | PrivateFileError::Writable(_)
            | PrivateFileError::Symlink => io::ErrorKind::PermissionDenied,
        };
        io::Error::new(kind, error)
    }

    fn directory(path: &Path) -> io::Result<File> {
        aterm_uds::kept_out(|| {
            OpenOptions::new()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
                .open(path)
        })
    }

    fn trusted_ancestor(file: &File) -> io::Result<Metadata> {
        let metadata = file.metadata()?;
        if !metadata.is_dir() || (metadata.uid() != 0 && metadata.uid() != astream_sys::euid()) {
            return Err(denied());
        }
        let shared_sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if metadata.mode() & 0o022 != 0 && !shared_sticky_root {
            return Err(denied());
        }
        if metadata.uid() == astream_sys::euid() {
            match astream_sys::owner_private_dir(file)? {
                Ok(_) | Err(astream_sys::NotPrivate::Mode(_)) => {}
                Err(_) => return Err(denied()),
            }
        }
        Ok(metadata)
    }

    impl Admission {
        pub(super) fn open(path: &Path, probes: Probes) -> io::Result<Self> {
            if !path.is_absolute() || path.file_name().is_none() {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let parent_path = path.parent().ok_or_else(denied)?;
            if parent_path.components().count() > MAX_ANCESTORS {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let mut at = PathBuf::new();
            let mut ancestors = Vec::new();
            let mut parent = None;
            for component in parent_path.components() {
                match component {
                    Component::RootDir | Component::Normal(_) => at.push(component.as_os_str()),
                    _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
                }
                let file = directory(&at)?;
                ancestors.push((at.clone(), Identity::of(&trusted_ancestor(&file)?)));
                parent = Some(file);
            }
            let parent = parent.ok_or_else(denied)?;
            (probes.directory)(&parent)?;
            let held = aterm_uds::kept_out(|| parent.try_clone())?;
            let directory = aterm_uds::private_file::PrivateDirectory::from_file(held)?;
            let file = directory.view()?.open_lock(LOCK)?;
            (probes.file)(&file)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(denied());
            }
            let lock_identity = Identity::of(&metadata);
            let lock = (probes.lock)(file)?;
            let admission = Self {
                ancestors,
                parent,
                directory,
                lock,
                lock_identity,
            };
            admission.intact(path, probes)?;
            Ok(admission)
        }

        pub(super) fn intact(&self, path: &Path, probes: Probes) -> io::Result<()> {
            if self
                .ancestors
                .last()
                .is_none_or(|(parent, _)| Some(parent.as_path()) != path.parent())
            {
                return Err(denied());
            }
            for (name, identity) in &self.ancestors {
                let file = directory(name)?;
                if Identity::of(&trusted_ancestor(&file)?) != *identity {
                    return Err(denied());
                }
            }
            (probes.directory)(&self.parent)?;
            (probes.file)(self.lock.file())?;
            let (file, metadata) = self
                .directory
                .view()?
                .open_private(LOCK)
                .map_err(private_error)?;
            (probes.file)(&file)?;
            if metadata.nlink() != 1 || Identity::of(&metadata) != self.lock_identity {
                return Err(denied());
            }
            Ok(())
        }
    }

    pub(super) struct Read {
        pub(super) book: Book,
        original: Option<(File, Metadata)>,
    }

    fn unchanged(before: &Metadata, after: &Metadata) -> bool {
        Identity::of(before) == Identity::of(after)
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && after.nlink() == 1
    }

    impl Read {
        pub(super) fn open(
            path: &Path,
            probes: Probes,
            admission: &Admission,
        ) -> Result<Self, Failure> {
            let name = record_name(path).map_err(|error| Failure::at("record_name", &error))?;
            let view = admission
                .directory
                .view()
                .map_err(|error| Failure::at("record_directory", &error))?;
            let (mut file, metadata) = match view.open_private(name) {
                Ok(opened) => opened,
                Err(aterm_uds::private_file::PrivateFileError::Missing) => {
                    return Ok(Self {
                        book: Book::default(),
                        original: None,
                    });
                }
                Err(error) => return Err(Failure::at("open_record", &private_error(error))),
            };
            (probes.file)(&file).map_err(|error| Failure::at("record_owner", &error))?;
            if metadata.nlink() != 1 || metadata.len() > MAX_BYTES as u64 {
                return Err(Failure::invalid("record_bound"));
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            (&mut file)
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| Failure::at("read_record", &error))?;
            if bytes.len() > MAX_BYTES {
                return Err(Failure::invalid("record_bound"));
            }
            let after = file
                .metadata()
                .map_err(|error| Failure::at("record_identity", &error))?;
            if !unchanged(&metadata, &after) {
                return Err(Failure::invalid("record_identity"));
            }
            let book = decode(&bytes).map_err(|_| Failure::invalid("decode_record"))?;
            Ok(Self {
                book,
                original: Some((file, metadata)),
            })
        }

        pub(super) fn intact(
            &self,
            path: &Path,
            probes: Probes,
            admission: &Admission,
        ) -> io::Result<()> {
            let view = admission.directory.view()?;
            match (&self.original, view.open_private(record_name(path)?)) {
                (None, Err(aterm_uds::private_file::PrivateFileError::Missing)) => Ok(()),
                (Some((held, before)), Ok((file, now))) => {
                    (probes.file)(held)?;
                    (probes.file)(&file)?;
                    if unchanged(before, &held.metadata()?) && unchanged(before, &now) {
                        Ok(())
                    } else {
                        Err(denied())
                    }
                }
                (_, Err(error)) => Err(private_error(error)),
                _ => Err(denied()),
            }
        }
    }

    pub(super) fn transact(
        path: &Path,
        incoming: Option<&Book>,
        probes: Probes,
    ) -> Result<Completed, Failure> {
        let admission =
            Admission::open(path, probes).map_err(|error| Failure::at("admit_store", &error))?;
        let mut read = Read::open(path, probes, &admission)?;
        read.intact(path, probes, &admission)
            .map_err(|error| Failure::at("record_identity", &error))?;
        let Some(incoming) = incoming else {
            admission
                .intact(path, probes)
                .map_err(|error| Failure::at("store_identity", &error))?;
            return Ok(Completed {
                book: read.book,
                persistence: if read.original.is_some() {
                    Persistence::Restored
                } else {
                    Persistence::Missing
                },
            });
        };
        read.book
            .merge(incoming)
            .map_err(|_| Failure::invalid("merge_record"))?;
        let bytes = encode(&read.book).map_err(|_| Failure::invalid("encode_record"))?;
        if bytes.len() > MAX_BYTES {
            return Err(Failure::invalid("record_bound"));
        }
        admission
            .intact(path, probes)
            .map_err(|error| Failure::at("store_identity", &error))?;
        read.intact(path, probes, &admission)
            .map_err(|error| Failure::at("record_identity", &error))?;
        let mut check = || {
            admission.intact(path, probes)?;
            read.intact(path, probes, &admission)
        };
        let landed = (probes.publish)(Publication {
            path,
            bytes: &bytes,
            check: &mut check,
            directory: &admission.directory,
        })
        .map_err(|error| Failure::at("publish_record", &error))?;
        let persistence = match landed {
            Err(error) => {
                Persistence::PublishedDurabilityUnknown(Failure::at("flush_directory", &error))
            }
            Ok(()) => match admission.intact(path, probes) {
                Ok(()) => Persistence::Saved,
                Err(error) => Persistence::PublishedDurabilityUnknown(Failure::at(
                    "published_store_identity",
                    &error,
                )),
            },
        };
        Ok(Completed {
            book: read.book,
            persistence,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "vitals_boot_cost_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who holds a draft journal, and the two ways a journal nobody holds leaves `drafts/`: its last holder's release, and the launch sweep (`docs/NATIVE_TAB_APPS_DESIGN.md` §5.11).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use super::{
    ContentFingerprint, JournalDocumentKey, JournalImageGeneration, JournalLockPatience,
    JournalOwnerId, MAX_JOURNAL_BYTES, holds_no_draft, journal_lock_path,
    recover_journal_image_for, with_journal_lock,
};
use crate::native_document_io::parse_hex16;

/// The directory beside the journals that holds every owner's lease.
const OWNERS_DIR: &str = ".owners";

/// At most this many owners on one image. A set past it (more live processes
/// than ever share one document) is not written, so that image is marked
/// [`UNKNOWN_HOLDERS`] and never taken: dropping an owner would let it go.
const MAX_OWNERS: usize = 32;

/// The mark naming an image's owners: `<the image's fingerprint>:<id>,<id>…`,
/// each 16 lowercase hex digits, or [`UNKNOWN_HOLDERS`].
#[cfg(target_os = "macos")]
const OWNERS_MARK: &std::ffi::CStr = c"com.aterm.draft-owners";
#[cfg(all(unix, not(target_os = "macos")))]
const OWNERS_MARK: &std::ffi::CStr = c"user.aterm.draft-owners";
#[cfg(windows)]
const OWNERS_MARK: &str = "com.aterm.draft-owners";

/// The mark of a publication whose holders cannot all be named: never taken.
const UNKNOWN_HOLDERS: &[u8] = b"?";

/// Room for a fingerprint and [`MAX_OWNERS`] ids, with their separators.
const MARK_CAPACITY: usize = 17 + MAX_OWNERS * 17;

/// This process's lease as a journal owner, held (locked) for as long as the host lives — in production, the process's life.
#[derive(Debug)]
pub(crate) struct JournalOwner {
    id: JournalOwnerId,
    sender: mpsc::Sender<Message>,
    #[cfg(test)]
    finished: Arc<std::sync::Mutex<mpsc::Receiver<()>>>,
}

impl JournalOwner {
    pub(crate) const fn id(&self) -> JournalOwnerId {
        self.id
    }

    /// Answers once this owner's custody worker has run every release and dropped its lease.
    #[cfg(test)]
    pub(crate) fn finished_for_test(&self) -> Arc<std::sync::Mutex<mpsc::Receiver<()>>> {
        Arc::clone(&self.finished)
    }
}

#[cfg(test)]
std::thread_local! {
    static ENQUEUED: std::cell::RefCell<Option<mpsc::Sender<()>>> = const { std::cell::RefCell::new(None) };
}

// Only this worker owns files and the lease. Messages from UI contain memory only.
type Job = Box<dyn FnOnce(&mut Custody) + Send>;
enum Message {
    Run(Job),
    Release(u64),
}

pub(super) struct Custody {
    next: u64,
    images: BTreeMap<u64, HeldImage>,
}

struct HeldImage {
    path: PathBuf,
    pin: Option<Candidate>,
    live: Arc<AtomicBool>,
}

impl Custody {
    /// Called within acquisition's journal lock, before the result can be delivered.
    pub(super) fn register(
        &mut self,
        owner: &Arc<JournalOwner>,
        path: &Path,
        key: JournalDocumentKey,
    ) -> Result<Registration, String> {
        let id = self.next;
        self.next = id
            .checked_add(1)
            .ok_or("journal registration IDs exhausted")?;
        let pin = Candidate::open(path.to_path_buf(), key);
        let live = Arc::new(AtomicBool::new(true));
        self.images.insert(
            id,
            HeldImage {
                path: path.to_path_buf(),
                pin,
                live: live.clone(),
            },
        );
        Ok(Registration(Arc::new(Release {
            id,
            live,
            owner: owner.clone(),
        })))
    }

    /// An explicit privacy purge revokes every local writer of these names. IDs are
    /// never reused, so stale plans and receipt drops cannot touch a later open.
    pub(super) fn revoke(&mut self, paths: &[PathBuf]) {
        self.images.retain(|_, held| {
            if paths.contains(&held.path) {
                held.live.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }

    fn release(&mut self, id: u64, owner: JournalOwnerId) {
        let Some(HeldImage { path, pin, live }) = self.images.remove(&id) else {
            return;
        };
        live.store(false, Ordering::Release);
        // A registration for this name also protects an identical republish on a new inode.
        // Never remove the process-wide mark while another local open can still write it.
        if self.images.values().any(|held| {
            held.path == path
                || pin
                    .as_ref()
                    .zip(held.pin.as_ref())
                    .is_some_and(|(leaving, held)| leaving.stamp.same_file(&held.stamp))
        }) {
            return;
        }
        let Some(pin) = pin else { return };
        let (Some(owners), Some((image, clean))) = (owners_dir_of(&path), pin.read_image()) else {
            return;
        };
        pin.settle(&owners, image, clean, Some(owner));
    }
}

/// Clones share one release authority. The last drop only sends a nonblocking message;
/// file pins, locks and the owner's final lease are destroyed by the worker.
#[derive(Clone, Debug)]
pub(crate) struct Registration(Arc<Release>);

#[derive(Debug)]
struct Release {
    id: u64,
    live: Arc<AtomicBool>,
    owner: Arc<JournalOwner>,
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.owner.sender.send(Message::Release(self.id));
    }
}

impl Registration {
    /// Adoption only reads this memory flag; it never waits for the worker.
    pub(super) fn live(&self) -> bool {
        self.0.live.load(Ordering::Acquire)
    }

    pub(super) fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Custody, u64) -> T + Send + 'static,
    ) -> Result<T, String> {
        let id = self.0.id;
        self.0
            .owner
            .run(move |custody| custody.images.contains_key(&id).then(|| work(custody, id)))?
            .ok_or_else(|| "journal registration was revoked".to_string())
    }
}

/// Mutation, acknowledgement and replacement of the registration's physical pin are
/// one journal-lock transaction. No path is reopened later to infer release authority.
pub(super) fn operate<T>(
    path: &Path,
    key: JournalDocumentKey,
    patience: JournalLockPatience,
    custody: Option<(&mut Custody, u64)>,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_journal_lock(path, patience, || {
        let result = work();
        if let Some((custody, id)) = custody {
            let pin = result
                .as_ref()
                .ok()
                .and_then(|_| Candidate::open(path.to_path_buf(), key));
            if let Some(held) = custody.images.get_mut(&id) {
                held.pin = pin;
            }
        }
        result
    })
}

impl JournalOwner {
    pub(super) fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Custody) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (sender, receiver) = mpsc::channel();
        #[cfg(all(test, unix))]
        let copies = super::FORKED_LOCK_COPIES.with(|copies| copies.borrow().clone());
        self.sender
            .send(Message::Run(Box::new(move |custody| {
                #[cfg(all(test, unix))]
                let previous = super::FORKED_LOCK_COPIES.with(|slot| slot.replace(copies));
                let result = work(custody);
                #[cfg(all(test, unix))]
                super::FORKED_LOCK_COPIES.with(|slot| *slot.borrow_mut() = previous);
                let _ = sender.send(result);
            })))
            .map_err(|_| "journal custody worker unavailable")?;
        #[cfg(test)]
        ENQUEUED.with(|slot| {
            if let Some(queued) = slot.borrow_mut().take() {
                queued.send(()).expect("queued work observed");
            }
        });
        #[cfg(test)]
        let result = match receiver.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(value) => Ok(value),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(mpsc::RecvError),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("journal custody worker did not reply within the hang detector")
            }
        };
        #[cfg(not(test))]
        let result = receiver.recv();
        result.map_err(|_| "journal custody worker stopped".to_string())
    }
}

fn start_worker(
    id: JournalOwnerId,
    lease: crate::native_document_host::HeldAdvisoryLock,
) -> Result<JournalOwner, String> {
    let (sender, receiver) = mpsc::channel();
    #[cfg(test)]
    let (done, finished) = mpsc::channel();
    crate::qos::spawn_lane_thread(
        "aterm-journal-custody",
        crate::qos::Role::Background,
        move || {
            let mut custody = Custody {
                next: 1,
                images: BTreeMap::new(),
            };
            let mut failed = false;
            // Retain panic payloads too: their destructors are arbitrary code. Nothing
            // uncertain may unwind this lease while logical owner handles still live.
            let mut failures = Vec::new();
            while let Ok(message) = receiver.recv() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match message {
                        Message::Run(job) if !failed => job(&mut custody),
                        Message::Release(registration) if !failed => {
                            custody.release(registration, id)
                        }
                        // Dropping a rejected job disconnects its reply, so its caller
                        // returns an error. Failure is never authority to settle an image.
                        _ => {}
                    }
                }));
                if let Err(payload) = result {
                    failed = true;
                    failures.push(payload);
                }
            }
            // Every logical owner/registration sender is gone. All final filesystem
            // destruction, including uncertain pins and the lease, stays on this worker.
            drop(custody);
            drop(lease);
            drop(failures);
            #[cfg(test)]
            let _ = done.send(());
        },
    )
    .map_err(|error| format!("journal custody worker: {error}"))?;
    Ok(JournalOwner {
        id,
        sender,
        #[cfg(test)]
        finished: Arc::new(std::sync::Mutex::new(finished)),
    })
}

/// Take a fresh owner identity under `root`: a lease file created exclusively, locked, and proved still linked at its name — a sweep that probed it between the create and the lock (and so took it for a dead owner's) unlinks it, and then another id is tried.
pub(crate) fn establish(root: &Path) -> Result<JournalOwner, String> {
    let dir = root.join(OWNERS_DIR);
    super::create_private_dir(&dir).map_err(|error| format!("journal owners: {error}"))?;
    for _ in 0..16 {
        let mut raw = [0_u8; 8];
        aterm_uds::rand::fill(&mut raw).map_err(|error| format!("no OS entropy: {error}"))?;
        let Some(id) = JournalOwnerId::from_raw(u64::from_le_bytes(raw)) else {
            continue;
        };
        let path = dir.join(id.lease_name());
        let file = match sys::create_lease(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("journal owner lease: {error}")),
        };
        let held = match crate::native_document_host::HeldAdvisoryLock::try_lock(file) {
            Ok(held) => held,
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("journal owner lease: {error}"));
            }
        };
        if names_file(&path, held.file()) {
            return start_worker(id, held);
        }
    }
    Err("could not take a journal owner lease".to_string())
}

/// Whether `path` names exactly the open file `file`.
pub(super) fn names_file(path: &Path, file: &File) -> bool {
    match (sys::stamp_at(path), sys::stamp(file)) {
        (Ok(named), Ok(open)) => named.same_file(&open),
        _ => false,
    }
}

/// Whether an in-place write can change only this image, with no preserved hard-link alias.
pub(super) fn uniquely_linked(file: &File) -> std::io::Result<bool> {
    sys::stamp(file).map(|stamp| stamp.uniquely_linked())
}

fn owners_dir_of(image: &Path) -> Option<PathBuf> {
    image.parent().map(|root| root.join(OWNERS_DIR))
}

/// Whether owner `id` is still running: its lease exists and someone holds it.
fn owner_alive(owners: &Path, id: JournalOwnerId) -> bool {
    let lease = match sys::open_regular(&owners.join(id.lease_name())) {
        Ok(lease) => lease,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    match crate::native_document_host::HeldAdvisoryLock::try_lock(lease) {
        Ok(_probe) => false,
        Err(_) => true,
    }
}

/// What an image says of its owners.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnerMark {
    /// No owner set: written by an aterm from before owner identity, or marked
    /// [`UNKNOWN_HOLDERS`].
    Unmarked,
    /// The owners of the image whose bytes have this fingerprint.
    Owners(ContentFingerprint, BTreeSet<JournalOwnerId>),
    /// A mark that does not parse, or a read that failed: never taken.
    Unreadable,
}

impl OwnerMark {
    /// The owners this names on an image whose bytes are `image`: none for a
    /// mark of other bytes, which a replace carried over (it names nobody).
    fn owners_of(self, image: ContentFingerprint) -> Option<BTreeSet<JournalOwnerId>> {
        match self {
            Self::Owners(marked, owners) if marked == image => Some(owners),
            _ => None,
        }
    }
}

fn parse_mark(value: &[u8]) -> OwnerMark {
    if value == UNKNOWN_HOLDERS {
        return OwnerMark::Unmarked;
    }
    let Some((image, ids)) = std::str::from_utf8(value)
        .ok()
        .and_then(|text| text.split_once(':'))
    else {
        return OwnerMark::Unreadable;
    };
    let Some(image) = parse_hex16(image) else {
        return OwnerMark::Unreadable;
    };
    let mut owners = BTreeSet::new();
    for id in ids.split(',') {
        match JournalOwnerId::parse(id) {
            Some(id) => {
                owners.insert(id);
            }
            None => return OwnerMark::Unreadable,
        }
    }
    OwnerMark::Owners(ContentFingerprint(image), owners)
}

/// The mark naming `owners` on the image whose bytes are `image`: `None` for
/// no owner or past [`MAX_OWNERS`].
fn format_mark(image: ContentFingerprint, owners: &BTreeSet<JournalOwnerId>) -> Option<Vec<u8>> {
    if owners.is_empty() || owners.len() > MAX_OWNERS {
        return None;
    }
    let ids = owners
        .iter()
        .map(|id| format!("{:016x}", id.raw()))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{:016x}:{ids}", image.0).into_bytes())
}

/// The mark on `file`, which `path` names.
fn read_mark(path: &Path, file: &File) -> OwnerMark {
    let mut value = [0_u8; MARK_CAPACITY];
    match sys::read_mark(path, file, &mut value) {
        Ok(None) => OwnerMark::Unmarked,
        Ok(Some(len)) => parse_mark(&value[..len.min(value.len())]),
        Err(_) => OwnerMark::Unreadable,
    }
}

/// Write `owners` of the image whose bytes are `image` onto `file`, which
/// `path` names.
fn write_mark(
    path: &Path,
    file: &File,
    image: ContentFingerprint,
    owners: &BTreeSet<JournalOwnerId>,
) -> std::io::Result<()> {
    let value =
        format_mark(image, owners).ok_or_else(|| std::io::Error::other("no owner set to write"))?;
    sys::write_mark(path, file, &value)
}

/// The mark for a publication of bytes `bytes` over `path` (whose image was
/// observed as `expected`, under the journal lock), held by `owner` — and, when
/// the image it replaces already holds exactly these bytes, by that image's
/// live owners too: any of them may be appending to these bytes, so the claim
/// carries over.
fn publication_mark(
    path: &Path,
    expected: JournalImageGeneration,
    bytes: ContentFingerprint,
    owner: Option<JournalOwnerId>,
) -> Vec<u8> {
    let (Some(owner), Some(owners_dir)) = (owner, owners_dir_of(path)) else {
        return UNKNOWN_HOLDERS.to_vec();
    };
    let mut owners = BTreeSet::from([owner]);
    if expected.exists && expected.fingerprint == bytes {
        let previous = sys::open_regular(path)
            .ok()
            .and_then(|image| read_mark(path, &image).owners_of(bytes));
        let Some(previous) = previous else {
            return UNKNOWN_HOLDERS.to_vec();
        };
        owners.extend(
            previous
                .into_iter()
                .filter(|id| *id == owner || owner_alive(&owners_dir, *id)),
        );
    }
    format_mark(bytes, &owners).unwrap_or_else(|| UNKNOWN_HOLDERS.to_vec())
}

/// Mark `temporary` (at `temporary_path`), about to be published over `path`, with [`publication_mark`], and return that mark for [`confirm_publication`].
pub(super) fn mark_publication(
    temporary: &File,
    temporary_path: &Path,
    path: &Path,
    expected: JournalImageGeneration,
    bytes: ContentFingerprint,
    owner: Option<JournalOwnerId>,
) -> Vec<u8> {
    let mark = publication_mark(path, expected, bytes, owner);
    if let Err(error) = sys::write_mark(temporary_path, temporary, &mark) {
        aterm_log::debug!("draft journal published without an owner mark: {error}");
    }
    mark
}

/// `path` was just published from a temporary marked `mark`, under the journal lock: make the image carry exactly that mark.
pub(super) fn confirm_publication(path: &Path, mark: &[u8]) {
    #[cfg(windows)]
    {
        let mut value = [0_u8; MARK_CAPACITY];
        let Ok(image) = sys::open_regular(path) else {
            return;
        };
        if matches!(sys::read_mark(path, &image, &mut value), Ok(Some(len)) if value.get(..len) == Some(mark))
        {
            return;
        }
        if sys::write_mark(path, &image, mark).is_err() {
            sys::remove_mark(path);
        }
    }
    #[cfg(not(windows))]
    let _ = (path, mark);
}

/// `owner` just appended to `file`, the image at `path`, in place and under the journal lock, so that its bytes are now `image`: name `owner` alone on them. No other holder holds these bytes: one that held the image before them proves its own tail against the file before it appends, and no longer finds it there. A failed write leaves a mark of other bytes, which names nobody, so the image is never taken.
pub(super) fn mark_append(
    path: &Path,
    file: &File,
    image: ContentFingerprint,
    owner: Option<JournalOwnerId>,
) {
    let mark = owner
        .filter(|_| owners_dir_of(path).is_some())
        .and_then(|owner| format_mark(image, &BTreeSet::from([owner])))
        .unwrap_or_else(|| UNKNOWN_HOLDERS.to_vec());
    if let Err(error) = sys::write_mark(path, file, &mark) {
        aterm_log::debug!("draft journal appended without an owner mark: {error}");
    }
}

/// A re-seat found the image it last published (bytes `image`) still in place: join its owner set, so the image names this process even when another wrote these same bytes since.
pub(super) fn adopt(path: &Path, image: ContentFingerprint, owner: Option<JournalOwnerId>) {
    let (Some(owner), Some(owners_dir)) = (owner, owners_dir_of(path)) else {
        return;
    };
    let Ok(file) = sys::open_regular(path) else {
        return;
    };
    let Some(mut owners) = read_mark(path, &file).owners_of(image) else {
        return;
    };
    if owners.contains(&owner) {
        return;
    }
    owners.retain(|id| owner_alive(&owners_dir, *id));
    owners.insert(owner);
    if let Err(error) = write_mark(path, &file, image, &owners) {
        aterm_log::debug!("draft journal owner set not joined: {error}");
        let _ = sys::write_mark(path, &file, UNKNOWN_HOLDERS);
    }
}

/// A keyed journal a release or a sweep may take, as one look found it: its open file, which pins it, and the stamp that look read.
#[derive(Debug)]
pub(super) struct Candidate {
    path: PathBuf,
    key: JournalDocumentKey,
    image: File,
    stamp: sys::Stamp,
}

impl Candidate {
    fn open(path: PathBuf, key: JournalDocumentKey) -> Option<Self> {
        let image = sys::open_regular(&path).ok()?;
        let stamp = sys::stamp(&image).ok()?;
        (stamp.len() <= MAX_JOURNAL_BYTES as u64).then_some(Self {
            path,
            key,
            image,
            stamp,
        })
    }

    /// The fingerprint of its bytes, and whether they decode to a journal that
    /// holds no draft.
    fn read_image(&self) -> Option<(ContentFingerprint, bool)> {
        use std::io::{Read as _, Seek as _, SeekFrom};

        (&self.image).seek(SeekFrom::Start(0)).ok()?;
        let mut bytes =
            Vec::with_capacity(usize::try_from(self.stamp.len().saturating_add(1)).unwrap_or(0));
        (&self.image)
            .take(MAX_JOURNAL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return None;
        }
        let clean = recover_journal_image_for(self.key, &bytes).is_ok_and(|(read, tail, fault)| {
            fault.is_none()
                && holds_no_draft(&read)
                && tail.is_none_or(|tail| tail.byte_len() == bytes.len() as u64)
        });
        Some((ContentFingerprint::of(&bytes), clean))
    }

    /// Under the journal lock, on the very file read (bytes `image`, `clean`
    /// when they hold no draft): take `leaving` off its owner set, then remove
    /// it with its lock file when it is clean and every owner left on it is
    /// gone; otherwise write the smaller set.
    fn settle(
        self,
        owners: &Path,
        image: ContentFingerprint,
        clean: bool,
        leaving: Option<JournalOwnerId>,
    ) -> bool {
        let Self {
            path,
            image: file,
            stamp,
            ..
        } = self;
        with_journal_lock(&path, JournalLockPatience::Sweep, || {
            if !sys::stamp_at(&path).is_ok_and(|now| now == stamp) {
                return Ok(false);
            }
            let Some(mut set) = read_mark(&path, &file).owners_of(image) else {
                return Ok(false);
            };
            if leaving.is_some_and(|leaving| !set.remove(&leaving)) {
                return Ok(false);
            }
            if clean && set.iter().all(|id| !owner_alive(owners, *id)) {
                drop(file);
                fs::remove_file(&path).map_err(|error| error.to_string())?;
                let _ = fs::remove_file(journal_lock_path(&path)?);
                return Ok(true);
            }
            if leaving.is_some() && !set.is_empty() {
                // A refusal leaves this process named: kept until it exits.
                let _ = write_mark(&path, &file, image, &set);
            }
            Ok(false)
        })
        .unwrap_or(false)
    }
}

/// What one sweep did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct JournalSweep {
    /// Clean journals nobody holds, removed with their lock files.
    pub(crate) removed: usize,
    /// Keyed journals kept: held, unsaved, unmarked or unreadable.
    pub(crate) kept: usize,
    /// Leftovers of writers that are gone: a lock file whose journal is gone, a publication's temporary.
    pub(crate) leftovers: usize,
    /// Leases of owners that are gone.
    pub(crate) dead_owners: usize,
}

/// Start [`sweep_unheld_journals`] over `root`, then the sweep of dead makers' grid scratch folders ([`aterm_types::private_edit::sweep_dead`]), on a background thread: no window or open waits on it, and each journal or folder it takes is one no live process holds.
#[cfg(not(test))]
pub(crate) fn spawn_launch_sweep(root: PathBuf) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-journal-sweep",
        crate::qos::Role::Background,
        move || {
            let sweep = sweep_unheld_journals(&root);
            if sweep != JournalSweep::default() {
                aterm_log::info!("draft journal sweep: {sweep:?}");
            }
            // A grid scratch folder whose maker was killed mid-edit still holds what it
            // revealed: it goes at launch (`aterm_types::private_edit`).
            let scratch = aterm_types::private_edit::sweep_dead(
                &aterm_types::private_edit::scratch_root(),
                None,
            );
            if scratch > 0 {
                aterm_log::info!("private edit sweep: {scratch} scratch folder(s) removed");
            }
        },
    );
    if let Err(error) = spawned {
        aterm_log::warn!("draft journal sweep not started: {error}");
    }
}

/// The launch sweep (module docs): every keyed journal in `root` that carries an owner set, whose owners are all gone and that holds no draft is removed with its lock file; then the leftovers of writers that are gone, and the leases of owners that are gone.
pub(crate) fn sweep_unheld_journals(root: &Path) -> JournalSweep {
    let mut sweep = JournalSweep::default();
    let owners = root.join(OWNERS_DIR);
    let Ok(entries) = fs::read_dir(root) else {
        return sweep;
    };
    let names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let mut gone_locks = BTreeSet::new();
    for name in &names {
        let Some(key) = keyed_image_key(name) else {
            continue;
        };
        let swept = sweep_candidate(&root.join(name), key, &owners)
            .is_some_and(|(candidate, image)| candidate.settle(&owners, image, true, None));
        if swept {
            sweep.removed += 1;
            gone_locks.insert(format!(".{name}.lock"));
        } else {
            sweep.kept += 1;
        }
    }
    for name in names.iter().filter(|name| !gone_locks.contains(*name)) {
        if sweep_leftover(root, name) {
            sweep.leftovers += 1;
        }
    }
    sweep.dead_owners = sweep_dead_owners(&owners);
    sweep
}

/// The sweep's lock-free look at the keyed journal at `path`: a candidate
/// when its mark names only owners that are gone, on exactly its bytes, and
/// those hold no draft.
fn sweep_candidate(
    path: &Path,
    key: JournalDocumentKey,
    owners: &Path,
) -> Option<(Candidate, ContentFingerprint)> {
    let candidate = Candidate::open(path.to_path_buf(), key)?;
    let mark = read_mark(path, &candidate.image);
    let OwnerMark::Owners(_, set) = &mark else {
        return None;
    };
    if set.iter().any(|id| owner_alive(owners, *id)) {
        return None;
    }
    let (image, clean) = candidate.read_image()?;
    (clean && mark.owners_of(image).is_some()).then_some((candidate, image))
}

/// `<16 hex>.mac.atdj`, a keyed journal's name ([`super::DraftJournalHost::path_for`]):
/// its key.
fn keyed_image_key(name: &str) -> Option<JournalDocumentKey> {
    let hex = name.strip_suffix(".mac.atdj")?;
    (hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(hex, 16).ok())
        .flatten()
        .map(JournalDocumentKey)
}

/// A keyed or legacy journal's name (never a preserved copy's).
fn is_image_name(name: &str) -> bool {
    keyed_image_key(name).is_some()
        || name
            .strip_suffix(".atdj")
            .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Remove `name` when it is a dead writer's leftover, under the journal lock
/// of the image it belongs to: a publication's temporary
/// (`.<image>.<pid>-<n>.tmp`; every writer removes its own before it lets the
/// lock go), or a lock file (`.<image>.lock`) whose journal is gone.
fn sweep_leftover(root: &Path, name: &str) -> bool {
    let Some(dotted) = name.strip_prefix('.') else {
        return false;
    };
    let (image, temporary) = if let Some(image) = dotted.strip_suffix(".lock") {
        (image, false)
    } else if let Some(stem) = dotted.strip_suffix(".tmp") {
        let Some((image, writer)) = stem.rsplit_once('.') else {
            return false;
        };
        let Some((pid, nonce)) = writer.split_once('-') else {
            return false;
        };
        if pid.is_empty()
            || nonce.is_empty()
            || !pid
                .bytes()
                .chain(nonce.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        (image, true)
    } else {
        return false;
    };
    if !is_image_name(image) {
        return false;
    }
    let image_path = root.join(image);
    let leftover = root.join(name);
    with_journal_lock(&image_path, JournalLockPatience::Sweep, || {
        let image_gone = fs::symlink_metadata(&image_path).is_err();
        if !temporary && !image_gone {
            return Ok(false);
        }
        let removed = fs::remove_file(&leftover).is_ok();
        if image_gone && temporary {
            // Taking the lock made its file again; it names no journal.
            let _ = fs::remove_file(journal_lock_path(&image_path)?);
        }
        Ok(removed)
    })
    .unwrap_or(false)
}

/// Remove the lease of every owner that is gone: acquired, still linked at its
/// name, unlinked while held.
fn sweep_dead_owners(owners: &Path) -> usize {
    let Ok(entries) = fs::read_dir(owners) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(JournalOwnerId::parse_lease)
        else {
            continue;
        };
        let path = owners.join(id.lease_name());
        let Ok(lease) = sys::open_regular(&path) else {
            continue;
        };
        let Ok(held) = crate::native_document_host::HeldAdvisoryLock::try_lock(lease) else {
            continue;
        };
        if names_file(&path, held.file()) && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The platform half: opening a file without following a link, a file's
/// stamp, and the mark's storage.
#[cfg(unix)]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    use super::OWNERS_MARK;

    /// A file's identity (device and inode), size and modification time.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Stamp {
        dev: u64,
        ino: u64,
        len: u64,
        links: u64,
        mtime: (i64, i64),
    }

    impl Stamp {
        fn of(metadata: &std::fs::Metadata) -> Self {
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                len: metadata.len(),
                links: metadata.nlink(),
                mtime: (metadata.mtime(), metadata.mtime_nsec()),
            }
        }

        pub(super) const fn len(&self) -> u64 {
            self.len
        }

        pub(super) const fn uniquely_linked(&self) -> bool {
            self.links == 1
        }

        pub(super) const fn same_file(&self, other: &Self) -> bool {
            self.dev == other.dev && self.ino == other.ino
        }
    }

    pub(super) fn stamp(file: &File) -> std::io::Result<Stamp> {
        #[cfg(test)]
        super::check_fs_thread();
        file.metadata().map(|metadata| Stamp::of(&metadata))
    }

    /// The stamp of what `path` names, never following a link.
    pub(super) fn stamp_at(path: &Path) -> std::io::Result<Stamp> {
        #[cfg(test)]
        super::check_fs_thread();
        std::fs::symlink_metadata(path).map(|metadata| Stamp::of(&metadata))
    }

    /// Open an existing regular file without following a link or waiting on a
    /// FIFO.
    pub(super) fn open_regular(path: &Path) -> std::io::Result<File> {
        #[cfg(test)]
        super::check_fs_thread();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(file)
    }

    pub(super) fn create_lease(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
    }

    /// The mark on `file` into `value`: its length, or `None` when it has none.
    pub(super) fn read_mark(
        _path: &Path,
        file: &File,
        value: &mut [u8],
    ) -> std::io::Result<Option<usize>> {
        // SAFETY: `file` is open for the call, the name is a NUL-terminated constant, and `value` is a live buffer of the length passed.
        #[cfg(target_os = "macos")]
        let read = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                OWNERS_MARK.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        // SAFETY: as above.
        #[cfg(not(target_os = "macos"))]
        let read = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                OWNERS_MARK.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        match usize::try_from(read) {
            Ok(len) => Ok(Some(len)),
            Err(_) => {
                let error = std::io::Error::last_os_error();
                #[cfg(target_os = "macos")]
                let missing = libc::ENOATTR;
                #[cfg(not(target_os = "macos"))]
                let missing = libc::ENODATA;
                if error.raw_os_error() == Some(missing) {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub(super) fn write_mark(_path: &Path, file: &File, value: &[u8]) -> std::io::Result<()> {
        // SAFETY: `file` is open for the call, the name is a NUL-terminated constant, and `value` outlives it with the length passed.
        #[cfg(target_os = "macos")]
        let written = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                OWNERS_MARK.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        // SAFETY: as above.
        #[cfg(not(target_os = "macos"))]
        let written = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                OWNERS_MARK.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if written == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// The platform half on Windows: the mark is an alternate data stream of the
/// file (`<path>:com.aterm.draft-owners`), read and written by name — always
/// under the journal lock, after the name is proved to name the file in hand,
/// or on a temporary only its writer knows — and a file's stamp is
/// `GetFileInformationByHandle`'s volume, index, size and last write.
#[cfg(windows)]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::io::{Read as _, Write as _};
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::windows::io::AsRawHandle as _;
    use std::path::{Path, PathBuf};

    use super::OWNERS_MARK;

    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: [u32; 2],
        last_access_time: [u32; 2],
        last_write_time: [u32; 2],
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    /// A file's identity (volume and index), size and last write.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Stamp {
        volume: u32,
        index: u64,
        len: u64,
        links: u32,
        last_write: u64,
    }

    impl Stamp {
        pub(super) const fn uniquely_linked(&self) -> bool {
            self.links == 1
        }

        pub(super) fn len(&self) -> u64 {
            self.len
        }

        pub(super) fn same_file(&self, other: &Self) -> bool {
            self.volume == other.volume && self.index == other.index
        }
    }

    pub(super) fn stamp(file: &File) -> std::io::Result<Stamp> {
        #[cfg(test)]
        super::check_fs_thread();
        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: `file` owns a live handle and the out pointer addresses writable storage for exactly one BY_HANDLE_FILE_INFORMATION.
        let ok = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a nonzero return initializes the whole structure.
        let information = unsafe { information.assume_init() };
        let pair = |high: u32, low: u32| (u64::from(high) << 32) | u64::from(low);
        Ok(Stamp {
            volume: information.volume_serial_number,
            index: pair(information.file_index_high, information.file_index_low),
            len: pair(information.file_size_high, information.file_size_low),
            links: information.number_of_links,
            last_write: pair(
                information.last_write_time[1],
                information.last_write_time[0],
            ),
        })
    }

    /// The stamp of what `path` names, never following a reparse point.
    pub(super) fn stamp_at(path: &Path) -> std::io::Result<Stamp> {
        #[cfg(test)]
        super::check_fs_thread();
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        stamp(&file)
    }

    fn not_regular() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")
    }

    /// Open an existing regular file without following a reparse point.
    pub(super) fn open_regular(path: &Path) -> std::io::Result<File> {
        #[cfg(test)]
        super::check_fs_thread();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(not_regular());
        }
        Ok(file)
    }

    pub(super) fn create_lease(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn stream(path: &Path) -> PathBuf {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":");
        stream.push(OWNERS_MARK);
        PathBuf::from(stream)
    }

    /// The mark on the file `path` names into `value`: its length, or `None` when it has none.
    pub(super) fn read_mark(
        path: &Path,
        _file: &File,
        value: &mut [u8],
    ) -> std::io::Result<Option<usize>> {
        let stream = match OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(stream(path))
        {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut read = Vec::with_capacity(value.len() + 1);
        stream.take(value.len() as u64 + 1).read_to_end(&mut read)?;
        let Some(slot) = value.get_mut(..read.len()) else {
            return Err(std::io::Error::other("owner mark too long"));
        };
        slot.copy_from_slice(&read);
        Ok(Some(read.len()))
    }

    pub(super) fn write_mark(path: &Path, _file: &File, value: &[u8]) -> std::io::Result<()> {
        let mut stream = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(stream(path))?;
        stream.write_all(value)
    }

    /// Drop the mark of the file `path` names: unmarked, it is never taken.
    pub(super) fn remove_mark(path: &Path) {
        let _ = std::fs::remove_file(stream(path));
    }
}

#[cfg(test)]
#[path = "native_document_journal_owner_tests.rs"]
mod tests;

#[cfg(test)]
thread_local! { static FORBID_FS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn check_fs_thread() {
    FORBID_FS.with(|forbidden| {
        assert!(
            !forbidden.get(),
            "journal filesystem work on retirement/adoption caller"
        )
    });
}

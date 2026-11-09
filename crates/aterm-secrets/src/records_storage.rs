// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A records file becomes a directory before old local writers can truncate its history.
//! A durable intent makes retries reuse one preparation. The original file remains beside
//! the directory, and a separately published pin selects it only after validation, so a
//! missing current file never falls back to an earlier snapshot.
//! The pin's required reader capacity only rises, before data publication, so a smaller
//! format-aware build refuses a later larger history instead of shortening it.

use crate::{Error, Result, fsx};
use aterm_digest::Sha256;
use aterm_dirfd::{self as fd, AtFlags, FileType, Mode, OFlags, RenameFlags};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DATA: &str = "current";
const HEADER: &str = "layout";
const MAGIC: &str = "aterm-records-layout/1";
const PIN_MAGIC: &str = "aterm-records-pin/1";
const META_LIMIT: usize = 1024;

/// No data or transition witness exists at this logical entry.
pub(crate) fn absent(entry: &Path) -> Result<bool> {
    Ok(fsx::kind(entry)? == fsx::Kind::Missing
        && fsx::kind(&pin_path(entry)?)? == fsx::Kind::Missing
        && fsx::kind(&intent_path(entry)?)? == fsx::Kind::Missing)
}

fn refused(why: &str) -> Error {
    Error::new(format!("membership records storage: {why}"))
}

fn syscall<T>(result: fd::Result<T>) -> Result<T> {
    result.map_err(std::io::Error::from).map_err(Error::from)
}

/// The data file a reader selected, with what it selected that file from as it stood.
pub(crate) struct Selected {
    entry: PathBuf,
    path: PathBuf,
    witnesses: Option<Witnesses>,
}

/// What stood at a selection's pin, entry and intent: each one's kind, stamp, owner, mode and links,
/// from one `lstat` ([`fsx::Seen`]).
type Witnesses = [Option<fsx::Seen>; 3];

impl Selected {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Hold the selection after reading its data: the pin, the entry and the intent still stand
    /// as they were found, or the selection is resolved again, with what that refuses.
    pub(crate) fn confirm(&self) -> Result<()> {
        if self.witnesses.is_some() && selection_witnesses(&self.entry).ok() == self.witnesses {
            return Ok(());
        }
        data_path(&self.entry).map(|_| ())
    }
}

/// Whether `witnesses` show no pin, and either a regular file at the entry (an intent beside it is
/// not read) or nothing at the entry and no intent: the entry is then the data ([`data_path`]),
/// and the witnesses hold all `data_path` looks at.
const fn is_plain(witnesses: &Witnesses) -> bool {
    matches!(
        witnesses,
        [None, None, None]
            | [
                None,
                Some(fsx::Seen {
                    kind: fsx::Kind::File,
                    ..
                }),
                _,
            ]
    )
}

fn selection_witnesses(entry: &Path) -> Result<Witnesses> {
    Ok([
        fsx::seen_if_present(&pin_path(entry)?)?,
        fsx::seen_if_present(entry)?,
        fsx::seen_if_present(&intent_path(entry)?)?,
    ])
}

/// [`data_path`], for a reader that holds the selection across its read
/// ([`Selected::confirm`]). Where the witnesses show a plain records file or none ([`is_plain`]),
/// they already hold what `data_path` looks at, and the entry is what it answers. A pinned
/// layout whose files all stand as they did when a reader last resolved it answers what it did
/// then, without being read again ([`Judgements`]).
pub(crate) fn selected(entry: &Path) -> Result<Selected> {
    JUDGEMENTS.selected(entry)
}

/// The judgements this process's readers share ([`selected`], [`standing`]).
static JUDGEMENTS: Judgements = Judgements::new();

/// Every file a selection of the data at a records entry is judged by, as it stood when a reader
/// looked ([`standing`]). Equal only to a look at the same files standing as they were.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Standing {
    /// The pin, the entry and the intent ([`selection_witnesses`]). Where they show a plain records
    /// file or none ([`is_plain`]), these are all [`data_path`] looks at.
    witnesses: Witnesses,
    /// For anything else the selection takes (a pinned layout, or a transition it can still read
    /// through), the rest of what it judged ([`Judged`]).
    judged: Option<Box<Judged>>,
}

/// What stands at `entry` for a reader that keeps what it read from its selected data while each
/// file the selection is judged by stands as it was (`client::vault_memo`), so it need not select
/// the data again to know it would select the same: `None` where the selection refuses them (an
/// unfinished transition, an unsafe or inconsistent layout, a capacity larger than this reader
/// holds) or they cannot be seen. One `lstat` of the pin, the entry and the intent decides it for
/// a plain records file; a pinned layout standing as a reader last judged it is not judged again
/// ([`Judgements`]).
pub(crate) fn standing(entry: &Path) -> Option<Standing> {
    JUDGEMENTS.standing(entry)
}

/// The files a [`Standing`] of `entry` stands for, besides the parent: the entry, its pin, its
/// intent, and a layout's header and data file.
///
/// # Errors
/// The entry has no valid name ([`pin_path`]).
#[cfg(test)]
pub(crate) fn standing_files(entry: &Path) -> Result<[PathBuf; 5]> {
    Ok([
        entry.to_path_buf(),
        pin_path(entry)?,
        intent_path(entry)?,
        entry.join(HEADER),
        entry.join(DATA),
    ])
}

/// [`data_path`], as [`selected`] answers it: for a reader that only names the data file.
pub(crate) fn reader_path(entry: &Path) -> Result<PathBuf> {
    selected(entry).map(|selected| selected.path)
}

/// What [`data_path`] judged of a pinned layout, as it stood: the parent's identity, owner and
/// mode ([`Context::new`]); the pin, the entry and the intent ([`Witnesses`]) and the layout's
/// header and data file, each one's kind, stamp, owner, mode and links from one `lstat`
/// ([`fsx::Seen`]); and the capacity the reader held ([`check_capacity`]). A file renamed over one
/// of them or given another owner, mode or link shows another, and so does one written since, but
/// for a rewrite in place of the same length within one tick of a coarse clock, which no writer of
/// these files makes ([`fsx::FileStamp`]).
#[derive(Clone, PartialEq, Eq)]
struct Judged {
    entry: PathBuf,
    held: usize,
    parent: (u64, u64, u32, u32),
    witnesses: Witnesses,
    files: [Option<fsx::Seen>; 2],
}

impl Judged {
    fn of(entry: &Path, witnesses: Witnesses) -> Result<Self> {
        let parent = std::fs::symlink_metadata(
            entry
                .parent()
                .ok_or_else(|| refused("no parent directory"))?,
        )?;
        Ok(Self {
            entry: entry.to_path_buf(),
            held: crate::membership_log::RecordBudgets::current().held,
            parent: (parent.dev(), parent.ino(), parent.uid(), parent.mode()),
            witnesses,
            files: [
                fsx::seen_if_present(&entry.join(HEADER))?,
                fsx::seen_if_present(&entry.join(DATA))?,
            ],
        })
    }
}

/// The pinned layouts readers resolved lately, each as it stood ([`Judged`]). A Mac's records
/// past [`crate::crossing::past_legacy`] sit in such a layout, and each Commons check of a quiet
/// repair round judges them for its vault memo's stamps ([`standing`]): resolving it each time
/// opened and read the pin, the parent, the directory, its header and its data file, 20% of such
/// a round over a log of 9,605 records (2026-11-08,
/// `docs/measured/commons-long-log-rounds-2026-11-07.md`).
struct Judgements(Mutex<VecDeque<Judged>>);

/// The most layouts [`Judgements`] keeps: one for each records entry a process reads.
const JUDGED: usize = 8;

impl Judgements {
    const fn new() -> Self {
        Self(Mutex::new(VecDeque::new()))
    }

    fn selected(&self, entry: &Path) -> Result<Selected> {
        let witnesses = selection_witnesses(entry).ok();
        let path = match witnesses {
            Some(stood) if is_plain(&stood) => entry.to_path_buf(),
            Some(
                stood @ [
                    Some(fsx::Seen {
                        kind: fsx::Kind::File,
                        ..
                    }),
                    Some(fsx::Seen {
                        kind: fsx::Kind::Dir,
                        ..
                    }),
                    _,
                ],
            ) => {
                self.layout(entry, stood)?;
                entry.join(DATA)
            }
            _ => data_path(entry)?,
        };
        Ok(Selected {
            entry: entry.to_path_buf(),
            path,
            witnesses,
        })
    }

    /// [`standing`]: the shapes [`Self::selected`] tells apart, from the same `lstat`s, with the
    /// judgement of a pinned layout it would take ([`Self::layout`]), and any other shape judged by
    /// [`data_path`] afresh.
    fn standing(&self, entry: &Path) -> Option<Standing> {
        let witnesses = selection_witnesses(entry).ok()?;
        let judged = match witnesses {
            stood if is_plain(&stood) => None,
            [
                Some(fsx::Seen {
                    kind: fsx::Kind::File,
                    ..
                }),
                Some(fsx::Seen {
                    kind: fsx::Kind::Dir,
                    ..
                }),
                _,
            ] => Some(Box::new(self.layout(entry, witnesses).ok()?)),
            _ => {
                data_path(entry).ok()?;
                Some(Box::new(Judged::of(entry, witnesses).ok()?))
            }
        };
        Some(Standing { witnesses, judged })
    }

    /// [`data_path`]'s judgement of the pinned layout at `entry`, whose pin, entry and intent
    /// stood as `witnesses` say (a prepared layout keeps its intent, which `data_path` does not
    /// read once the pin stands), as its files stood before it: taken from the last judgement of
    /// it while its files all stand as they did then. A judgement answers for its files only
    /// where they stood still across it. Its data file is the one inside the entry.
    fn layout(&self, entry: &Path, witnesses: Witnesses) -> Result<Judged> {
        let before = Judged::of(entry, witnesses)?;
        if self.0.lock().is_ok_and(|judged| judged.contains(&before)) {
            return Ok(before);
        }
        data_path(entry)?;
        let after = selection_witnesses(entry).and_then(|stood| Judged::of(entry, stood));
        if after.is_ok_and(|after| after == before)
            && let Ok(mut judged) = self.0.lock()
        {
            judged.retain(|other| other.entry != before.entry);
            if judged.len() >= JUDGED {
                judged.pop_front();
            }
            judged.push_back(before.clone());
        }
        Ok(before)
    }
}

/// Resolve existing custody without changing it. An incomplete transition is unavailable.
pub(crate) fn data_path(entry: &Path) -> Result<PathBuf> {
    let pin_path = pin_path(entry)?;
    if let Some(pin) = fsx::read_private(&pin_path, META_LIMIT)? {
        let context = Context::new(entry)?;
        context.active_under(|| {
            context.pin_is_private()?;
            Ok(pin.as_slice().to_vec())
        })?;
        return Ok(entry.join(DATA));
    }
    match fsx::kind(entry)? {
        fsx::Kind::File => Ok(entry.to_path_buf()),
        fsx::Kind::Missing => {
            if fsx::kind(&intent_path(entry)?)? != fsx::Kind::Missing {
                let intent = Context::new(entry)?
                    .intent()?
                    .ok_or_else(|| refused("the preparation intent changed during resolution"))?;
                if intent.original.is_some() {
                    return Err(refused("the intended original records file is missing"));
                }
            }
            Ok(entry.to_path_buf())
        }
        fsx::Kind::Dir => Err(refused("an interrupted transition needs its writer lock")),
        _ => Err(refused("the records entry is not an owned file or layout")),
    }
}

/// Whether a writer declared preparation whose readiness pin is not published yet.
pub(crate) fn pending(entry: &Path) -> Result<bool> {
    Ok(fsx::kind(&pin_path(entry)?)? == fsx::Kind::Missing
        && (fsx::kind(&intent_path(entry)?)? != fsx::Kind::Missing
            || fsx::kind(entry)? == fsx::Kind::Dir))
}

/// Resume a prepared transition under the caller's existing records writer lock.
pub(crate) fn recover_pending(entry: &Path) -> Result<()> {
    if pending(entry)? {
        let _guard = writer_lock(entry)?;
        return recover_locked(entry);
    }
    if fsx::read_private(&pin_path(entry)?, META_LIMIT)?.is_some() {
        return data_path(entry).map(|_| ());
    }
    Ok(())
}

fn recover_locked(entry: &Path) -> Result<()> {
    if fsx::read_private(&pin_path(entry)?, META_LIMIT)?.is_some() {
        return data_path(entry).map(|_| ());
    }
    if pending(entry)? {
        Context::new(entry)?.prepare(|_| Ok(()))?;
    }
    Ok(())
}

/// Select the write target, preparing a fenced layout only when required. The caller holds
/// its records writer lock throughout selection and publication.
pub(crate) fn write_path(entry: &Path, fence: bool) -> Result<WriteTarget> {
    let guard = writer_lock(entry)?;
    select_write_path(entry, fence, guard)
}

/// Accept a snapshot only while its file is unchanged, before preparing any layout or
/// raising its capacity. The returned target keeps that comparison and the write atomic
/// with respect to other writers using the records lock.
pub(crate) fn write_path_if_unchanged(
    entry: &Path,
    fence: bool,
    expected: Option<fsx::FileStamp>,
) -> Result<WriteTarget> {
    let guard = writer_lock(entry)?;
    if fsx::stamp_if_present(&data_path(entry)?)? != expected {
        return Err(refused(
            "the records changed since this writer read them; retry after reading",
        ));
    }
    select_write_path(entry, fence, guard)
}

fn select_write_path(entry: &Path, fence: bool, guard: WriterLock) -> Result<WriteTarget> {
    recover_locked(entry)?;
    if fence && fsx::kind(entry)? != fsx::Kind::Dir {
        Context::new(entry)?.prepare(|_| Ok(()))?;
    }
    if fsx::kind(entry)? == fsx::Kind::Dir {
        Context::new(entry)?.raise_capacity()?;
    }
    Ok(WriteTarget {
        path: data_path(entry)?,
        _guard: guard,
    })
}

/// Keep capacity selection and the caller's actual data write under the same process-shared
/// lock, including when separate servers share a store directory.
pub(crate) struct WriteTarget {
    path: PathBuf,
    _guard: WriterLock,
}

/// Release the logical writer even while a forked child retains its file description.
struct WriterLock(File);

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl std::ops::Deref for WriteTarget {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for WriteTarget {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

fn writer_lock(entry: &Path) -> Result<WriterLock> {
    let context = Context::new(entry)?;
    let file: std::fs::File = aterm_uds::kept_out(|| {
        syscall(fd::openat(
            &context.parent,
            format!("{}.writing", context.name).as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        ))
        .map(File::from)
    })?;
    private_file(&file, 0)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "exclusive records writer lock; WriterLock releases it by LOCK_UN before close"
    )]
    file.lock()?;
    Ok(WriterLock(file))
}

fn check_capacity(required: usize) -> Result<()> {
    let available = crate::membership_log::RecordBudgets::current().held;
    if required > available {
        return Err(refused(&format!(
            "this custody requires capacity {required}; this reader holds {available} records"
        )));
    }
    Ok(())
}

/// The readiness pin beside `entry` (`records.layout`): where it stands, the data file is the one
/// inside the entry ([`data_path`]).
pub(crate) fn pin_path(entry: &Path) -> Result<PathBuf> {
    side_path(entry, "layout")
}

pub(crate) fn intent_path(entry: &Path) -> Result<PathBuf> {
    side_path(entry, "preparing")
}

fn side_path(entry: &Path, suffix: &str) -> Result<PathBuf> {
    let name = entry
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or_else(|| refused("invalid records entry name"))?;
    Ok(entry.with_file_name(format!("{name}.{suffix}")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Intent,
    Copied,
    Prepared,
    Exchanged,
    Pinned,
}

#[cfg(test)]
pub(crate) fn interrupt_after_exchange(entry: &Path) -> Result<()> {
    Context::new(entry)?.prepare(|step| {
        if step == Step::Exchanged {
            Err(refused("test interruption after exchange"))
        } else {
            Ok(())
        }
    })
}

struct Context {
    entry: PathBuf,
    name: String,
    pin: String,
    intent: String,
    parent: File,
}

impl Context {
    fn new(entry: &Path) -> Result<Self> {
        let parent_path = entry
            .parent()
            .ok_or_else(|| refused("no parent directory"))?;
        let metadata = std::fs::symlink_metadata(parent_path)?;
        if !metadata.is_dir() || metadata.uid() != fsx::euid() || metadata.mode() & 0o022 != 0 {
            return Err(refused("the parent directory is not safely owned"));
        }
        let canonical = parent_path.canonicalize()?;
        let parent = File::from(syscall(fd::openat(
            fd::CWD,
            canonical.as_path(),
            dir_flags(),
            Mode::empty(),
        ))?);
        let opened = parent.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(refused("the parent directory changed while opening"));
        }
        let pin = pin_path(entry)?;
        let intent = intent_path(entry)?;
        Ok(Self {
            entry: entry.to_path_buf(),
            name: entry.file_name().unwrap().to_str().unwrap().to_owned(),
            pin: pin.file_name().unwrap().to_str().unwrap().to_owned(),
            intent: intent.file_name().unwrap().to_str().unwrap().to_owned(),
            parent,
        })
    }

    fn directory(&self) -> Result<File> {
        let directory = File::from(syscall(fd::openat(
            &self.parent,
            self.name.as_str(),
            dir_flags(),
            Mode::empty(),
        ))?);
        let metadata = directory.metadata()?;
        if metadata.uid() != fsx::euid() || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused("the layout directory is not private and owned"));
        }
        Ok(directory)
    }

    fn header(&self, directory: &File) -> Result<Layout> {
        #[cfg(test)]
        fsx::count_read(&self.entry.join(HEADER));
        let wire = read_file(directory, HEADER, META_LIMIT)?;
        self.decode_layout(&wire)
    }

    fn decode_layout(&self, wire: &[u8]) -> Result<Layout> {
        let header = Layout::decode(wire)?;
        if header.entry != Sha256::digest(self.name.as_bytes()) {
            return Err(refused("the layout belongs to another records entry"));
        }
        check_capacity(header.capacity)?;
        Ok(header)
    }

    fn intent(&self) -> Result<Option<Layout>> {
        #[cfg(test)]
        fsx::count_read(&self.entry.with_file_name(&self.intent));
        optional_regular(&self.parent, &self.intent, META_LIMIT)?
            .map(|mut file| self.decode_layout(&read_bounded(&mut file, META_LIMIT)?))
            .transpose()
    }

    fn original(&self) -> Result<Option<File>> {
        optional_regular(
            &self.parent,
            &self.name,
            crate::membership_log::MAX_RECORDS_STREAM,
        )
    }

    fn original_for(&self, header: &Layout) -> Result<Option<File>> {
        match (self.original()?, header.original) {
            (Some(mut file), Some((dev, ino))) => {
                let metadata = file.metadata()?;
                if (metadata.dev(), metadata.ino()) != (dev, ino)
                    || copy_digest(&mut file, &mut std::io::sink())? != (header.len, header.digest)
                {
                    return Err(refused(
                        "the original changed after preparation was declared",
                    ));
                }
                file.seek(SeekFrom::Start(0))?;
                Ok(Some(file))
            }
            (None, None) => Ok(None),
            _ => Err(refused("the intended original no longer matches")),
        }
    }

    fn active(&self) -> Result<usize> {
        self.active_under(|| {
            #[cfg(test)]
            fsx::count_read(&self.entry.with_file_name(&self.pin));
            read_file(&self.parent, &self.pin, META_LIMIT)
        })
    }

    /// The pinned capacity, with `pin` answering the pin's bytes once the layout it pins holds.
    fn active_under(&self, pin: impl FnOnce() -> Result<Vec<u8>>) -> Result<usize> {
        let directory = self.directory()?;
        let header = self.header(&directory)?;
        let capacity = header.pinned_capacity(&pin()?)?;
        check_capacity(capacity)?;
        #[cfg(test)]
        fsx::count_read(&self.entry.join(DATA));
        regular(&directory, DATA, crate::membership_log::MAX_RECORDS_STREAM)?;
        Ok(capacity)
    }

    /// [`private_file`]'s checks of the pin in this parent, for a reader that read it within its
    /// bound.
    fn pin_is_private(&self) -> Result<()> {
        let stat = match fd::fstatat(&self.parent, self.pin.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) if error.is_noent() => return Err(refused("a layout file is missing")),
            Err(error) => return syscall(Err(error)),
        };
        if !FileType::from_raw_mode(stat.st_mode).is_file()
            || stat.st_uid != fsx::euid()
            || stat.st_mode & 0o7777 != 0o600
            || stat.st_nlink != 1
        {
            return Err(refused("a layout file is not bounded, private and owned"));
        }
        Ok(())
    }

    fn raise_capacity(&self) -> Result<()> {
        let before = self.active()?;
        let capacity = crate::membership_log::RecordBudgets::current().held;
        if capacity > before {
            let header = self.header(&self.directory()?)?;
            fsx::write_atomic(
                &self.entry.with_file_name(&self.pin),
                &header.pin(capacity),
                0o600,
            )?;
        }
        Ok(())
    }

    fn prepare(&self, after: impl Fn(Step) -> Result<()>) -> Result<()> {
        if fsx::kind(&self.entry)? == fsx::Kind::Dir {
            return self.finish(after);
        }
        let header = match self.intent()? {
            Some(header) => header,
            None => {
                let mut original = self.original()?;
                let identity = original
                    .as_ref()
                    .map(|file| file.metadata())
                    .transpose()?
                    .map(|metadata| (metadata.dev(), metadata.ino()));
                let (len, digest) = match original.as_mut() {
                    Some(file) => copy_digest(file, &mut std::io::sink())?,
                    None => (0, Sha256::digest(b"")),
                };
                let header = Layout {
                    entry: Sha256::digest(self.name.as_bytes()),
                    nonce: aterm_uds::rand::hex_token::<16>()?,
                    len,
                    digest,
                    original: identity,
                    capacity: crate::membership_log::RecordBudgets::current().held,
                };
                install_file(&self.parent, &self.intent, &header.encode())?;
                aterm_durable::durable(&self.parent)?;
                header
            }
        };
        after(Step::Intent)?;
        let mut original = self.original_for(&header)?;
        let old_name = format!(".aterm-records-{}", header.nonce);
        match fd::mkdirat(&self.parent, old_name.as_str(), Mode::RWXU) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == libc::EEXIST => {}
            Err(error) => return syscall(Err(error)),
        }
        let directory = File::from(syscall(fd::openat(
            &self.parent,
            old_name.as_str(),
            dir_flags(),
            Mode::empty(),
        ))?);
        let metadata = directory.metadata()?;
        if metadata.uid() != fsx::euid() || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused(
                "the preparation directory is not private and owned",
            ));
        }
        for child in std::fs::read_dir(self.entry.with_file_name(&old_name))? {
            let child = child?;
            let name = child.file_name();
            if ![DATA, HEADER, "current.building", "layout.building"]
                .iter()
                .any(|allowed| name == std::ffi::OsStr::new(*allowed))
            {
                return Err(refused(
                    "the preparation directory contains an unknown file",
                ));
            }
        }
        match optional_regular(&directory, DATA, crate::membership_log::MAX_RECORDS_STREAM)? {
            Some(mut data) => {
                if copy_digest(&mut data, &mut std::io::sink())? != (header.len, header.digest) {
                    return Err(refused("prepared data does not match its intent"));
                }
            }
            None => {
                let mut data = scratch(
                    &directory,
                    "current.building",
                    crate::membership_log::MAX_RECORDS_STREAM,
                )?;
                let observed = match original.as_mut() {
                    Some(file) => copy_digest(file, &mut data)?,
                    None => (0, Sha256::digest(b"")),
                };
                if observed != (header.len, header.digest) {
                    return Err(refused("the original changed while its copy was prepared"));
                }
                aterm_durable::durable(&data)?;
                syscall(fd::renameat_with(
                    &directory,
                    "current.building",
                    &directory,
                    DATA,
                    RenameFlags::NOREPLACE,
                ))?;
            }
        }
        aterm_durable::durable(&directory)?;
        after(Step::Copied)?;
        install_file(&directory, HEADER, &header.encode())?;
        aterm_durable::durable(&directory)?;
        aterm_durable::durable(&self.parent)?;
        after(Step::Prepared)?;
        self.original_for(&header)?;
        syscall(fd::renameat_with(
            &self.parent,
            old_name.as_str(),
            &self.parent,
            self.name.as_str(),
            if header.original.is_some() {
                RenameFlags::EXCHANGE
            } else {
                RenameFlags::NOREPLACE
            },
        ))?;
        aterm_durable::durable(&self.parent)?;
        after(Step::Exchanged)?;
        self.finish(after)
    }

    fn finish(&self, after: impl Fn(Step) -> Result<()>) -> Result<()> {
        let directory = self.directory()?;
        let header = self.header(&directory)?;
        if self
            .intent()?
            .is_none_or(|intent| intent.encode() != header.encode())
        {
            return Err(refused("the layout has no matching preparation intent"));
        }
        let mut current = regular(&directory, DATA, crate::membership_log::MAX_RECORDS_STREAM)?;
        if copy_digest(&mut current, &mut std::io::sink())? != (header.len, header.digest) {
            return Err(refused("prepared custody changed before pin publication"));
        }
        let old_name = format!(".aterm-records-{}", header.nonce);
        match header.original {
            Some((dev, ino)) => {
                let mut old = regular(
                    &self.parent,
                    &old_name,
                    crate::membership_log::MAX_RECORDS_STREAM,
                )?;
                let metadata = old.metadata()?;
                if (metadata.dev(), metadata.ino()) != (dev, ino)
                    || copy_digest(&mut old, &mut std::io::sink())? != (header.len, header.digest)
                {
                    return Err(refused(
                        "the retained predecessor changed during preparation",
                    ));
                }
            }
            None => {
                if fsx::kind(&self.entry.with_file_name(&old_name))? != fsx::Kind::Missing {
                    return Err(refused("an unexpected predecessor appeared"));
                }
            }
        }
        // Install within the parent directory. Moving a pin out of the layout would need
        // two directory flushes and could lose both names if a crash split those flushes.
        install_file(&self.parent, &self.pin, &header.pin(header.capacity))?;
        after(Step::Pinned)?;
        self.active().map(|_| ())
    }
}

struct Layout {
    entry: [u8; 32],
    nonce: String,
    len: usize,
    digest: [u8; 32],
    original: Option<(u64, u64)>,
    capacity: usize,
}

impl Layout {
    fn encode(&self) -> Vec<u8> {
        let original = self
            .original
            .map_or_else(|| "-".to_owned(), |(dev, ino)| format!("{dev} {ino}"));
        format!(
            "{MAGIC}\nentry {}\nnonce {}\ninitial {} {}\noriginal {original}\ncapacity {}\n",
            hex(&self.entry),
            self.nonce,
            self.len,
            hex(&self.digest),
            self.capacity
        )
        .into_bytes()
    }

    fn pin(&self, capacity: usize) -> Vec<u8> {
        format!(
            "{PIN_MAGIC}\n{}\ncapacity {capacity}\n",
            hex(&Sha256::digest(self.encode()))
        )
        .into_bytes()
    }

    fn pinned_capacity(&self, wire: &[u8]) -> Result<usize> {
        let text = std::str::from_utf8(wire).map_err(|_| refused("invalid pin text"))?;
        let capacity = text
            .lines()
            .nth(2)
            .and_then(|line| line.strip_prefix("capacity "))
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|capacity| *capacity >= self.capacity && *capacity <= u32::MAX as usize)
            .ok_or_else(|| refused("invalid pinned capacity"))?;
        if self.pin(capacity) != wire {
            return Err(refused("the layout pin does not match"));
        }
        Ok(capacity)
    }

    fn decode(wire: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(wire).map_err(|_| refused("invalid layout text"))?;
        let rows: Vec<_> = text.lines().collect();
        if rows.len() != 6 || rows[0] != MAGIC {
            return Err(refused("invalid layout header"));
        }
        let entry = rows[1]
            .strip_prefix("entry ")
            .and_then(unhex)
            .ok_or_else(|| refused("invalid entry digest"))?;
        let nonce = rows[2]
            .strip_prefix("nonce ")
            .filter(|nonce| {
                nonce.len() == 32
                    && nonce
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or_else(|| refused("invalid layout generation"))?
            .to_owned();
        let (len, digest) = rows[3]
            .strip_prefix("initial ")
            .and_then(|s| s.split_once(' '))
            .ok_or_else(|| refused("invalid initial custody"))?;
        let len = len.parse().map_err(|_| refused("invalid custody length"))?;
        if len > crate::membership_log::MAX_RECORDS_STREAM {
            return Err(refused("custody exceeds its bound"));
        }
        let digest = unhex(digest).ok_or_else(|| refused("invalid custody digest"))?;
        let original = rows[4]
            .strip_prefix("original ")
            .ok_or_else(|| refused("invalid predecessor"))?;
        let original = if original == "-" {
            None
        } else {
            let (dev, ino) = original
                .split_once(' ')
                .ok_or_else(|| refused("invalid predecessor identity"))?;
            Some((
                dev.parse()
                    .map_err(|_| refused("invalid predecessor device"))?,
                ino.parse()
                    .map_err(|_| refused("invalid predecessor inode"))?,
            ))
        };
        let capacity = rows[5]
            .strip_prefix("capacity ")
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|capacity| *capacity > 0 && *capacity <= u32::MAX as usize)
            .ok_or_else(|| refused("invalid preparation capacity"))?;
        let header = Self {
            entry,
            nonce,
            len,
            digest,
            original,
            capacity,
        };
        if header.encode() != wire {
            return Err(refused("noncanonical layout header"));
        }
        Ok(header)
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn regular(parent: &File, name: &str, max: usize) -> Result<File> {
    optional_regular(parent, name, max)?.ok_or_else(|| refused("a layout file is missing"))
}

fn optional_regular(parent: &File, name: &str, max: usize) -> Result<Option<File>> {
    let opened = fd::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    );
    let file = match opened {
        Ok(file) => File::from(file),
        Err(error) if error.is_noent() => return Ok(None),
        Err(error) => return syscall(Err(error)),
    };
    private_file(&file, max)?;
    Ok(Some(file))
}

fn private_file(file: &File, max: usize) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != fsx::euid()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > max as u64
    {
        return Err(refused("a layout file is not bounded, private and owned"));
    }
    Ok(())
}

fn read_file(parent: &File, name: &str, max: usize) -> Result<Vec<u8>> {
    read_bounded(&mut regular(parent, name, max)?, max)
}

fn read_bounded(file: &mut File, max: usize) -> Result<Vec<u8>> {
    let len = usize::try_from(file.metadata()?.len())
        .unwrap_or(max)
        .min(max);
    let mut bytes = Vec::with_capacity(len.saturating_add(1));
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(refused("a layout file grew past its bound"));
    }
    Ok(bytes)
}

/// Publish a bounded piece of declared preparation once, using one reusable scratch file.
fn install_file(parent: &File, name: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > META_LIMIT {
        return Err(refused("preparation metadata exceeds its bound"));
    }
    if let Some(mut existing) = optional_regular(parent, name, META_LIMIT)? {
        if read_bounded(&mut existing, META_LIMIT)? == bytes {
            return Ok(());
        }
        return Err(refused(
            "existing preparation metadata differs from its intent",
        ));
    }
    let temporary = format!("{name}.building");
    let mut file = scratch(parent, &temporary, META_LIMIT)?;
    file.write_all(bytes)?;
    aterm_durable::durable(&file)?;
    syscall(fd::renameat_with(
        parent,
        temporary.as_str(),
        parent,
        name,
        RenameFlags::NOREPLACE,
    ))?;
    aterm_durable::durable(parent)?;
    Ok(())
}

/// Only bounded private preparation scratch is rewritten; published files are never truncated.
fn scratch(parent: &File, name: &str, max: usize) -> Result<File> {
    let mut file = File::from(syscall(fd::openat(
        parent,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    ))?);
    private_file(&file, max)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// Copy and hash within the same record-byte bound, with one fixed working buffer.
fn copy_digest(input: &mut File, output: &mut dyn std::io::Write) -> Result<(usize, [u8; 32])> {
    let mut buffer = [0; 64 * 1024];
    let mut len = 0usize;
    let mut digest = Sha256::new();
    loop {
        let n = match input.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            read => read?,
        };
        if n == 0 {
            break;
        }
        len = len
            .checked_add(n)
            .filter(|len| *len <= crate::membership_log::MAX_RECORDS_STREAM)
            .ok_or_else(|| refused("a record file grew past its bound"))?;
        output.write_all(&buffer[..n])?;
        digest.update(&buffer[..n]);
    }
    Ok((len, digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; N];
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn fixture() -> (aterm_tempfile::TempDir, PathBuf) {
        let root = aterm_tempfile::tempdir().unwrap();
        let entry = root.path().join("records");
        fsx::write_atomic(&entry, b"original history", 0o600).unwrap();
        (root, entry)
    }

    fn stop_at(context: &Context, cut: Step) -> Result<()> {
        context.prepare(|step| {
            if step == cut {
                Err(refused("interrupted test transition"))
            } else {
                Ok(())
            }
        })
    }

    fn prepare_path(entry: &Path) -> PathBuf {
        write_path(entry, true).unwrap().path.clone()
    }

    fn budget(held: usize) -> crate::membership_log::RecordBudgets {
        crate::membership_log::RecordBudgets {
            weighed: 16,
            keyed: 128,
            held,
        }
    }

    /// A reader resolves a pinned layout once while every file `data_path` judges stands as it
    /// did ([`Judgements`]): another selection reads its header no more. Each change `data_path`
    /// refuses that the selection's own witnesses do not see (the parent opened to its group, the
    /// header rewritten in place, the data file opened to others or grown past its bound, a
    /// smaller capacity) is refused again.
    /// NEGATIVE CONTROLS (2026-11-08): a judgement without the parent's mode takes the layout
    /// after the parent's change, without the header's `lstat` after the header's, without the
    /// data file's `lstat` after either of the data file's, without the data file's stamp alone
    /// after its growth, and without the capacity after the smaller one. APFS moves a file's
    /// inode-change time at each `chmod`, so no change here shows a mode the stamp kept, which
    /// [`fsx::Seen`] holds for a coarser clock.
    #[test]
    fn a_reader_resolves_an_unchanged_layout_once_and_a_changed_one_again() {
        /// A change to a judged layout, the capacity its reader then holds, and what makes it.
        struct Change {
            what: &'static str,
            held: usize,
            make: fn(&Path),
        }
        fn mode(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let changes = [
            Change {
                what: "the parent opened to its group",
                held: 32,
                make: |entry| mode(entry.parent().unwrap(), 0o770),
            },
            Change {
                what: "the header rewritten in place",
                held: 32,
                make: |entry| std::fs::write(entry.join(HEADER), b"not a layout").unwrap(),
            },
            Change {
                what: "the data file opened to others",
                held: 32,
                make: |entry| mode(&entry.join(DATA), 0o644),
            },
            Change {
                what: "the data file grown past its bound",
                held: 32,
                make: |entry| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(entry.join(DATA))
                        .unwrap()
                        .set_len(crate::membership_log::MAX_RECORDS_STREAM as u64 + 1)
                        .unwrap();
                },
            },
            Change {
                what: "a smaller capacity",
                held: 16,
                make: |_| {},
            },
        ];
        for Change {
            what: change,
            held,
            make,
        } in changes
        {
            let (_root, entry) = fixture();
            let data = budget(32).within(|| prepare_path(&entry));
            let judgements = Judgements::new();
            budget(32).within(|| {
                assert_eq!(judgements.selected(&entry).unwrap().path(), data.as_path());
                let read = fsx::reads_of(&entry.join(HEADER));
                assert_eq!(judgements.selected(&entry).unwrap().path(), data.as_path());
                assert_eq!(fsx::reads_of(&entry.join(HEADER)), read, "read again");
            });
            make(&entry);
            budget(held).within(|| {
                assert!(data_path(&entry).is_err(), "{change}: data_path takes it");
                assert!(
                    judgements.selected(&entry).is_err(),
                    "{change}: a reader takes it"
                );
            });
        }
    }

    #[test]
    fn a_stale_snapshot_never_prepares_or_raises_the_layout() {
        for fenced in [false, true] {
            let (_root, entry) = fixture();
            let path = if fenced {
                budget(32).within(|| prepare_path(&entry))
            } else {
                entry.clone()
            };
            let expected = fsx::stamp(&path);
            fsx::write_atomic(&path, b"another writer's newer history", 0o600).unwrap();
            let before = std::fs::read(&path).unwrap();
            budget(64).within(|| {
                assert!(write_path_if_unchanged(&entry, true, expected).is_err());
            });
            budget(32).within(|| {
                assert_eq!(data_path(&entry).unwrap(), path);
                assert_eq!(std::fs::read(&path).unwrap(), before);
            });
            assert_eq!(pin_path(&entry).unwrap().exists(), fenced);
        }
    }

    #[test]
    fn an_absent_snapshot_refuses_a_new_file_before_preparing_a_layout() {
        let root = aterm_tempfile::tempdir().unwrap();
        let entry = root.path().join("records");
        fsx::write_atomic(&entry, b"new history", 0o600).unwrap();
        assert!(write_path_if_unchanged(&entry, true, None).is_err());
        assert_eq!(std::fs::read(&entry).unwrap(), b"new history");
        assert!(!pin_path(&entry).unwrap().exists());
        assert!(!intent_path(&entry).unwrap().exists());
    }

    #[test]
    fn a_larger_writer_fences_format_aware_smaller_readers_before_changing_data() {
        let (_root, entry) = fixture();
        let data = budget(32).within(|| prepare_path(&entry));
        budget(64).within(|| {
            let target = write_path(&entry, false).unwrap();
            assert_eq!(std::fs::read(&*target).unwrap(), b"original history");
            assert!(budget(32).within(|| data_path(&entry)).is_err());
            fsx::write_atomic(&target, b"new history", 0o600).unwrap();
        });
        budget(32).within(|| {
            assert!(data_path(&entry).is_err());
            assert!(write_path(&entry, false).is_err());
        });
        budget(64).within(|| {
            assert_eq!(data_path(&entry).unwrap(), data);
            assert_eq!(std::fs::read(&data).unwrap(), b"new history");
        });
    }

    #[test]
    fn a_smaller_writer_cannot_finish_a_larger_pending_layout() {
        for cut in [Step::Intent, Step::Exchanged] {
            let (_root, entry) = fixture();
            budget(64).within(|| {
                assert!(stop_at(&Context::new(&entry).unwrap(), cut).is_err());
            });
            budget(32).within(|| {
                assert!(recover_pending(&entry).is_err());
                assert!(!pin_path(&entry).unwrap().exists());
            });
            budget(64).within(|| {
                recover_pending(&entry).unwrap();
                assert_eq!(
                    std::fs::read(data_path(&entry).unwrap()).unwrap(),
                    b"original history"
                );
            });
        }
    }

    #[test]
    fn the_write_target_keeps_a_process_shared_lock_until_the_write_finishes() {
        let (_root, entry) = fixture();
        let target = write_path(&entry, true).unwrap();
        // A sibling fork retains this open file description until it execs. Keep a copy
        // here so releasing the logical writer cannot depend on that scheduling window.
        let inherited = target._guard.0.try_clone().unwrap();
        let other = fsx::open_lock(&entry.with_file_name("records.writing")).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "probe the held writer lock without waiting"
        )]
        let during = other.try_lock();
        assert!(matches!(during, Err(std::fs::TryLockError::WouldBlock)));
        fsx::write_atomic(&target, b"written under lock", 0o600).unwrap();
        drop(target);
        #[expect(
            clippy::disallowed_methods,
            reason = "prove dropping the write target released the writer lock"
        )]
        let after = other.try_lock();
        after.unwrap();
        other.unlock().unwrap();
        drop(inherited);
    }

    #[test]
    fn layout_preserves_the_original_and_selects_later_writes() {
        let (root, entry) = fixture();
        let data = prepare_path(&entry);
        assert_eq!(std::fs::read(&data).unwrap(), b"original history");
        let context = Context::new(&entry).unwrap();
        let layout = context.header(&context.directory().unwrap()).unwrap();
        let original = root.path().join(format!(".aterm-records-{}", layout.nonce));
        assert_eq!(std::fs::read(&original).unwrap(), b"original history");
        fsx::write_atomic(&data, b"expanded history", 0o600).unwrap();
        assert_eq!(prepare_path(&entry), data);
        recover_pending(&entry).unwrap();
        assert_eq!(
            std::fs::read(data_path(&entry).unwrap()).unwrap(),
            b"expanded history"
        );
        assert_eq!(std::fs::read(original).unwrap(), b"original history");
        assert!(fsx::read_owned(&entry, 1024).is_err());
        assert!(fsx::write_atomic(&entry, b"old writer", 0o600).is_err());
    }

    #[test]
    fn interruption_keeps_custody_and_only_a_writer_finishes_a_pending_transition() {
        for cut in [
            Step::Intent,
            Step::Copied,
            Step::Prepared,
            Step::Exchanged,
            Step::Pinned,
        ] {
            let (_root, entry) = fixture();
            let context = Context::new(&entry).unwrap();
            assert!(stop_at(&context, cut).is_err());
            let before = fsx::kind(&pin_path(&entry).unwrap()).unwrap();
            let read = data_path(&entry);
            assert_eq!(read.is_err(), cut == Step::Exchanged);
            assert_eq!(fsx::kind(&pin_path(&entry).unwrap()).unwrap(), before);
            recover_pending(&entry).unwrap();
            let selected = data_path(&entry).unwrap();
            assert_eq!(std::fs::read(&selected).unwrap(), b"original history");
            assert_ne!(selected, entry);
            let selected = prepare_path(&entry);
            assert_eq!(std::fs::read(&selected).unwrap(), b"original history");
        }
    }

    #[test]
    fn preparation_retries_reuse_one_declared_generation() {
        let (root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        let mut generation = None;
        for _ in 0..3 {
            assert!(stop_at(&context, Step::Prepared).is_err());
            let intent = context.intent().unwrap().unwrap();
            if let Some(before) = &generation {
                assert_eq!(before, &intent.nonce);
            }
            generation = Some(intent.nonce);
            let copies = std::fs::read_dir(root.path())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".aterm-records-")
                })
                .count();
            assert_eq!(copies, 1);
            assert_eq!(std::fs::read(&entry).unwrap(), b"original history");
        }
        recover_pending(&entry).unwrap();
        assert_eq!(
            std::fs::read(data_path(&entry).unwrap()).unwrap(),
            b"original history"
        );
    }

    #[test]
    fn an_interrupted_derived_copy_resumes_from_its_existing_intent() {
        let (_root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        assert!(stop_at(&context, Step::Intent).is_err());
        let intent = context.intent().unwrap().unwrap();
        let staged = entry.with_file_name(format!(".aterm-records-{}", intent.nonce));
        fsx::ensure_private_dir(&staged).unwrap();
        fsx::write_atomic(&staged.join("current.building"), b"partial", 0o600).unwrap();
        assert_eq!(data_path(&entry).unwrap(), entry);
        recover_pending(&entry).unwrap();
        assert_eq!(
            std::fs::read(data_path(&entry).unwrap()).unwrap(),
            b"original history"
        );
    }

    #[test]
    fn an_interrupted_pin_write_resumes_without_moving_the_prepared_files() {
        let (_root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        assert!(stop_at(&context, Step::Exchanged).is_err());
        let current = fsx::stamp(&entry.join(DATA)).unwrap();
        let header = fsx::stamp(&entry.join(HEADER)).unwrap();
        fsx::write_atomic(
            &entry.with_file_name("records.layout.building"),
            b"partial pin",
            0o600,
        )
        .unwrap();
        assert!(data_path(&entry).is_err());
        recover_pending(&entry).unwrap();
        assert_eq!(fsx::stamp(&entry.join(DATA)), Some(current));
        assert_eq!(fsx::stamp(&entry.join(HEADER)), Some(header));
        assert_eq!(
            std::fs::read(data_path(&entry).unwrap()).unwrap(),
            b"original history"
        );
    }

    #[test]
    fn an_intent_does_not_turn_a_missing_original_into_empty_custody() {
        let (root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        assert!(stop_at(&context, Step::Intent).is_err());
        let saved = root.path().join("saved-original");
        std::fs::rename(&entry, &saved).unwrap();
        assert!(data_path(&entry).is_err());
        assert!(recover_pending(&entry).is_err());
        assert_eq!(std::fs::read(saved).unwrap(), b"original history");
    }

    #[test]
    fn an_active_pin_never_falls_back_after_current_or_layout_is_lost() {
        let (root, entry) = fixture();
        let data = prepare_path(&entry);
        std::fs::rename(&data, entry.join("saved-current")).unwrap();
        assert!(data_path(&entry).is_err());
        assert!(recover_pending(&entry).is_err());
        let saved = root.path().join("saved-layout");
        std::fs::rename(&entry, &saved).unwrap();
        assert!(data_path(&entry).is_err());
        fsx::write_atomic(&entry, b"an old flat snapshot", 0o600).unwrap();
        assert!(data_path(&entry).is_err());
        assert!(write_path(&entry, false).is_err());
        assert_eq!(
            std::fs::read(saved.join("saved-current")).unwrap(),
            b"original history"
        );
    }

    #[test]
    fn a_changed_predecessor_refuses_pending_recovery_without_losing_either_copy() {
        let (root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        assert!(stop_at(&context, Step::Exchanged).is_err());
        let layout = context.header(&context.directory().unwrap()).unwrap();
        let original = root.path().join(format!(".aterm-records-{}", layout.nonce));
        fsx::write_atomic(&original, b"a concurrent old write", 0o600).unwrap();
        assert!(recover_pending(&entry).is_err());
        assert!(data_path(&entry).is_err());
        assert_eq!(
            std::fs::read(entry.join(DATA)).unwrap(),
            b"original history"
        );
        assert_eq!(std::fs::read(original).unwrap(), b"a concurrent old write");
        assert!(!pin_path(&entry).unwrap().exists());
    }

    /// NEGATIVE CONTROLS (2026-11-05): with the entry's `lstat` replaced by `None` in
    /// `selection_witnesses`, the entry's arm confirms the replaced entry
    /// (`the selection still stood`); with the intent's stamp replaced the same way, the intent's
    /// arm does.
    #[test]
    fn a_selection_resolves_again_once_its_entry_or_its_intent_moved() {
        let refusal = |held: &Selected| {
            held.confirm()
                .map(|()| "the selection still stood".to_owned())
                .unwrap_or_else(|error| error.to_string())
        };

        // What an interrupted transition leaves, in place of the file the reader selected.
        let (root, entry) = fixture();
        let held = selected(&entry).unwrap();
        held.confirm().unwrap();
        std::fs::rename(&entry, root.path().join("aside")).unwrap();
        fsx::ensure_private_dir(&entry).unwrap();
        let refused = refusal(&held);
        assert!(
            refused.contains("an interrupted transition needs its writer lock"),
            "{refused}"
        );

        // A preparation that names the original, appearing over the entry the reader found empty.
        let (root, entry) = fixture();
        assert!(stop_at(&Context::new(&entry).unwrap(), Step::Intent).is_err());
        let intent = intent_path(&entry).unwrap();
        let saved = root.path().join("saved-intent");
        std::fs::rename(&intent, &saved).unwrap();
        std::fs::rename(&entry, root.path().join("saved-original")).unwrap();
        let held = selected(&entry).unwrap();
        held.confirm().unwrap();
        std::fs::rename(&saved, &intent).unwrap();
        let refused = refusal(&held);
        assert!(
            refused.contains("the intended original records file is missing"),
            "{refused}"
        );
    }

    /// NEGATIVE CONTROLS (2026-11-05), `selected`'s witness arm loosened each way: with any kind
    /// at the entry, the symlink selects the entry (`a symlink`, `left: Ok(entry), right: Err(..)`);
    /// with `Dir` or `Other` admitted alone, the directory or the FIFO does; with the intent's slot
    /// `_`, the missing original does; with the pin's slot `_`, the pin beside a file does.
    #[test]
    fn a_selection_from_its_witnesses_answers_as_data_path_does() {
        let answer = |path: Result<PathBuf>| path.map_err(|error| error.to_string());
        let shaped = |shape: &str, make: &dyn Fn(&Path, &Path), refused: bool| {
            let (root, entry) = fixture();
            make(root.path(), &entry);
            let resolved = answer(data_path(&entry));
            assert_eq!(resolved.is_err(), refused, "{shape}: {resolved:?}");
            assert_eq!(
                answer(selected(&entry).map(|held| held.path().to_path_buf())),
                resolved,
                "{shape}"
            );
        };
        shaped("a regular file", &|_, _| {}, false);
        shaped(
            "nothing",
            &|_, entry| std::fs::remove_file(entry).unwrap(),
            false,
        );
        shaped(
            "a symlink",
            &|root, entry| {
                let kept = root.join("kept");
                std::fs::rename(entry, &kept).unwrap();
                symlink(&kept, entry).unwrap();
            },
            true,
        );
        shaped(
            "a directory",
            &|_, entry| {
                std::fs::remove_file(entry).unwrap();
                fsx::ensure_private_dir(entry).unwrap();
            },
            true,
        );
        shaped(
            "a FIFO",
            &|_, entry| {
                std::fs::remove_file(entry).unwrap();
                let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
                    .arg(entry)
                    .status()
                    .expect("mkfifo");
                assert!(made.success());
            },
            true,
        );
        shaped(
            "an intent whose original is gone",
            &|root, entry| {
                assert!(stop_at(&Context::new(entry).unwrap(), Step::Intent).is_err());
                std::fs::rename(entry, root.join("saved-original")).unwrap();
            },
            true,
        );
        shaped(
            "a pinned layout",
            &|_, entry| drop(write_path(entry, true).unwrap()),
            false,
        );
        shaped(
            "a pin beside a regular file",
            &|_, entry| {
                fsx::write_atomic(&pin_path(entry).unwrap(), b"no pin", 0o600).unwrap();
            },
            true,
        );
    }

    #[test]
    fn symlinks_hard_links_and_public_layout_files_are_refused() {
        for slot in [DATA, HEADER, "pin"] {
            let (_root, entry) = fixture();
            write_path(&entry, true).unwrap();
            let path = if slot == "pin" {
                pin_path(&entry).unwrap()
            } else {
                entry.join(slot)
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(data_path(&entry).is_err(), "public {slot}");
        }
        // NEGATIVE CONTROL (2026-11-05): with `context.pin_is_private()?` gone from `data_path`,
        // both arms resolve, since `fsx::read_private` admits either pin.
        for read_only in [false, true] {
            let (root, entry) = fixture();
            write_path(&entry, true).unwrap();
            let pin = pin_path(&entry).unwrap();
            if read_only {
                std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o400)).unwrap();
            } else {
                std::fs::hard_link(&pin, root.path().join("kept-pin")).unwrap();
            }
            assert!(data_path(&entry).is_err(), "read only: {read_only}");
            assert!(selected(&entry).is_err(), "read only: {read_only}");
        }
        for link in [false, true] {
            let (_root, entry) = fixture();
            let data = prepare_path(&entry);
            let kept = entry.join("kept-current");
            std::fs::rename(&data, &kept).unwrap();
            if link {
                symlink(&kept, &data).unwrap();
            } else {
                std::fs::hard_link(&kept, &data).unwrap();
            }
            assert!(data_path(&entry).is_err());
            assert_eq!(std::fs::read(kept).unwrap(), b"original history");
        }
    }

    #[test]
    fn a_fresh_layout_has_no_invented_predecessor() {
        let root = aterm_tempfile::tempdir().unwrap();
        let entry = root.path().join("records");
        let data = prepare_path(&entry);
        assert!(std::fs::read(data).unwrap().is_empty());
        let context = Context::new(&entry).unwrap();
        let layout = context.header(&context.directory().unwrap()).unwrap();
        assert!(layout.original.is_none());
        assert!(
            !root
                .path()
                .join(format!(".aterm-records-{}", layout.nonce))
                .exists()
        );
    }

    #[test]
    fn an_old_open_descriptor_can_only_change_the_preserved_predecessor() {
        let (_root, entry) = fixture();
        let mut old = std::fs::OpenOptions::new()
            .write(true)
            .open(&entry)
            .unwrap();
        let data = prepare_path(&entry);
        old.write_all(b"OLD").unwrap();
        assert_eq!(
            std::fs::read(data_path(&entry).unwrap()).unwrap(),
            b"original history"
        );
        assert_eq!(std::fs::read(data).unwrap(), b"original history");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A records file becomes a directory before old local writers can truncate its history.
//! A durable intent makes retries reuse one preparation. The original file remains beside
//! the directory, and a separately published pin selects it only after validation, so a
//! missing current file never falls back to an earlier snapshot.
//! The pin's required reader capacity only rises, before data publication, so a smaller
//! format-aware build refuses a later larger history instead of shortening it.
//! The file and the prepared directory trade names by one exchange on Unix, and on Windows, which
//! has none, by three renames that never replace, carried on by the next writer where a cut
//! stopped them (`Context::exchange`).

use crate::{Error, Result, fsx};
use aterm_digest::Sha256;
use aterm_dirfd::{self as fd, Mode, OFlags, RenameFlags};
#[cfg(unix)]
use aterm_dirfd::{AtFlags, FileType};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Which rewrite made what a records entry holds (`rewrite::generation`): what a consumer that
/// keeps a place in the records keeps the place under, and what a write of the records file
/// says its store was written under ([`crate::membership_log::store::Written::generation`]).
/// Two that are equal name one order of the records the rewrite kept; the number alone does
/// not (`rewrite`'s "The generation is a number and the rewrite that made it"). The rewrite is
/// Unix only; an entry on any other platform is never rewritten, and is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordsGeneration {
    /// How many rewrites made what the entry holds: 0 for an entry never rewritten or absent.
    pub number: u64,
    /// The SHA-256 of the layout header the rewrite's pin names, which binds the data the
    /// rewrite was computed from and the data it wrote; `None` exactly where `number` is 0.
    pub rewrite: Option<[u8; 32]>,
}

/// The rewrite of a layout's data, the second transition (`docs/DESIGN-vault-log-2026-11-14.md`
/// step 4): dark, but for the generation a consumer reads (`rewrite::generation`). Its one
/// caller outside the tests is a Mac that reads format 2 taking a served root into its file
/// (`client::persist_stored`), which no running build is.
#[cfg(unix)]
pub(crate) mod rewrite;

const DATA: &str = "current";
const HEADER: &str = "layout";
const MAGIC: &str = "aterm-records-layout/1";
const PIN_MAGIC: &str = "aterm-records-pin/1";
const META_LIMIT: usize = 1024;
/// The most bytes a records file kept whole, or the data of the layout this transition makes of
/// it, holds: format 1's capacity ([`crate::crossing::capacity`], the storage class of byte
/// bound). The transition, its header and its pin (`/1`) are format 1's, as every build since
/// the fence reads them; a rewritten layout's data is held to its own format's
/// (`rewrite::bytes_of`).
const DATA_LIMIT: usize =
    crate::membership_log::Reading::Format(crate::crossing::FORMAT_ONE).capacity();

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
    /// Whether a reader of format 2 selected it at a rewritten layout (`rewrite::selected`),
    /// which [`data_path`] refuses: its confirmation is that reader's.
    #[cfg(unix)]
    rewritten: bool,
}

/// What stood at a selection's pin and entry, and at its intent where neither stands
/// ([`selection_witnesses`]): each one's kind, stamp, owner, mode and links, from one `lstat`
/// ([`fsx::Seen`]). The intent's slot is `None` wherever a pin or an entry stands, as
/// [`data_path`] reads the intent only where neither does.
type Witnesses = [Option<fsx::Seen>; 3];

impl Selected {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Hold the selection after reading its data: the pin and the entry, and the intent where the
    /// selection looked at it ([`selection_witnesses`]), still stand as they were found, or the
    /// selection is resolved again, with what that refuses.
    pub(crate) fn confirm(&self) -> Result<()> {
        #[cfg(unix)]
        if self.rewritten {
            return rewrite::confirm(&self.entry, &self.path);
        }
        if self.witnesses.is_some() && selection_witnesses(&self.entry, None).ok() == self.witnesses
        {
            return Ok(());
        }
        data_path(&self.entry).map(|_| ())
    }
}

/// Whether `witnesses` show no pin, and either a regular file at the entry (an intent beside it is
/// neither read nor looked at: [`selection_witnesses`]) or nothing at the entry and no intent: the
/// entry is then the data ([`data_path`]), and the witnesses hold all `data_path` looks at.
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

/// What stands at `entry`'s pin and at `entry`, and at its intent only where neither of those
/// stands, each from one `lstat` (`glance`'s where the caller looks through one): what
/// [`data_path`] looks at to select the data, which reads the intent only where no pin and
/// nothing at the entry stand. The intent's path is named first, so an entry whose name gives none
/// errors whatever stands. An arm of `data_path` that read the intent anywhere else would need
/// these witnesses widened to it.
fn selection_witnesses(entry: &Path, glance: Option<&fsx::Glance>) -> Result<Witnesses> {
    let intent = intent_path(entry)?;
    let pin = fsx::seen_if_present_in(glance, &pin_path(entry)?)?;
    let at_entry = fsx::seen_if_present_in(glance, entry)?;
    let intent = if pin.is_none() && at_entry.is_none() {
        fsx::seen_if_present_in(glance, &intent)?
    } else {
        None
    };
    Ok([pin, at_entry, intent])
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
    /// The pin and the entry, and the intent where neither of those stands
    /// ([`selection_witnesses`]). Where they show a plain records file or none ([`is_plain`]),
    /// these are all [`data_path`] looks at.
    witnesses: Witnesses,
    /// For anything else the selection takes (a pinned layout, or a transition it can still read
    /// through), the rest of what it judged ([`Judged`]).
    judged: Option<Box<Judged>>,
}

/// What stands at `entry` for a reader that keeps what it read from its selected data while each
/// file the selection is judged by stands as it was (`client::vault_memo`), so it need not select
/// the data again to know it would select the same: `None` where the selection refuses them (an
/// unfinished transition, an unsafe or inconsistent layout, a capacity larger than this reader
/// holds) or they cannot be seen. One `lstat` of the pin and one of the entry decide it for a
/// plain records file, which leaves an intent beside it unlooked at, and one of the intent too
/// where neither stands ([`selection_witnesses`]); a pinned layout standing as a reader last judged
/// it is not judged again ([`Judgements`]).
pub(crate) fn standing(entry: &Path, glance: Option<&fsx::Glance>) -> Option<Standing> {
    JUDGEMENTS.standing(entry, glance)
}

/// The files a [`Standing`] of `entry` stands for, besides the parent: the entry, its pin, its
/// intent, and a layout's header and data file, the last two named whether or not the entry is
/// a layout now (under a plain records file no such path can be looked up).
///
/// # Errors
/// The entry has no valid name ([`pin_path`]).
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
/// mode ([`Context::new`]); the pin and the entry, and the intent where neither stands
/// ([`Witnesses`]), and the layout's header and data file, each one's kind, stamp, owner, mode and
/// links from one `lstat` ([`fsx::Seen`]); and the capacity the reader held ([`check_capacity`]).
/// A file renamed over one of them or given another owner, mode or link shows another, and so does
/// one written since, but for a rewrite in place of the same length within one tick of a coarse
/// clock, which no writer of these files makes ([`fsx::FileStamp`]).
#[derive(Clone, PartialEq, Eq)]
struct Judged {
    entry: PathBuf,
    held: usize,
    parent: ParentFacts,
    witnesses: Witnesses,
    files: [Option<fsx::Seen>; 2],
}

/// The parent's identity, owner and mode ([`Context::new`]).
#[cfg(unix)]
type ParentFacts = (u64, u64, u32, u32);

/// The parent's identity and access on Windows: its volume, file id and attributes, and the
/// digest of its owner and access list ([`fsx::access_digest`]).
#[cfg(windows)]
type ParentFacts = (u64, fsx::Ino, u32, [u8; 32]);

impl Judged {
    /// What `entry`'s files show, each `lstat` `glance`'s where the caller looks through one
    /// ([`fsx::lstat_in`]).
    fn of(entry: &Path, witnesses: Witnesses, glance: Option<&fsx::Glance>) -> Result<Self> {
        let parent_path = entry
            .parent()
            .ok_or_else(|| refused("no parent directory"))?;
        // The parent is judged by its identity, owner and mode (on Windows its identity,
        // attributes, owner and access list), never its times ([`fsx::folder_in`]): the vault
        // folder, where every write of the state moves them.
        let parent = fsx::folder_in(glance, parent_path)?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        Ok(Self {
            entry: entry.to_path_buf(),
            held: crate::membership_log::RecordBudgets::current().held,
            #[cfg(unix)]
            parent: (parent.dev(), parent.ino(), parent.uid(), parent.mode()),
            #[cfg(windows)]
            parent: (
                parent.st_dev,
                parent.st_ino,
                parent.attributes,
                fsx::access_digest(parent_path)?,
            ),
            witnesses,
            files: [
                fsx::seen_if_present_in(glance, &entry.join(HEADER))?,
                fsx::seen_if_present_in(glance, &entry.join(DATA))?,
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
        let witnesses = selection_witnesses(entry, None).ok();
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
                self.layout(entry, stood, None)?;
                entry.join(DATA)
            }
            _ => data_path(entry)?,
        };
        Ok(Selected {
            entry: entry.to_path_buf(),
            path,
            witnesses,
            #[cfg(unix)]
            rewritten: false,
        })
    }

    /// [`standing`]: the shapes [`Self::selected`] tells apart, from the same `lstat`s, with the
    /// judgement of a pinned layout it would take ([`Self::layout`]), and any other shape judged by
    /// [`data_path`] afresh.
    fn standing(&self, entry: &Path, glance: Option<&fsx::Glance>) -> Option<Standing> {
        let witnesses = selection_witnesses(entry, glance).ok()?;
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
            ] => Some(Box::new(self.layout(entry, witnesses, glance).ok()?)),
            _ => {
                data_path(entry).ok()?;
                Some(Box::new(Judged::of(entry, witnesses, None).ok()?))
            }
        };
        Some(Standing { witnesses, judged })
    }

    /// [`data_path`]'s judgement of the pinned layout at `entry`, whose pin and entry stood as
    /// `witnesses` say (a prepared layout keeps its intent, which `data_path` does not read once
    /// the pin stands, so the witnesses do not look at it), as its files stood before it: taken
    /// from the last judgement of it while its files all stand as they did then. A judgement
    /// answers for its files only where they stood still across it, so the look after it is fresh
    /// even where the one before it is a check's `glance` ([`fsx::Glance`]). Its data file is the
    /// one inside the entry.
    fn layout(
        &self,
        entry: &Path,
        witnesses: Witnesses,
        glance: Option<&fsx::Glance>,
    ) -> Result<Judged> {
        let before = Judged::of(entry, witnesses, glance)?;
        if self.0.lock().is_ok_and(|judged| judged.contains(&before)) {
            return Ok(before);
        }
        data_path(entry)?;
        let after =
            selection_witnesses(entry, None).and_then(|stood| Judged::of(entry, stood, None));
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

/// Resolve existing custody without changing it. An incomplete transition is unavailable. It reads
/// the intent only where no pin and nothing at the entry stand, and [`selection_witnesses`] looks
/// at the intent only there: an arm that read it anywhere else would need those witnesses widened.
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
                    // A Windows exchange cut short between moving the original to its preserved
                    // name and placing the folder at the entry ([`Context::exchange`]): the next
                    // writer finishes it.
                    #[cfg(windows)]
                    if fsx::kind(&entry.with_file_name(parked_name(&intent)))? != fsx::Kind::Missing
                    {
                        return Err(refused("an interrupted transition needs its writer lock"));
                    }
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
#[cfg(any(unix, test))]
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

/// The vault's head beside `entry` (`records.head`): a cache of the data file's live state that
/// an open reads in place of the file where it names the file as it is
/// (`membership_log::head`). It is beside the entry, as the pin and the intent are, whatever
/// layout the entry takes, and no transition reads or needs it: a head that names another
/// file than the one selected is not read. A rewrite removes it with the data it names
/// (`records_storage::rewrite`), so it keeps none of the records the rewrite dropped.
pub(crate) fn head_path(entry: &Path) -> Result<PathBuf> {
    side_path(entry, "head")
}

fn side_path(entry: &Path, suffix: &str) -> Result<PathBuf> {
    let name = entry
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or_else(|| refused("invalid records entry name"))?;
    Ok(entry.with_file_name(format!("{name}.{suffix}")))
}

/// A durable point of a transition of the entry, after which a test may end it as a crash would:
/// of the first, which makes a records file a layout ([`Context::prepare`]: `Intent`, `Copied`,
/// `Prepared`, on Windows `Parked` and `Moved` where a records file is kept whole
/// (`Context::exchange`), `Exchanged`, `Pinned`), and of a rewrite (`records_storage::rewrite`:
/// `Intent`, `Copied`, `Prepared`, `Pinned`, then `Exchanged` once the predecessor's files are
/// unlinked).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Intent,
    Copied,
    Prepared,
    /// The prepared folder has its swap name ([`parked_name`]).
    #[cfg(windows)]
    Parked,
    /// The records file kept whole has its preserved name, the entry is empty.
    #[cfg(windows)]
    Moved,
    Exchanged,
    Pinned,
}

#[cfg(all(test, unix))]
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
        #[cfg(unix)]
        let parent = {
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
            parent
        };
        // The folder judged by its name opened as itself ([`fsx::owned_unshared_dir_id`]), then
        // opened from its volume's root through no reparse point (`aterm_dirfd::pin`), and the two
        // held to one file id.
        #[cfg(windows)]
        let parent = {
            let judged = fsx::owned_unshared_dir_id(parent_path)
                .ok_or_else(|| refused("the parent directory is not safely owned"))?;
            let canonical = parent_path.canonicalize()?;
            let parent = File::from(syscall(fd::pin(&canonical))?);
            if identity(&parent)? != judged {
                return Err(refused("the parent directory changed while opening"));
            }
            parent
        };
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
        #[cfg(unix)]
        let private = {
            let metadata = directory.metadata()?;
            metadata.uid() == fsx::euid() && metadata.mode() & 0o7777 == 0o700
        };
        #[cfg(windows)]
        let private = private_dir_at(&self.parent, &self.name, &directory)?;
        if !private {
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
        optional_regular(&self.parent, &self.name, DATA_LIMIT)
    }

    fn original_for(&self, header: &Layout) -> Result<Option<File>> {
        match (self.original()?, header.original) {
            (Some(mut file), Some((dev, ino))) => {
                if identity(&file)? != (dev, ino)
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
        regular(&directory, DATA, DATA_LIMIT)?;
        Ok(capacity)
    }

    /// [`private_file`]'s checks of the pin in this parent, for a reader that read it within its
    /// bound.
    fn pin_is_private(&self) -> Result<()> {
        #[cfg(windows)]
        {
            optional_regular(&self.parent, &self.pin, usize::MAX)?
                .ok_or_else(|| refused("a layout file is missing"))?;
            Ok(())
        }
        #[cfg(unix)]
        {
            let stat = match fd::fstatat(&self.parent, self.pin.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            {
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
                let identity = original.as_ref().map(identity).transpose()?;
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
                durable_dir(&self.parent)?;
                header
            }
        };
        after(Step::Intent)?;
        let old_name = format!(".aterm-records-{}", header.nonce);
        // A Windows exchange a cut stopped once its folder was parked: carried on from where the
        // names stand, never prepared again ([`Self::exchange`]).
        #[cfg(windows)]
        if fsx::kind(&self.entry.with_file_name(parked_name(&header)))? != fsx::Kind::Missing {
            self.exchange(&header, &old_name, &after)?;
            after(Step::Exchanged)?;
            return self.finish(after);
        }
        let mut original = self.original_for(&header)?;
        match fd::mkdirat(&self.parent, old_name.as_str(), Mode::RWXU) {
            Ok(()) => {}
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == libc::EEXIST => {}
            #[cfg(windows)]
            Err(error) if error == fd::Errno::EXIST => {}
            Err(error) => return syscall(Err(error)),
        }
        let directory = File::from(syscall(fd::openat(
            &self.parent,
            old_name.as_str(),
            dir_flags(),
            Mode::empty(),
        ))?);
        #[cfg(unix)]
        let private = {
            let metadata = directory.metadata()?;
            metadata.uid() == fsx::euid() && metadata.mode() & 0o7777 == 0o700
        };
        #[cfg(windows)]
        let private = private_dir_at(&self.parent, &old_name, &directory)?;
        if !private {
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
        match optional_regular(&directory, DATA, DATA_LIMIT)? {
            Some(mut data) => {
                if copy_digest(&mut data, &mut std::io::sink())? != (header.len, header.digest) {
                    return Err(refused("prepared data does not match its intent"));
                }
            }
            None => {
                let mut data = scratch(&directory, "current.building", DATA_LIMIT)?;
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
        durable_dir(&directory)?;
        after(Step::Copied)?;
        install_file(&directory, HEADER, &header.encode())?;
        durable_dir(&directory)?;
        durable_dir(&self.parent)?;
        after(Step::Prepared)?;
        #[cfg(unix)]
        {
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
        }
        #[cfg(windows)]
        if header.original.is_some() {
            self.exchange(&header, &old_name, &after)?;
        } else {
            self.original_for(&header)?;
            syscall(fd::renameat_with(
                &self.parent,
                old_name.as_str(),
                &self.parent,
                self.name.as_str(),
                RenameFlags::NOREPLACE,
            ))?;
        }
        durable_dir(&self.parent)?;
        after(Step::Exchanged)?;
        self.finish(after)
    }

    /// Windows's exchange of the prepared folder at `preserved` (`.aterm-records-<nonce>`) with
    /// the records file kept whole at the entry, which NTFS cannot make in one step
    /// ([`RenameFlags::EXCHANGE`]): three renames, none of which replaces an entry, each moving
    /// only the file it names by that file's id (`aterm_dirfd::renameat_judged`): `Parked`, the
    /// folder to its swap name ([`parked_name`]); `Moved`, the original to `preserved`, where the
    /// layout keeps it as its predecessor; and the folder to the entry, after which the names stand
    /// as one exchange leaves them. Each step is taken from where the names stand, so a writer that
    /// finds an exchange a cut stopped ([`Self::prepare`], which looks for the parked folder before
    /// it prepares anything) takes the step the cut writer would have taken next: the original
    /// stands at one name and the folder at one name at every step, a reader reads the original
    /// whole while it is at the entry and is refused while the entry is empty ([`data_path`]),
    /// and the next writer always finishes (`SwarmRecordsExchange`, `aterm-models-swarm`). The
    /// original is held to the intent (its id and its bytes) before it moves, and
    /// [`Self::finish`] holds it there again, at `preserved`, before the pin.
    #[cfg(windows)]
    fn exchange(
        &self,
        header: &Layout,
        preserved: &str,
        after: &impl Fn(Step) -> Result<()>,
    ) -> Result<()> {
        let (dev, ino) = header
            .original
            .ok_or_else(|| refused("an exchange with no records file kept whole"))?;
        let parked = parked_name(header);
        // The folder at `name`, private and owned, by its id.
        let folder = |name: &str| -> Result<fd::FileId> {
            let opened = File::from(syscall(fd::openat(
                &self.parent,
                name,
                dir_flags(),
                Mode::empty(),
            ))?);
            if !private_dir_at(&self.parent, name, &opened)? {
                return Err(refused(
                    "the preparation directory is not private and owned",
                ));
            }
            let (dev, ino) = identity(&opened)?;
            Ok(fd::FileId { dev, ino })
        };
        if fsx::kind(&self.entry.with_file_name(&parked))? == fsx::Kind::Missing {
            let id = folder(preserved)?;
            syscall(fd::renameat_judged(
                &self.parent,
                preserved,
                &self.parent,
                parked.as_str(),
                RenameFlags::NOREPLACE,
                id,
            ))?;
            after(Step::Parked)?;
        }
        if fsx::kind(&self.entry)? != fsx::Kind::Missing {
            self.original_for(header)?;
            syscall(fd::renameat_judged(
                &self.parent,
                self.name.as_str(),
                &self.parent,
                preserved,
                RenameFlags::NOREPLACE,
                fd::FileId { dev, ino },
            ))?;
            after(Step::Moved)?;
        }
        let id = folder(&parked)?;
        syscall(fd::renameat_judged(
            &self.parent,
            parked.as_str(),
            &self.parent,
            self.name.as_str(),
            RenameFlags::NOREPLACE,
            id,
        ))
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
        let mut current = regular(&directory, DATA, DATA_LIMIT)?;
        if copy_digest(&mut current, &mut std::io::sink())? != (header.len, header.digest) {
            return Err(refused("prepared custody changed before pin publication"));
        }
        let old_name = format!(".aterm-records-{}", header.nonce);
        match header.original {
            Some((dev, ino)) => {
                let mut old = regular(&self.parent, &old_name, DATA_LIMIT)?;
                if identity(&old)? != (dev, ino)
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
    original: Option<(u64, fsx::Ino)>,
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
        if len > DATA_LIMIT {
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

/// The prepared folder's name beside the entry while a Windows exchange moves the records file
/// kept whole to the folder's own name ([`Context::exchange`]): `.aterm-records-<nonce>.swap`,
/// of the preparation `header` declares, a name no Unix build makes or reads.
#[cfg(windows)]
fn parked_name(header: &Layout) -> String {
    format!(".aterm-records-{}.swap", header.nonce)
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

#[cfg(unix)]
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

/// [`private_file`] on Windows: a plain file of one link and at most `max` bytes, the user's under
/// the owner-only list ([`fsx::owner_only`], read off the handle, which a read or write open
/// gives `READ_CONTROL`), what a Unix `0600` file of this user's is.
#[cfg(windows)]
fn private_file(file: &File, max: usize) -> Result<()> {
    let metadata = fsx::meta_of(file)?;
    if !metadata.file_type().is_file()
        || !fsx::owner_only(file)?
        || metadata.st_nlink != 1
        || metadata.size > max as u64
    {
        return Err(refused("a layout file is not bounded, private and owned"));
    }
    Ok(())
}

/// The device and file number of the file `file` is open on: its `fstat`'s device and inode;
/// on Windows its handle's volume and file id.
fn identity(file: &File) -> std::io::Result<(u64, fsx::Ino)> {
    Ok(fsx::id_of(&fsx::meta_of(file)?))
}

/// Whether the folder `name` in `parent`, open as `opened` (a listing open, which reads no
/// access list), is the user's alone, what a Unix `0700` folder of this user's is: judged on a
/// second open of the same name for its facts (`OFlags::PATH`, with `READ_CONTROL`), held to
/// the same file id.
#[cfg(windows)]
fn private_dir_at(parent: &File, name: &str, opened: &File) -> Result<bool> {
    let facts = File::from(syscall(fd::openat(
        parent,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ))?);
    let metadata = fsx::meta_of(&facts)?;
    Ok(metadata.file_type().is_dir()
        && fsx::id_of(&metadata) == identity(opened)?
        && fsx::owner_only(&facts)?)
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

/// Finish a change of the entries of the open directory `dir`: its flush on Unix. Windows has no
/// flush of a directory (`FlushFileBuffers` on one is `Access is denied`, os error 5) and NTFS
/// commits a directory's entries through its own log, as `aterm_durable`'s `Flush::dir` holds.
fn durable_dir(dir: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        aterm_durable::durable(dir)
    }
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(())
    }
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
    durable_dir(parent)?;
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
            .filter(|len| *len <= DATA_LIMIT)
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
#[path = "records_storage_tests.rs"]
mod tests;

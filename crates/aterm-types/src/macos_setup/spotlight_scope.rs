// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether setup's administrator step owes an account's home folder its Spotlight scope, read
//! with no prompt: the work `aterm init` names ([`owed`], `aterm_machines::root_steps::owed`),
//! the automatic ask it and System State's window share ([`automatic_owed`]), and the receipt
//! reading the root step itself goes by (`aterm_machines::init_spotlight::Scope`).
//!
//! The scope is the owner's list ([`kept`]): Spotlight indexes /Applications and
//! ~/Applications, the app bundles abrew installs ([`ABREW_CASKS`]), ~/Documents, ~/Desktop and
//! the synced drives (the File Provider folders under ~/Library/CloudStorage, iCloud Drive's
//! ~/Library/Mobile Documents), and everything else on the data volume is kept out, with two
//! exceptions: /Users and /Volumes ([`VOLUME_KEPT`]). An exclusion takes a folder's whole
//! subtree and has no exception inside it, so the scope is spelled as the kept folders' siblings
//! ([`wanted`]): each folder on the way from the home to a kept folder (the home, ~/Library,
//! and the folders down to abrew's casks) is descended into and its other folders are left out;
//! so are the data volume's other top-level folders and /Users//Shared.
//!
//! The decision is here, over a port ([`Reads`]): the window does not link `aterm-machines`
//! outside its tests, so a reading kept there would be one the window had to write again.
//! `aterm-machines` answers the port from its `System`, the window from this Mac ([`Live`]).
//!
//! The root LAN broker's share of the scope is here too ([`broker_home_reading`],
//! [`broker_plan_over`]): the console account's own top-level home folders the scope keeps out,
//! by the same rule and receipt, which the broker reads as root and the account, the window and
//! `sync.spotlight` read with no root before they ask (`docs/DESIGN-lan-broker-2026-11-05.md`
//! §2).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// The receipt the root step's apply leaves: every folder it has excluded, one path a line, in
/// a root-owned file every account reads. Spotlight's configuration itself is root's alone, so
/// this is what a reading goes by ([`state`]). A folder stays on it after a person takes its
/// entry out of the configuration: that is how the step knows the removal was theirs and never
/// adds the folder again. A folder the person kept out from the page is in [`YOURS`], not here.
pub const RECEIPT: &str = "/Library/Application Support/aterm/spotlight-scope";

/// The folders a failed apply could not exclude, in a root-owned file beside the receipt.
/// Like the receipt, a line of it under a home says setup's password already scoped that home
/// once, so no automatic ask is owed for it again ([`automatic_owed`]): a Mac whose
/// configuration cannot be edited is not asked for its password again and again. A person's own
/// setup has only a folder it does not name left to exclude ([`owed`]), any root transaction
/// that runs setup's root steps for another of them tries every folder once more, and an apply
/// that succeeds removes the file.
pub const REFUSED: &str = "/Library/Application Support/aterm/spotlight-scope.refused";

/// The folders a person kept out of Spotlight from the System Resources page's Spotlight
/// document (Keep a folder out…), one path a line, in a root-owned file beside the receipt,
/// written by the same root apply (`aterm_machines::init_spotlight::KeepOut`). They are the
/// person's, never setup's: the receipt does not name them, the scope step never removes an
/// exclusion, and the page lists them as kept out by you.
pub const YOURS: &str = "/Library/Application Support/aterm/spotlight-scope.yours";

/// The most a receipt is read for, in bytes.
pub const RECEIPT_MAX: usize = 256 * 1024;

/// Root's own lock beside the receipt: every root writer of Spotlight's configuration and of the
/// files beside it (the password route's step, the root LAN broker's apply, the page's Keep a
/// folder out…) holds it from its reading of [`RECEIPT`], [`BROKERED`] or [`YOURS`] to its last
/// write, so no copy, check and rename of the configuration and no rewrite of a receipt lands
/// between another's reading and its write. The broker only tries it and answers busy while
/// another holds it; the others wait for it with a bound. `mds`, which rewrites the
/// configuration when it likes, is no party to it: an edit whose copy it rewrote is made again
/// from a fresh copy, a bounded number of times.
pub const LOCK: &str = "/Library/Application Support/aterm/spotlight-scope.lock";

/// Root's record of the receipt's lines the root LAN broker added, one path a line, in a
/// root-owned file beside the receipt that every account reads: the broker's half of the
/// receipt ([`BROKER_MAX`]) counts only these, so the password route's lines never use it up
/// ([`broker_fit`]). The broker alone writes it, after its exclusion and before the receipt, as
/// its lines the receipt still names and the folders it added; a reader counts only its lines
/// the receipt names, so a line the receipt lost (a run that stopped between the two writes, a
/// receipt root replaced) counts for nothing.
pub const BROKERED: &str = "/Library/Application Support/aterm/spotlight-scope.broker";

/// The home folder's top-level folders Spotlight keeps indexing: where a person's own files are
/// searched for, and the apps installed for one account, still found by name.
pub const HOME_KEPT: [&str; 3] = ["Documents", "Desktop", "Applications"];

/// The home folder the scope descends into rather than excludes: it holds the synced drives
/// ([`SYNCED`]) and abrew's casks ([`ABREW_CASKS`]), and an exclusion of it would take them too.
/// Its other folders are kept out.
pub const LIBRARY: &str = "Library";

/// ~/Library's folders that hold the synced drives: every File Provider domain (each sync
/// service's drive, iCloud Drive's own among them) under `CloudStorage`, and iCloud Drive's
/// `Mobile Documents`. Neither is ever listed, so a drive added later is indexed with no reading
/// of it.
pub const SYNCED: [&str; 2] = ["CloudStorage", "Mobile Documents"];

/// Where abrew keeps each cask's app bundle, under the home at atpkg's default prefix
/// (`aterm_toolchain::default_atpkg_prefix`, `atpkg`'s `Prefix::caskroom`): its
/// `Applications` folder holds only links to these, which Spotlight does not index as apps, so
/// the casks themselves stay indexed and an app abrew installed is still found by name. The
/// folders on the way to it are descended into, never excluded whole. A prefix configured
/// elsewhere is kept only where it lies outside what [`wanted`] names.
pub const ABREW_CASKS: &str = "Library/Application Support/aterm/pkg/abrew/Caskroom";

/// The data volume's top-level folders the scope does not exclude: `Applications`, which is
/// kept; `Users`, which is scoped one account at a time (the console account's home, and
/// [`SHARED`]), so another account's home stays indexed until that account is at the console
/// when the step runs; and `Volumes`, which holds other volumes' mount points, each with its own
/// index (a path there names another volume as soon as one is mounted at it).
pub const VOLUME_KEPT: [&str; 3] = ["Applications", "Users", "Volumes"];

/// The folder every account shares, which the scope keeps out.
pub const SHARED: &str = "/Users//Shared";

/// The Mac's own applications folder, which the scope keeps.
pub const APPLICATIONS: &str = "/Applications";

/// A part of what Spotlight keeps indexing ([`kept`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// /Applications, ~/Applications and abrew's casks.
    Applications,
    /// ~/Documents.
    Documents,
    /// ~/Desktop.
    Desktop,
    /// The synced drives: ~/Library/CloudStorage and ~/Library/Mobile Documents.
    SyncedDrives,
}

/// The folders Spotlight keeps indexing for `home`, each with its part of the owner's list: the
/// one statement of the scope's kept side, which [`wanted`] keeps out none of.
#[must_use]
pub fn kept(home: &Path) -> Vec<(Part, PathBuf)> {
    let library = home.join(LIBRARY);
    vec![
        (Part::Applications, PathBuf::from(APPLICATIONS)),
        (Part::Applications, home.join("Applications")),
        (Part::Applications, home.join(ABREW_CASKS)),
        (Part::Documents, home.join("Documents")),
        (Part::Desktop, home.join("Desktop")),
        (Part::SyncedDrives, library.join(SYNCED[0])),
        (Part::SyncedDrives, library.join(SYNCED[1])),
    ]
}

/// Apple's indexing manager: `mdutil -s <volume>` prints the volume's indexing state
/// ([`Reads::volume_status`], [`indexing`]).
pub const MDUTIL: &str = "/usr/bin/mdutil";

/// The writable macOS data volume, which holds every home folder.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The console device, which the account logged in at this Mac's screen owns.
pub const CONSOLE: &str = "/dev/console";

/// The system's list of firmlinks: one a line, the path the system volume's root shows, a tab,
/// and the folder's path on the data volume relative to its top. A plain system file every
/// account reads with no prompt.
pub const FIRMLINKS: &str = "/usr/share/firmlinks";

/// The most [`FIRMLINKS`] is read for, in bytes.
const FIRMLINKS_MAX: usize = 64 * 1024;

/// What a reading needs of one path, as `lstat` answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The owner's uid.
    pub uid: u32,
    /// A folder.
    pub dir: bool,
    /// A symbolic link (never followed).
    pub symlink: bool,
    /// The device and inode numbers (`st_dev`, `st_ino`), where they were read: a folder on
    /// another device is a mount point, and a data-volume folder the system volume's root shows
    /// under the same pair is a firmlink ([`wanted`]).
    pub id: Option<(u64, u64)>,
    /// The permission bits, set-id and sticky bits among them (`st_mode & 0o7777`).
    pub mode: u32,
}

/// What the reading reads of a machine. No method may put a question on the screen: only the
/// listings of the home, the folders on its way to a kept folder and the data volume's top
/// level, each entry's metadata, the home's ancestors' and [`FIRMLINKS`], root's own files
/// beside the receipt, the mount table, the clock and this account's own record of what the
/// broker left ([`BROKER_LEFT`]) are read, never what any other folder holds.
pub trait Reads {
    /// Whether this is macOS, the only system with Spotlight.
    fn macos(&self) -> bool;

    /// This process's effective uid.
    fn euid(&self) -> u32;

    /// The home folder of the account this process runs as.
    fn home(&self) -> Option<PathBuf>;

    /// `path`'s own metadata, a final link not followed.
    ///
    /// # Errors
    /// `lstat`'s: [`io::ErrorKind::NotFound`] where nothing is there.
    fn entry(&self, path: &Path) -> io::Result<Entry>;

    /// `path` with every link in it resolved, as `realpath` answers it.
    ///
    /// # Errors
    /// `realpath`'s.
    fn resolve(&self, path: &Path) -> io::Result<PathBuf>;

    /// The names in the folder `dir`.
    ///
    /// # Errors
    /// The folder could not be listed.
    fn names(&self, dir: &Path) -> io::Result<Vec<String>>;

    /// [`Self::names`], read until more than `max` names are found: `None` once more are, so a
    /// folder of any size costs a reader at most `max` names.
    ///
    /// # Errors
    /// The folder could not be listed.
    fn names_within(&self, dir: &Path, max: usize) -> io::Result<Option<Vec<String>>> {
        let names = self.names(dir)?;
        Ok((names.len() <= max).then_some(names))
    }

    /// The home directory services name for the account `uid`: `None` where they name none, or
    /// where this reader reads none, so the broker's rules scope no home.
    fn home_of(&self, uid: u32) -> Option<PathBuf> {
        let _ = uid;
        None
    }

    /// Every path a filesystem is mounted on now, each firmlinked spelling of one on the data
    /// volume beside it (`/Users//a/x` beside `/System/Volumes/Data/Users//a/x`), read with no
    /// question to any mounted filesystem: `None` where the table could not be read whole, or
    /// where this reader reads none, so the broker's rules read no home.
    fn mount_points(&self) -> Option<BTreeSet<PathBuf>> {
        None
    }

    /// The bytes of this account's record of the folders the root LAN broker left as they are
    /// ([`BROKER_LEFT`] in its state folder), at most [`BROKER_LEFT_MAX`] of them: `None` where
    /// there is none, it is longer, or this reader reads none.
    fn broker_left(&self) -> Option<Vec<u8>> {
        None
    }

    /// Unix seconds now, by which this account's dated record of what the broker left is read
    /// ([`broker_would_ask`]): `None` where this reader reads no clock, so such a record stands
    /// at any age.
    fn now(&self) -> Option<u64> {
        None
    }

    /// The bytes of root's own file at `path`, at most `max`: a regular file, a final link not
    /// followed, owned by uid 0, that neither its group nor others may write and that carries
    /// no set-id bit.
    ///
    /// # Errors
    /// The file could not be opened or read, is not such a file, or is longer than `max`.
    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>>;

    /// What `mdutil -s` printed for the data volume ([`MDUTIL`], [`DATA_VOLUME`]), from a run
    /// that exited 0.
    ///
    /// # Errors
    /// One line: the run failed, timed out or exited non-zero.
    fn volume_status(&self) -> Result<Vec<u8>, String>;
}

/// Read the data volume's indexing state from its block of `mdutil -s`, by the one parser
/// System State's Spotlight reading uses ([`aterm_vitals::spotlight::volumes`]). Unknown or
/// failed output never means disabled.
///
/// # Errors
/// The command failed, its output could not be read, it named no [`DATA_VOLUME`] block, or that
/// block reported no recognized state.
pub fn indexing(reads: &dyn Reads) -> Result<bool, String> {
    use aterm_vitals::spotlight::Indexing;
    let printed = reads.volume_status()?;
    let text = std::str::from_utf8(&printed).map_err(|error| error.to_string())?;
    let volumes = aterm_vitals::spotlight::volumes(text)?;
    match volumes
        .iter()
        .find(|v| v.path == DATA_VOLUME)
        .map(|v| &v.indexing)
    {
        Some(Indexing::Enabled) => Ok(true),
        Some(Indexing::Disabled) => Ok(false),
        _ => Err("Spotlight indexing status is unrecognized".into()),
    }
}

/// The plain folders of `dir` other than `keep`: each that is not hidden (Spotlight never
/// indexes a dot-folder), not named `*.noindex` (nor those), no link, and not a mount point
/// (where both `dir`'s and its device numbers were read and differ). An entry whose metadata
/// cannot be read is left out, as is a name a receipt line cannot carry.
fn children(reads: &dyn Reads, dir: &Path, keep: &[String]) -> io::Result<BTreeSet<PathBuf>> {
    Ok(children_among(
        reads,
        dir,
        reads.names(dir)?,
        keep,
        folder_on,
    ))
}

/// [`children`] among `names`, the names of `dir`'s listing, each entry held to `on` beside
/// `dir`'s device: [`folder_on`] for the scope, [`folder_known_on`] for the root LAN broker.
fn children_among(
    reads: &dyn Reads,
    dir: &Path,
    names: Vec<String>,
    keep: &[String],
    on: fn(Entry, Option<u64>) -> bool,
) -> BTreeSet<PathBuf> {
    let device = reads
        .entry(dir)
        .ok()
        .and_then(|entry| entry.id)
        .map(|id| id.0);
    names
        .into_iter()
        .filter(|name| plain(name) && !keep.contains(name))
        .map(|name| dir.join(name))
        .filter(|path| reads.entry(path).is_ok_and(|entry| on(entry, device)))
        .collect()
}

/// Whether `entry` is a folder, no link, and on `device` where both numbers were read.
fn folder_on(entry: Entry, device: Option<u64>) -> bool {
    entry.dir
        && !entry.symlink
        && match (device, entry.id) {
            (Some(device), Some((theirs, _))) => device == theirs,
            _ => true,
        }
}

/// [`folder_on`] with both device numbers read: the root LAN broker's stricter rule, under which
/// numbers unread are no proof that a folder is on its parent's device.
fn folder_known_on(entry: Entry, device: Option<u64>) -> bool {
    device.is_some() && entry.id.map(|id| id.0) == device && folder_on(entry, device)
}

/// Whether a folder's `name` can be a candidate: not hidden, not `*.noindex`, and with no
/// character a receipt line cannot carry.
fn plain(name: &str) -> bool {
    !name.starts_with('.') && !name.ends_with(".noindex") && !name.chars().any(char::is_control)
}

/// The data volume's own device and inode numbers, where they were read.
fn volume(reads: &dyn Reads) -> Option<(u64, u64)> {
    reads
        .entry(Path::new(DATA_VOLUME))
        .ok()
        .and_then(|entry| entry.id)
}

/// The data volume's top-level folders other than [`VOLUME_KEPT`] and the one that holds
/// `home`, each spelled as the system volume's root shows it where that is the same folder (a
/// firmlink: `/opt`, `/Library`), and under [`DATA_VOLUME`] where the root shows another folder
/// or none (`/System/Volumes/Data/System`, `/System/Volumes/Data/MobileSoftwareUpdate`). A folder
/// is a candidate only with its device and inode read and on the volume's device, so a mount
/// point (the automounter's `home`) is never one, and none is while the volume's own numbers are
/// unread. The folder that holds the home is found by its numbers, among those of the home's
/// ancestors as recorded and as resolved, so a home reached through a link (`/homes/alice`,
/// `/homes` a link to `System/Volumes/Data/homes`, or to `/System/Volumes/Data/opt/homes`)
/// keeps its folder however either path spells it.
fn volume_tops(reads: &dyn Reads, home: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let Some((device, _)) = volume(reads) else {
        return Ok(BTreeSet::new());
    };
    let resolved = reads.resolve(home).ok();
    let holding: BTreeSet<(u64, u64)> = std::iter::once(home)
        .chain(resolved.as_deref())
        .flat_map(Path::ancestors)
        .filter_map(|dir| reads.entry(dir).ok().and_then(|entry| entry.id))
        .collect();
    Ok(reads
        .names(Path::new(DATA_VOLUME))?
        .into_iter()
        .filter(|name| plain(name) && !VOLUME_KEPT.contains(&name.as_str()))
        .filter_map(|name| {
            let entry = reads.entry(&Path::new(DATA_VOLUME).join(&name)).ok()?;
            let id = entry.id.filter(|id| id.0 == device)?;
            (entry.dir && !entry.symlink && !holding.contains(&id))
                .then(|| spelled(reads, &name, id))
        })
        .collect())
}

/// How the scope spells the data volume's top-level folder `name` whose numbers are `id`:
/// `/<name>` where the system volume's root shows that same folder there, else its path under
/// [`DATA_VOLUME`].
fn spelled(reads: &dyn Reads, name: &str, id: (u64, u64)) -> PathBuf {
    let rooted = Path::new("/").join(name);
    if reads.entry(&rooted).ok().and_then(|entry| entry.id) == Some(id) {
        rooted
    } else {
        Path::new(DATA_VOLUME).join(name)
    }
}

/// The firmlinks ([`FIRMLINKS`]) inside the data-volume folders of `tops` that the system root
/// does not show at their own name (`/System/Volumes/Data/usr` holds `/usr/local`), each at the
/// path the root shows it: a folder excluded under its data-volume path is then also excluded
/// under the path the rest of the system reaches it by. A firmlink counts only where both paths
/// are plain folders whose device and inode were read and agree; a list that cannot be read adds
/// none.
fn firmlinked(reads: &dyn Reads, tops: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let under: Vec<&PathBuf> = tops
        .iter()
        .filter(|top| top.parent() == Some(Path::new(DATA_VOLUME)))
        .collect();
    if under.is_empty() {
        return BTreeSet::new();
    }
    let Ok(text) = reads
        .root_file(Path::new(FIRMLINKS), FIRMLINKS_MAX)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    else {
        return BTreeSet::new();
    };
    let id = |path: &Path| {
        reads
            .entry(path)
            .ok()
            .filter(|entry| entry.dir && !entry.symlink)
            .and_then(|entry| entry.id)
    };
    text.lines()
        .filter(|line| !line.chars().any(|c| c.is_control() && c != '\t'))
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(rooted, relative)| {
            let rooted = Path::new(rooted);
            let data = Path::new(DATA_VOLUME).join(relative);
            let inside = under
                .iter()
                .any(|top| data.starts_with(top) && data != **top);
            (rooted.is_absolute()
                && !rooted.starts_with(DATA_VOLUME)
                && inside
                && id(&data).is_some()
                && id(&data) == id(rooted))
            .then(|| rooted.to_path_buf())
        })
        .collect()
}

/// The folders kept under `home` ([`kept`] less /Applications): the leaves the descent keeps.
fn home_kept(home: &Path) -> BTreeSet<PathBuf> {
    kept(home)
        .into_iter()
        .map(|(_, path)| path)
        .filter(|path| path.starts_with(home) && path != home)
        .collect()
}

/// The plain folders beside what the scope keeps under `home`: for the home and each folder on
/// the way from it to a kept folder, its plain folders that are neither kept nor themselves on
/// the way. A folder on the way below the home is descended into only where it is a plain folder
/// on its parent's device (no link, no mount point) inside one descended into; one that is
/// absent or is not is neither listed nor left out.
fn beside_kept(reads: &dyn Reads, home: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let leaves = home_kept(home);
    // In path order a folder comes before everything inside it.
    let way: BTreeSet<PathBuf> = leaves
        .iter()
        .flat_map(|leaf| leaf.ancestors().skip(1))
        .filter(|dir| dir.starts_with(home))
        .map(Path::to_path_buf)
        .collect();
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out = BTreeSet::new();
    for dir in &way {
        if dir != home {
            let Some(parent) = dir.parent().filter(|parent| reached.contains(*parent)) else {
                continue;
            };
            let device = reads
                .entry(parent)
                .ok()
                .and_then(|entry| entry.id)
                .map(|id| id.0);
            if !reads.entry(dir).is_ok_and(|entry| folder_on(entry, device)) {
                continue;
            }
        }
        reached.insert(dir.clone());
        let keep = kept_in(&leaves, &way, dir);
        match children(reads, dir, &keep) {
            Ok(folders) => out.extend(folders),
            Err(error) if dir != home && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(unlisted(dir, &error)),
        }
    }
    Ok(out)
}

/// The names of `dir`'s folders the descent keeps or goes down into: the kept `leaves` and the
/// folders on the `way` to them whose parent is `dir`.
fn kept_in(leaves: &BTreeSet<PathBuf>, way: &BTreeSet<PathBuf>, dir: &Path) -> Vec<String> {
    leaves
        .iter()
        .chain(way)
        .filter(|path| path.parent() == Some(dir))
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// The names of `home`'s own folders the scope keeps or goes down into ([`kept_in`] of the
/// home): Documents, Desktop, Applications and Library.
fn kept_at_home(home: &Path) -> Vec<String> {
    let leaves = home_kept(home);
    let way: BTreeSet<PathBuf> = leaves
        .iter()
        .flat_map(|leaf| leaf.ancestors().skip(1))
        .filter(|dir| dir.starts_with(home))
        .map(Path::to_path_buf)
        .collect();
    kept_in(&leaves, &way, home)
}

/// The folders Spotlight should leave out for `home`, the siblings of what the scope keeps
/// ([`kept`]): the plain folders beside each kept folder under the home, down the way to it (the
/// home's other than [`HOME_KEPT`] and [`LIBRARY`]; ~/Library's other than [`SYNCED`] and the
/// way to [`ABREW_CASKS`]; and so on down); the data volume's plain top-level folders other than
/// [`VOLUME_KEPT`] and the one holding the home (`volume_tops`), with the firmlinks inside those
/// the root does not show at their own name (`firmlinked`); and [`SHARED`] where it is a plain
/// folder on the volume. A home on another volume leaves out nothing of its own: the data
/// volume's configuration governs only the data volume, whose folders it still leaves out. The
/// home and the folders on the way to a kept folder are listed, as is the data volume's top
/// level; each of their entries, the home's ancestors and the few named folders are `lstat`ed;
/// nothing inside any other folder is read: a read inside Documents, Desktop, Downloads, another
/// app's data or a synced drive can raise a macOS privacy question. A plain folder is one that
/// is not hidden, not `*.noindex`, no link and no mount point; an entry whose metadata cannot be
/// read is left out, as is a name a receipt line cannot carry.
///
/// # Errors
/// The home folder, a folder on the way to a kept folder (other than its absence) or the data
/// volume's top level could not be listed.
pub fn wanted(reads: &dyn Reads, home: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut out = if on_data_volume(reads, home) {
        beside_kept(reads, home)?
    } else {
        BTreeSet::new()
    };
    let tops =
        volume_tops(reads, home).map_err(|error| unlisted(Path::new(DATA_VOLUME), &error))?;
    out.extend(firmlinked(reads, &tops));
    out.extend(tops);
    let shared = Path::new(SHARED);
    let device = volume(reads).map(|id| id.0);
    if !home.starts_with(shared)
        && reads
            .entry(shared)
            .is_ok_and(|entry| folder_on(entry, device))
    {
        out.insert(shared.to_path_buf());
    }
    Ok(out)
}

/// The words for a folder `dir` that could not be listed.
fn unlisted(dir: &Path, error: &io::Error) -> String {
    format!("{}: {error}", dir.display())
}

/// Whether `home` is on the data volume, whose configuration alone the step edits: `false` only
/// where both device numbers were read and differ.
fn on_data_volume(reads: &dyn Reads, home: &Path) -> bool {
    match (
        volume(reads),
        reads.entry(home).ok().and_then(|entry| entry.id),
    ) {
        (Some((volume, _)), Some((theirs, _))) => volume == theirs,
        _ => true,
    }
}

/// The paths root's file at `file` lists ([`RECEIPT`], [`REFUSED`]): none while there is no
/// such file.
///
/// # Errors
/// A file stands there that is not root's own regular file, or it could not be read.
pub fn listed(reads: &dyn Reads, file: &str) -> Result<BTreeSet<PathBuf>, String> {
    let path = Path::new(file);
    match reads.entry(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("{file}: {error}")),
    }
    let bytes = reads
        .root_file(path, RECEIPT_MAX)
        .map_err(|error| format!("{file}: {error}"))?;
    let text = String::from_utf8(bytes).map_err(|error| format!("{file}: {error}"))?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// The paths root's file at `file` lists, as root reads it ([`listed`]): none where nothing is
/// there or what is there is not root's own file (another account's, a link, a folder, one its
/// group or others may write, or one with a set-id bit), which root's next write replaces. Root's
/// own file that cannot be read whole is an error, never none, so no write of root's drops the
/// lines it holds: the other homes' folders, and the record of each folder a person took out of
/// Spotlight's exclusions.
///
/// # Errors
/// Root's own file there could not be read within [`RECEIPT_MAX`] bytes, or its metadata could
/// not be read.
pub fn listed_as_root(reads: &dyn Reads, file: &str) -> Result<BTreeSet<PathBuf>, String> {
    match reads.entry(Path::new(file)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(format!("{file}: {error}")),
        Ok(entry) if !roots_own(&entry) => Ok(BTreeSet::new()),
        Ok(_) => listed(reads, file),
    }
}

/// Whether `entry` is root's own file by [`Reads::root_file`]'s rule, as its metadata says.
const fn roots_own(entry: &Entry) -> bool {
    entry.uid == 0 && !entry.dir && !entry.symlink && entry.mode & 0o6022 == 0
}

/// How Spotlight stands for one home folder ([`state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The data volume is not indexed at all, which the step never turns back on.
    Disabled,
    /// The receipt names every folder [`wanted`] names: how many.
    Scoped(usize),
    /// No receipt names these folders, so the step has never excluded them.
    Indexed {
        /// The folders.
        folders: BTreeSet<PathBuf>,
        /// Whether the receipt names none of the scope's folders beyond the home's top level, so
        /// the step has never scoped the rest of the volume for this account (a first run, or a
        /// Mac that applied the home-only scope): setup's automatic ask owes that once
        /// ([`automatic_owed`]). Otherwise the folders are newcomers, at the home's top level, in
        /// ~/Library, on the way to abrew's casks or at the volume's top level, made by a person,
        /// an agent, apps or macOS: they wait for the next root transaction that runs for another
        /// reason, whose apply excludes them too, or a person's own setup ([`owed`]).
        asks: bool,
    },
}

/// Read how Spotlight stands for `home` ([`wanted`]), by the receipt alone, as root and as an
/// ordinary account: a folder the receipt names is done with, whether the configuration still
/// excludes it or a person took its entry out since. Root reads a receipt that is not root's
/// own file as none, which the step's apply then replaces ([`listed_as_root`]).
///
/// # Errors
/// The indexing state, a folder [`wanted`] lists, root's own receipt, or an ordinary account's
/// receipt could not be read.
pub fn state(reads: &dyn Reads, home: &Path) -> Result<State, String> {
    if !indexing(reads)? {
        return Ok(State::Disabled);
    }
    let wanted = wanted(reads, home)?;
    let noted = if reads.euid() == 0 {
        listed_as_root(reads, RECEIPT)?
    } else {
        listed(reads, RECEIPT)?
    };
    let folders: BTreeSet<PathBuf> = wanted.difference(&noted).cloned().collect();
    if folders.is_empty() {
        return Ok(State::Scoped(wanted.len()));
    }
    let at_top = |path: &PathBuf| path.parent() == Some(home);
    let beyond: Vec<&PathBuf> = wanted.iter().filter(|path| !at_top(path)).collect();
    let asks = !beyond.is_empty() && beyond.iter().all(|path| folders.contains(*path));
    Ok(State::Indexed { folders, asks })
}

/// Whether a folder of this account's home is still Spotlight's to index though setup excludes
/// it, as the account reads it with no prompt: a folder indexed that no receipt names, unless an
/// apply already failed to exclude every such folder ([`REFUSED`]). It is work setup has, not a
/// password owed: a folder made since setup's password scoped the home (a checkout, a clone, an
/// agent's worktree, made all day long) at the home's top level is excluded with no password by
/// the root LAN broker, which the converge pass's `sync.spotlight` and every clone aterm
/// publishes ask ([`exclude_owed`]); one the broker does not take (in ~/Library, at the data
/// volume's top level, or any where no broker of this build serves) is excluded by the next
/// root transaction that runs setup's root steps for another of them, by the person's own press
/// of Finish setup, or by their own typed `aterm init` that applies or plans, which asks the
/// broker first. What asks by itself is [`automatic_owed`]. Only the account at the console is
/// read, since that is the home the root step scopes, and a state that cannot be read is not
/// owed.
#[must_use]
pub fn owed(reads: &dyn Reads) -> bool {
    let Some(home) = console_home(reads) else {
        return false;
    };
    match state(reads, &home) {
        Ok(State::Indexed { folders, .. }) => {
            // A refusal that cannot be read refuses nothing: the folders are owed.
            owed_of(&folders, &listed(reads, REFUSED).unwrap_or_default())
        }
        _ => false,
    }
}

/// Whether the scope step owes the folders `indexed` (those Spotlight may index that the scope
/// keeps out) its work, given the folders an apply already failed to keep out (`refused`): some
/// folder of `indexed` is not among them. [`owed`] and the System Resources page judge by it.
#[must_use]
pub fn owed_of(indexed: &BTreeSet<PathBuf>, refused: &BTreeSet<PathBuf>) -> bool {
    !indexed.is_empty() && !indexed.is_subset(refused)
}

/// Whether this process runs as the ordinary account at the console on macOS: the account whose
/// home the scope step scopes, and so the one whose page may offer it.
#[must_use]
pub fn console_account(reads: &dyn Reads) -> bool {
    console_home(reads).is_some()
}

/// Whether setup's automatic ask owes this account's home folder its Spotlight scope: only
/// while setup's password has never scoped this home, a folder of it still indexed and neither
/// [`RECEIPT`] nor [`REFUSED`] naming any folder in it; or, once, while it never scoped the rest
/// of the volume for it ([`State::Indexed`]'s `asks`: a home scoped by the home-only rule),
/// unless an apply already failed to exclude every such folder. The window and every automatic
/// run of `aterm init` read it with the broker's own reading beside it
/// ([`automatic_owed_beside`]). Once a password scoped the home, a folder made in it since asks
/// no password again by itself ([`owed`] says who excludes it):
/// asking again for each new folder is a nag, at an update or a restart as at any other time,
/// and the one password is asked early and batched. The home is read by itself, so another
/// account's receipt lines do not scope this one, and a home whose every folder an apply failed
/// to exclude is not asked for again when a folder is made in it. A state that cannot be read
/// is not owed (a receipt that is not root's own among them): no password is asked for want of
/// a reading.
#[must_use]
pub fn automatic_owed(reads: &dyn Reads) -> bool {
    let Some(home) = console_home(reads) else {
        return false;
    };
    state(reads, &home).is_ok_and(|state| automatic_owed_of(reads, &home, &state))
}

/// [`automatic_owed`] for a `state` of `home` already read, so a reader that says how the scope
/// stands (`aterm init`'s Spotlight indexing row) judges the ask with no second `mdutil -s`. The
/// caller holds the console rule ([`console_account`]).
#[must_use]
pub fn automatic_owed_of(reads: &dyn Reads, home: &Path, state: &State) -> bool {
    let State::Indexed { folders, asks } = state else {
        return false;
    };
    // A receipt that cannot be read made `state` an error; a refusal that cannot be read (not
    // root's own file) says nothing of this home.
    let refused = listed(reads, REFUSED).unwrap_or_default();
    let at_top = |path: &PathBuf| path.parent() == Some(home);
    let upgrade = *asks
        && !folders
            .iter()
            .filter(|path| !at_top(path))
            .all(|path| refused.contains(path));
    let mut noted = listed(reads, RECEIPT).unwrap_or_default();
    noted.extend(refused);
    upgrade || !noted.iter().any(at_top)
}

/// Whether setup's automatic ask owes this account's home its Spotlight scope with the root
/// LAN broker's reading beside it: [`automatic_owed`], unless a broker of this build `serves`
/// this Mac and would take now every folder the ask owes ([`broker_would_take`]). The broker
/// takes only the home's own top-level folders, so a home whose scope owes a folder in ~/Library
/// or at the data volume's top level (a home the password never scoped has some) is still owed
/// the password, as is one the broker refuses, leaves as it is for a cause that stands, already
/// left these folders as they are, or could not be read for: a Mac whose broker cannot serve
/// the step asks the password once. The one rule the window and every automatic run of
/// `aterm init` read (`aterm_machines::init_spotlight::automatic_scope_owed`,
/// [`automatic_owed_here`]); `serves` is read only where the scope is owed.
#[must_use]
pub fn automatic_owed_beside(reads: &dyn Reads, serves: impl FnOnce() -> bool) -> bool {
    let Some(home) = console_home(reads) else {
        return false;
    };
    let Ok(state) = state(reads, &home) else {
        return false;
    };
    if !automatic_owed_of(reads, &home, &state) {
        return false;
    }
    let State::Indexed { folders, .. } = &state else {
        return true;
    };
    !(serves() && matches!(broker_would_take(reads), Some(Ok(taken)) if folders.is_subset(&taken)))
}

/// [`automatic_owed_beside`] for this account on this Mac ([`Live`]), `serves` the window's
/// reading of the broker (`aterm_uds::lan::install::serves_this_build_here`): `false` off
/// macOS.
#[must_use]
pub fn automatic_owed_here(serves: impl FnOnce() -> bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        automatic_owed_beside(&Live, serves)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = serves;
        false
    }
}

/// The most folders one request of the root LAN broker excludes ([`broker_plan_over`]): a home
/// with more owed takes another request for the rest.
pub const SCOPE_BATCH: usize = 64;

/// The most receipt lines the LAN broker leaves under one home ([`broker_fit`]): past them it
/// adds none, so no account grows root's receipt without bound.
pub const HOME_LINES: usize = 1024;

/// The most bytes of the receipt's lines the root LAN broker added ([`BROKERED`]) it keeps
/// ([`broker_fit`]): half the receipt's bound. The other half is always the password route's,
/// whose lines for a home's ~/Library and the data volume's top level (a couple of hundred
/// folders a home) still fit however many homes the broker served first; the password route's
/// own lines, however many, take none of the broker's half.
pub const BROKER_MAX: usize = RECEIPT_MAX / 2;

/// The most receipt bytes the LAN broker leaves under one home ([`broker_fit`]): an eighth of
/// [`BROKER_MAX`], so no account's long names (a line can be some 265 bytes) take the room
/// every other account's home shares, and eight homes fill their shares before the broker's
/// half of the receipt is full.
pub const HOME_BYTES: usize = BROKER_MAX / 8;

/// The most names the LAN broker reads of a home's own listing ([`broker_home_reading`]): a
/// home with more is left as it is, so no account makes root read a listing without bound.
pub const LISTING_MAX: usize = 4096;

/// Why the broker leaves Spotlight as it is where root's receipt, or its record of its own lines
/// of it ([`BROKERED`]), is no file of root's own, or is past its bound: the password route
/// replaces such a receipt, the broker never does.
pub const RECEIPT_UNREAD: &str = "the LAN broker left Spotlight as it is: setup's receipt, or \
                                  its record of its own lines, is not root's own file within \
                                  its bound";

/// Why the root LAN broker leaves a home's folders as they are ([`broker_plan_over`]), in a
/// fixed sentence that names no path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// A rule refuses this account or its home: it stands until the account at the console,
    /// its home or the folders above it change.
    Refused(String),
    /// What the broker reads keeps it from excluding, until an input changes: the receipt or its
    /// record of its own lines unread, the receipt full or past its bound, the broker's half of
    /// it or the home's share of it full, or the home's listing past its bound.
    Stands(String),
    /// A read that did not answer (Spotlight's indexing state, the mount table, the home's
    /// listing, a reading past its deadline): the next request may read it.
    Unread(String),
}

impl Leave {
    /// The sentence.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Refused(why) | Self::Stands(why) | Self::Unread(why) => why,
        }
    }
}

/// What the broker reads of one home before it plans ([`broker_home_reading`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeReading {
    /// The home, held to [`broker_home_reading`]'s rules.
    pub home: PathBuf,
    /// Its own top-level folders the scope leaves out ([`wanted`]'s at the home's top level:
    /// every plain folder but Documents, Desktop, Applications and Library), none another
    /// filesystem is mounted on.
    pub wanted: BTreeSet<PathBuf>,
}

/// The home of `uid` the root LAN broker scopes, and its folders, by the broker's rules; the
/// account reads the same with no root. Root must never edit or exclude a path an ordinary
/// account can steer outside that account's own top-level home folders, so:
///
/// - `uid` is not root's, and owns [`CONSOLE`], read now: no other account and no session away
///   from the console is served.
/// - Its home is the one directory services name for it ([`Reads::home_of`]), a plain absolute
///   path with no `.`, `..`, doubled or trailing `/`, no control character and no byte that is
///   not UTF-8; it is on the data volume, no filesystem mounted on it or on a folder above it
///   but the root's (the data volume's configuration governs no other volume's index); each
///   folder above it is a folder (no link) of root's that neither group nor others may write,
///   so no account can swap the home or a folder above it between the check and the edit; and
///   the home itself is a folder (no link) of the account's own with no sticky bit.
/// - Its listing is read to at most [`LISTING_MAX`] names, and a name another filesystem is
///   mounted on is never read: a read of it reaches into that filesystem, which an account can
///   mount and keep from answering. A mount made between the table's reading and the read is
///   the broker's deadline's (`aterm_machines::init_spotlight::broker_duty`).
/// - The folders are those the scope leaves out at the home's top level, by [`wanted`]'s own
///   rule, held stricter where the device numbers go unread: plain (not hidden, not
///   `*.noindex`, no link, its numbers read and on the home's device) and none of Documents,
///   Desktop, Applications or Library, which the scope keeps or goes down into. The broker takes
///   nothing below the home's top level and nothing outside the home: those are the password
///   route's.
///
/// # Errors
/// [`Leave::Refused`] for a rule the account or its home fails; [`Leave::Stands`] for a listing
/// past its bound; [`Leave::Unread`] for a mount table or a listing that could not be read.
pub fn broker_home_reading(reads: &dyn Reads, uid: u32) -> Result<HomeReading, Leave> {
    let mounts = reads
        .mount_points()
        .ok_or_else(|| Leave::Unread("the mount table could not be read whole".to_owned()))?;
    let home = broker_home(reads, uid, &mounts)?;
    let names = reads
        .names_within(&home, LISTING_MAX)
        .map_err(|_| Leave::Unread("this account's home could not be listed".to_owned()))?
        .ok_or_else(|| {
            Leave::Stands(format!(
                "the LAN broker left Spotlight as it is: this account's home holds more than \
                 {LISTING_MAX} names"
            ))
        })?;
    let unmounted = names
        .into_iter()
        .filter(|name| !mounts.contains(&home.join(name)))
        .collect();
    let wanted = children_among(
        reads,
        &home,
        unmounted,
        &kept_at_home(&home),
        folder_known_on,
    );
    Ok(HomeReading { home, wanted })
}

/// [`broker_home_reading`]'s home alone.
fn broker_home(reads: &dyn Reads, uid: u32, mounts: &BTreeSet<PathBuf>) -> Result<PathBuf, Leave> {
    let refused = |why: &str| Leave::Refused(format!("the LAN broker scopes no home here: {why}"));
    if uid == 0 {
        return Err(refused("root's is never one"));
    }
    if !reads
        .entry(Path::new(CONSOLE))
        .is_ok_and(|console| console.uid == uid)
    {
        return Err(refused("this account is not the one at this Mac's console"));
    }
    let home = reads
        .home_of(uid)
        .ok_or_else(|| refused("directory services name no home for this account"))?;
    if !plain_absolute(&home) {
        return Err(refused("this account's home is not a plain absolute path"));
    }
    if mounts.contains(&home) {
        return Err(refused("this account's home is a filesystem of its own"));
    }
    // Stricter than the password route's rule ([`on_data_volume`]): numbers unread are no
    // proof of the data volume, so they refuse the home too.
    let on_volume = match (
        volume(reads),
        reads.entry(&home).ok().and_then(|entry| entry.id),
    ) {
        (Some((volume, _)), Some((theirs, _))) => volume == theirs,
        _ => false,
    };
    if !on_volume
        || home
            .ancestors()
            .skip(1)
            .any(|above| above != Path::new("/") && mounts.contains(above))
    {
        return Err(refused("this account's home is not on the data volume"));
    }
    for above in home.ancestors().skip(1) {
        if !reads.entry(above).is_ok_and(|entry| {
            entry.dir && !entry.symlink && entry.uid == 0 && entry.mode & 0o022 == 0
        }) {
            return Err(refused(
                "a folder above this account's home is not root's alone",
            ));
        }
    }
    if !reads.entry(&home).is_ok_and(|entry| {
        entry.dir && !entry.symlink && entry.uid == uid && entry.mode & 0o1000 == 0
    }) {
        return Err(refused("this account's home is not a folder of its own"));
    }
    Ok(home)
}

/// Whether `path` is absolute and spelled as its normal components rejoined (no `.`, `..`,
/// doubled or trailing `/`), in UTF-8 with no control character: a home path whose every
/// receipt line and `plutil` argument is the path itself.
fn plain_absolute(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !path.is_absolute() || text.chars().any(char::is_control) {
        return false;
    }
    let mut rejoined = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => rejoined.push(name),
            _ => return false,
        }
    }
    rejoined.as_os_str() == path.as_os_str()
}

/// The folders one request of the root LAN broker excludes for the home `read_home` reads
/// ([`broker_home_reading`]), by the broker's rules: none where the data volume is not indexed
/// (the home is not read then) or none is owed. Root's receipt and the broker's record of its own
/// lines of it ([`BROKERED`]) are read strictly; the folders are the home's wanted ones the
/// receipt does not name, as many as [`broker_fit`] takes. The password route's [`REFUSED`] is
/// not read.
///
/// # Errors
/// `read_home`'s; [`Leave::Unread`] for an indexing state that could not be read; else
/// [`Leave::Stands`]: a receipt or record that is not root's own file within its bound, or a
/// bound [`broker_fit`] leaves every folder out by.
pub fn broker_plan_over(
    reads: &dyn Reads,
    read_home: impl FnOnce() -> Result<HomeReading, Leave>,
) -> Result<BTreeSet<PathBuf>, Leave> {
    match indexing(reads) {
        Ok(true) => {}
        Ok(false) => return Ok(BTreeSet::new()),
        Err(_) => {
            return Err(Leave::Unread(
                "Spotlight's indexing state could not be read".to_owned(),
            ));
        }
    }
    let HomeReading { home, wanted } = read_home()?;
    let (noted, brokered) = broker_lines(reads)?;
    broker_fit(&noted, &brokered, &home, wanted)
}

/// Root's receipt and the broker's record of its own lines of it ([`BROKERED`]), each read
/// strictly ([`listed`]), as the broker plans and applies by them.
///
/// # Errors
/// [`Leave::Stands`] ([`RECEIPT_UNREAD`]): either is not root's own file within its bound.
pub fn broker_lines(reads: &dyn Reads) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>), Leave> {
    let unread = |_: String| Leave::Stands(RECEIPT_UNREAD.to_owned());
    Ok((
        listed(reads, RECEIPT).map_err(unread)?,
        listed(reads, BROKERED).map_err(unread)?,
    ))
}

/// Why [`broker_fit`] left a folder out: the bound its line would pass.
const HOME_FULL: &str = "this home's share of the receipt is full";
const HALF_FULL: &str = "the broker's half of the receipt is full; the rest is the password \
                         route's";
const RECEIPT_FULL: &str = "the receipt would pass its bound";

/// The folders of `owed` the root LAN broker takes for `home` beside root's receipt `noted` and
/// its record of its own lines of it, `brokered` ([`BROKERED`], counted only where the receipt
/// names them): `owed`'s folders at the home's own top level the receipt does not name, in the
/// order given, at most [`SCOPE_BATCH`]. The home keeps at most [`HOME_LINES`] lines and
/// [`HOME_BYTES`] bytes of the receipt (every line of its top level counts, the password
/// route's among them), the broker's own lines stay within [`BROKER_MAX`], and the whole receipt
/// within [`RECEIPT_MAX`]: a folder whose line would pass a byte bound is left out and the next
/// is tried. The broker's plan judges by it over the receipt it read ([`broker_plan_over`]), and
/// its apply again over the receipt it reads under root's lock ([`LOCK`]), so a password route's
/// run in between moves no bound.
///
/// # Errors
/// [`Leave::Stands`] where a folder is owed and none fits, naming the bound that left out the
/// first.
pub fn broker_fit(
    noted: &BTreeSet<PathBuf>,
    brokered: &BTreeSet<PathBuf>,
    home: &Path,
    owed: impl IntoIterator<Item = PathBuf>,
) -> Result<BTreeSet<PathBuf>, Leave> {
    let line = |path: &PathBuf| path.as_os_str().len() + 1;
    let at_home = |path: &PathBuf| path.parent() == Some(home);
    let mut lines = noted.iter().filter(|path| at_home(path)).count();
    let mut bytes: usize = noted.iter().filter(|path| at_home(path)).map(line).sum();
    let mut own: usize = brokered.intersection(noted).map(line).sum();
    let mut total: usize = noted.iter().map(line).sum();
    let mut folders = BTreeSet::new();
    let mut left_out = None;
    for path in owed
        .into_iter()
        .filter(|path| at_home(path) && !noted.contains(path))
    {
        if folders.len() == SCOPE_BATCH {
            break;
        }
        if lines >= HOME_LINES {
            left_out.get_or_insert(HOME_FULL);
            break;
        }
        let size = line(&path);
        let over = if bytes + size > HOME_BYTES {
            Some(HOME_FULL)
        } else if own + size > BROKER_MAX {
            Some(HALF_FULL)
        } else if total + size > RECEIPT_MAX {
            Some(RECEIPT_FULL)
        } else {
            None
        };
        if let Some(why) = over {
            left_out.get_or_insert(why);
            continue;
        }
        lines += 1;
        bytes += size;
        own += size;
        total += size;
        folders.insert(path);
    }
    match left_out {
        Some(why) if folders.is_empty() => Err(Leave::Stands(format!(
            "the LAN broker left Spotlight as it is: {why}"
        ))),
        _ => Ok(folders),
    }
}

/// [`broker_plan_over`] of `uid`'s home, read here ([`broker_home_reading`]).
///
/// # Errors
/// [`broker_plan_over`]'s.
pub fn broker_plan(reads: &dyn Reads, uid: u32) -> Result<BTreeSet<PathBuf>, Leave> {
    broker_plan_over(reads, || broker_home_reading(reads, uid))
}

/// What the root LAN broker would exclude for this account's home, as the account reads its
/// rules with no root ([`broker_plan`]): what `sync.spotlight` reads before it asks, so it asks
/// only where the broker would act and reads a cause that stands with no ask. Folders the
/// password route failed to exclude ([`REFUSED`]) are still owed, as the broker never reads that
/// record. `None` where there is no such home here: off macOS, as root, or in an account other
/// than the console's.
#[must_use]
pub fn broker_owed(reads: &dyn Reads) -> Option<Result<BTreeSet<PathBuf>, Leave>> {
    console_home(reads)?;
    Some(broker_plan(reads, reads.euid()))
}

/// This account's record of the folders the root LAN broker last left as they are, in its state
/// folder: a first line `at <unix seconds>`, when it was written, then the folders of an ask the
/// broker refused, left Spotlight as it is for, excluded none of, or failed to exclude
/// ([`exclude_owed`]), one a line ([`broker_left_text`]). While the broker would take only
/// folders it names, the password owes the step whatever its age ([`broker_would_take`]), and
/// nothing asks the broker again for [`LEFT_KEPT`] ([`broker_would_ask`]), so a cause that
/// cleared (Full Disk Access granted, a share freed) is asked about again within a day; a folder
/// made since is a new ask.
pub const BROKER_LEFT: &str = "spotlight-broker-left";

/// How long this account's record of what the broker left keeps the broker from being asked
/// again for the same folders ([`broker_would_ask`]), and how long the broker remembers an
/// exclusion of the same folders that failed: a cause that stands costs one ask a day.
pub const LEFT_KEPT: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The most bytes of [`BROKER_LEFT`] read: [`SCOPE_BATCH`] folders' lines with room to spare.
pub const BROKER_LEFT_MAX: usize = 64 * 1024;

/// Why the broker is not asked for folders it already left as they are ([`BROKER_LEFT`]).
pub const LEFT: &str = "the LAN broker left these folders as they are, and is asked again \
                        once another folder is made or a day has passed";

/// [`BROKER_LEFT`]'s text for `folders` left as they are at `now` (unix seconds): the line
/// `at <now>`, then the folders, one a line.
#[must_use]
pub fn broker_left_text(folders: &BTreeSet<PathBuf>, now: u64) -> String {
    let mut text = format!("at {now}\n");
    for path in folders.iter().filter_map(|path| path.to_str()) {
        text.push_str(path);
        text.push('\n');
    }
    text
}

/// [`BROKER_LEFT`]'s `bytes` as written: when (its first line's `at <unix seconds>`; `None`
/// where there is no such line) and the folders it names.
fn left_record(bytes: &[u8]) -> (Option<u64>, BTreeSet<PathBuf>) {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines().peekable();
    let at = lines
        .next_if(|line| line.starts_with("at "))
        .and_then(|line| line["at ".len()..].parse().ok());
    (
        at,
        lines
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect(),
    )
}

/// Whether a record of what the broker left written `at` still stands `now` ([`LEFT_KEPT`]):
/// at any age where the reader reads no clock, else only one dated within the last
/// [`LEFT_KEPT`], never one undated or dated after `now`.
fn left_stands(at: Option<u64>, now: Option<u64>) -> bool {
    let Some(now) = now else {
        return true;
    };
    at.is_some_and(|at| at <= now && now - at < LEFT_KEPT.as_secs())
}

/// [`broker_owed`], less folders the broker already left as they are: where every folder it
/// would take is one [`BROKER_LEFT`] names, whatever the record's age, it would take none of
/// them now ([`Leave::Stands`], [`LEFT`]). What the automatic ask reads, so a home whose folders
/// the broker left owes the password while the record stands, and only the broker's exclusion of
/// them, which removes it, takes the step off the password.
#[must_use]
pub fn broker_would_take(reads: &dyn Reads) -> Option<Result<BTreeSet<PathBuf>, Leave>> {
    broker_left_over(reads, |_| true)
}

/// [`broker_would_take`] with only a record dated within the last [`LEFT_KEPT`] standing
/// ([`Reads::now`]): what `sync.spotlight` reads before it asks, so folders the broker left are
/// asked for again once a day, and a cause that cleared since is heard.
#[must_use]
pub fn broker_would_ask(reads: &dyn Reads) -> Option<Result<BTreeSet<PathBuf>, Leave>> {
    let now = reads.now();
    broker_left_over(reads, |at| left_stands(at, now))
}

/// [`broker_owed`] less folders a record of what the broker left names, where `stands` says the
/// record, written when it says, still stands.
fn broker_left_over(
    reads: &dyn Reads,
    stands: impl FnOnce(Option<u64>) -> bool,
) -> Option<Result<BTreeSet<PathBuf>, Leave>> {
    let reading = broker_owed(reads)?;
    let left = reads
        .broker_left()
        .map(|bytes| left_record(&bytes))
        .filter(|(at, _)| stands(*at))
        .map(|(_, folders)| folders)
        .unwrap_or_default();
    Some(match reading {
        Ok(folders) if !folders.is_empty() && folders.is_subset(&left) => {
            Err(Leave::Stands(LEFT.to_owned()))
        }
        reading => reading,
    })
}

/// What one ask of the root LAN broker did for this account's home ([`exclude_owed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// Nothing was owed ([`broker_owed`] read no folder the broker would take), so the broker
    /// was not asked.
    Nothing,
    /// The broker excluded this many folders, and it would take none now.
    Done(u32),
    /// The broker excluded this many folders, and it would take more: the next ask takes them.
    More(u32),
    /// The broker could not serve the ask just now: another exclusion, or another root edit of
    /// Spotlight, held it off; root's lock was not taken for a cause that passes (a full disk,
    /// no file descriptor free); its own reading of the home did not answer in time; or `mds`
    /// rewrote the configuration under each of its edits. The next ask, on the clock, may be
    /// served.
    Busy,
    /// No broker answered the request though this account's reading said one of this build
    /// serves: a broker that launchd restarts, or one adopting this build. The next ask, on the
    /// clock, may be served.
    Unavailable(String),
    /// The broker answered that it leaves the folders as they are, though this account's reading
    /// of its rules said it would take them: it refused the account or its home, left Spotlight
    /// as it is (its receipt, a home's share of it, or an earlier edit of the same folders that
    /// failed), or excluded none of them. A definite answer: the same folders are not asked for
    /// again for a day ([`BROKER_LEFT`], [`LEFT_KEPT`]).
    Left(String),
    /// The broker's edit failed. It does not try the same folders again for a day, or until it
    /// restarts, and they are not asked for again for a day ([`BROKER_LEFT`]).
    Failed(String),
}

/// Ask the root LAN broker (`broker`, `aterm_uds::lan::scope_home`) to exclude this account's
/// home folders [`broker_would_ask`] reads it would take, and read the home again after its
/// answer ([`broker_owed`]). The broker is asked only where it would take a folder; [`Pass`] says
/// what came of it, beside the folders it was asked for (none where it was not asked), which a
/// [`Pass::Left`] or [`Pass::Failed`] keeps in [`BROKER_LEFT`].
#[cfg(unix)]
pub fn exclude_owed(
    reads: &dyn Reads,
    broker: &dyn Fn() -> aterm_uds::lan::ScopeAnswer,
) -> (Pass, BTreeSet<PathBuf>) {
    use aterm_uds::lan::ScopeAnswer;
    let folders = match broker_would_ask(reads) {
        Some(Ok(folders)) if !folders.is_empty() => folders,
        _ => return (Pass::Nothing, BTreeSet::new()),
    };
    // The home is read again only for an answer that excluded folders.
    let still_owed = || matches!(broker_owed(reads), Some(Ok(left)) if !left.is_empty());
    let pass = match broker() {
        ScopeAnswer::Excluded(added) if !still_owed() => Pass::Done(added),
        ScopeAnswer::Excluded(0) => Pass::Left(
            "the LAN broker excluded none of this home's folders still indexed".to_owned(),
        ),
        ScopeAnswer::Excluded(added) => Pass::More(added),
        ScopeAnswer::Busy => Pass::Busy,
        ScopeAnswer::Unserved(why) => Pass::Unavailable(why),
        ScopeAnswer::Refused(why) | ScopeAnswer::Unchanged(why) => Pass::Left(why),
        ScopeAnswer::Failed(why) => Pass::Failed(why),
    };
    (pass, folders)
}

/// The home of the ordinary account this process runs as, while that account is the one at the
/// console on macOS: the home the root step scopes, and the one the root LAN broker serves.
#[must_use]
pub fn console_home(reads: &dyn Reads) -> Option<PathBuf> {
    if !reads.macos() || reads.euid() == 0 {
        return None;
    }
    reads
        .entry(Path::new(CONSOLE))
        .is_ok_and(|console| console.uid == reads.euid())
        .then(|| reads.home())
        .flatten()
}

/// Whether `folder` is one a person may keep out of Spotlight from the page (Keep a folder
/// out…), for the console account's `home` with `yours` ([`YOURS`]) already kept out: an
/// absolute path of plain components, spelled as they rebuild it (no `.` or `..`, no repeated or
/// trailing `/`, no newline or other control character), strictly inside ~/Documents, ~/Desktop or a synced drive ([`kept`]'s Documents, Desktop and
/// synced-drive folders), and not one of `yours` or inside one. Everything else is either kept
/// indexed by the owner's list (the kept folders themselves, Applications, so apps are found by
/// name) or already Apply scope's to keep out (every folder beside the kept ones). The rule
/// is lexical: nothing is read, since a read inside Documents, Desktop or a synced drive can
/// raise a macOS privacy question. The window judges the open panel's folder by it, and root
/// judges it again before it writes.
///
/// # Errors
/// Why the folder is not one to keep out, in the page's words.
pub fn keep_out(home: &Path, yours: &BTreeSet<PathBuf>, folder: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    let text = folder.to_string_lossy();
    if !folder.is_absolute()
        || text.chars().any(char::is_control)
        || folder
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        // `components` drops an interior `.` and repeated or trailing `/`: root writes the
        // folder as given and judges it by its spelling, so only the one spelling is taken.
        || folder.as_os_str() != folder.components().collect::<PathBuf>().as_os_str()
    {
        return Err("choose a folder by its full path".to_owned());
    }
    if !folder.starts_with(home) {
        return Err("not in the home of the account at this Mac's screen".to_owned());
    }
    if yours.iter().any(|kept| folder.starts_with(kept)) {
        return Err("you already kept it out".to_owned());
    }
    let parts = kept(home);
    if parts.iter().any(|(_, path)| path == folder) {
        return Err("Spotlight keeps that folder: choose one inside it".to_owned());
    }
    let inside = |of: &dyn Fn(Part) -> bool| {
        parts
            .iter()
            .any(|(part, path)| of(*part) && folder.starts_with(path))
    };
    if inside(&|part| part == Part::Applications) {
        return Err("Applications stay indexed, so apps are found by name".to_owned());
    }
    if inside(&|part| part != Part::Applications) {
        Ok(folder.to_path_buf())
    } else {
        Err(
            "Apply scope keeps every folder beside Documents, Desktop and the synced drives out; \
             choose one inside them"
                .to_owned(),
        )
    }
}

/// The file in aterm's own state folder (`aterm_types::dirs::state_dir`) that records the
/// person's pause of Spotlight indexing (the page's Pause indexing): one line, `paused <unix ms>`
/// or `resumed <unix ms>`. The window writes it when its root run ended well, and ends it once a
/// reading of the data volume made after it says how indexing stands ([`pause`]). Nothing of
/// setup's reads or writes it, so setup never applies or lifts a pause.
pub const PAUSE_RECORD: &str = "spotlight-paused";

/// What the pause record says the person last did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Pause indexing ran.
    Paused,
    /// Resume indexing ran.
    Resumed,
}

/// The pause record ([`PAUSE_RECORD`]): what the person did, and when its run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseRecord {
    /// Pause or Resume.
    pub mark: Mark,
    /// When, unix ms.
    pub at_unix_ms: i64,
}

impl PauseRecord {
    /// The record's one line.
    #[must_use]
    pub fn line(&self) -> String {
        let word = match self.mark {
            Mark::Paused => "paused",
            Mark::Resumed => "resumed",
        };
        format!("{word} {}\n", self.at_unix_ms)
    }

    /// The record `text` holds; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (word, at) = text.trim_end_matches('\n').split_once(' ')?;
        let mark = match word {
            "paused" => Mark::Paused,
            "resumed" => Mark::Resumed,
            _ => return None,
        };
        Some(Self {
            mark,
            at_unix_ms: at.parse().ok()?,
        })
    }
}

/// The most the pause record is read for, in bytes.
const PAUSE_MAX: usize = 64;

/// The pause record in `dir`, where one stands and reads.
#[must_use]
pub fn read_pause(dir: &Path) -> Option<PauseRecord> {
    use std::io::Read as _;
    let file = std::fs::File::open(dir.join(PAUSE_RECORD)).ok()?;
    let mut text = String::new();
    file.take(PAUSE_MAX as u64).read_to_string(&mut text).ok()?;
    PauseRecord::parse(&text)
}

/// Write `record` as the pause record in `dir`, replacing any, through a file beside it.
///
/// # Errors
/// The folder or the file could not be written.
pub fn record_pause(dir: &Path, record: &PauseRecord) -> Result<(), String> {
    crate::scheme_install::write_atomically(
        dir,
        &dir.join(PAUSE_RECORD),
        PAUSE_RECORD,
        record.line().as_bytes(),
        true,
    )
    .map_err(|error| error.to_string())
}

/// End the pause record in `dir` where it still holds `record`: a later Pause or Resume wrote
/// over it is left.
pub fn end_pause(dir: &Path, record: &PauseRecord) {
    if read_pause(dir).as_ref() == Some(record) {
        let _ = std::fs::remove_file(dir.join(PAUSE_RECORD));
    }
}

/// How the person's pause stands ([`pause`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pause {
    /// The person paused indexing then, and no reading since says macOS turned it back on.
    ByYou {
        /// When the pause ran, unix ms.
        since_unix_ms: i64,
    },
    /// The person resumed indexing, and no reading has been made since.
    Resuming,
    /// Indexing is off on the data volume, and the person did not pause it.
    Off,
    /// Indexing is on on the data volume.
    On,
    /// No reading of the data volume, and no record.
    Unread,
}

/// How the pause stands by the `record` and the data volume's last reading: whether indexing
/// read enabled, and when the `mdutil -s` run that read it started, unix ms. Only a run that
/// started after the record counts: one that started before a pause and ended after it read the
/// volume before the pause. A pause holds until such a reading says indexing is on (macOS turned
/// it back on, and the record ends); a resume reads Resuming until such a reading, which ends it
/// and is read as it says. With no record the reading says Off or On. The second value says the
/// record ends ([`end_pause`]).
#[must_use]
pub fn pause(record: Option<&PauseRecord>, data_volume: Option<(bool, i64)>) -> (Pause, bool) {
    let read = |enabled: bool| if enabled { Pause::On } else { Pause::Off };
    let Some(record) = record else {
        return (data_volume.map_or(Pause::Unread, |(on, _)| read(on)), false);
    };
    let after = data_volume.filter(|(_, started)| *started > record.at_unix_ms);
    match (record.mark, after) {
        (Mark::Paused, Some((true, _))) => (Pause::On, true),
        (Mark::Paused, _) => (
            Pause::ByYou {
                since_unix_ms: record.at_unix_ms,
            },
            false,
        ),
        (Mark::Resumed, Some((on, _))) => (read(on), true),
        (Mark::Resumed, None) => (Pause::Resuming, false),
    }
}

/// How long [`Live`] waits for `mdutil -s` before the state counts as unread.
#[cfg(unix)]
const STATUS_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// This machine, read by this process: `lstat`, `realpath`, a folder listing (bounded where a
/// reader asks), the home directory services name for an account, the mount table as
/// `getfsstat(MNT_NOWAIT)` reads it, this account's own record of what the broker left, a
/// bounded read of an opened file whose own metadata is held to [`Reads::root_file`]'s rule, and
/// one `mdutil -s`.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

#[cfg(unix)]
impl Reads for Live {
    fn macos(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn euid(&self) -> u32 {
        aterm_uds::peer::our_uid()
    }

    fn home(&self) -> Option<PathBuf> {
        aterm_uds::account::home_of(self.euid())
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Entry {
            uid: meta.uid(),
            dir: meta.is_dir(),
            symlink: meta.file_type().is_symlink(),
            id: Some((meta.dev(), meta.ino())),
            mode: meta.mode() & 0o7777,
        })
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn names_within(&self, dir: &Path, max: usize) -> io::Result<Option<Vec<String>>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            if names.len() == max {
                return Ok(None);
            }
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(Some(names))
    }

    fn home_of(&self, uid: u32) -> Option<PathBuf> {
        aterm_uds::account::home_of(uid)
    }

    fn mount_points(&self) -> Option<BTreeSet<PathBuf>> {
        let table = aterm_sysprobe::volumes::table();
        table.complete.then(|| table.points().into_iter().collect())
    }

    fn broker_left(&self) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let path = crate::dirs::account_state_dir()?.join(BROKER_LEFT);
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(BROKER_LEFT_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= BROKER_LEFT_MAX).then_some(bytes)
    }

    fn now(&self) -> Option<u64> {
        aterm_time::unix_secs()
    }

    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        read_owned(path, 0, max)
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        let mut status = aterm_uds::spawnfd::command(MDUTIL);
        status.args(["-s", DATA_VOLUME]);
        let out = aterm_uds::deadline::output_within(&mut status, STATUS_WAIT)
            .map_err(|error| format!("{MDUTIL} -s {DATA_VOLUME}: {error}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!("Spotlight status: {}", out.status))
        }
    }
}

/// The bytes, at most `max`, of the regular file at `path` that `owner` owns, a final link not
/// followed, judged by the opened file's own metadata ([`Reads::root_file`]'s rule for
/// `owner` 0).
#[cfg(unix)]
fn read_owned(path: &Path, owner: u32, max: usize) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let bound = max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    // Nonblocking and refusing a final link: a FIFO or a link put there waits on nothing.
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o6022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if meta.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(bound as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "spotlight_scope_tests.rs"]
mod tests;

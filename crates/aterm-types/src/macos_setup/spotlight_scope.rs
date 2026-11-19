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
}

/// What the reading reads of a machine. No method may put a question on the screen: only the
/// listings of the home, the folders on its way to a kept folder and the data volume's top
/// level, each entry's metadata, the home's ancestors' and [`FIRMLINKS`] are read, never what
/// any other folder holds.
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
    let device = reads
        .entry(dir)
        .ok()
        .and_then(|entry| entry.id)
        .map(|id| id.0);
    Ok(reads
        .names(dir)?
        .into_iter()
        .filter(|name| plain(name) && !keep.contains(name))
        .map(|name| dir.join(name))
        .filter(|path| {
            reads
                .entry(path)
                .is_ok_and(|entry| folder_on(entry, device))
        })
        .collect())
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
        let keep: Vec<String> = leaves
            .iter()
            .chain(&way)
            .filter(|path| path.parent() == Some(dir.as_path()))
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        match children(reads, dir, &keep) {
            Ok(folders) => out.extend(folders),
            Err(error) if dir != home && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(unlisted(dir, &error)),
        }
    }
    Ok(out)
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

/// How Spotlight stands for one home folder ([`state`]).
#[derive(Debug, PartialEq, Eq)]
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
/// own file as none, which the step's apply then replaces.
///
/// # Errors
/// The indexing state, a folder [`wanted`] lists, or an ordinary account's receipt could not
/// be read.
pub fn state(reads: &dyn Reads, home: &Path) -> Result<State, String> {
    if !indexing(reads)? {
        return Ok(State::Disabled);
    }
    let wanted = wanted(reads, home)?;
    let noted = if reads.euid() == 0 {
        listed(reads, RECEIPT).unwrap_or_default()
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
/// agent's worktree, made all day long) is excluded by the next root transaction that runs
/// setup's root steps for another of them, by the person's own press of Finish setup, or by
/// their own typed `aterm init` that applies or plans. What asks by itself is
/// [`automatic_owed`]. Only the account at the console is read, since that is the home the root
/// step scopes, and a state that cannot be read is not owed.
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
/// unless an apply already failed to exclude every such folder. Once a password scoped the home, a
/// folder made in it since asks no password again by itself ([`owed`] says who excludes it):
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

/// [`automatic_owed`] for this account on this Mac ([`Live`]): `false` off macOS.
#[must_use]
pub fn automatic_owed_here() -> bool {
    #[cfg(target_os = "macos")]
    {
        automatic_owed(&Live)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The home of the ordinary account this process runs as, while that account is the one at the
/// console on macOS: the home the root step scopes.
fn console_home(reads: &dyn Reads) -> Option<PathBuf> {
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

/// This machine, read by this process: `lstat`, `realpath`, a folder listing, a bounded read of
/// an opened file whose own metadata is held to [`Reads::root_file`]'s rule, and one
/// `mdutil -s`.
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Init keeps Spotlight to Applications, Documents, Desktop and the synced drives, in its root
//! transaction ([`Scope`]).
//! Cache placement also keeps Rust docs and build output out of Spotlight.

use crate::init_observe::{LastReading, Reading, TOOLCHAIN_NOTE};
use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, User};
use crate::port::{Cmd, System};
use aterm_toolchain::layout;
use aterm_types::macos_setup::spotlight_scope::{self, Leave, State as Standing};
use aterm_uds::lan::{ScopeAnswer, ScopePlan};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// The group's id, as `aterm init --only` and every remedy that names it spell it.
pub const ID: &str = "init.spotlight";

/// The root step's id ([`Scope`]).
pub const SCOPE_ID: &str = "init.spotlight.scope";

/// Apple's indexing manager and the writable macOS data volume, as the shared reading names
/// them.
const MDUTIL: &str = spotlight_scope::MDUTIL;
const DATA_VOLUME: &str = spotlight_scope::DATA_VOLUME;

/// Apple's property-list editor: each value is its own argument, so a folder's name needs no
/// quoting, and an edit keeps the file's format.
const PLUTIL: &str = "/usr/bin/plutil";

/// `killall`, which ends `mds` so launchd starts one that reads the new list.
const KILLALL: &str = "/usr/bin/killall";

/// The data volume's Spotlight configuration, which only root reads or writes. Its
/// [`EXCLUSIONS`] array is the list of folders Spotlight leaves out.
const VOLUME_CONFIG: &str = "/System/Volumes/Data/.Spotlight-V100/VolumeConfiguration.plist";

/// The key of the excluded folders in [`VOLUME_CONFIG`]: an array of path strings.
const EXCLUSIONS: &str = "Exclusions";

/// The receipt [`Scope`]'s apply leaves, which every reading goes by
/// ([`spotlight_scope::RECEIPT`]).
pub const SCOPE_RECEIPT: &str = spotlight_scope::RECEIPT;

/// The folders a failed apply could not exclude ([`spotlight_scope::REFUSED`]).
pub const SCOPE_REFUSED: &str = spotlight_scope::REFUSED;

/// Root's one lock on Spotlight's configuration and the files beside it
/// ([`spotlight_scope::LOCK`], [`hold_scope`]).
pub const SCOPE_LOCK: &str = spotlight_scope::LOCK;

/// The LAN broker's record of its own lines of the receipt ([`spotlight_scope::BROKERED`]).
pub const SCOPE_BROKERED: &str = spotlight_scope::BROKERED;

/// How many times in all an edit of Spotlight's configuration is made, each from a fresh copy,
/// where `mds` rewrote the configuration between the copy and its rename ([`Scope::exclude`]):
/// `mds` is no party to root's lock, and each copy reads its last write.
const EDIT_TRIES: usize = 3;

/// How long the password route's step and the page's Keep a folder out… wait for another holder
/// of [`SCOPE_LOCK`]: a hang detector over a broker apply, which holds it for at most a batch's
/// inserts, or another root step's couple of hundred.
pub const LOCK_WAIT: Duration = Duration::from_secs(120);

/// Hold [`SCOPE_LOCK`] through `sys` ([`System::hold_root_lock`]), waiting at most `wait` for
/// another holder; its folder, the receipt's, is made first. Every root writer of Spotlight's
/// configuration or of the receipt, [`SCOPE_BROKERED`], [`SCOPE_REFUSED`] or
/// [`spotlight_scope::YOURS`] holds it from its reading of them to its last write: [`Scope`]'s
/// apply, the LAN broker's ([`broker_duty`]) and [`KeepOut`]'s.
///
/// # Errors
/// `WouldBlock` where another holder kept it past `wait`; the folder's, the open's or the lock's
/// own error, a file there that is not root's included.
fn hold_scope(sys: &dyn System, wait: Duration) -> io::Result<Box<dyn std::any::Any>> {
    let lock = Path::new(SCOPE_LOCK);
    if let Some(dir) = lock.parent() {
        sys.create_dir_all(dir, 0o755)?;
    }
    sys.hold_root_lock(
        lock,
        wait,
        "another of root's Spotlight edits holds its lock",
    )
}

/// Why [`Scope::exclude`] left the configuration as it was.
#[derive(Debug)]
enum Unexcluded {
    /// The configuration changed between the copy and its rename on each of [`EDIT_TRIES`]
    /// tries (`mds` rewrote it): nothing was written, and the folders were refused nothing, so
    /// no record says they were.
    Changed(String),
    /// Any other cause: the folders could not be excluded.
    Failed(String),
}

impl From<Unexcluded> for String {
    fn from(unexcluded: Unexcluded) -> Self {
        match unexcluded {
            Unexcluded::Changed(why) | Unexcluded::Failed(why) => why,
        }
    }
}

/// `sys` as the shared reading's port ([`spotlight_scope::Reads`]): the reading itself is
/// `aterm-types`' so the window, which does not link this crate outside its tests, makes the
/// same one from its own file reads.
pub(crate) struct Port<'a>(pub(crate) &'a dyn System);

impl spotlight_scope::Reads for Port<'_> {
    fn macos(&self) -> bool {
        self.0.macos()
    }

    fn now(&self) -> Option<u64> {
        Some(self.0.now())
    }

    fn euid(&self) -> u32 {
        self.0.euid()
    }

    fn home(&self) -> Option<PathBuf> {
        self.0.account_home()
    }

    fn entry(&self, path: &Path) -> io::Result<spotlight_scope::Entry> {
        let stat = self.0.stat(path)?;
        Ok(spotlight_scope::Entry {
            uid: stat.uid,
            dir: stat.dir,
            symlink: stat.symlink,
            id: self.0.file_id(path).ok().map(|id| (id.dev, id.ino)),
            mode: stat.mode & 0o7777,
        })
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        self.0.canonicalize(path)
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.0.list_dir(dir)
    }

    fn names_within(&self, dir: &Path, max: usize) -> io::Result<Option<Vec<String>>> {
        self.0.list_dir_within(dir, max)
    }

    fn home_of(&self, uid: u32) -> Option<PathBuf> {
        self.0.home_of(uid)
    }

    fn mount_points(&self) -> Option<BTreeSet<PathBuf>> {
        self.0.mount_points()
    }

    fn broker_left(&self) -> Option<Vec<u8>> {
        let bytes = self.0.read_file(&broker_left_record(self.0)?).ok()?;
        (bytes.len() <= spotlight_scope::BROKER_LEFT_MAX).then_some(bytes)
    }

    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        self.0.read_owned_file(path, 0, max, false)
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        let ran = self
            .0
            .run_complete_output(&Cmd::new(MDUTIL).args(["-s", DATA_VOLUME]))
            .map_err(|error| error.to_string())?;
        if ran.success() {
            Ok(ran.stdout)
        } else {
            Err(format!("Spotlight status: {}", ran.describe()))
        }
    }
}

/// The folders Spotlight should leave out for `home` ([`spotlight_scope::wanted`]).
///
/// # Errors
/// The home folder, a folder on the way to a kept one or the data volume's top level could not
/// be listed.
fn wanted(sys: &dyn System, home: &Path) -> Result<BTreeSet<PathBuf>, String> {
    spotlight_scope::wanted(&Port(sys), home)
}

/// The paths root's file at `file` lists ([`spotlight_scope::listed`]).
///
/// # Errors
/// A file stands there that is not root's own regular file, or it could not be read.
pub(crate) fn listed(sys: &dyn System, file: &str) -> Result<BTreeSet<PathBuf>, String> {
    spotlight_scope::listed(&Port(sys), file)
}

/// Write `paths`, one a line, as root's file at `file`, readable by every account: never past
/// [`spotlight_scope::RECEIPT_MAX`], which no reader reads.
///
/// # Errors
/// The lines would pass that bound, or the folder or the file could not be written.
pub(crate) fn list(sys: &dyn System, file: &str, paths: &BTreeSet<PathBuf>) -> Result<(), String> {
    let text: String = paths
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect();
    if text.len() > spotlight_scope::RECEIPT_MAX {
        return Err(format!(
            "{file}: {} bytes would pass its bound of {}",
            text.len(),
            spotlight_scope::RECEIPT_MAX
        ));
    }
    let file = Path::new(file);
    if let Some(dir) = file.parent() {
        sys.create_dir_all(dir, 0o755)
            .map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    sys.write_file(file, text.as_bytes(), 0o644)
        .map_err(|error| format!("{}: {error}", file.display()))
}

/// `plutil <args> <config>`.
fn config_cmd(args: &[&str], config: &Path) -> Cmd {
    Cmd::new(PLUTIL).args(args).arg(config)
}

/// The folders the configuration at `config` excludes, as root reads it: every string of its
/// [`EXCLUSIONS`] array, a person's own choices in System Settings included.
///
/// # Errors
/// The configuration could not be read (only root reads it), has no such array, or the array
/// is no property list this crate reads.
fn excluded(sys: &dyn System, config: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let cmd = config_cmd(&["-extract", EXCLUSIONS, "xml1", "-o", "-"], config);
    let ran = sys
        .run_complete_output(&cmd)
        .map_err(|error| format!("{}: {error}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    match crate::plist::parse(&ran.stdout)? {
        PValue::Array(entries) => Ok(entries
            .into_iter()
            .filter_map(|entry| match entry {
                PValue::Str(path) => Some(PathBuf::from(path)),
                _ => None,
            })
            .collect()),
        _ => Err(format!(
            "{}: {EXCLUSIONS} is not an array",
            config.display()
        )),
    }
}

/// Run `cmd` to a zero exit.
fn ran_ok(sys: &dyn System, cmd: &Cmd) -> Result<(), String> {
    let ran = sys
        .run(cmd)
        .map_err(|error| format!("{}: {error}", cmd.display()))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

/// Read how Spotlight stands for `home`, by the receipt alone
/// ([`spotlight_scope::state`]).
///
/// # Errors
/// The indexing state, a folder the scope lists, or an ordinary account's receipt could not be
/// read.
fn scope(sys: &dyn System, home: &Path) -> Result<Standing, String> {
    spotlight_scope::state(&Port(sys), home)
}

/// Whether a folder of the console account's home is still Spotlight's to index though setup
/// excludes it ([`spotlight_scope::owed`]): what [`crate::root_steps::owed`] names. It is work
/// setup has, which an explicit request of the person's takes up; what asks for the password by
/// itself is [`automatic_scope_owed`].
#[must_use]
pub fn scope_owed(sys: &dyn System) -> bool {
    spotlight_scope::owed(&Port(sys))
}

/// Whether setup's automatic ask owes this account's home its Spotlight scope
/// ([`spotlight_scope::automatic_owed_beside`]: only while setup's password never scoped the
/// home or the rest of the data volume for it, unless a root LAN broker of this build serves,
/// [`broker_serves`], and would take every folder owed, which it then excludes with no
/// password; it takes only the home's own top-level folders): the one rule every run of
/// `aterm init` and the window ([`spotlight_scope::automatic_owed_here`]) read.
#[must_use]
pub fn automatic_scope_owed(sys: &dyn System) -> bool {
    spotlight_scope::automatic_owed_beside(&Port(sys), || broker_serves(sys))
}

/// `folders`, summed by where they are: the home's own folders by name, those anywhere in
/// ~/Library and the rest of the data volume's by count, so a first run's couple of hundred
/// reads as one line.
fn summary(folders: &BTreeSet<PathBuf>, home: &Path) -> String {
    let library = home.join(spotlight_scope::LIBRARY);
    let named: Vec<String> = folders
        .iter()
        .filter(|path| path.parent() == Some(home))
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let in_library = folders
        .iter()
        .filter(|path| path.starts_with(&library))
        .count();
    let rest = folders.len() - named.len() - in_library;
    let counted = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if !named.is_empty() {
        parts.push(format!(
            "{} ({})",
            counted(named.len(), "home folder"),
            named.join(", ")
        ));
    }
    if in_library > 0 {
        parts.push(counted(in_library, "Library folder"));
    }
    if rest > 0 {
        parts.push(counted(rest, "data-volume folder"));
    }
    parts.join(", ")
}

/// What the scope keeps, as a row says it.
const KEPT_WORDS: &str = "Applications, Documents, Desktop and synced drives";

/// Keep Spotlight to the owner's list, in setup's existing root transaction: Applications,
/// Documents, Desktop and the synced drives stay indexed ([`spotlight_scope::kept`]), and every
/// folder beside them ([`wanted`]: the console account's other home folders, its Library's other
/// folders down the way to abrew's casks, the data volume's other top-level folders but /Users
/// and /Volumes, and /Users//Shared) joins the data volume's excluded folders. An exclusion takes
/// a whole subtree, so ~/Library itself is never one: its other children are, which leaves the
/// synced drives under it indexed.
/// Nothing of the person's is moved, renamed or marked, and what Spotlight already indexed of
/// a folder is kept, so removing its entry makes it searchable again; the step adds a folder
/// once and never again ([`SCOPE_RECEIPT`]). A folder made later beside the kept ones is
/// indexed until the step next runs, which it asks no password for by itself: the next root
/// transaction that runs for another reason, or the person's own setup, excludes it (a home's
/// first scope, and the rest of the volume for a home scoped before it was part of the scope,
/// do ask: [`automatic_scope_owed`]); one made inside an excluded
/// folder is out by its subtree. Whole-volume indexing is no step of setup's: a volume found off
/// is left off, and this step never runs `mdutil -i` (only the person's own [`Indexing`] does).
/// A folder the person kept out ([`KeepOut`]) is neither on its receipt nor ever removed.
///
/// The home is the console account's whoever's password runs the transaction, as its other
/// steps are that account's (developer mode's group, the name rule): the root run is told of
/// no other account. A home on another volume keeps its own folders: the data volume's
/// configuration does not govern that volume's index; the data volume's folders are still
/// left out.
///
/// An ordinary account cannot do this: without root a `.metadata_never_index` file in an
/// ordinary folder is not honoured, and only root reads the configuration. macOS reads the
/// list when `mds` starts, and SIP refuses `launchctl kickstart` of it, so apply ends `mds`
/// with SIGKILL and launchd starts another (measured on macOS 27.0.1).
pub(crate) struct Scope;

impl Scope {
    /// The home folder the step scopes: the console account's.
    fn home(ctx: &Ctx) -> Option<PathBuf> {
        ctx.console.as_ref().map(|user| PathBuf::from(&user.home))
    }

    /// Put `add` on the configuration's excluded folders, keeping every entry already there,
    /// and restart `mds`. The configuration is never edited where it stands: a copy beside it
    /// takes every insert, is checked, and replaces it in one rename, so a run cut short
    /// leaves the list as it was. A configuration rewritten between the copy and the rename is
    /// left as it was rewritten: the caller holds [`SCOPE_LOCK`], so no root writer of ours did
    /// it, `mds` did, and the edit is made again at once from a fresh copy, which reads that
    /// write, up to [`EDIT_TRIES`] times in all. One rewritten under each is
    /// [`Unexcluded::Changed`]: the password route's step and the page's Keep a folder out fail
    /// on it, and the broker answers busy. With nothing to add, nothing is written and `mds` is
    /// left running.
    fn exclude(sys: &dyn System, add: &BTreeSet<PathBuf>) -> Result<(), Unexcluded> {
        let config = Path::new(VOLUME_CONFIG);
        let mut tries = 0;
        loop {
            if add.is_empty() || excluded(sys, config).is_ok_and(|now| add.is_subset(&now)) {
                return Ok(());
            }
            tries += 1;
            match Self::exclude_staged(sys, config, add) {
                Err(Unexcluded::Changed(_)) if tries < EDIT_TRIES => {}
                Err(Unexcluded::Changed(_)) => {
                    return Err(Unexcluded::Changed(format!(
                        "{VOLUME_CONFIG} changed while it was being edited, each of {EDIT_TRIES} \
                         times: Spotlight kept rewriting it, so the folders were left as \
                         Spotlight wrote it"
                    )));
                }
                done => return done,
            }
        }
    }

    /// [`Self::exclude`]'s edit of a copy of `config` and its rename over it.
    fn exclude_staged(
        sys: &dyn System,
        config: &Path,
        add: &BTreeSet<PathBuf>,
    ) -> Result<(), Unexcluded> {
        use Unexcluded::Failed;
        let dir = config
            .parent()
            .ok_or_else(|| Failed("the configuration has no folder".to_owned()))?;
        let scratch = crate::port::Scratch::new_in(sys, dir, "spotlight")
            .map_err(|error| Failed(format!("{}: {error}", dir.display())))?;
        let staged = scratch.path().join("VolumeConfiguration.plist");
        sys.copy_file(config, &staged, 0o600)
            .map_err(|error| Failed(format!("{VOLUME_CONFIG}: {error}")))?;
        let copied = sys
            .sha256(&staged)
            .map_err(|error| Failed(format!("{}: {error}", staged.display())))?;
        // A configuration with no list yet gets an empty one; where that fails too (the key
        // holds something else), the first reading's refusal is the reason.
        let before = match excluded(sys, &staged) {
            Ok(excluded) => excluded,
            Err(why) => {
                ran_ok(
                    sys,
                    &config_cmd(&["-insert", EXCLUSIONS, "-array"], &staged),
                )
                .map_err(|_| Failed(why))?;
                excluded(sys, &staged).map_err(Failed)?
            }
        };
        for path in add.difference(&before) {
            ran_ok(
                sys,
                &config_cmd(
                    &[
                        "-insert",
                        &format!("{EXCLUSIONS}.0"),
                        "-string",
                        &path.to_string_lossy(),
                    ],
                    &staged,
                ),
            )
            .map_err(Failed)?;
        }
        ran_ok(sys, &config_cmd(&["-lint"], &staged)).map_err(Failed)?;
        let after = excluded(sys, &staged).map_err(Failed)?;
        if let Some(lost) = add
            .difference(&after)
            .chain(before.difference(&after))
            .next()
        {
            return Err(Failed(format!(
                "{VOLUME_CONFIG} would not list {} after the edit",
                lost.display()
            )));
        }
        let was = sys
            .stat(config)
            .map_err(|error| Failed(format!("{VOLUME_CONFIG}: {error}")))?;
        sys.set_owner_and_mode(&staged, was.uid, was.gid, was.mode)
            .map_err(|error| Failed(format!("{}: {error}", staged.display())))?;
        if sys.sha256(config).ok().as_ref() != Some(&copied) {
            return Err(Unexcluded::Changed(format!(
                "{VOLUME_CONFIG} changed while it was being edited: Spotlight rewrote it"
            )));
        }
        sys.rename(&staged, config)
            .map_err(|error| Failed(format!("{VOLUME_CONFIG}: {error}")))?;
        let kill = Cmd::new(KILLALL).args(["-9", "mds"]);
        let ran = sys
            .run(&kill)
            .map_err(|error| Failed(format!("{}: {error}", kill.display())))?;
        // With no mds running there is none to end: launchd's next one reads the list.
        if !ran.success() && !ran.err().contains("No matching processes") {
            return Err(Failed(format!("{}: {}", kill.display(), ran.describe())));
        }
        Ok(())
    }
}

impl Item for Scope {
    fn id(&self) -> String {
        SCOPE_ID.into()
    }
    fn family(&self) -> &'static str {
        "spotlight"
    }
    fn title(&self) -> String {
        format!("Spotlight indexes {KEPT_WORDS} only")
    }
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "Spotlight is macOS's".into(),
            };
        }
        let Some(home) = Self::home(ctx) else {
            return Probe::NotApplicable {
                why: "no one is logged in at this Mac's console".into(),
            };
        };
        match scope(sys, &home) {
            Ok(Standing::Disabled) => Probe::Ok {
                now: "indexing disabled".into(),
            },
            Ok(Standing::Scoped(folders)) => Probe::Ok {
                now: format!("setup has excluded each of {folders} folders beside {KEPT_WORDS}"),
            },
            Ok(Standing::Indexed { folders, .. }) => Probe::Change {
                from: format!("indexed: {}", summary(&folders, &home)),
                to: format!("excluded; {KEPT_WORDS} stay indexed"),
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    /// Exclude each folder no receipt names yet ([`Scope::exclude`]), then write the receipt.
    /// A folder the receipt names is never added again: where the configuration lacks it, a
    /// person took it out. The receipt comes last, so a run that stopped before it leaves a
    /// home never scoped still asked for ([`automatic_scope_owed`]) and any home's folders
    /// still to exclude ([`scope_owed`]); the next one finds the folders excluded already and
    /// only writes the receipt. A failure other than a configuration `mds` rewrote under each
    /// of its edits leaves [`SCOPE_REFUSED`] naming the folders it could not exclude. The home is
    /// read before the lock, so a reading that hangs holds no other root writer; it holds
    /// [`SCOPE_LOCK`] from its reading of the receipt to its last write, waiting [`LOCK_WAIT`]
    /// for a LAN broker's apply or the page's step under way, and fails, naming nothing refused,
    /// where that holder keeps it longer.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        if !sys.macos() {
            return Err("Spotlight is macOS's".into());
        }
        if sys.euid() != 0 {
            return Err("Spotlight's excluded folders belong to setup's administrator step".into());
        }
        let home = Self::home(ctx).ok_or("no one is logged in at this Mac's console")?;
        // The home's reading takes no lock: a reading that hangs holds no other root writer.
        let wanted = wanted(sys, &home)?;
        let _held = hold_scope(sys, LOCK_WAIT).map_err(|error| format!("{SCOPE_LOCK}: {error}"))?;
        let noted = spotlight_scope::listed_as_root(&Port(sys), SCOPE_RECEIPT)?;
        let add: BTreeSet<PathBuf> = wanted.difference(&noted).cloned().collect();
        note_and_exclude(sys, &noted, &add, Route::Password).map_err(String::from)
    }
    fn revert(&self) -> Option<String> {
        Some(format!(
            "as root, remove the folder's entry from {EXCLUSIONS} in {VOLUME_CONFIG}, then \
             {KILLALL} -9 mds; setup does not add it again"
        ))
    }
}

/// The root route an exclusion is made by ([`note_and_exclude`]), which says which record beside
/// the receipt it keeps.
#[derive(Debug, Clone, Copy)]
enum Route<'a> {
    /// The password route's own apply ([`Scope`]): it keeps [`SCOPE_REFUSED`], which that route
    /// alone reads ([`spotlight_scope::owed`]). A failure writes the folders it could not
    /// exclude there, best effort (without it a home never scoped is only asked for again), and
    /// a success removes the record. It never writes [`SCOPE_BROKERED`].
    Password,
    /// The LAN broker's ([`broker_duty`]), its record of its own lines of the receipt as read
    /// under the lock in hand: it writes [`SCOPE_BROKERED`] as those lines the receipt still
    /// names and the folders it adds, after the exclusion and before the receipt, and neither
    /// reads nor writes [`SCOPE_REFUSED`].
    Broker(&'a BTreeSet<PathBuf>),
}

/// Exclude `add` from Spotlight ([`Scope::exclude`]), then write root's receipt as what it
/// named (`noted`) and `add`: the one apply the password route's root step and the root LAN
/// broker share, which differ only in the record their `route` keeps. The receipt comes last,
/// so a run that stopped before it leaves the folders owed, and the next finds them excluded
/// already and only writes the receipt; the broker's record comes before it, so a line the
/// receipt lacks is one no reader counts.
///
/// A receipt `noted` and `add` would take past [`spotlight_scope::RECEIPT_MAX`] fails before
/// Spotlight is edited: no route writes a receipt no reader reads, and none excludes a folder
/// its receipt then cannot name. The caller holds [`SCOPE_LOCK`] and read `noted` under it.
/// A configuration `mds` rewrote under each of the edit's tries ([`Unexcluded::Changed`]) is no
/// refusal of the folders: [`SCOPE_REFUSED`] is left as it is, so the password route's next run
/// asks for them again, and the broker answers busy.
///
/// # Errors
/// [`Unexcluded::Changed`] for a configuration rewritten under each try; [`Unexcluded::Failed`]
/// for a receipt that would pass its bound, any other failure of the exclusion, or a record or
/// receipt that could not be written.
fn note_and_exclude(
    sys: &dyn System,
    noted: &BTreeSet<PathBuf>,
    add: &BTreeSet<PathBuf>,
    route: Route<'_>,
) -> Result<(), Unexcluded> {
    let mut receipt = noted.clone();
    receipt.extend(add.iter().cloned());
    let bytes: usize = receipt.iter().map(|path| path.as_os_str().len() + 1).sum();
    let done = if bytes > spotlight_scope::RECEIPT_MAX {
        Err(Unexcluded::Failed(format!(
            "{SCOPE_RECEIPT} would pass its bound of {} bytes with these folders: Spotlight is \
             left as it is",
            spotlight_scope::RECEIPT_MAX
        )))
    } else {
        Scope::exclude(sys, add)
            .and_then(|()| match route {
                Route::Password => Ok(()),
                Route::Broker(brokered) => {
                    let mut own: BTreeSet<PathBuf> =
                        brokered.intersection(noted).cloned().collect();
                    own.extend(add.iter().cloned());
                    list(sys, SCOPE_BROKERED, &own).map_err(Unexcluded::Failed)
                }
            })
            .and_then(|()| list(sys, SCOPE_RECEIPT, &receipt).map_err(Unexcluded::Failed))
    };
    if matches!(route, Route::Password) {
        match &done {
            Ok(()) => {
                if sys.stat(Path::new(SCOPE_REFUSED)).is_ok() {
                    let _ = sys.remove(Path::new(SCOPE_REFUSED));
                }
            }
            Err(Unexcluded::Failed(_)) => drop(list(sys, SCOPE_REFUSED, add)),
            Err(Unexcluded::Changed(_)) => {}
        }
    }
    done
}

/// The longest the LAN broker's reading of one home takes ([`broker_duty`]) before its request
/// leaves Spotlight as it is: a hang detector over reads of a few thousand names' metadata, which
/// a filesystem an account mounts in its home and keeps from answering would hold without end.
/// Two plans, the pace and an apply stay within the client's own bound for the request.
pub const READ_DEADLINE: Duration = Duration::from_secs(30);

/// The LAN broker's Spotlight duty over `sys` (`aterm lan serve` hands it `port::Live`, as root):
/// the exclusion of the console account's own new top-level home folders, worked out from what
/// root reads, never from the request, which names nothing (`docs/DESIGN-lan-broker-2026-11-05.md`
/// §2). Root must never edit or exclude a path an ordinary account can steer outside that
/// account's own top-level home folders, so a plan holds to the shared reading's rules
/// ([`spotlight_scope::broker_home_reading`], [`spotlight_scope::broker_plan_over`]), read for
/// each request, which the account reads too, with no root, before it asks:
///
/// - The peer the kernel names is not root, owns `/dev/console`, and its home is a plain path on
///   the data volume no filesystem is mounted on, under folders root's alone, of its own with no
///   sticky bit.
/// - The folders are the home's own top-level ones the scope leaves out ([`Scope`]'s rule there),
///   never one in ~/Library, at the data volume's top level or in /Users//Shared, and none while
///   the volume is not indexed (the person's pause among the causes).
/// - Root's receipt is read strictly, a folder it names is never added again, and neither the
///   password route's [`SCOPE_REFUSED`] nor the person's kept-out folders
///   ([`spotlight_scope::YOURS`]) is read or written.
/// - An apply holds root's one lock on Spotlight's configuration and the receipt
///   ([`SCOPE_LOCK`]), tried and never waited for: while the password route's step or the
///   page's holds it the request is answered busy, as it is where the lock was not taken for
///   another cause that passes (a full disk, no descriptor left). The receipt and the broker's
///   record of its own lines ([`SCOPE_BROKERED`]) are read again under it, which drops from the
///   plan every folder another route excluded since, and holds what is left to the plan's bounds
///   again ([`spotlight_scope::broker_fit`]).
/// - The home's listing is read to at most [`spotlight_scope::LISTING_MAX`] names, a name another
///   filesystem is mounted on is never read, and the reading runs on a thread of its own that a
///   request waits [`READ_DEADLINE`] for; while one that outlived it still runs, no other starts,
///   so a filesystem that never answers holds one thread and no lock.
/// - At most [`spotlight_scope::SCOPE_BATCH`] folders a request; a home keeps at most
///   [`spotlight_scope::HOME_LINES`] lines and [`spotlight_scope::HOME_BYTES`] bytes of the
///   receipt, the broker's own lines at most [`spotlight_scope::BROKER_MAX`], and the receipt
///   stays within its bound.
/// - A configuration `mds` rewrote under the edit is edited again from a fresh copy, a bounded
///   number of times ([`Scope::exclude`]); one it rewrote under each is answered busy.
/// - The same account's same folders whose apply failed are not tried again for
///   [`spotlight_scope::LEFT_KEPT`], or until the broker restarts.
///
/// What the account steers is only which of its own top-level folders' names reach `plutil` as
/// separate arguments and `mds` as path strings: an account can already keep any of those
/// folders out of Spotlight by renaming it `*.noindex`.
#[must_use]
pub fn broker_duty(sys: Arc<dyn System + Send + Sync>) -> Arc<dyn aterm_uds::lan::ScopeDuty> {
    broker_duty_within(sys, READ_DEADLINE)
}

/// [`broker_duty`] whose reading of a home waits at most `deadline`.
#[must_use]
pub fn broker_duty_within(
    sys: Arc<dyn System + Send + Sync>,
    deadline: Duration,
) -> Arc<dyn aterm_uds::lan::ScopeDuty> {
    Arc::new(BrokerDuty {
        sys,
        failed: Mutex::new(None),
        deadline,
        reading: Arc::new(AtomicBool::new(false)),
    })
}

/// [`broker_duty`]'s state: the system, the last plan whose apply failed and when, by the
/// duty's clock, and the reading of a home under way past its deadline.
struct BrokerDuty {
    sys: Arc<dyn System + Send + Sync>,
    failed: Mutex<Option<(ScopePlan, u64)>>,
    deadline: Duration,
    /// Set while a reading of a home runs; one past its deadline keeps it set until it ends.
    reading: Arc<AtomicBool>,
}

impl BrokerDuty {
    /// `uid`'s home as [`spotlight_scope::broker_home_reading`] reads it, on a thread of its own,
    /// waited for at most [`Self::deadline`].
    fn read_home(&self, uid: u32) -> Result<spotlight_scope::HomeReading, Leave> {
        if self.reading.swap(true, Ordering::AcqRel) {
            return Err(Leave::Unread(
                "an earlier reading of a home has not ended".to_owned(),
            ));
        }
        let (sent, heard) = std::sync::mpsc::sync_channel(1);
        let sys = Arc::clone(&self.sys);
        let reading = Arc::clone(&self.reading);
        let spawned = std::thread::Builder::new()
            .name("lan-scope-read".into())
            .spawn(move || {
                read_then_end(
                    reading,
                    || spotlight_scope::broker_home_reading(&Port(sys.as_ref()), uid),
                    |read| drop(sent.send(read)),
                );
            });
        if spawned.is_err() {
            self.reading.store(false, Ordering::Release);
            return Err(Leave::Unread(
                "the LAN broker could not start its reading of this home".to_owned(),
            ));
        }
        heard.recv_timeout(self.deadline).unwrap_or_else(|_| {
            Err(Leave::Unread(format!(
                "this account's home did not answer within {} s",
                self.deadline.as_secs()
            )))
        })
    }
}

/// The end of a reading of a home ([`BrokerDuty::read_home`]), however its thread ends, a panic
/// included: the next request may start another.
struct ReadingEnds(Arc<AtomicBool>);

/// A reading of a home as its thread runs it ([`BrokerDuty::read_home`]): `read`, then the
/// reading's end ([`ReadingEnds`]), and only then `send`, which hands the result to the request,
/// so a request that hears it never finds this reading still held and answers the next busy.
fn read_then_end<R>(reading: Arc<AtomicBool>, read: impl FnOnce() -> R, send: impl FnOnce(R)) {
    let read = {
        let _ends = ReadingEnds(reading);
        read()
    };
    send(read);
}

impl Drop for ReadingEnds {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// A leave's answer to a client: a refusal refused, a cause that stands Spotlight left as it
/// is, and a read that did not answer busy, which the client asks again on the clock.
fn answer_of(leave: Leave) -> ScopeAnswer {
    match leave {
        Leave::Refused(why) => ScopeAnswer::Refused(why),
        Leave::Stands(why) => ScopeAnswer::Unchanged(why),
        Leave::Unread(why) => busy(&why),
    }
}

/// The busy answer, journaled with `why`: the client asks again, the broker does not.
fn busy(why: &str) -> ScopeAnswer {
    aterm_log::info!("Spotlight: the LAN broker answered busy, and the client asks again: {why}");
    ScopeAnswer::Busy
}

/// Whether root's lock not taken for `error` ([`hold_scope`]) is a cause that passes, which the
/// broker answers busy and the client asks again on the clock: another holder, a full disk or
/// quota making its folder, an interrupted call, or no memory or descriptor left. Any other (a
/// link at the lock's path, a file there that is not root's) stands until something else
/// changes it, and is a definite leave.
fn passing(error: &io::Error) -> bool {
    use io::ErrorKind::{Interrupted, OutOfMemory, QuotaExceeded, StorageFull, WouldBlock};
    matches!(
        error.kind(),
        WouldBlock | StorageFull | QuotaExceeded | Interrupted | OutOfMemory
    ) || matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

impl aterm_uds::lan::ScopeDuty for BrokerDuty {
    fn plan(&self, uid: u32) -> Result<ScopePlan, ScopeAnswer> {
        let sys = self.sys.as_ref();
        let folders = spotlight_scope::broker_plan_over(&Port(sys), || self.read_home(uid))
            .map_err(answer_of)?;
        let plan = ScopePlan { uid, folders };
        let now = sys.now();
        let failed_lately = self
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(failed, at)| {
                *failed == plan && *at <= now && now - at < spotlight_scope::LEFT_KEPT.as_secs()
            });
        if !plan.folders.is_empty() && failed_lately {
            return Err(ScopeAnswer::Unchanged(
                "the LAN broker's last exclusion of these same folders failed, and it does not try \
                 them again for a day, or until it restarts"
                    .to_owned(),
            ));
        }
        Ok(plan)
    }

    /// Exclude `plan`'s folders under [`SCOPE_LOCK`], tried and never waited for: busy where the
    /// password route or the page's step holds it, or where it was not taken for another cause
    /// that passes ([`passing`]). The receipt and the broker's record of its own lines are read
    /// under it: a folder the receipt names since the plan was made (the password route's run in
    /// between) is not added again, since where the configuration lacks it the person took it
    /// out, and what is left is held to the plan's bounds again over what was read
    /// ([`spotlight_scope::broker_fit`]), so that run moves no bound. A configuration `mds`
    /// rewrote under each of the edit's tries is busy too, and the plan is not remembered as
    /// failed: nothing of ours failed, and the client's next ask tries it again.
    fn apply(&self, plan: &ScopePlan) -> ScopeAnswer {
        let sys = self.sys.as_ref();
        let _held = match hold_scope(sys, Duration::ZERO) {
            Ok(held) => held,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return ScopeAnswer::Busy,
            Err(error) if passing(&error) => {
                return busy(&format!(
                    "root's lock on Spotlight's excluded folders was not taken: {error}"
                ));
            }
            Err(_) => {
                return ScopeAnswer::Unchanged(
                    "the LAN broker left Spotlight as it is: root's lock on its excluded folders \
                     could not be taken"
                        .to_owned(),
                );
            }
        };
        let (noted, brokered) = match spotlight_scope::broker_lines(&Port(sys)) {
            Ok(lines) => lines,
            Err(leave) => return answer_of(leave),
        };
        let Some(home) = plan.folders.first().and_then(|folder| folder.parent()) else {
            return ScopeAnswer::Excluded(0);
        };
        let add = match spotlight_scope::broker_fit(
            &noted,
            &brokered,
            home,
            plan.folders.iter().cloned(),
        ) {
            Ok(add) => add,
            Err(leave) => return answer_of(leave),
        };
        let mut failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        match note_and_exclude(sys, &noted, &add, Route::Broker(&brokered)) {
            Ok(()) => {
                *failed = None;
                ScopeAnswer::Excluded(u32::try_from(add.len()).unwrap_or(u32::MAX))
            }
            Err(Unexcluded::Changed(why)) => busy(&why),
            Err(Unexcluded::Failed(_)) => {
                *failed = Some((plan.clone(), sys.now()));
                ScopeAnswer::Failed(
                    "the LAN broker could not exclude these folders: its edit of Spotlight's \
                     configuration, or its receipt, failed"
                        .to_owned(),
                )
            }
        }
    }

    fn now(&self) -> u64 {
        self.sys.now()
    }

    fn sleep(&self, period: std::time::Duration) {
        self.sys.sleep(period);
    }
}

/// Why `sync.spotlight` asks nothing where no root LAN broker of this build serves
/// ([`broker_reading`]).
pub const NOT_SERVED: &str = "no LAN broker of this build serves this Mac: a home setup's \
                              password never scoped is the password's, and a person's own \
                              `aterm init` asks the broker first, else the Mac password";

/// What the root LAN broker would exclude for this account's home, as the account reads its
/// rules ([`spotlight_scope::broker_would_ask`]: folders the broker left as they are within the
/// last day are not asked for), read through `sys`: what `sync.spotlight` reads before it asks.
/// Where no broker of this build serves ([`broker_serves`]) it leaves the folders as they are
/// ([`NOT_SERVED`]). `None` where this account has no home at the console.
#[must_use]
pub fn broker_reading(sys: &dyn System) -> Option<Result<BTreeSet<PathBuf>, Leave>> {
    spotlight_scope::console_home(&Port(sys))?;
    if !broker_serves(sys) {
        return Some(Err(Leave::Stands(NOT_SERVED.to_owned())));
    }
    spotlight_scope::broker_would_ask(&Port(sys))
}

/// Where this account keeps [`spotlight_scope::BROKER_LEFT`]: its state folder, none for root.
fn broker_left_record(sys: &dyn System) -> Option<PathBuf> {
    if sys.euid() == 0 {
        return None;
    }
    Some(sys.account_state_dir()?.join(spotlight_scope::BROKER_LEFT))
}

/// The folders a pass makes while it makes them, and the one asker of the root LAN broker
/// beside the lanes that make them ([`Self::ask_while_made`]): what every clone aterm publishes
/// asks, so the clone leaves Spotlight's index now, not at the next pass's `sync.spotlight`. A
/// lane only notes its folder ([`Self::made`]) and goes on; the asker asks once for every folder
/// noted since its last ask began (the request names nothing, and the broker works out the
/// folders itself), so a broker's pace of up to a minute holds no lane, and a pass of many
/// clones asks a few times, not once a clone. Its answers are journaled; nothing fails for them.
pub(crate) struct MadeFolders {
    state: Mutex<Made>,
    wake: std::sync::Condvar,
}

/// [`MadeFolders`]' state: a folder noted since the last ask began, and the makers still at
/// work.
#[derive(Default)]
struct Made {
    noted: bool,
    makers: usize,
}

/// A maker of folders at work ([`MadeFolders::maker`]): the asker ends once every one has
/// ended, a lane that panicked included, and nothing it noted is left unasked.
pub(crate) struct Maker<'a>(&'a MadeFolders);

impl Drop for Maker<'_> {
    fn drop(&mut self) {
        let mut made = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        made.makers -= 1;
        drop(made);
        self.0.wake.notify_all();
    }
}

impl MadeFolders {
    /// None noted, and no maker at work.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(Made::default()),
            wake: std::sync::Condvar::new(),
        }
    }

    /// A maker at work until the returned guard drops: take one before the asker starts and
    /// one for each lane, each before the lane is spawned.
    pub(crate) fn maker(&self) -> Maker<'_> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .makers += 1;
        Maker(self)
    }

    /// Note a folder just made at `made`, where it is a top-level folder of this account's own
    /// home while the account is the one at the console ([`spotlight_scope::console_home`]):
    /// the asker asks for it. Nothing waits.
    pub(crate) fn made(&self, sys: &dyn System, made: &Path) {
        let home = spotlight_scope::console_home(&Port(sys));
        if home.is_none() || made.parent() != home.as_deref() {
            return;
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .noted = true;
        self.wake.notify_all();
    }

    /// The asker, on a thread of its own beside the makers: ask the root LAN broker through
    /// `sys` ([`System::scope_home`]) once for the folders noted since the last ask began,
    /// until every maker has ended and nothing noted is left ([`next`]).
    pub(crate) fn ask_while_made(&self, sys: &dyn System) {
        let mut made = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match next(&mut made) {
                Next::Ask => {
                    drop(made);
                    let answer = sys.scope_home();
                    aterm_log::info!(
                        "Spotlight: the LAN broker answered {} for folders made in this account's \
                         home",
                        answer.class()
                    );
                    made = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                }
                Next::Wait => {
                    made = self.wake.wait(made).unwrap_or_else(PoisonError::into_inner);
                }
                Next::End => return,
            }
        }
    }
}

/// What the asker does next ([`MadeFolders::ask_while_made`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Ask for the folders noted since the last ask began.
    Ask,
    /// Wait for a folder noted or a maker's end.
    Wait,
    /// Every maker has ended, and nothing noted is left unasked.
    End,
}

/// The asker's next step by `made`: ask where a folder was noted (taking the note, so a folder
/// noted during the ask is the next ask's), end only once no maker is at work, else wait.
fn next(made: &mut Made) -> Next {
    if std::mem::take(&mut made.noted) {
        Next::Ask
    } else if made.makers == 0 {
        Next::End
    } else {
        Next::Wait
    }
}

/// Ask the root LAN broker through `sys` ([`System::scope_home`]) to exclude what
/// [`broker_reading`] reads it would take, and read the home again after its answer
/// ([`spotlight_scope::exclude_owed`]): what `sync.spotlight` applies. Folders the broker left
/// as they are, or failed to exclude, are kept in this account's
/// [`spotlight_scope::BROKER_LEFT`] with the time, so neither is asked for again until another
/// folder is made or [`spotlight_scope::LEFT_KEPT`] has passed, and the password owes a home
/// they leave never scoped while the record stands; an ask that excluded every folder owed
/// removes the record. Keeping it is best effort: without it, the next pass asks again.
#[must_use]
pub fn exclude_by_broker(sys: &dyn System) -> spotlight_scope::Pass {
    use spotlight_scope::Pass;
    let (pass, folders) = spotlight_scope::exclude_owed(&Port(sys), &|| sys.scope_home());
    if let Some(record) = broker_left_record(sys) {
        match &pass {
            Pass::Left(_) | Pass::Failed(_) => {
                let text = spotlight_scope::broker_left_text(&folders, sys.now());
                if let Err(error) = sys.write_file(&record, text.as_bytes(), 0o600) {
                    aterm_log::info!(
                        "Spotlight: the record of folders the LAN broker left was not kept: {error}"
                    );
                }
            }
            Pass::Done(_) if sys.stat(&record).is_ok() => {
                let _ = sys.remove(&record);
            }
            _ => {}
        }
    }
    pass
}

/// Whether the root LAN broker of this build serves this account's requests with no password,
/// as the account reads it ([`aterm_uds::lan::install::Reading::serves_with`]): this build's own
/// install as [`crate::items::lan::reading`] reads it, and launchd running its job, the rule the
/// window reads too ([`aterm_uds::lan::install::serves_this_build_here`]). Where it does and the
/// broker would take every folder owed, the broker excludes them, and no automatic ask owes the
/// password that step ([`automatic_scope_owed`]).
pub(crate) fn broker_serves(sys: &dyn System) -> bool {
    crate::items::lan::reading(sys).is_some_and(|reading| {
        reading.is_ok_and(|reading| reading.serves_with(|| crate::items::lan::job_state(sys)))
    })
}

// The person's own Spotlight steps (SD-96), each run alone from System Resources' Spotlight
// document by `sudo aterm machines here apply --plain --only <step>` and never by setup:
// `Indexing` pauses or resumes the data volume's indexing, `Rebuild` erases and rebuilds its
// index, and `KeepOut` keeps one folder of theirs out. Setup's own `Scope` leaves a volume found
// off as it is, and never removes an exclusion, so it neither lifts a pause nor takes a kept-out
// folder back.

/// `mdutil <step's arguments> /System/Volumes/Data` for `step`; `None` for a step that runs no
/// `mdutil`.
fn mdutil_cmd(step: aterm_types::macos_setup::root_only::Step) -> Option<Cmd> {
    step.mdutil()
        .map(|args| Cmd::new(MDUTIL).args(args.iter().copied()).arg(DATA_VOLUME))
}

/// The data volume's indexing turned off (`on: false`, the person's Pause indexing) or on
/// (Resume indexing) with `mdutil -i`, read back by `mdutil -s`.
pub(crate) struct Indexing {
    pub(crate) on: bool,
}

impl Indexing {
    const fn step(&self) -> aterm_types::macos_setup::root_only::Step {
        use aterm_types::macos_setup::root_only::Step;
        if self.on {
            Step::SpotlightResume
        } else {
            Step::SpotlightPause
        }
    }

    const fn word(on: bool) -> &'static str {
        if on { "indexing on" } else { "indexing off" }
    }
}

impl Item for Indexing {
    fn id(&self) -> String {
        format!("{ID}.{}", if self.on { "resume" } else { "pause" })
    }
    fn family(&self) -> &'static str {
        self.step().family()
    }
    fn title(&self) -> String {
        format!("Spotlight {} on the data volume", Self::word(self.on))
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "Spotlight is macOS's".into(),
            };
        }
        match spotlight_scope::indexing(&Port(sys)) {
            Ok(on) if on == self.on => Probe::Ok {
                now: Self::word(on).into(),
            },
            Ok(on) => Probe::Change {
                from: Self::word(on).into(),
                to: Self::word(self.on).into(),
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != 0 {
            return Err("only root turns Spotlight indexing off or on".into());
        }
        let cmd = mdutil_cmd(self.step()).ok_or("the step runs no mdutil")?;
        ran_ok(sys, &cmd)
    }
    fn revert(&self) -> Option<String> {
        Some(format!(
            "sudo {MDUTIL} -i {} {DATA_VOLUME}",
            if self.on { "off" } else { "on" }
        ))
    }
}

/// The data volume's index erased and rebuilt with `mdutil -E` (the person's Rebuild index…):
/// indexing reads on before and after, so the rebuild is the item's refresh of a state that
/// stands, journaled before it runs. A volume whose indexing is off has no index to rebuild.
pub(crate) struct Rebuild;

impl Item for Rebuild {
    fn id(&self) -> String {
        format!("{ID}.rebuild")
    }
    fn family(&self) -> &'static str {
        aterm_types::macos_setup::root_only::Step::SpotlightRebuild.family()
    }
    fn title(&self) -> String {
        "Spotlight's index of the data volume rebuilt".into()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "Spotlight is macOS's".into(),
            };
        }
        match spotlight_scope::indexing(&Port(sys)) {
            Ok(true) => Probe::Ok {
                now: "indexing on".into(),
            },
            Ok(false) => Probe::Unusable {
                why: "indexing is off on the data volume, so it has no index to rebuild".into(),
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("a rebuild runs as the index's refresh".into())
    }
    fn refresh_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        Some(current().and_then(|()| {
            if sys.euid() != 0 {
                return Err("only root rebuilds Spotlight's index".into());
            }
            let cmd = mdutil_cmd(aterm_types::macos_setup::root_only::Step::SpotlightRebuild)
                .ok_or("the step runs no mdutil")?;
            ran_ok(sys, &cmd)
        }))
    }
    fn refresh_changes(&self, _sys: &dyn System, _ctx: &Ctx) -> bool {
        true
    }
}

/// One folder the person chose kept out of Spotlight (Keep a folder out…): it joins the data
/// volume's excluded folders by [`Scope::exclude`], the scope step's own checked edit, and
/// [`spotlight_scope::YOURS`] records it as theirs. Setup's receipt never names it, and nothing
/// removes an exclusion, so the scope step never takes it back. The folder is judged by its
/// words alone against the console account's home ([`spotlight_scope::keep_out`]); nothing reads
/// it, and a volume whose indexing is off still has it kept out, for when indexing is back on.
pub(crate) struct KeepOut {
    pub(crate) folder: PathBuf,
}

impl KeepOut {
    /// The folder, judged against the console account's home and the person's other folders.
    fn judged(&self, ctx: &Ctx, yours: &BTreeSet<PathBuf>) -> Result<PathBuf, String> {
        let home = Scope::home(ctx).ok_or("no one is logged in at this Mac's console")?;
        let others: BTreeSet<PathBuf> = yours
            .iter()
            .filter(|path| **path != self.folder)
            .cloned()
            .collect();
        spotlight_scope::keep_out(&home, &others, &self.folder)
    }
}

impl Item for KeepOut {
    fn id(&self) -> String {
        format!("{ID}.keep-out")
    }
    fn family(&self) -> &'static str {
        aterm_types::macos_setup::root_only::Step::SpotlightKeepOut.family()
    }
    fn title(&self) -> String {
        format!("{} kept out of Spotlight by you", self.folder.display())
    }
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "Spotlight is macOS's".into(),
            };
        }
        let yours = match listed(sys, spotlight_scope::YOURS) {
            Ok(yours) => yours,
            Err(why) => return Probe::Unusable { why },
        };
        if let Err(why) = self.judged(ctx, &yours) {
            return Probe::Unusable { why };
        }
        let out =
            excluded(sys, Path::new(VOLUME_CONFIG)).is_ok_and(|now| now.contains(&self.folder));
        if out && yours.contains(&self.folder) {
            Probe::Ok {
                now: "kept out by you".into(),
            }
        } else {
            Probe::Change {
                from: if out {
                    "kept out, not recorded as yours".into()
                } else {
                    format!("indexed: {}", self.folder.display())
                },
                to: "kept out by you".into(),
            }
        }
    }
    /// Exclude the folder, then record it as the person's: a run cut short before the record
    /// leaves the exclusion, and the next one only writes the record. It holds [`SCOPE_LOCK`]
    /// from its reading of the record to its write, waiting [`LOCK_WAIT`] for another holder.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        if !sys.macos() {
            return Err("Spotlight is macOS's".into());
        }
        if sys.euid() != 0 {
            return Err("Spotlight's excluded folders are root's to change".into());
        }
        let _held = hold_scope(sys, LOCK_WAIT).map_err(|error| format!("{SCOPE_LOCK}: {error}"))?;
        let mut yours = listed(sys, spotlight_scope::YOURS)?;
        let folder = self.judged(ctx, &yours)?;
        Scope::exclude(sys, &BTreeSet::from([folder.clone()])).map_err(String::from)?;
        yours.insert(folder);
        list(sys, spotlight_scope::YOURS, &yours)
    }
    fn revert(&self) -> Option<String> {
        Some(format!(
            "as root, remove {} from {EXCLUSIONS} in {VOLUME_CONFIG} and from {}, then {KILLALL} \
             -9 mds",
            self.folder.display(),
            spotlight_scope::YOURS
        ))
    }
}

/// The items of the person's one Spotlight step `step` ([`HereBaseline::Only`]), `folder` being
/// the one [`aterm_types::macos_setup::root_only::Step::SpotlightKeepOut`] keeps out.
///
/// [`HereBaseline::Only`]: crate::controller::commands::HereBaseline::Only
///
/// # Errors
/// The step is no Spotlight step, or a keep-out names no folder.
pub(crate) fn person_items(
    step: aterm_types::macos_setup::root_only::Step,
    folder: Option<&Path>,
) -> Result<Vec<Box<dyn Item>>, String> {
    use aterm_types::macos_setup::root_only::Step;
    Ok(match step {
        Step::SpotlightScope => vec![Box::new(Scope)],
        Step::SpotlightPause => vec![Box::new(Indexing { on: false })],
        Step::SpotlightResume => vec![Box::new(Indexing { on: true })],
        Step::SpotlightRebuild => vec![Box::new(Rebuild)],
        Step::SpotlightKeepOut => vec![Box::new(KeepOut {
            folder: folder
                .ok_or("a keep-out names its folder (--keep-out)")?
                .to_path_buf(),
        })],
        Step::Power | Step::Updates => return Err(format!("{} is no Spotlight step", step.word())),
    })
}

/// The rustup profile that installs no documentation.
const MINIMAL: &str = "minimal";

/// Where a toolchain keeps `rust-docs`' offline HTML: the one folder rustup's manifest names
/// for the component.
const DOCS_HTML: &str = "share/doc/rust/html";

/// The name `rust-docs`' HTML is set aside under, beside where it was: Spotlight never reads a
/// folder whose name ends in `.noindex`.
const DOCS_ASIDE: &str = "html.noindex";

/// What the removal that runs in the background does, as this user: rustup takes the component
/// out of its records (removing the link that stands where the HTML was), then the HTML set aside
/// goes. `$1` is rustup, `$2` the toolchain and `$3` the folder set aside.
const REMOVE_IN_BACKGROUND: &str =
    "\"$1\" component remove rust-docs --toolchain \"$2\" && /bin/rm -rf -- \"$3\"";

/// Whether the build-cache row searches this login's home folder for the person's own project
/// build folders that Spotlight indexes, which its line notes ([`BuildNoindex`]). Spotlight's
/// answer may place one in a folder macOS guards (Documents, Desktop, Downloads, iCloud Drive,
/// another app's data), and the first read of a file there can put macOS's question about that
/// folder on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projects {
    /// Search, and note what Spotlight indexes.
    Noted,
    /// Search nothing, and note none: automatic runs and runs without the person at the
    /// console do not discover paths whose first read can raise a macOS privacy question.
    Unread,
}

/// Keep this login's Rust docs and aterm's build caches out of Spotlight, the build-cache row
/// searching for the person's project build folders as `projects` says.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx, projects: Projects) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init Rust toolchain settings require exactly one local user".into());
    };
    let home = Path::new(&user.home);
    // This process's `CARGO_HOME`, `RUSTUP_HOME` and `XDG_CONFIG_HOME` are the user's only where
    // the process runs as that user's login (its `HOME` is theirs): another account's (root's,
    // for a user init set up) name its own folders, and a test's user is no login of the test
    // process's (the Linux lane's guest exports its own `CARGO_HOME`).
    let own = std::env::var_os("HOME").is_some_and(|own| Path::new(&own) == home);
    let set = |name: &str| {
        own.then(|| std::env::var_os(name))
            .flatten()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let rustup = Rustup {
        user: user.clone(),
        cargo_home: set("CARGO_HOME").unwrap_or_else(|| home.join(".cargo")),
        rustup_home: set("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup")),
    };
    let pkg = aterm_toolchain::atpkg_prefix(home, set("XDG_CONFIG_HOME").as_deref());
    let builds = BuildNoindex::new(
        user.clone(),
        home.to_path_buf(),
        pkg,
        rustup.cargo_home.clone(),
        projects,
    );
    Ok(vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup, LastReading::default())),
        Box::new(builds),
    ])
}

/// The one item of [`items`] that keeps this login's `rust-docs` out of Spotlight
/// (`init.spotlight.rust-docs@<user>`), as bare `aterm doctor` applies it.
///
/// # Errors
/// As [`items`].
pub(crate) fn rust_docs_item(ctx: &Ctx) -> Result<Box<dyn Item>, String> {
    items(ctx, Projects::Unread)?
        .into_iter()
        .find(|item| item.id().starts_with("init.spotlight.rust-docs@"))
        .ok_or_else(|| "aterm init composes no rust-docs item".to_owned())
}

/// How long one Spotlight search of a home folder for build folders stands ([`Inventory`]),
/// found or failed: a setup run, a check and System State's reading share one for this
/// long instead of each searching the home folder again. Check again searches anew
/// ([`search_again`]).
const INVENTORY_AGE: u64 = 5 * 60;

/// What the build cache's `.ubd` folder keeps from macOS's fseventsd: every file event under it
/// (`aterm_tempfile::QUIET`), which fseventsd otherwise parses, hands to each whole-volume
/// watcher and keeps in its history. Observed on macOS 27.0, documented nowhere: a macOS that
/// stops honouring the name loses only that benefit.
const QUIET_LINE: &str = "cargo-build.noindex sits in .ubd, whose file events fseventsd drops \
                          before it delivers or keeps them (observed on macOS 27.0)";

/// The Spotlight query for build folders: the `CACHEDIR.TAG` cargo writes at the top of
/// each build folder, so the answer is a line per build folder (and per other tool's cache
/// tag), never one per artifact (a developer's home holds thousands of `.rmeta` files,
/// over the complete capture).
const BUILD_MARKER: &str = "kMDItemFSName == \"CACHEDIR.TAG\"";

/// What the comment line of the cache tag cargo writes says of its maker: "# This file is a
/// cache directory tag created by cargo."
const MADE_BY_CARGO: &str = "created by cargo";

/// The most a cache tag of cargo's is read for, in bytes: cargo writes under 200.
const TAG_MAX: u64 = 4096;

/// The files cargo keeps at the top of a cargo home, whatever names it: its package
/// cache's locks (`.package-cache`; `.package-cache-mutate` since cargo 1.74) and its
/// global cache tracker (`.global-cache`, since 1.78). A folder holding one is a cargo
/// home, whose `registry` and `git` are cargo's download caches.
const CARGO_HOME_MARKS: [&str; 3] = [".package-cache", ".package-cache-mutate", ".global-cache"];

/// Cargo's download caches in its home, each tagged as a build folder is.
const CARGO_CACHES: [&str; 2] = ["registry", "git"];

/// What one search of a home folder found: the cargo build folders Spotlight indexes, or
/// why it could not say.
type Found = Result<BTreeSet<PathBuf>, String>;

/// The process's one [`Inventory`], which every item outside a test reads.
static SHARED: std::sync::OnceLock<Arc<Inventory>> = std::sync::OnceLock::new();

/// Forget the process's last Spotlight search for build folders, found or failed, so the
/// next reading of the Rust in Spotlight row searches the home folder again: Check again's,
/// which the person pressed to see the Mac as it is now. It never waits on a search under
/// way (one can take a minute), so the window that calls it stays live.
pub fn search_again() {
    if let Some(inventory) = SHARED.get() {
        inventory.forget();
    }
}

/// The last Spotlight search for build folders: the home it searched, when (Unix seconds),
/// the [`Inventory::generation`] it began in, and what it found.
type Kept = (PathBuf, u64, u64, Found);

/// The last Spotlight search for build folders, which readers share ([`INVENTORY_AGE`]).
#[derive(Debug, Default)]
struct Inventory {
    /// The last search, held for its whole run so readers share one search.
    kept: Mutex<Option<Kept>>,
    /// Bumped by [`Self::forget`]: a search that began in an earlier generation is stale.
    /// An atomic, so a forget never waits on the lock a search under way holds.
    generation: AtomicU64,
}

impl Inventory {
    /// The process's one inventory ([`SHARED`]); a test's item keeps its own.
    fn shared() -> Arc<Self> {
        #[cfg(not(test))]
        {
            SHARED.get_or_init(Arc::default).clone()
        }
        #[cfg(test)]
        {
            Arc::default()
        }
    }

    /// Make the search kept stale, and any under way, so the next read searches again
    /// ([`search_again`]). It takes no lock: a search under way holds it.
    fn forget(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// What a search of `home` taken at most [`INVENTORY_AGE`] before `now`, and since the
    /// last [`Self::forget`], found, else `search`'s, kept for the next reader: a search
    /// that failed stands as long as one that found, so a home Spotlight cannot answer for
    /// is not searched on every read.
    fn read(&self, home: &Path, now: u64, search: impl FnOnce() -> Found) -> Found {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = self.generation.load(Ordering::Acquire);
        if let Some((at_home, at, taken_in, found)) = kept.as_ref()
            && at_home == home
            && *taken_in == generation
            && *at <= now
            && now - at < INVENTORY_AGE
        {
            return found.clone();
        }
        let found = search();
        *kept = Some((home.to_path_buf(), now, generation, found.clone()));
        found
    }
}

/// Keep aterm's cargo build cache and tooling cache in `.noindex` folders.
///
/// atpkg installs each Trust toolchain build as `store/trust/<n>.noindex` with `<n>` a
/// link to it, and its rustup view as `rustup/<name>.noindex` behind `rustup/<name>`, which
/// Spotlight never reads ([`layout`]). It lays a plain build or view there itself, never
/// while it is in use or a program runs from it (a program started from one may still be
/// under macOS's first-exec check, and syspolicyd crashes when that file's folder is
/// renamed mid-check). So a plain one left is aterm's own move still to come, and the item
/// moves nothing: the build in use and its view, which atpkg moves at its next update,
/// are one quiet note ([`TOOLCHAIN_NOTE`]), and a build no longer in use, which atpkg moves
/// at its next pass, goes unsaid. Neither is a step or a change.
///
/// The person's own cargo build folders that Spotlight indexes, whatever their names, are
/// theirs, never aterm's to rename or link: the row only counts them in a quiet note and
/// lists them in its line, and they are no step, no change and nothing the run waits on.
/// A failed search for them costs only the note.
#[derive(Debug)]
struct BuildNoindex {
    user: User,
    home: PathBuf,
    /// atpkg's prefix ([`aterm_toolchain::atpkg_prefix`]): its store and rustup views.
    pkg: PathBuf,
    /// The cargo home this process's environment names where it is the user's own login
    /// (`CARGO_HOME`, else `~/.cargo`),
    /// whose download caches (`registry`, `git`) carry the same cache tag as a build
    /// folder and are none. An app launched from the Dock has launchd's environment, never
    /// the `CARGO_HOME` a shell's startup file sets, so a cargo home is also recognised by
    /// what cargo keeps in it ([`CARGO_HOME_MARKS`]).
    cargo_home: PathBuf,
    /// The last probe's reading, which is System State's.
    reading: LastReading,
    /// Whether the row searches for the person's project build folders ([`Projects`]).
    projects: Projects,
    /// The Spotlight search for project build folders, shared for [`INVENTORY_AGE`].
    inventory: Arc<Inventory>,
}

impl BuildNoindex {
    fn new(
        user: User,
        home: PathBuf,
        pkg: PathBuf,
        cargo_home: PathBuf,
        projects: Projects,
    ) -> Self {
        Self {
            user,
            home,
            pkg,
            cargo_home,
            reading: LastReading::default(),
            projects,
            inventory: Inventory::shared(),
        }
    }

    fn caches_parent(&self) -> PathBuf {
        self.home.join("Library/Caches")
    }

    /// The shared build cache's root ([`aterm_toolchain::build_cache::CACHE_UNDER_HOME`]):
    /// `.noindex` for Spotlight, in a `.ubd` folder for fseventsd ([`QUIET_LINE`]).
    fn cargo_build_noindex(&self) -> PathBuf {
        self.home
            .join(aterm_toolchain::build_cache::CACHE_UNDER_HOME)
    }

    fn tooling_noindex(&self) -> PathBuf {
        self.home.join("Library/Caches/aterm/tooling.noindex")
    }

    fn store(&self) -> PathBuf {
        self.pkg.join(layout::STORE_DIR)
    }

    /// The Trust toolchain atpkg laid where Spotlight still indexes it, by its layout
    /// contract ([`layout`]): the build in use and the rustup views laid from it, which
    /// atpkg moves at its next update; and whether an older build is still plain, which
    /// atpkg moves at its next pass and the row leaves unsaid.
    fn exposed_toolchain(&self) -> (Vec<PathBuf>, bool) {
        let builds = layout::spotlight_exposed_builds(&self.pkg);
        let older = builds.iter().any(|build| !build.active);
        let mut in_use: Vec<PathBuf> = builds
            .into_iter()
            .filter(|build| build.active)
            .map(|build| build.dir)
            .collect();
        in_use.extend(
            layout::spotlight_exposed_views(&self.pkg)
                .into_iter()
                .map(|view| view.dir),
        );
        (in_use, older)
    }

    fn dir_present(sys: &dyn System, path: &Path) -> Result<bool, String> {
        match sys.stat(path) {
            Ok(s) => Ok(s.dir && !s.symlink),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn present(sys: &dyn System, path: &Path) -> Result<bool, String> {
        match sys.stat(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The caches apply makes that are not there yet (none without `~/Library/Caches`).
    fn missing_caches(&self, sys: &dyn System) -> Result<Vec<PathBuf>, String> {
        let mut missing = Vec::new();
        if Self::dir_present(sys, &self.caches_parent())? {
            for dir in [self.cargo_build_noindex(), self.tooling_noindex()] {
                if !Self::dir_present(sys, &dir)? {
                    missing.push(dir);
                }
            }
        }
        Ok(missing)
    }

    /// `folders`, comma-separated.
    fn listed(folders: &[PathBuf]) -> String {
        let names: Vec<String> = folders.iter().map(|d| d.display().to_string()).collect();
        names.join(", ")
    }

    /// The row's line on the toolchain aterm is using, which atpkg moves at its next update.
    fn toolchain_line(in_use: &[PathBuf]) -> String {
        format!("{TOOLCHAIN_NOTE}: {}", Self::listed(in_use))
    }

    /// The person's own cargo build folders under `self.home` that Spotlight indexes, from
    /// the [`Inventory`] (`None` when Spotlight could not be asked, or this row searches
    /// nothing ([`Projects::Unread`]), which costs only the row's note), less any gone since
    /// the search.
    fn spotlit_projects(&self, sys: &dyn System) -> Option<Vec<PathBuf>> {
        if self.projects == Projects::Unread {
            return None;
        }
        let indexed = self
            .inventory
            .read(&self.home, sys.now(), || self.search(sys))
            .ok()?;
        let mut out = Vec::new();
        for cand in indexed {
            if Self::dir_present(sys, &cand).ok()?
                && Self::present(sys, &cand.join("CACHEDIR.TAG")).ok()?
            {
                out.push(cand);
            }
        }
        Some(out)
    }

    /// Whether the cache tag at `tag` is one cargo wrote: a small file whose comment line
    /// names cargo as its maker. An empty one is none, as a pipe or a device reads.
    fn tagged_by_cargo(sys: &dyn System, tag: &Path) -> bool {
        if !sys
            .stat(tag)
            .is_ok_and(|s| !s.dir && !s.symlink && s.len > 0 && s.len <= TAG_MAX)
        {
            return false;
        }
        sys.read_file(tag).is_ok_and(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .any(|line| line.starts_with('#') && line.contains(MADE_BY_CARGO))
        })
    }

    /// The paths the cargo home the environment names is reached by: as named, and the
    /// folder it resolves to where that is a link (`~/.cargo` linked to a visible folder),
    /// since Spotlight reports a folder by its own path.
    fn cargo_homes(&self, sys: &dyn System) -> Vec<PathBuf> {
        let mut homes = vec![self.cargo_home.clone()];
        if let Ok(real) = sys.canonicalize(&self.cargo_home)
            && real != self.cargo_home
        {
            homes.push(real);
        }
        homes
    }

    /// Whether `dir` holds what cargo keeps at the top of its home ([`CARGO_HOME_MARKS`]).
    fn holds_cargo_home(sys: &dyn System, dir: &Path) -> bool {
        CARGO_HOME_MARKS
            .iter()
            .any(|mark| sys.stat(&dir.join(mark)).is_ok_and(|s| !s.dir))
    }

    /// Whether `folder` is cargo's own, where it keeps its download caches: never a
    /// project's build folder. Anything in the cargo home the environment names, by any of
    /// its paths (`homes`, [`Self::cargo_homes`]), is; one that holds the home folder
    /// (`CARGO_HOME=$HOME`) leaves out only its caches, `registry` and `git`. So is
    /// anything in the `registry` or `git` of any folder under the home that holds what
    /// cargo keeps at its home's top ([`Self::holds_cargo_home`]): the cargo home a shell
    /// names that this process's environment does not.
    fn cargo_keeps(&self, sys: &dyn System, homes: &[PathBuf], folder: &Path) -> bool {
        let named = homes.iter().any(|home| {
            if self.home.starts_with(home) {
                CARGO_CACHES
                    .into_iter()
                    .any(|cache| folder.starts_with(home.join(cache)))
            } else {
                folder.starts_with(home)
            }
        });
        named
            || folder
                .ancestors()
                .take_while(|dir| *dir != self.home && dir.starts_with(&self.home))
                .any(|dir| {
                    dir.file_name()
                        .is_some_and(|name| CARGO_CACHES.iter().any(|cache| name == *cache))
                        && dir
                            .parent()
                            .is_some_and(|home| Self::holds_cargo_home(sys, home))
                })
    }

    /// Ask Spotlight which cargo build folders under `self.home` it indexes: the folders
    /// holding a [`BUILD_MARKER`] it holds, one line each, that cargo wrote.
    fn search(&self, sys: &dyn System) -> Found {
        let mdfind_bin = Path::new("/usr/bin/mdfind");
        sys.stat(mdfind_bin)
            .map_err(|e| format!("{}: {e}", mdfind_bin.display()))?;
        let cmd = Cmd::new(mdfind_bin).as_user(&self.user).args([
            "-onlyin",
            &self.home.to_string_lossy(),
            "-0",
            BUILD_MARKER,
        ]);
        let ran = sys
            .run_complete_stdout(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        let text = std::str::from_utf8(&ran.stdout)
            .map_err(|_| "Spotlight inventory is not UTF-8".to_owned())?;
        if !text.is_empty() && !text.ends_with('\0') {
            return Err("Spotlight inventory is not a complete NUL-delimited list".into());
        }
        // The marker sits at the top of its build folder: the folder is its parent, under
        // the home folder and in no `.noindex` folder. Cargo tags its own download caches
        // in its home (`registry`, `git`) the same way: they are no project's build folder.
        let cargo_homes = self.cargo_homes(sys);
        let mut candidates = BTreeSet::new();
        for raw in text.split('\0').filter(|p| !p.is_empty()) {
            let Some(folder) = Path::new(raw).parent() else {
                continue;
            };
            let kept_out = folder
                .components()
                .any(|part| part.as_os_str().to_string_lossy().ends_with(".noindex"));
            if !kept_out
                && folder.starts_with(&self.home)
                && folder != self.home
                && !self.cargo_keeps(sys, &cargo_homes, folder)
            {
                candidates.insert(folder.to_path_buf());
            }
        }
        // A folder counts when cargo wrote its tag, whatever its name (a `CARGO_TARGET_DIR`
        // or `--target-dir` names its own), and once: a build folder inside one already
        // found (a build script's own, under its `OUT_DIR`) is part of it. In path order a
        // folder's descendants follow it, so the last one kept is the only one a later
        // folder can sit inside.
        let mut found = BTreeSet::new();
        for folder in candidates {
            if found
                .last()
                .is_some_and(|outer: &PathBuf| folder.starts_with(outer))
            {
                continue;
            }
            if Self::tagged_by_cargo(sys, &folder.join("CACHEDIR.TAG")) {
                found.insert(folder);
            }
        }
        Ok(found)
    }

    /// The row's line on the person's own project folders Spotlight indexes.
    fn projects_line(projects: &[PathBuf]) -> String {
        format!(
            "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {}",
            Self::listed(projects)
        )
    }
}

impl Item for BuildNoindex {
    fn id(&self) -> String {
        format!("init.spotlight.build-noindex@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "Rust build caches and Trust toolchain sysroots stay in .noindex folders, the build \
         cache in a .ubd folder fseventsd drops"
            .into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.reading.set(None);
        let has_caches = match Self::dir_present(sys, &self.caches_parent()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        let has_store = match Self::dir_present(sys, &self.store()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        if !has_caches && !has_store {
            return Probe::NotApplicable {
                why: format!(
                    "neither {} nor {} exists",
                    self.caches_parent().display(),
                    self.store().display()
                ),
            };
        }
        let missing = match self.missing_caches(sys) {
            Ok(missing) => missing,
            Err(why) => return Probe::Unusable { why },
        };
        let (in_use, older) = self.exposed_toolchain();
        let projects = self.spotlit_projects(sys).unwrap_or_default();
        // What the row only notes: the toolchain aterm is using, which atpkg moves at its
        // next update, and the person's project folders. Neither is a step or a change. A
        // plain build no longer in use is atpkg's to move at its next pass.
        let mut noted = Vec::new();
        if !in_use.is_empty() {
            noted.push(Self::toolchain_line(&in_use));
        }
        if !projects.is_empty() {
            noted.push(Self::projects_line(&projects));
        }
        let (projects, toolchain) = (projects.len(), !in_use.is_empty());
        if missing.is_empty() {
            let mut now = if in_use.is_empty() && !older {
                "cargo-build.noindex, tooling.noindex, and the Trust toolchain are .noindex"
            } else {
                "cargo-build.noindex and tooling.noindex are .noindex"
            }
            .to_owned();
            now.push_str(&format!("; {QUIET_LINE}"));
            if !noted.is_empty() {
                self.reading.set(Some(Reading::KeptOut {
                    projects,
                    toolchain,
                }));
                now.push_str(&format!("; {}", noted.join("; ")));
            }
            return Probe::Ok { now };
        }
        self.reading.set(Some(Reading::BuildsIndexed {
            projects,
            toolchain,
        }));
        let mut from: Vec<String> = missing
            .iter()
            .filter_map(|dir| dir.file_name())
            .map(|name| format!("{} absent", name.to_string_lossy()))
            .collect();
        from.extend(noted);
        Probe::Change {
            from: from.join("; "),
            to: "cargo-build.noindex in .ubd, and tooling.noindex".to_owned(),
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    /// Make the missing caches. Nothing else is its to move: atpkg moves its toolchain
    /// builds, and the person's own project folders are theirs.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for dir in self.missing_caches(sys)? {
            sys.create_dir_all(&dir, 0o755)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

/// This login's rustup, where it keeps its toolchains.
#[derive(Debug, Clone)]
struct Rustup {
    user: User,
    cargo_home: PathBuf,
    rustup_home: PathBuf,
}

impl Rustup {
    fn bin(&self) -> PathBuf {
        self.cargo_home.join("bin/rustup")
    }

    /// `rustup <args>` as this user, its homes named so it reads the folders probed.
    fn cmd(&self, args: &[&str]) -> Cmd {
        Cmd::new(self.bin())
            .as_user(&self.user)
            .env("CARGO_HOME", self.cargo_home.to_string_lossy())
            .env("RUSTUP_HOME", self.rustup_home.to_string_lossy())
            .args(args)
    }

    /// The trimmed standard output of a run that succeeded.
    fn read(&self, sys: &dyn System, args: &[&str]) -> Result<String, String> {
        let cmd = self.cmd(args);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        Ok(ran.out().trim().to_owned())
    }

    /// `Some(why)` when there is no rustup to ask.
    fn absent(&self, sys: &dyn System) -> Result<Option<String>, String> {
        match sys.stat(&self.bin()) {
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Some(format!("no rustup at {}", self.bin().display())))
            }
            Err(e) => Err(format!("{}: {e}", self.bin().display())),
        }
    }

    /// The toolchains rustup installed that carry `rust-docs`, and where each one's HTML is.
    fn with_docs(&self, sys: &dyn System) -> Result<Vec<Docs>, String> {
        let list = self.read(sys, &["toolchain", "list"])?;
        let mut out = Vec::new();
        for name in list.lines().filter_map(|l| l.split_whitespace().next()) {
            let dir = self.rustup_home.join("toolchains").join(name);
            if sys.stat(&dir).is_ok_and(|s| s.symlink) {
                continue;
            }
            let record = dir.join("lib/rustlib/components");
            let text = match sys.read_file(&record) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", record.display())),
            };
            if text
                .lines()
                .any(|c| c == "rust-docs" || c.starts_with("rust-docs-"))
            {
                let html = dir.join(DOCS_HTML);
                // A link in its place stands for the HTML set aside.
                let indexed = sys.read_link(&html).is_err()
                    && match sys.stat(&html) {
                        Ok(stat) => stat.dir && !stat.symlink,
                        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                        Err(e) => return Err(format!("{}: {e}", html.display())),
                    };
                out.push(Docs {
                    toolchain: name.to_owned(),
                    html,
                    indexed,
                });
            }
        }
        Ok(out)
    }

    /// Set `docs`' HTML aside where Spotlight never reads it, at once: the folder is renamed
    /// `html.noindex` and a link to it stands in its place, so rustup's removal still finds the
    /// folder its manifest names. Then start rustup's removal of the component, and the HTML's,
    /// in the background ([`REMOVE_IN_BACKGROUND`]): removing tens of thousands of files is no
    /// step a setup run waits on.
    fn set_aside(&self, sys: &dyn System, docs: &Docs) -> Result<(), String> {
        let aside = docs.html.with_file_name(DOCS_ASIDE);
        // An aside an earlier run left (its removal failed, then rustup put the docs back) is not
        // this run's to remove or to merge with: the row says where it is.
        match sys.stat(&aside) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "{} is there already; remove it, then run this again",
                    aside.display()
                ));
            }
            Err(e) => return Err(format!("{}: {e}", aside.display())),
        }
        sys.rename(&docs.html, &aside)
            .map_err(|e| format!("{} -> {}: {e}", docs.html.display(), aside.display()))?;
        sys.symlink(Path::new(DOCS_ASIDE), &docs.html)
            .map_err(|e| format!("{} -> {DOCS_ASIDE}: {e}", docs.html.display()))?;
        if sys.euid() == 0 {
            // Run as root for this user: the link is theirs, as the folder it replaces was.
            sys.chown(&docs.html, self.user.uid, self.user.gid)
                .map_err(|e| format!("{}: {e}", docs.html.display()))?;
        }
        let removal = Cmd::new("/bin/sh")
            .as_user(&self.user)
            .env("CARGO_HOME", self.cargo_home.to_string_lossy())
            .env("RUSTUP_HOME", self.rustup_home.to_string_lossy())
            .args(["-c", REMOVE_IN_BACKGROUND, "sh"])
            .arg(self.bin())
            .arg(docs.toolchain.as_str())
            .arg(&aside);
        sys.spawn_detached(&removal, Path::new("/dev/null"))
            .map(drop)
            .map_err(|e| format!("{}: {e}", removal.display()))
    }
}

/// A toolchain rustup installed with `rust-docs` ([`Rustup::with_docs`]).
#[derive(Debug)]
struct Docs {
    toolchain: String,
    /// Where its HTML is: [`DOCS_HTML`] in the toolchain.
    html: PathBuf,
    /// The HTML is a plain folder there, which Spotlight indexes. Otherwise it was set aside in
    /// [`DOCS_ASIDE`] behind a link, or is gone, and its removal is rustup's, under way or ended.
    indexed: bool,
}

/// rustup installs its `minimal` profile, so a toolchain installed later brings no docs.
#[derive(Debug)]
struct Profile(Rustup);

impl Item for Profile {
    fn id(&self) -> String {
        format!("init.spotlight.rustup-profile@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "rustup installs its minimal profile (no offline docs for Spotlight)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        match self.0.read(sys, &["show", "profile"]) {
            Err(why) => Probe::Unusable { why },
            Ok(p) if p == MINIMAL => Probe::Ok {
                now: "rustup's profile is minimal".into(),
            },
            Ok(p) => Probe::Change {
                from: format!("rustup's profile is {p}, which installs rust-docs"),
                to: "minimal".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.0.read(sys, &["set", "profile", MINIMAL]).map(drop)
    }

    fn revert(&self) -> Option<String> {
        Some(format!("{} set profile default", self.0.bin().display()))
    }
}

/// No toolchain rustup installed carries `rust-docs`; the last probe's reading is
/// System State's.
#[derive(Debug)]
struct NoDocs(Rustup, LastReading);

impl Item for NoDocs {
    fn id(&self) -> String {
        format!("init.spotlight.rust-docs@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "no rustup toolchain carries rust-docs (offline HTML Spotlight indexes)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.1.set(None);
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        let docs = match self.0.with_docs(sys) {
            Ok(docs) => docs,
            Err(why) => return Probe::Unusable { why },
        };
        let names = |indexed: bool| {
            docs.iter()
                .filter(|docs| docs.indexed == indexed)
                .map(|docs| docs.toolchain.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if docs.iter().any(|docs| docs.indexed) {
            self.1.set(Some(Reading::DocsInstalled));
            return Probe::Change {
                from: format!("rust-docs installed in {}", names(true)),
                to: format!("set aside as {DOCS_ASIDE} at once, then removed in the background"),
            };
        }
        Probe::Ok {
            now: if docs.is_empty() {
                "no rustup toolchain carries rust-docs".into()
            } else {
                format!(
                    "rust-docs in {} is set aside as {DOCS_ASIDE}, which Spotlight never reads; \
                     rustup removes it in the background",
                    names(false)
                )
            },
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.1.detail()
    }

    /// Set each indexed toolchain's docs aside at once and remove them in the background
    /// ([`Rustup::set_aside`]): the journal's one entry names both.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for docs in self.0.with_docs(sys)? {
            if docs.indexed {
                self.0.set_aside(sys, &docs)?;
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "while the background removal has not run, replace each toolchain's \
             {DOCS_HTML} link with its {DOCS_ASIDE}; once it has, {} component add rust-docs \
             --toolchain <each toolchain it was removed from>",
            self.0.bin().display()
        ))
    }
}

#[cfg(test)]
#[path = "init_spotlight_tests.rs"]
pub(crate) mod tests;

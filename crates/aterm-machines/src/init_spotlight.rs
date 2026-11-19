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
use aterm_types::macos_setup::spotlight_scope::{self, State as Standing};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

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

/// `sys` as the shared reading's port ([`spotlight_scope::Reads`]): the reading itself is
/// `aterm-types`' so the window, which does not link this crate outside its tests, makes the
/// same one from its own file reads.
pub(crate) struct Port<'a>(pub(crate) &'a dyn System);

impl spotlight_scope::Reads for Port<'_> {
    fn macos(&self) -> bool {
        self.0.macos()
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
        })
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        self.0.canonicalize(path)
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.0.list_dir(dir)
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

/// Write `paths`, one a line, as root's file at `file`, readable by every account.
pub(crate) fn list(sys: &dyn System, file: &str, paths: &BTreeSet<PathBuf>) -> Result<(), String> {
    let text: String = paths
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect();
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
/// ([`spotlight_scope::automatic_owed`]: only while setup's password never scoped the home),
/// the one reading every run of `aterm init` and the window
/// ([`spotlight_scope::automatic_owed_here`]) share.
#[must_use]
pub fn automatic_scope_owed(sys: &dyn System) -> bool {
    spotlight_scope::automatic_owed(&Port(sys))
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
    /// leaves the list as it was. A configuration `mds` rewrote meanwhile is left alone, and
    /// the step fails. With nothing to add, nothing is written and `mds` is left running.
    fn exclude(sys: &dyn System, add: &BTreeSet<PathBuf>) -> Result<(), String> {
        let config = Path::new(VOLUME_CONFIG);
        if add.is_empty() || excluded(sys, config).is_ok_and(|now| add.is_subset(&now)) {
            return Ok(());
        }
        let dir = config.parent().ok_or("the configuration has no folder")?;
        let scratch = crate::port::Scratch::new_in(sys, dir, "spotlight")
            .map_err(|error| format!("{}: {error}", dir.display()))?;
        let staged = scratch.path().join("VolumeConfiguration.plist");
        sys.copy_file(config, &staged, 0o600)
            .map_err(|error| format!("{VOLUME_CONFIG}: {error}"))?;
        let copied = sys
            .sha256(&staged)
            .map_err(|error| format!("{}: {error}", staged.display()))?;
        // A configuration with no list yet gets an empty one; where that fails too (the key
        // holds something else), the first reading's refusal is the reason.
        let before = match excluded(sys, &staged) {
            Ok(excluded) => excluded,
            Err(why) => {
                ran_ok(
                    sys,
                    &config_cmd(&["-insert", EXCLUSIONS, "-array"], &staged),
                )
                .map_err(|_| why)?;
                excluded(sys, &staged)?
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
            )?;
        }
        ran_ok(sys, &config_cmd(&["-lint"], &staged))?;
        let after = excluded(sys, &staged)?;
        if let Some(lost) = add
            .difference(&after)
            .chain(before.difference(&after))
            .next()
        {
            return Err(format!(
                "{VOLUME_CONFIG} would not list {} after the edit",
                lost.display()
            ));
        }
        let was = sys
            .stat(config)
            .map_err(|error| format!("{VOLUME_CONFIG}: {error}"))?;
        sys.set_owner_and_mode(&staged, was.uid, was.gid, was.mode)
            .map_err(|error| format!("{}: {error}", staged.display()))?;
        if sys.sha256(config).ok().as_ref() != Some(&copied) {
            return Err(format!("{VOLUME_CONFIG} changed while it was being edited"));
        }
        sys.rename(&staged, config)
            .map_err(|error| format!("{VOLUME_CONFIG}: {error}"))?;
        let kill = Cmd::new(KILLALL).args(["-9", "mds"]);
        let ran = sys
            .run(&kill)
            .map_err(|error| format!("{}: {error}", kill.display()))?;
        // With no mds running there is none to end: launchd's next one reads the list.
        if !ran.success() && !ran.err().contains("No matching processes") {
            return Err(format!("{}: {}", kill.display(), ran.describe()));
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
    /// only writes the receipt. A failure leaves [`SCOPE_REFUSED`] naming the folders it could not exclude.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        if !sys.macos() {
            return Err("Spotlight is macOS's".into());
        }
        if sys.euid() != 0 {
            return Err("Spotlight's excluded folders belong to setup's administrator step".into());
        }
        let home = Self::home(ctx).ok_or("no one is logged in at this Mac's console")?;
        let wanted = wanted(sys, &home)?;
        let mut noted = listed(sys, SCOPE_RECEIPT).unwrap_or_default();
        let add: BTreeSet<PathBuf> = wanted.difference(&noted).cloned().collect();
        noted.extend(wanted);
        let done = Self::exclude(sys, &add).and_then(|()| list(sys, SCOPE_RECEIPT, &noted));
        match &done {
            Ok(()) => {
                if sys.stat(Path::new(SCOPE_REFUSED)).is_ok() {
                    let _ = sys.remove(Path::new(SCOPE_REFUSED));
                }
            }
            // Best effort: without it a home never scoped is only asked for again.
            Err(_) => drop(list(sys, SCOPE_REFUSED, &add)),
        }
        done
    }
    fn revert(&self) -> Option<String> {
        Some(format!(
            "as root, remove the folder's entry from {EXCLUSIONS} in {VOLUME_CONFIG}, then \
             {KILLALL} -9 mds; setup does not add it again"
        ))
    }
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
    /// leaves the exclusion, and the next one only writes the record.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        if !sys.macos() {
            return Err("Spotlight is macOS's".into());
        }
        if sys.euid() != 0 {
            return Err("Spotlight's excluded folders are root's to change".into());
        }
        let mut yours = listed(sys, spotlight_scope::YOURS)?;
        let folder = self.judged(ctx, &yours)?;
        Scope::exclude(sys, &BTreeSet::from([folder.clone()]))?;
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

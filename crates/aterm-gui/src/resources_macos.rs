// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System Resources page's macOS configuration section (SD-97): each macOS setting that
//! changes what this Mac spends, its Now against its Wanted, and where they differ the one press
//! that applies it ([`ConfigRow::action`]).
//!
//! The readings that spawn a process (this aterm's `machines macos-reading`, which alone runs
//! `pmset` and `softwareupdate --schedule`, since only aterm-machines names those root tools,
//! and reads Software Update's keys as its own items read them; and `tmutil isexcluded`) run on
//! a background thread of the host's slot ([`MacosSlot`]), only while the page is on screen, at once when it comes on screen and after
//! an apply ends, and otherwise at most once every [`READ_EVERY`]: these settings rarely move.
//! The Spotlight scope comes from SD-95's probe ([`crate::resources_spotlight::ScopeSlot`]),
//! fseventsd's footprint from the reading the page already holds (the deck's split, or the
//! stream's own read of the services), and the data volume's indexing from SD-93's. The
//! judgment ([`rows`]) runs on the window's thread from those in-memory values alone, by the
//! same functions `aterm machines` and `aterm init` judge by (`aterm_types::macos_setup`).
//!
//! Power and Software Update are judged against ALab's starter baseline only on an ALab Mac
//! (System State's `alab_lockup`); elsewhere they read Now and ALab's baseline with no
//! difference and no press. The page never offers a press that turns an automatic install off.
//! No build folder under a folder macOS guards (Documents, Desktop, Downloads, the synced
//! drives) is opened, stat'ed or marked: it reads "not read here".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::baseline::{self, Updates};
use aterm_types::macos_setup::fseventsd::{self, Sent};
use aterm_types::macos_setup::power;
#[cfg(target_os = "macos")]
use aterm_types::macos_setup::readout::Readout;
use aterm_types::macos_setup::root_only::Step;
use aterm_types::macos_setup::spotlight_scope;
use aterm_types::macos_setup::updates::{self, UpdatesNow};
use aterm_vitals::Snapshot;
use aterm_vitals::services::{FSEVENTSD, large_bar};
use aterm_vitals::spotlight::Indexing;
use aterm_vitals::words::mem;

use crate::resources_spotlight::{Now, ScopeReading, what_is_indexed};

/// How often the slot reads again while the page stays on screen.
pub(crate) const READ_EVERY: Duration = Duration::from_secs(600);

/// A build folder's standing with Time Machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tm {
    /// Time Machine skips it (`tmutil isexcluded`: its mark, or macOS's standard exclusions).
    Out,
    /// Time Machine backs it up.
    In,
    /// It sits in a folder macOS guards, named here: the page does not read it.
    #[cfg(any(target_os = "macos", test))]
    Guarded(String),
    /// It does not exist yet: Time Machine has nothing of it to back up.
    Absent,
    /// `tmutil` did not answer: why.
    #[cfg(any(target_os = "macos", test))]
    Unread(String),
}

/// What the slot read of this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacosReading {
    /// Each setting of the starter's `[power.ac]`, read.
    pub(crate) power: Vec<power::Row>,
    /// Software Update as read.
    pub(crate) updates: UpdatesNow,
    /// fseventsd's last restart aterm sent ([`fseventsd::stamp_path`]).
    pub(crate) last_restart: Option<Sent>,
    /// Each build folder and its standing with Time Machine.
    pub(crate) build_folders: Vec<(PathBuf, Tm)>,
    /// Whether this process runs as the account at the console, whose home the scope step
    /// scopes ([`spotlight_scope::console_account`]).
    pub(crate) console: bool,
    /// The account's home, for the folders' words.
    pub(crate) home: Option<PathBuf>,
    /// When it was read, unix seconds.
    pub(crate) read_s: u64,
}

impl MacosReading {
    /// A reading of nothing, for `why`.
    pub(crate) fn unread(why: &str) -> Self {
        Self {
            power: Vec::new(),
            updates: UpdatesNow {
                check: Err(why.to_owned()),
                keys: Vec::new(),
            },
            last_restart: None,
            build_folders: Vec::new(),
            console: false,
            home: None,
            read_s: 0,
        }
    }

    /// `path` as a person reads it: under the home as `~/…`.
    fn shown(&self, path: &Path) -> String {
        match self
            .home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
        {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }
}

/// The folders under `home` macOS guards: the page opens, stats and marks nothing in them.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn guarded(home: &Path) -> [PathBuf; 5] {
    [
        home.join("Documents"),
        home.join("Desktop"),
        home.join("Downloads"),
        home.join("Library/CloudStorage"),
        home.join("Library/Mobile Documents"),
    ]
}

/// The guarded folder `path` is in, as a person reads it; `None` where it is in none. A pure
/// path test: no I/O.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn guarded_by(home: &Path, path: &Path) -> Option<String> {
    guarded(home)
        .into_iter()
        .find(|folder| path.starts_with(folder))
        .map(|folder| {
            folder.strip_prefix(home).map_or_else(
                |_| folder.display().to_string(),
                |rest| format!("~/{}", rest.display()),
            )
        })
}

/// How the slot reads this Mac and marks a folder.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MacosPort {
    pub(crate) read: fn() -> MacosReading,
    pub(crate) mark: fn(&Path) -> std::io::Result<()>,
}

impl MacosPort {
    /// Reads and marks nothing.
    pub(crate) const INERT: Self = Self {
        read: || MacosReading::unread("inert"),
        mark: |_| Ok(()),
    };

    /// This Mac.
    const LIVE: Self = Self {
        read: read_live,
        mark: mark_live,
    };

    /// The port a host starts with: a test's host reads nothing.
    pub(crate) const fn host() -> Self {
        if cfg!(test) { Self::INERT } else { Self::LIVE }
    }
}

/// The build folders whose Time Machine standing the section reads: the shared build cache
/// ([`aterm_toolchain::build_cache::cache_root`]). A checkout's `target/` cargo marks itself.
#[cfg(target_os = "macos")]
fn build_folders(home: &Path) -> Vec<PathBuf> {
    vec![aterm_toolchain::build_cache::cache_root(home)]
}

/// How the slot asks after a build folder that sits in no guarded folder: whether it exists
/// (an `lstat`), and whether Time Machine skips it.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TmReads {
    pub(crate) exists: fn(&Path) -> std::io::Result<bool>,
    pub(crate) excluded: fn(&Path) -> Result<bool, String>,
}

/// Each of `folders` under `home` and its standing with Time Machine. A folder in a folder macOS
/// guards ([`guarded_by`]) is never handed to `reads`: nothing stats, reads or marks it. One that
/// does not exist is [`Tm::Absent`], never asked of `tmutil`, which answers `[UNKNOWN]` for it.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn standings(home: &Path, folders: Vec<PathBuf>, reads: TmReads) -> Vec<(PathBuf, Tm)> {
    folders
        .into_iter()
        .map(|path| {
            let tm = match guarded_by(home, &path) {
                Some(folder) => Tm::Guarded(folder),
                None => match (reads.exists)(&path) {
                    Ok(false) => Tm::Absent,
                    Err(error) => Tm::Unread(format!("not checked: {error}")),
                    Ok(true) => match (reads.excluded)(&path) {
                        Ok(true) => Tm::Out,
                        Ok(false) => Tm::In,
                        Err(why) => Tm::Unread(why),
                    },
                },
            };
            (path, tm)
        })
        .collect()
}

/// Whether `path` exists, read without following a last symbolic link.
#[cfg(target_os = "macos")]
fn exists_live(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_live() -> MacosReading {
    #[cfg(target_os = "macos")]
    {
        let scope = spotlight_scope::Live;
        let home = spotlight_scope::Reads::home(&scope);
        let build_folders = home
            .as_deref()
            .map(|home| {
                standings(
                    home,
                    build_folders(home),
                    TmReads {
                        exists: exists_live,
                        excluded: tm_excluded,
                    },
                )
            })
            .unwrap_or_default();
        let readout = machines_reading();
        MacosReading {
            power: power::rows(&readout, power::Source::Ac, &baseline::starter_power_ac()),
            updates: updates::now(&readout),
            last_restart: fseventsd::stamp_path()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| Sent::parse(&bytes)),
            build_folders,
            console: spotlight_scope::console_account(&scope),
            home,
            read_s: unix_now(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        MacosReading::unread("unsupported")
    }
}

/// How long `aterm machines macos-reading` may take: two `pmset` reads, `softwareupdate
/// --schedule` and one `defaults export`, each well under a second.
#[cfg(target_os = "macos")]
const MACHINES_READ_WAIT: Duration = Duration::from_secs(30);

/// This Mac's pmset and Software Update reading, as this aterm's `machines macos-reading`
/// printed it, run as the person: only aterm-machines runs `pmset` and `softwareupdate` (census
/// `machines-root-door`). Where it did not answer, a readout that answers every read with why.
#[cfg(target_os = "macos")]
fn machines_reading() -> Readout {
    let read = || -> Result<Readout, String> {
        let exe = std::env::current_exe().map_err(|error| format!("this aterm: {error}"))?;
        let mut command = aterm_uds::spawnfd::command(&exe);
        command.args(["machines", "macos-reading"]);
        let out = aterm_uds::deadline::output_within(&mut command, MACHINES_READ_WAIT)
            .map_err(|error| format!("aterm machines macos-reading: {error}"))?;
        if !out.status.success() {
            return Err(format!("aterm machines macos-reading: {}", out.status));
        }
        Readout::parse(&String::from_utf8_lossy(&out.stdout))
            .map_err(|error| format!("aterm machines macos-reading: {error}"))
    };
    read().unwrap_or_else(|why| {
        aterm_log::info!("resources: macOS configuration: {why}");
        Readout::default()
    })
}

#[cfg(target_os = "macos")]
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether Time Machine skips `path`: `tmutil isexcluded`, which answers for its mark and for
/// macOS's standard exclusions (all of `~/Library/Caches`), with no root.
#[cfg(target_os = "macos")]
fn tm_excluded(path: &Path) -> Result<bool, String> {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/tmutil");
    command.arg("isexcluded").arg(path);
    let out = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(10))
        .map_err(|error| format!("tmutil isexcluded: {error}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("[Excluded]") {
        Ok(true)
    } else if text.contains("[Included]") {
        Ok(false)
    } else {
        Err(format!("tmutil isexcluded printed {:?}", text.trim()))
    }
}

fn mark_live(path: &Path) -> std::io::Result<()> {
    aterm_uds::backup::Backups::SYSTEM.exclude_path(path)
}

#[derive(Default)]
struct SlotState {
    said: Option<(&'static str, String)>,
    reading: Option<Arc<MacosReading>>,
    rev: u64,
    ticked: Option<Instant>,
    running: bool,
    forced: bool,
}

/// The page's reader of these settings, one a host ([`crate::vitals_host::VitalsHost`]).
pub(crate) struct MacosSlot {
    port: MacosPort,
    state: Mutex<SlotState>,
}

impl Default for MacosSlot {
    fn default() -> Self {
        Self::new(MacosPort::host())
    }
}

impl MacosSlot {
    pub(crate) fn new(port: MacosPort) -> Self {
        Self {
            port,
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The last reading and its revision.
    pub(crate) fn reading(&self) -> Option<(Arc<MacosReading>, u64)> {
        let state = self.lock();
        state.reading.clone().map(|reading| (reading, state.rev))
    }

    /// What the last press of row `slug` did, for the page to say under it.
    pub(crate) fn say(&self, slug: &'static str, words: String) {
        self.lock().said = Some((slug, words));
    }

    /// What the last press did, and of which row.
    pub(crate) fn said(&self) -> Option<(&'static str, String)> {
        self.lock().said.clone()
    }

    /// Read again at the next service, whatever the cadence: an apply ended.
    pub(crate) fn force(&self) {
        self.lock().forced = true;
    }

    /// The port's mark, for the host's press of the build folders' Apply.
    pub(crate) const fn port(&self) -> MacosPort {
        self.port
    }

    /// At `now`, with the page `shown` on screen: read where none is running and the page just
    /// came on screen or an apply ended ([`Self::force`]), or none ran within [`READ_EVERY`]. On a
    /// background thread (inline in a test). Whether a read started.
    pub(crate) fn service(self: &Arc<Self>, shown: bool, now: Instant) -> bool {
        if !shown {
            return false;
        }
        {
            let mut state = self.lock();
            let due = state.forced
                || state
                    .ticked
                    .is_none_or(|at| now.saturating_duration_since(at) >= READ_EVERY);
            if state.running || !due {
                return false;
            }
            state.ticked = Some(now);
            state.running = true;
            state.forced = false;
        }
        let slot = Arc::clone(self);
        let tick = move || {
            let reading = (slot.port.read)();
            let mut state = slot.lock();
            state.reading = Some(Arc::new(reading));
            state.rev = state.rev.wrapping_add(1);
            state.running = false;
        };
        if cfg!(test) {
            tick();
            return true;
        }
        match crate::qos::spawn_lane_thread(
            "resources-macos-configuration",
            crate::qos::Role::Background,
            tick,
        ) {
            Ok(_) => true,
            Err(error) => {
                aterm_log::warn!(
                    "System Resources: the macOS configuration read did not start: {error}"
                );
                self.lock().running = false;
                false
            }
        }
    }
}

/// The one press a row offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Act {
    /// A single compiled root step, under the password sheet bound to the page's view.
    Root(Step),
    /// fseventsd's restart: a tab that runs `aterm init --only init.fseventsd`
    /// ([`crate::App::restart_service`]).
    RestartFseventsd,
    /// Mark the build folders Time Machine backs up, with no password.
    MarkBuildFolders,
}

/// The six rows' slugs, in order: each row's key and its Apply's action end in one.
pub(crate) const SLUGS: [&str; 6] = [
    "spotlight-scope",
    "build-spotlight",
    "build-time-machine",
    "file-events",
    "power",
    "software-update",
];

/// The pending press of a root step the Spotlight document's buttons run (SD-96): no row's
/// slug, so while one runs the section's every Apply… is idle and none reads busy.
pub(crate) const SPOTLIGHT_DOCUMENT: &str = "spotlight-document";

/// The slug of the row whose press `act` is ([`SPOTLIGHT_DOCUMENT`] for the Spotlight
/// document's own steps, which no row offers).
pub(crate) const fn slug_of(act: Act) -> &'static str {
    match act {
        Act::Root(Step::SpotlightScope) => SLUGS[0],
        Act::MarkBuildFolders => SLUGS[2],
        Act::RestartFseventsd => SLUGS[3],
        Act::Root(Step::Power) => SLUGS[4],
        Act::Root(Step::Updates) => SLUGS[5],
        Act::Root(
            Step::SpotlightPause
            | Step::SpotlightResume
            | Step::SpotlightRebuild
            | Step::SpotlightKeepOut,
        ) => SPOTLIGHT_DOCUMENT,
    }
}

/// What the host hands the page for the section: the slot's reading and its revision, whether
/// this is an ALab Mac, the row whose press is pending, and what the last press did.
#[derive(Clone, Debug, Default)]
pub(crate) struct MacosPage {
    pub(crate) reading: Option<(Arc<MacosReading>, u64)>,
    pub(crate) alab: bool,
    pub(crate) pending: Option<&'static str>,
    pub(crate) said: Option<(&'static str, String)>,
}

impl PartialEq for MacosPage {
    fn eq(&self, other: &Self) -> bool {
        self.reading.as_ref().map(|(_, rev)| *rev) == other.reading.as_ref().map(|(_, rev)| *rev)
            && self.alab == other.alab
            && self.pending == other.pending
            && self.said == other.said
    }
}

/// One setting of the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigRow {
    pub(crate) slug: &'static str,
    pub(crate) name: &'static str,
    pub(crate) now: String,
    pub(crate) wanted: String,
    /// Whether Now reads and is not Wanted.
    pub(crate) differs: bool,
    /// The press that applies Wanted, where the page offers one.
    pub(crate) action: Option<Act>,
    /// Why it reads as it does.
    pub(crate) words: String,
}

impl ConfigRow {
    /// The row in words, so colour is never the only signal.
    pub(crate) fn said(&self) -> String {
        format!(
            "{}: now {}; wanted {}{}; {}.",
            self.name,
            self.now,
            self.wanted,
            if self.differs {
                ", differs from wanted"
            } else {
                ""
            },
            self.words,
        )
    }
}

/// The data volume's indexing as SD-93's reading in `snap` carries it.
fn data_volume(snap: Option<&Snapshot>) -> Option<&Indexing> {
    snap?
        .spotlight
        .as_ref()?
        .volumes
        .as_ref()?
        .iter()
        .find(|v| v.path == spotlight_scope::DATA_VOLUME)
        .map(|v| &v.indexing)
}

/// fseventsd's footprint, its coalition's live members and the Mac's memory, as `snap` holds
/// them: the stream's own read of the services, else the deck's split by coalition.
fn fseventsd_in(snap: &Snapshot) -> Option<(u64, u32, u64)> {
    if let Some(reading) = snap.services.as_deref() {
        let service = reading
            .services
            .iter()
            .find(|s| s.name == FSEVENTSD.name && s.uid == FSEVENTSD.uid)?;
        return Some((service.footprint, service.live, reading.total));
    }
    let facts = snap.why.as_deref().filter(|f| f.lane.on())?;
    let total = snap.mem.as_ref()?.sample.total;
    let row = facts
        .split
        .rows
        .iter()
        .find(|r| r.lead.as_ref().is_some_and(|lead| FSEVENTSD.leads(lead)))?;
    Some((row.footprint?, row.live, total))
}

fn ago(seconds: u64) -> String {
    match seconds {
        s if s < 120 => format!("{s} s"),
        s if s < 2 * 3600 => format!("{} min", s / 60),
        s if s < 2 * 86_400 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn spotlight_rows(
    macos: Option<&MacosReading>,
    scope: Option<&ScopeReading>,
    snap: Option<&Snapshot>,
) -> [ConfigRow; 2] {
    let volume = data_volume(snap);
    let mut judged = what_is_indexed(scope, volume);
    let build = judged
        .pop()
        .expect("what_is_indexed ends with the build output");
    let wanted = format!(
        "{} indexed, everything else kept out",
        crate::resources_spotlight::wanted_words()
    );
    let unread = judged.iter().any(|row| row.now == Now::Unread);
    let differing: Vec<_> = judged.iter().filter(|row| row.differs()).collect();
    let now = if matches!(volume, Some(Indexing::Disabled)) {
        "indexing off on the data volume".to_owned()
    } else if !differing.is_empty() {
        differing
            .iter()
            .map(|row| {
                // A clause mid-sentence: the scope row's name starts low.
                let mut name = row.name.chars();
                let first = name.next().map(|c| c.to_lowercase().collect::<String>());
                format!(
                    "{}{} {}",
                    first.unwrap_or_default(),
                    name.as_str(),
                    row.now.word().to_lowercase()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else if unread {
        "not read yet".to_owned()
    } else {
        "as wanted".to_owned()
    };
    let words = judged
        .iter()
        .filter(|row| row.differs() || row.now == Now::Unread)
        .map(|row| format!("{}: {}", row.name, row.words))
        .collect::<Vec<_>>()
        .join("; ");
    let owed = scope.is_some_and(|scope| {
        let listing = &scope.listing;
        match (&listing.wanted, &listing.receipt) {
            (Ok(wanted), Ok(receipt)) => spotlight_scope::owed_of(
                &wanted.difference(receipt).cloned().collect(),
                &listing.refused,
            ),
            _ => false,
        }
    });
    let console = macos.is_some_and(|m| m.console);
    let offered =
        !differing.is_empty() && matches!(volume, Some(Indexing::Enabled)) && console && owed;
    let mut words = if words.is_empty() {
        "setup's receipt and Spotlight agree".to_owned()
    } else {
        words
    };
    if !differing.is_empty() && !offered {
        words.push_str(if !console {
            "; the scope is applied from the account at the console"
        } else if !owed {
            "; setup already tried each folder Spotlight still indexes, or a person took it back"
        } else {
            "; the data volume's indexing is not read yet"
        });
    }
    [
        ConfigRow {
            slug: SLUGS[0],
            name: "Spotlight scope",
            now,
            wanted,
            differs: !differing.is_empty(),
            action: offered.then_some(Act::Root(Step::SpotlightScope)),
            words,
        },
        ConfigRow {
            slug: SLUGS[1],
            name: "Build folders out of Spotlight",
            now: build.now.word().to_lowercase(),
            wanted: build.wanted.word().to_lowercase(),
            differs: build.differs(),
            // No mark keeps a folder out of Spotlight without root, and the page renames
            // nothing: the name keeps it out.
            action: None,
            words: build.words,
        },
    ]
}

fn time_machine_row(macos: Option<&MacosReading>) -> ConfigRow {
    let mut row = ConfigRow {
        slug: SLUGS[2],
        name: "Build folders out of Time Machine",
        now: "not read yet".to_owned(),
        wanted: "out of Time Machine".to_owned(),
        differs: false,
        action: None,
        words: "tmutil isexcluded reads each build folder".to_owned(),
    };
    let Some(macos) = macos.filter(|m| !m.build_folders.is_empty()) else {
        return row;
    };
    let inside: Vec<String> = macos
        .build_folders
        .iter()
        .filter(|(_, tm)| *tm == Tm::In)
        .map(|(path, _)| macos.shown(path))
        .collect();
    let out = macos
        .build_folders
        .iter()
        .filter(|(_, tm)| *tm == Tm::Out)
        .count();
    let mut clauses = Vec::new();
    if out > 0 {
        clauses.push(format!(
            "Time Machine skips {}",
            if out == 1 {
                macos
                    .build_folders
                    .iter()
                    .find(|(_, tm)| *tm == Tm::Out)
                    .map_or_else(String::new, |(path, _)| macos.shown(path))
            } else {
                format!("{out} build folders")
            }
        ));
    }
    if !inside.is_empty() {
        clauses.push(format!("Time Machine backs up {}", inside.join(", ")));
    }
    for (path, tm) in &macos.build_folders {
        match tm {
            #[cfg(any(target_os = "macos", test))]
            Tm::Guarded(folder) => clauses.push(format!(
                "{} not read here: macOS guards {folder}",
                macos.shown(path)
            )),
            #[cfg(any(target_os = "macos", test))]
            Tm::Unread(why) => clauses.push(format!("{}: {why}", macos.shown(path))),
            Tm::Absent => clauses.push(format!(
                "{} does not exist yet: nothing of it to back up",
                macos.shown(path)
            )),
            Tm::Out | Tm::In => {}
        }
    }
    let absent = macos
        .build_folders
        .iter()
        .filter(|(_, tm)| *tm == Tm::Absent)
        .count();
    row.now = if inside.is_empty() {
        if out > 0 {
            "out".to_owned()
        } else if absent == macos.build_folders.len() {
            "no build folder yet".to_owned()
        } else {
            "not read".to_owned()
        }
    } else {
        format!("{} of {} in", inside.len(), macos.build_folders.len())
    };
    row.differs = !inside.is_empty();
    row.action = row.differs.then_some(Act::MarkBuildFolders);
    row.words = clauses.join("; ");
    row
}

fn file_events_row(macos: Option<&MacosReading>, snap: Option<&Snapshot>) -> ConfigRow {
    let held = snap.and_then(fseventsd_in);
    let restarted = macos.and_then(|m| {
        m.last_restart.map(|sent| {
            format!(
                "aterm last restarted it {} ago",
                ago(m.read_s.saturating_sub(sent.at_s))
            )
        })
    });
    let Some((footprint, live, total)) = held else {
        return ConfigRow {
            slug: SLUGS[3],
            name: "File events service",
            now: "not read yet".to_owned(),
            wanted: "under the services bar".to_owned(),
            differs: false,
            action: None,
            words: restarted.unwrap_or_else(|| {
                "fseventsd's footprint comes with the reading the page holds".to_owned()
            }),
        };
    };
    let offered = fseventsd::restart_offered(footprint, live, total);
    let mut words = vec![format!(
        "{}: {}",
        FSEVENTSD.what,
        fseventsd::holds(footprint, total)
    )];
    if offered {
        words.push(format!("a restart costs this: {}", FSEVENTSD.cost));
    }
    words.extend(restarted);
    ConfigRow {
        slug: SLUGS[3],
        name: "File events service",
        now: format!("holds {}", mem(footprint)),
        wanted: format!("under the {} bar", mem(large_bar(total))),
        differs: aterm_vitals::services::size(footprint, total)
            != aterm_vitals::services::Size::Usual,
        action: offered.then_some(Act::RestartFseventsd),
        words: words.join("; "),
    }
}

fn power_row(macos: Option<&MacosReading>, alab: bool) -> ConfigRow {
    let starter = baseline::starter_power_ac();
    let wanted = power::wanted(&starter)
        .into_iter()
        .map(|(setting, value)| format!("{} {}", setting.title, power::render(setting.kind, value)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut row = ConfigRow {
        slug: SLUGS[4],
        name: "Power and sleep",
        now: "not read yet".to_owned(),
        wanted: format!("ALab's baseline on AC power: {wanted}"),
        differs: false,
        action: None,
        words: "pmset -g custom".to_owned(),
    };
    let Some(macos) = macos.filter(|m| !m.power.is_empty()) else {
        return row;
    };
    row.now = macos
        .power
        .iter()
        .map(|r| match &r.now {
            Ok(value) => format!(
                "{} {}",
                r.setting.title,
                power::render(r.setting.kind, *value)
            ),
            Err(power::Unread::NotApplicable(_)) => {
                format!("{} not on this Mac", r.setting.title)
            }
            Err(power::Unread::Unusable(_)) => format!("{} not read", r.setting.title),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let differing: Vec<&str> = macos
        .power
        .iter()
        .filter(|r| r.differs())
        .map(|r| r.setting.title)
        .collect();
    let unread: Vec<String> = macos
        .power
        .iter()
        .filter_map(|r| match &r.now {
            Err(power::Unread::NotApplicable(why) | power::Unread::Unusable(why)) => {
                Some(format!("{}: {why}", r.setting.title))
            }
            Ok(_) => None,
        })
        .collect();
    let mut words = Vec::new();
    if alab {
        row.differs = !differing.is_empty();
        row.action = row.differs.then_some(Act::Root(Step::Power));
        if row.differs {
            words.push(format!("{} differ", differing.join(", ")));
        }
    } else {
        words.push(
            "this is not an ALab Mac, so ALab's baseline is shown and not applied".to_owned(),
        );
    }
    words.extend(unread);
    row.words = if words.is_empty() {
        "every setting reads as ALab's baseline".to_owned()
    } else {
        words.join("; ")
    };
    row
}

fn switch(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn updates_words(table: &Updates) -> String {
    let mut out = Vec::new();
    if let Some(on) = table.automatic_check {
        out.push(format!("check for updates {}", switch(on)));
    }
    for key in updates::KEYS
        .iter()
        .filter(|key| key.key != "ConfigDataInstall")
    {
        if let Some(on) = key.field.of(table) {
            out.push(format!("{} {}", key.what, switch(on)));
        }
    }
    out.join(", ")
}

fn updates_row(macos: Option<&MacosReading>, alab: bool) -> ConfigRow {
    let starter = baseline::starter_updates();
    let mut row = ConfigRow {
        slug: SLUGS[5],
        name: "Software Update",
        now: "not read yet".to_owned(),
        wanted: format!("ALab's baseline: {}", updates_words(&starter)),
        differs: false,
        action: None,
        words: "softwareupdate --schedule and com.apple.SoftwareUpdate".to_owned(),
    };
    let Some(macos) = macos.filter(|m| !m.updates.keys.is_empty() || m.updates.check.is_ok())
    else {
        return row;
    };
    let now = &macos.updates;
    let mut parts = vec![match &now.check {
        Ok(on) => format!("check for updates {}", switch(*on)),
        Err(_) => "check for updates not read".to_owned(),
    }];
    for (key, value) in &now.keys {
        parts.push(match value {
            Ok(updates::Held::Switch(on)) => format!("{} {}", key.what, switch(*on)),
            Ok(updates::Held::Unset) => format!("{} not set", key.what),
            Ok(updates::Held::Other(value)) => format!("{} holds {value}", key.what),
            Err(_) => format!("{} not read", key.what),
        });
    }
    row.now = parts.join(", ");
    let differing = updates::differs(now, &starter);
    let mut words = Vec::new();
    if alab {
        row.differs = !differing.is_empty();
        let lowers = updates::lowers_security(now, &starter);
        row.action = (row.differs && !lowers).then_some(Act::Root(Step::Updates));
        if row.differs {
            words.push(format!("{} differ", differing.join(", ")));
        }
        if row.differs && lowers {
            words.push(
                "applying it would turn an automatic install off, which the page never does"
                    .to_owned(),
            );
        }
    } else {
        words.push(
            "this is not an ALab Mac, so ALab's baseline is shown and not applied".to_owned(),
        );
    }
    if let Err(why) = &now.check {
        words.push(why.clone());
    }
    // Keys a failed read of their one domain left unread share its reason: say it once.
    let mut unread: Vec<(&str, Vec<&str>)> = Vec::new();
    for (key, value) in &now.keys {
        if let Err(why) = value {
            match unread.iter_mut().find(|(said, _)| *said == why.as_str()) {
                Some((_, whats)) => whats.push(key.what),
                None => unread.push((why.as_str(), vec![key.what])),
            }
        }
    }
    words.extend(
        unread
            .into_iter()
            .map(|(why, whats)| format!("{}: {why}", whats.join(", "))),
    );
    row.words = if words.is_empty() {
        "every setting reads as ALab's baseline".to_owned()
    } else {
        words.join("; ")
    };
    row
}

/// The section's six rows, judged from in-memory values alone: `macos` (the slot's reading),
/// `scope` (SD-95's probe), `snap` (the page's reading: fseventsd's footprint and the data
/// volume's indexing) and `alab` (whether this is an ALab Mac, the only one ALab's baseline is
/// applied to). An unread row differs in nothing and offers no press.
pub(crate) fn rows(
    macos: Option<&MacosReading>,
    scope: Option<&ScopeReading>,
    snap: Option<&Snapshot>,
    alab: bool,
) -> Vec<ConfigRow> {
    let [spotlight, build] = spotlight_rows(macos, scope, snap);
    vec![
        spotlight,
        build,
        time_machine_row(macos),
        file_events_row(macos, snap),
        power_row(macos, alab),
        updates_row(macos, alab),
    ]
}

#[cfg(test)]
#[path = "resources_macos_tests.rs"]
pub(crate) mod tests;

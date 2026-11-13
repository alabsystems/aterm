// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable-owned readiness observation for automatic converge jobs.
//! Metadata gates private token/inventory reads. A file watch over the files each look reads (the
//! control paths and the declarations that name them) rings the lane's doorbell ([`Watched`]), so
//! the lane looks when one changes. What no watch hears is read once a minute
//! ([`CHECKOUT_LOOK`]): the checkout scope, the admission of the folders above the control paths,
//! and a Git selection that moves no watched file.

use aterm_machines::init_github::{
    RepositoryCheckoutMetadata, RepositoryInputMark, RepositoryInputPathCache,
    RepositoryRefreshCursor, repository_input_mark,
};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::Paths;
use aterm_secrets::converge::RepositoryWork;
use aterm_uds::file_watch::FileWatch;
use aterm_uds::ready::Doorbell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How often a look reads the checkout scope (each selected checkout's Git metadata and every
/// folder above it): the most paths a look reads and most of its cost, and a watch on them would
/// hold a descriptor for each (kqueue). A change to the watched inputs captures the checkouts
/// afresh with the whole mark. What else no watch hears is read at this pace too: the admission
/// (owner and mode) of the folders above the control paths, the vault folder's among them,
/// which a watch on a file does not tell; and a Git selection that moves no watched file (a
/// first managed install while the system's Git is selected, a build's readiness record).
const CHECKOUT_LOOK: Duration = Duration::from_secs(60);

/// How long after a look that failed, or read a declaration within the settle of its change,
/// another is owed.
/// An edit within the clock's tick of a read can leave the stamp a watch compares unmoved; a read
/// [`SETTLE_NS`](aterm_toolchain::stat_digest::SETTLE_NS) after the change, on the whole-second
/// clock the declarations' settling reads, tells it.
fn settled_look() -> Duration {
    Duration::from_nanos(u64::try_from(aterm_toolchain::stat_digest::SETTLE_NS).unwrap_or(u64::MAX))
        + Duration::from_secs(1)
}

type Version = Result<[u8; 32], io::ErrorKind>;
type Admission = Vec<(PathBuf, Result<Policy, io::ErrorKind>)>;

/// Path admission can change while every leaf inode stays fixed. Directory timestamps,
/// sizes and inode replacements with the same policy do not invalidate local readiness.
#[derive(Clone, PartialEq, Eq)]
struct Policy {
    uid: u32,
    gid: u32,
    mode: u32,
    dir: bool,
    symlink: bool,
}

#[derive(PartialEq, Eq)]
struct Metadata {
    files: Vec<(PathBuf, Version)>,
    /// The declarations `files` were derived from.
    declarations: Vec<PathBuf>,
    admission: Admission,
    checkouts: Option<RepositoryCheckoutMetadata>,
}

/// What a read of the declarations says of the next: whether it may take the fast path, and
/// whether one was read within the settle of its change.
struct Declared {
    reusable: bool,
    settling: bool,
}

impl Metadata {
    fn read(
        sys: &dyn System,
        paths: &Paths,
        previous: Option<&RepositoryInputMark>,
        control: &mut RepositoryInputPathCache,
    ) -> Result<(Self, Declared), String> {
        let observed = control.observe(sys)?;
        let prefix: BTreeMap<_, _> = observed.prefix_facts.into_iter().collect();
        let files: Vec<_> = observed
            .paths
            .into_iter()
            .map(|path| {
                let version = sys.file_version(&path).map_err(|why| why.kind());
                (path, version)
            })
            .collect();
        let directories: BTreeSet<&std::path::Path> = files
            .iter()
            .flat_map(|(path, _)| path.ancestors().skip(1))
            // Complete materialization also requires the supplied vault's private state
            // root. It is an actual admission input, independent of credential leaf paths.
            .chain(paths.dir.ancestors())
            .collect();
        let admission = directories
            .into_iter()
            .map(|path| {
                let policy = prefix
                    .get(path)
                    .copied()
                    .unwrap_or_else(|| sys.stat(path).map_err(|why| why.kind()))
                    .map(|stat| Policy {
                        uid: stat.uid,
                        gid: stat.gid,
                        mode: stat.mode,
                        dir: stat.dir,
                        symlink: stat.symlink,
                    });
                (path.to_path_buf(), policy)
            })
            .collect();
        Ok((
            Self {
                files,
                declarations: observed.declarations,
                admission,
                checkouts: previous.map(|mark| mark.checkout_metadata(sys)),
            },
            Declared {
                reusable: observed.reusable,
                settling: observed.settling,
            },
        ))
    }
}

struct Inputs {
    paths: Paths,
    metadata: Option<Metadata>,
    seen: Option<RepositoryInputMark>,
    held: bool,
    control: RepositoryInputPathCache,
    /// When a look last read the checkout scope, by itself or with a whole mark.
    checkouts_read: Option<Instant>,
    /// When the last look failed, or read a declaration within the settle of its change: a later
    /// read owes what a watch may not hear ([`settled_look`]). A declaration missing or refused
    /// is no such read: what replaces it is heard.
    unsettled: Option<Instant>,
}

impl Inputs {
    fn new(sys: &dyn System, paths: &Paths, allowed: bool, now: Instant) -> Self {
        let mut inputs = Self {
            paths: paths.clone(),
            metadata: None,
            seen: None,
            held: !allowed,
            control: RepositoryInputPathCache::default(),
            checkouts_read: None,
            unsettled: None,
        };
        if allowed {
            let _ = inputs.changed(sys, true, now);
        }
        inputs
    }

    /// When these inputs owe a look though no watched input changed: the minute of what no watch
    /// hears ([`CHECKOUT_LOOK`]), and a look [`settled_look`] after one that failed or read a
    /// declaration within the settle of its change. Held inputs owe none, since what lifts the
    /// hold is watched.
    fn next_look(&self, now: Instant) -> Option<Instant> {
        if self.held {
            return None;
        }
        let minute = self.checkouts_read.map(|at| at + CHECKOUT_LOOK);
        let settle = self.unsettled.map(|at| at + settled_look());
        minute.into_iter().chain(settle).min().or(Some(now))
    }

    /// What the last look read that a watch hears: every control path and the declarations
    /// that named them.
    fn watched(&self) -> impl Iterator<Item = Watch> + '_ {
        self.metadata.iter().flat_map(|metadata| {
            metadata
                .files
                .iter()
                .map(|(path, _)| path)
                .chain(&metadata.declarations)
                .map(|path| Watch::File(path.clone()))
        })
    }

    /// The work the inputs at `now` call for. A look within [`CHECKOUT_LOOK`] of the last read
    /// of the checkout scope compares the rest of the inputs alone; when they changed, are
    /// unsettled or were held, it captures the whole mark, the checkout scope with it.
    fn changed(
        &mut self,
        sys: &dyn System,
        allowed: bool,
        now: Instant,
    ) -> Result<Option<RepositoryWork>, ()> {
        if !allowed {
            self.held = true;
            return Ok(None);
        }
        let checkouts = self
            .checkouts_read
            .is_none_or(|at| now.saturating_duration_since(at) >= CHECKOUT_LOOK);
        let Ok((mut metadata, Declared { reusable, settling })) = Metadata::read(
            sys,
            &self.paths,
            self.seen.as_ref().filter(|_| checkouts),
            &mut self.control,
        ) else {
            self.unsettled = Some(now);
            return Err(());
        };
        self.unsettled = settling.then_some(now);
        // A scope this look read is compared; one it left unread is not.
        let unchanged = self.metadata.as_ref().is_some_and(|before| {
            before.files == metadata.files
                && before.admission == metadata.admission
                && metadata
                    .checkouts
                    .as_ref()
                    .is_none_or(|read| before.checkouts.as_ref() == Some(read))
        });
        if reusable && !self.held && unchanged {
            if checkouts {
                self.checkouts_read = Some(now);
            }
            return Ok(None);
        }
        let Ok(mark) = repository_input_mark(sys) else {
            self.unsettled = Some(now);
            return Err(());
        };
        let work = if self.held
            || self
                .seen
                .as_ref()
                .is_none_or(|before| !before.same_source(&mark))
            || self
                .metadata
                .as_ref()
                .is_none_or(|prev| prev.admission != metadata.admission)
        {
            Some(RepositoryWork::Refresh)
        } else if self.seen.as_ref() != Some(&mark) {
            Some(RepositoryWork::Repair)
        } else {
            None
        };
        // Retain the state captured with the semantic scope. Resampling after capture could
        // cache a later repair next to an earlier mark and swallow that repair on the next tick.
        metadata.checkouts = Some(mark.captured_checkout_metadata());
        self.checkouts_read = Some(now);
        self.metadata = Some(metadata);
        self.seen = Some(mark);
        self.held = false;
        Ok(work)
    }

    fn request(
        &mut self,
        sys: &dyn System,
        allowed: bool,
        observed: aterm_secrets::converge::RefreshObservation,
        cursor: &mut RepositoryRefreshCursor,
        now: Instant,
    ) -> Option<aterm_secrets::converge::RepositoryRequest> {
        let work = self.changed(sys, allowed, now).ok()?;
        if allowed {
            cursor.observe(self.seen.as_ref(), observed, work)
        } else {
            None
        }
    }
}

#[derive(PartialEq, Eq)]
struct NamingIntent {
    number: Option<aterm_types::machine_name::Number>,
    nickname: aterm_types::machine_name::Nickname,
    class: aterm_types::machine_name::Class,
}

/// This subject has its own admission and semantic cursor: a repository hold or successful
/// repository receipt cannot swallow identity debt. The runner still re-reads every effect.
struct NamingInputs {
    paths: Paths,
    state: Option<PathBuf>,
    admitted: bool,
    metadata: Option<Version>,
    seen: Option<Option<NamingIntent>>,
}
impl NamingInputs {
    fn new(paths: &Paths, state: Option<PathBuf>) -> Self {
        Self {
            paths: paths.clone(),
            state,
            admitted: false,
            metadata: None,
            seen: None,
        }
    }

    /// The machine name this subject reads, which the observer's watch hears too.
    fn path(&self) -> Option<PathBuf> {
        self.state
            .as_ref()
            .map(|state| state.join(aterm_agent::machine_name::file::FILE))
    }

    fn request(
        &mut self,
        sys: &dyn System,
        allowed: impl FnOnce() -> bool,
    ) -> aterm_secrets::client::IdentityDebt {
        use aterm_secrets::client::IdentityDebt;
        if !self.admitted {
            self.admitted = sys.identity().is_ok_and(|identity| {
                identity.user.uid == sys.euid()
                    && sys.euid() != 0
                    && std::path::Path::new(&identity.user.home) == self.paths.home
            });
        }
        if !self.admitted || !allowed() {
            return IdentityDebt::None;
        }
        let Some(path) = self.path() else {
            return IdentityDebt::None;
        };
        let version = sys.file_version(&path).map_err(|why| why.kind());
        if self.metadata.as_ref() == Some(&version) {
            return IdentityDebt::None;
        }
        let name = match &version {
            Err(io::ErrorKind::NotFound) => None,
            Err(_) => return IdentityDebt::None,
            Ok(_) => {
                let Ok(bytes) = sys.read_owned_file(
                    &path,
                    sys.euid(),
                    aterm_agent::machine_name::file::FILE_MAX,
                    true,
                ) else {
                    return IdentityDebt::None;
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    return IdentityDebt::None;
                };
                let Ok(name) = aterm_agent::machine_name::file::MachineName::from_toml(text) else {
                    return IdentityDebt::None;
                };
                Some(NamingIntent {
                    number: name.num,
                    nickname: name.nick,
                    class: name.class,
                })
            }
        };
        let changed = self.seen.as_ref().is_some_and(|old| old != &name)
            || self.seen.is_none() && name.as_ref().is_some_and(|name| name.number.is_some());
        self.metadata = Some(version);
        self.seen = Some(name);
        if changed {
            IdentityDebt::Changed
        } else {
            IdentityDebt::None
        }
    }
}

struct Readiness {
    paths: Paths,
    ready: Option<(Inputs, RepositoryRefreshCursor)>,
}

impl Readiness {
    fn new(
        sys: &dyn System,
        paths: &Paths,
        allowed: impl Fn() -> bool,
        initial: impl FnOnce() -> aterm_secrets::converge::RefreshObservation,
        now: Instant,
    ) -> Self {
        let ready = sys
            .identity()
            .is_ok_and(|identity| {
                identity.user.uid == sys.euid()
                    && sys.euid() != 0
                    && std::path::Path::new(&identity.user.home) == paths.home
            })
            .then(|| {
                // Establish the generation BEFORE the first source capture. Another owner
                // may fulfill the next source before this actor gets its first callback.
                let initial = initial();
                let inputs = Inputs::new(sys, paths, allowed(), now);
                let cursor = RepositoryRefreshCursor::new(initial, inputs.seen.as_ref());
                (inputs, cursor)
            });
        Self {
            paths: paths.clone(),
            ready,
        }
    }

    fn request(
        &mut self,
        sys: &dyn System,
        allowed: impl Fn() -> bool,
        observed: aterm_secrets::converge::RefreshObservation,
        now: Instant,
    ) -> Option<aterm_secrets::converge::RepositoryRequest> {
        if self.ready.is_none() {
            // A temporary account lookup failure cannot retire this runtime's local-input
            // observer. The snapshot's receipt precedes this first actual source capture.
            self.ready = Self::new(sys, &self.paths, &allowed, || observed, now).ready;
        }
        let (inputs, cursor) = self.ready.as_mut()?;
        inputs.request(sys, allowed(), observed, cursor, now)
    }

    /// When a look is owed with nothing rung: the inputs' own ([`Inputs::next_look`]), or, while
    /// the account lookup has not admitted them, its retry, as a look that failed retries.
    fn next_look(&self, now: Instant) -> Option<Instant> {
        match &self.ready {
            Some((inputs, _)) => inputs.next_look(now),
            None => Some(now + settled_look()),
        }
    }

    /// What the last look read that a watch hears: nothing before the inputs are admitted.
    fn watched(&self) -> impl Iterator<Item = Watch> + '_ {
        self.ready.iter().flat_map(|(inputs, _)| inputs.watched())
    }
}

/// An input a look reads that a file watch can hear: a file, or a folder of records whose entries
/// are made, removed or replaced.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Watch {
    File(PathBuf),
    Folder(PathBuf),
}

/// The watch over what the last look read, ringing the lane's doorbell at each change it hears.
struct Watched {
    watch: FileWatch,
    keys: BTreeMap<Watch, u64>,
    next: u64,
}

impl Watched {
    /// A watch over this platform's file events ringing `rung`; `None` where it cannot start, and
    /// the inputs then owe a look at every turn of the lane.
    fn start(rung: Arc<Doorbell>) -> Option<Self> {
        FileWatch::platform("converge-inputs", Box::new(move |_| rung.ring()))
            .ok()
            .map(Self::over)
    }

    fn over(watch: FileWatch) -> Self {
        Self {
            watch,
            keys: BTreeMap::new(),
            next: 0,
        }
    }

    /// Watch exactly `want`: an input no look reads now is let go, and one newly read is
    /// watched, its first look ringing once, which folds in a change made before it armed.
    fn arm(&mut self, want: BTreeSet<Watch>) {
        let Self { watch, keys, next } = self;
        keys.retain(|input, key| {
            let kept = want.contains(input);
            if !kept {
                watch.unwatch(*key);
            }
            kept
        });
        for input in want {
            if keys.contains_key(&input) {
                continue;
            }
            let key = *next;
            *next += 1;
            match &input {
                Watch::File(path) => watch.watch(key, path.clone()),
                Watch::Folder(path) => watch.watch_directory(key, path.clone()),
            }
            keys.insert(input, key);
        }
    }
}

/// The observer's state, shared by its look and its answer of when the next is owed.
struct Observed {
    readiness: Readiness,
    watched: Option<Watched>,
    /// What every look reads apart from its control paths: the setup records, the shared
    /// refresh receipt and the machine's name. The sharing switch is the lane's own to watch, since its hold keeps the
    /// lane from looking here at all.
    fixed: BTreeSet<Watch>,
}

impl Observed {
    fn arm(&mut self) {
        if let Some(watched) = &mut self.watched {
            watched.arm(
                self.fixed
                    .iter()
                    .cloned()
                    .chain(self.readiness.watched())
                    .collect(),
            );
        }
    }

    /// When a look is owed though no watch rang: at once where nothing watches.
    fn next_look(&self, now: Instant) -> Option<Instant> {
        if self.watched.is_none() {
            return Some(now);
        }
        self.readiness.next_look(now)
    }
}

/// This observer follows only the runtime's own real account. The ordinary job still reads
/// current source, materialization, Stop/off and path admission before every effect. It rings
/// `rung` when an input it reads changes; without one it watches nothing and owes a look at
/// every turn of its lane.
pub(super) fn observe(
    paths: &aterm_secrets::client::Paths,
    _role: aterm_secrets::client::ConvergenceRole,
    rung: Option<Arc<Doorbell>>,
) -> aterm_secrets::client::ConvergenceInputs {
    let home = paths.home.clone();
    let sharing: AdmissionCheck =
        Arc::new(move || aterm_secrets::swarm::enabled_in_home(&home).unwrap_or(false));
    let shares = Arc::clone(&sharing);
    let repositories: AdmissionCheck = Arc::new(move || {
        shares()
            && !super::converge::setup_holds()
                .holds(aterm_types::macos_setup::Setting::Repositories)
    });
    observing(
        Arc::new(Live::new()),
        paths,
        aterm_types::dirs::account_state_dir(),
        repositories,
        sharing,
        rung,
    )
}

type AdmissionCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// The real factory's subjects and ordering over an injected account port. Both live callers
/// and existing-owner fixtures exercise this assembly, rather than only its leaf observer.
fn observing(
    sys: Arc<dyn System + Send>,
    paths: &Paths,
    state: Option<PathBuf>,
    repositories: AdmissionCheck,
    sharing: AdmissionCheck,
    rung: Option<Arc<Doorbell>>,
) -> aterm_secrets::client::ConvergenceInputs {
    let mut receipt = aterm_machines::sync::repository_refresh::Observer::default();
    let readiness = Readiness::new(
        sys.as_ref(),
        paths,
        || repositories(),
        || {
            state
                .as_ref()
                .map(|state| {
                    receipt.read(sys.as_ref(), state, aterm_time::unix_millis().unwrap_or(0))
                })
                .unwrap_or_default()
        },
        Instant::now(),
    );
    let mut naming = NamingInputs::new(paths, state.clone());
    let fixed =
        super::converge::setup_records()
            .map(Watch::Folder)
            .into_iter()
            .chain(state.as_deref().map(|state| {
                Watch::File(aterm_machines::sync::repository_refresh::record_path(state))
            }))
            .chain(naming.path().map(Watch::File))
            .collect();
    let mut observed = Observed {
        readiness,
        watched: rung.and_then(Watched::start),
        fixed,
    };
    observed.arm();
    let observed = Arc::new(Mutex::new(observed));
    let looked = Arc::clone(&observed);
    let repository_sys = Arc::clone(&sys);
    let naming_sys = Arc::clone(&sys);
    aterm_secrets::client::ConvergenceInputs::new(Box::new(move |refresh| {
        let mut observed = looked.lock().unwrap_or_else(PoisonError::into_inner);
        let request = observed.readiness.request(
            repository_sys.as_ref(),
            || repositories(),
            refresh,
            Instant::now(),
        );
        observed.arm();
        request
    }))
    .with_identity(Box::new(move || {
        naming.request(naming_sys.as_ref(), || sharing())
    }))
    .with_refresh(Box::new(move || {
        state
            .as_ref()
            .map(|state| receipt.read(sys.as_ref(), state, aterm_time::unix_millis().unwrap_or(0)))
            .unwrap_or_default()
    }))
    .with_next_look(Box::new(move || {
        observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_look(Instant::now())
    }))
}

#[cfg(test)]
#[path = "converge_inputs_tests.rs"]
mod tests;

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHO WATCHES THE HEADS (gap #28, 2026-09-26). The head watch used to run only in the
//! window's package thread, so a Mac with no aterm window found a new Claude Code or Codex
//! release at the index cadence of hours. It now runs in whichever aterm host is up — the
//! window, or, with no window open, ONE terminal session — and never in both kinds at once.
//! Two store-scoped lock files are the rendezvous; neither carries any authority (the
//! per-vendor `.watch` files still decide whose GET a minute is, and the pass still
//! re-verifies everything):
//!
//! * `vendor-head.hosts` — every window watching holds it SHARED for its watch's life;
//!   the session watcher holds it EXCLUSIVE for one round of checks at a time. A window
//!   and a session therefore never check at once, and a window that arrives waits at most
//!   the round in flight (one short GET) before the session never gets the lock again.
//! * `vendor-head.session` — the SEAT: the one session that may watch holds it
//!   exclusive for its life and writes its pid into it. Every other session stands by
//!   and tries the seat once a minute, so the second tab of a window-less Mac costs one
//!   `flock` a minute, not a watcher.
//!
//! The kernel releases both with the process, so a host that exits — cleanly or not —
//! hands the watch on within a minute. Fail-safe both ways: a window that cannot open the
//! rendezvous watches as it always did, and a session that cannot stands by and says so
//! once (a window, or the next update pass, still finds the release).

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{BUSY_RETRY, CADENCE};
use crate::lock::Flock;
use crate::store::Layout;

/// The rendezvous every watching host locks: shared by windows, exclusive per round by
/// the session watcher.
const HOSTS_FILE: &str = "vendor-head.hosts";
/// The one session watcher's seat, holding its pid.
const SEAT_FILE: &str = "vendor-head.session";
/// The seat's pid record is a few bytes; anything longer is not ours.
const MAX_SEAT_BYTES: u64 = 64;

/// Which kind of aterm host runs a head watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// The window's package thread: it watches whenever it is up, sharing the store's
    /// vendor files with every other window, as it always has.
    Window,
    /// A terminal session: it watches only from the seat, and only while no window does.
    Session,
}

/// Whether a host may start a round of head checks now.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admit {
    /// Yes; a session keeps the rendezvous until [`HostClaim::end_round`].
    Yes,
    /// Not now; ask again after this long.
    Later(Duration),
}

/// Where a session watcher stands, for the one log line each change is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// It holds the seat and checks when no window does.
    Watching,
    /// Another session holds the seat.
    SeatTaken,
    /// A window watches (or another session's round is in flight: the next try tells).
    WindowWatches,
    /// The rendezvous could not be opened or locked: nothing proves no window watches.
    Unavailable,
}

/// One host's claim on the store's rendezvous ([`Host`]).
pub(super) struct HostClaim {
    host: Host,
    prefix: PathBuf,
    /// A window's shared hold, kept once taken; a session's exclusive hold for one round.
    hosts: Option<Flock>,
    /// A session's seat, kept once taken.
    seat: Option<Flock>,
    standing: Option<Standing>,
    #[cfg(test)]
    admit_calls: u64,
}

impl std::fmt::Debug for HostClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClaim")
            .field("host", &self.host)
            .field("prefix", &self.prefix)
            .field("hosts", &self.hosts.is_some())
            .field("seat", &self.seat.is_some())
            .field("standing", &self.standing)
            .finish()
    }
}

impl HostClaim {
    pub(super) fn new(host: Host, layout: &Layout) -> Self {
        Self {
            host,
            prefix: layout.prefix.clone(),
            hosts: None,
            seat: None,
            standing: None,
            #[cfg(test)]
            admit_calls: 0,
        }
    }

    /// Whether this host may start a round now, and the one line a changed standing is
    /// worth (a session's only: a window's claim is taken once and kept).
    pub(super) fn admit(&mut self) -> (Admit, Option<String>) {
        #[cfg(test)]
        {
            self.admit_calls += 1;
        }
        match self.host {
            Host::Window => (self.admit_window(), None),
            Host::Session => {
                let (admit, standing) = self.admit_session();
                let note = (self.standing != Some(standing)).then(|| standing_note(standing));
                self.standing = Some(standing);
                (admit, note)
            }
        }
    }

    /// A window takes the rendezvous SHARED once and keeps it. Refused only while a
    /// session's round holds it, which ends within one GET: ask again in a slice. A
    /// rendezvous it cannot open or lock is no reason to stop watching — the window
    /// watched before there was one.
    fn admit_window(&mut self) -> Admit {
        if self.hosts.is_some() {
            return Admit::Yes;
        }
        let Some(file) = open_rendezvous(&self.prefix.join(HOSTS_FILE), true) else {
            return Admit::Yes;
        };
        match Flock::try_lock_shared(file) {
            Ok(held) => {
                self.hosts = Some(held);
                Admit::Yes
            }
            Err(std::fs::TryLockError::WouldBlock) => Admit::Later(BUSY_RETRY),
            Err(std::fs::TryLockError::Error(_)) => Admit::Yes,
        }
    }

    /// A session watches only from the seat and only while no window holds the
    /// rendezvous; it then holds the rendezvous EXCLUSIVE until its round ends. Unsure —
    /// either file unopenable or unlockable — it stands by.
    fn admit_session(&mut self) -> (Admit, Standing) {
        if self.hosts.is_some() {
            return (Admit::Yes, Standing::Watching);
        }
        if self.seat.is_none() {
            let Some(file) = open_rendezvous(&self.prefix.join(SEAT_FILE), true) else {
                return (Admit::Later(CADENCE), Standing::Unavailable);
            };
            match Flock::try_lock(file) {
                Ok(mut held) => {
                    // The pid is only what `doctor` names; a failed write names none.
                    let _ = write_pid(&mut held);
                    self.seat = Some(held);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return (Admit::Later(CADENCE), Standing::SeatTaken);
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return (Admit::Later(CADENCE), Standing::Unavailable);
                }
            }
        }
        let Some(file) = open_rendezvous(&self.prefix.join(HOSTS_FILE), true) else {
            return (Admit::Later(CADENCE), Standing::Unavailable);
        };
        match Flock::try_lock(file) {
            Ok(held) => {
                self.hosts = Some(held);
                (Admit::Yes, Standing::Watching)
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                (Admit::Later(CADENCE), Standing::WindowWatches)
            }
            Err(std::fs::TryLockError::Error(_)) => (Admit::Later(CADENCE), Standing::Unavailable),
        }
    }

    /// Whether this SESSION holds the rendezvous EXCLUSIVE right now. The seat and
    /// the last admission alone are not enough: a window can take its shared hold
    /// immediately after [`Self::end_round`]. A held toolchain move must re-admit
    /// before looking, then keep this hold through its pass launch.
    pub(super) fn seated_session(&self) -> bool {
        self.host == Host::Session && self.seat.is_some() && self.hosts.is_some()
    }

    /// A seated session refused only because a window owns the rendezvous
    /// retries on the next park slice; another tab without the seat waits the
    /// watch's minute cadence.
    pub(super) fn window_watches(&self) -> bool {
        self.host == Host::Session && self.standing == Some(Standing::WindowWatches)
    }

    #[cfg(test)]
    pub(super) fn admit_calls(&self) -> u64 {
        self.admit_calls
    }

    /// A round is over: a session lets the rendezvous go, so a window can take it; a
    /// window keeps its shared hold.
    pub(super) fn end_round(&mut self) {
        if self.host == Host::Session {
            self.hosts = None;
        }
    }
}

/// The line a session watcher's new standing is worth in its host's log.
fn standing_note(standing: Standing) -> String {
    match standing {
        Standing::Watching => String::from(
            "vendor head watch: this terminal session watches the vendor heads (no aterm \
             window does)",
        ),
        Standing::SeatTaken => String::from(
            "vendor head watch: another terminal session watches the vendor heads \u{2014} \
             this one stands by",
        ),
        Standing::WindowWatches => String::from(
            "vendor head watch: an aterm window watches the vendor heads \u{2014} this \
             terminal session stands by",
        ),
        Standing::Unavailable => String::from(
            "vendor head watch: this terminal session could not open the store's watch \
             rendezvous, so it cannot tell whether a window watches and stands by \u{2014} \
             an aterm window or the next update pass finds a new release",
        ),
    }
}

/// Open a rendezvous file without following a link, read-write and created when `create`
/// (a host), read-only and never created otherwise (a report). `None`: not openable, or
/// not a plain file.
fn open_rendezvous(path: &Path, create: bool) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.write(true).create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    (metadata.file_type().is_file() && !crate::platform::is_reparse(&metadata)).then_some(file)
}

fn write_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

/// The seat holder's pid, as it wrote it.
fn read_pid(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.take(MAX_SEAT_BYTES).read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

/// Which host watches this store's vendor heads right now, as `aterm pkg doctor` says it
/// ([`watcher`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Watcher {
    /// An aterm window holds the watch.
    Window,
    /// A terminal session holds the watch — its pid, as it recorded it.
    Session(Option<u32>),
    /// No host watches: no window, and no terminal session in the seat.
    Nobody,
    /// The rendezvous could not be read; why.
    Unknown(String),
}

/// Who watches `layout`'s vendor heads, read off the rendezvous without taking the watch:
/// each file is opened read-only, never created, and a lock is held only for the instant a
/// probe needs it. A window's shared hold admits a shared probe but refuses an exclusive
/// one; a session's round refuses both. A host trying its claim in that instant is told
/// to try again, as it would be by any other host.
#[must_use]
pub fn watcher(layout: &Layout) -> Watcher {
    let hosts = match probe(&layout.prefix.join(HOSTS_FILE)) {
        Ok(state) => state,
        Err(why) => return Watcher::Unknown(why),
    };
    if hosts == Probe::Shared {
        return Watcher::Window;
    }
    let seat_path = layout.prefix.join(SEAT_FILE);
    match probe(&seat_path) {
        Ok(Probe::Exclusive) => Watcher::Session(
            open_rendezvous(&seat_path, false).and_then(|mut file| read_pid(&mut file)),
        ),
        // A round holds the rendezvous only from the seat; the seat being free between
        // the two probes is a session that just exited.
        Ok(_) if hosts == Probe::Exclusive => Watcher::Session(None),
        Ok(_) => Watcher::Nobody,
        Err(why) => Watcher::Unknown(why),
    }
}

/// How a rendezvous file is held.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// Absent, or held by nobody.
    Free,
    /// Held shared (a window's watch).
    Shared,
    /// Held exclusive (a session's seat or round).
    Exclusive,
}

fn probe(path: &Path) -> Result<Probe, String> {
    let open = || open_rendezvous(path, false);
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(Probe::Free);
    }
    let Some(file) = open() else {
        return Err(format!("{} could not be opened", path.display()));
    };
    match Flock::try_lock(file) {
        Ok(_free) => return Ok(Probe::Free),
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(format!("{} could not be locked ({e})", path.display()));
        }
    }
    let Some(file) = open() else {
        return Err(format!("{} could not be opened", path.display()));
    };
    match Flock::try_lock_shared(file) {
        Ok(_shared) => Ok(Probe::Shared),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Probe::Exclusive),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(format!("{} could not be locked ({e})", path.display()))
        }
    }
}

/// `host`'s claim on `layout`'s rendezvous as its first round leaves it — a window's
/// shared hold, a session's seat between rounds — held until the value drops: for a test
/// elsewhere in the crate that needs a watching host in place.
#[cfg(test)]
pub(crate) fn claimed(layout: &Layout, host: Host) -> Box<dyn std::any::Any + Send> {
    let mut claim = HostClaim::new(host, layout);
    assert_eq!(claim.admit().0, Admit::Yes, "{host:?} admitted");
    claim.end_round();
    Box::new(claim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-hosts-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    /// THE RENDEZVOUS, CLAIM BY CLAIM (gap #28): a window's shared hold keeps every session
    /// out; a session's round keeps a window out until the round ends, and a window that
    /// then takes its hold keeps the session out for good; a second session never gets
    /// past the seat while the first lives; and every release hands on at once.
    #[test]
    fn a_window_and_a_session_never_both_hold_the_watch() {
        let l = layout("claims");
        assert_eq!(watcher(&l), Watcher::Nobody, "nothing has ever watched");

        // Session A takes the seat and a round; session B stands by at the seat.
        let mut a = HostClaim::new(Host::Session, &l);
        let (admit, note) = a.admit();
        assert_eq!(admit, Admit::Yes);
        assert!(note.unwrap().contains("this terminal session watches"));
        assert!(a.seated_session(), "the session holds the exclusive round");
        assert_eq!(
            watcher(&l),
            Watcher::Session(Some(std::process::id())),
            "mid-round, the report names the seat's pid"
        );
        let mut b = HostClaim::new(Host::Session, &l);
        let (admit, note) = b.admit();
        assert_eq!(admit, Admit::Later(CADENCE));
        assert!(note.unwrap().contains("another terminal session watches"));
        assert_eq!(b.admit(), (Admit::Later(CADENCE), None), "said once");

        // A window arriving mid-round is refused for one slice, not for good.
        let mut window = HostClaim::new(Host::Window, &l);
        assert_eq!(window.admit(), (Admit::Later(BUSY_RETRY), None));
        a.end_round();
        assert!(
            !a.seated_session(),
            "the seat alone does not reserve the space a window may take"
        );
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        assert_eq!(window.admit(), (Admit::Yes, None));
        assert!(!a.seated_session(), "the window now holds that space");
        assert_eq!(watcher(&l), Watcher::Window);

        // From now on the seat holder never gets a round, and says so once.
        let (admit, note) = a.admit();
        assert_eq!(admit, Admit::Later(CADENCE));
        assert!(note.unwrap().contains("an aterm window watches"));
        assert_eq!(a.admit(), (Admit::Later(CADENCE), None));
        // A second window shares the hold, as windows always shared the watch.
        let mut second = HostClaim::new(Host::Window, &l);
        assert_eq!(second.admit(), (Admit::Yes, None));
        drop(window);
        assert_eq!(
            a.admit().0,
            Admit::Later(CADENCE),
            "one window still watches"
        );
        drop(second);

        // The windows gone, the seat holder watches again; its exit hands the seat to B.
        let (admit, note) = a.admit();
        assert_eq!(admit, Admit::Yes);
        assert!(note.unwrap().contains("this terminal session watches"));
        a.end_round();
        drop(a);
        assert_eq!(watcher(&l), Watcher::Nobody);
        let (admit, note) = b.admit();
        assert_eq!(admit, Admit::Yes);
        assert!(note.unwrap().contains("this terminal session watches"));
        b.end_round();
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The real hosts of one store, as the derived model `AtpkgHeadWatchHosts` names them.
    struct Hosts {
        layout: Layout,
        window: Option<HostClaim>,
        sessions: [Option<HostClaim>; 2],
    }

    impl Hosts {
        fn new(layout: &Layout) -> Self {
            Self {
                layout: layout.clone(),
                window: None,
                sessions: [None, None],
            }
        }

        /// Do what `action` names, through the real claims.
        fn act(&mut self, action: &str) {
            let session = |name: &str| usize::from(name.starts_with("S2"));
            match action {
                "WindowOpens" => self.window = Some(HostClaim::new(Host::Window, &self.layout)),
                "WindowAdmits" => {
                    let _ = self.window.as_mut().expect("a window").admit();
                }
                "WindowCloses" => self.window = None,
                "S1Starts" | "S2Starts" => {
                    self.sessions[session(action)] =
                        Some(HostClaim::new(Host::Session, &self.layout));
                }
                "S1Admits" | "S2Admits" => {
                    let _ = self.sessions[session(action)]
                        .as_mut()
                        .expect("a session")
                        .admit();
                }
                "S1EndsRound" | "S2EndsRound" => self.sessions[session(action)]
                    .as_mut()
                    .expect("a session")
                    .end_round(),
                "S1Exits" | "S2Exits" => self.sessions[session(action)] = None,
                other => panic!("no real step for {other}"),
            }
        }

        /// The model's variables, read off the real claims.
        fn project(&self) -> std::collections::BTreeMap<&'static str, i64> {
            let window = self
                .window
                .as_ref()
                .map_or(0, |w| if w.hosts.is_some() { 2 } else { 1 });
            let session = |claim: &Option<HostClaim>| {
                claim.as_ref().map_or(0, |s| {
                    if s.hosts.is_some() {
                        3
                    } else if s.seat.is_some() {
                        2
                    } else {
                        1
                    }
                })
            };
            [
                ("w", window),
                ("s1", session(&self.sessions[0])),
                ("s2", session(&self.sessions[1])),
                ("orphan", 0),
            ]
            .into_iter()
            .collect()
        }
    }

    /// TIER-1: the REAL claims against the derived model `AtpkgHeadWatchHosts`, over every
    /// reachable step. Each reachable state is reached again by its shortest path through
    /// real `HostClaim`s on a fresh store, each enabled action is taken through them too,
    /// and the real hosts' holds must read as the model's successor — the window's shared
    /// lock refused exactly while a session's round holds the exclusive one, the seat and
    /// the round a session gets exactly when the model grants them. Negative control: the
    /// probe design's counterexample (`Buggy=1`) is walked through the real hosts, and the
    /// real session refuses the round the probe let it take.
    #[test]
    fn the_real_claims_refine_the_derived_model() {
        use aterm_spec::derive::atpkg_head_watch_hosts_model;
        let model = atpkg_head_watch_hosts_model();
        let mut paths: std::collections::BTreeMap<_, Vec<&'static str>> =
            std::collections::BTreeMap::new();
        let mut frontier = std::collections::VecDeque::from([(model.init_state(), Vec::new())]);
        while let Some((state, path)) = frontier.pop_front() {
            if paths.contains_key(&state) {
                continue;
            }
            for action in &model.actions {
                let mut next = state.clone();
                if model.fire(action.name, &mut next) && !paths.contains_key(&next) {
                    let mut longer = path.clone();
                    longer.push(action.name);
                    frontier.push_back((next, longer));
                }
            }
            paths.insert(state, path);
        }
        assert!(
            paths.len() >= 30,
            "the explorer reached the space: {}",
            paths.len()
        );
        let mut steps = 0;
        for (state, path) in &paths {
            for action in &model.actions {
                let mut next = state.clone();
                if !model.fire(action.name, &mut next) {
                    continue;
                }
                let l = layout(&format!("refines-{steps}"));
                let mut hosts = Hosts::new(&l);
                for step in path {
                    hosts.act(step);
                }
                assert_eq!(&hosts.project(), state, "reached by {path:?}");
                hosts.act(action.name);
                assert_eq!(
                    hosts.project(),
                    next,
                    "{} from {state:?} (reached by {path:?})",
                    action.name
                );
                steps += 1;
                drop(hosts);
                let _ = std::fs::remove_dir_all(&l.prefix);
            }
        }
        assert!(steps >= 100, "every reachable step was taken: {steps}");

        // The probe design: the session found the rendezvous free and let it go; a window
        // took it since; the buggy session rounds on its stale answer beside the window.
        // The real session asks for the round instead, and is refused.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut probed = buggy.init_state();
        for action in [
            "S1Starts",
            "S1Admits",
            "S1EndsRound",
            "WindowOpens",
            "WindowAdmits",
            "S1RoundsOnStaleProbe",
        ] {
            assert!(buggy.fire(action, &mut probed), "{action}");
        }
        assert!(!buggy.check_invariant("NeverBothKinds", &probed));
        let l = layout("refines-probe");
        let mut hosts = Hosts::new(&l);
        for step in [
            "S1Starts",
            "S1Admits",
            "S1EndsRound",
            "WindowOpens",
            "WindowAdmits",
            "S1Admits",
        ] {
            hosts.act(step);
        }
        let real = hosts.project();
        assert_eq!((real["w"], real["s1"]), (2, 2), "the real session stood by");
        assert!(
            !hosts.sessions[0]
                .as_ref()
                .expect("the session still holds the seat")
                .seated_session(),
            "a held toolchain look cannot borrow the old round's admission"
        );
        assert_ne!(real, probed, "the real hosts never reach the probe's state");
        assert!(model.check_invariant("NeverBothKinds", &real));
        drop(hosts);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// FAIL-SAFE BOTH WAYS: a rendezvous that is not a plain file (here a directory in its
    /// place) leaves a window watching as it always did, and a session standing by — it
    /// cannot prove no window watches — saying so once; the report says it could not tell.
    #[test]
    fn an_unopenable_rendezvous_keeps_the_window_and_stands_the_session_by() {
        let l = layout("unopenable");
        std::fs::create_dir_all(l.prefix.join(HOSTS_FILE)).unwrap();
        let mut window = HostClaim::new(Host::Window, &l);
        assert_eq!(window.admit(), (Admit::Yes, None));
        let mut session = HostClaim::new(Host::Session, &l);
        let (admit, note) = session.admit();
        assert_eq!(admit, Admit::Later(CADENCE));
        assert!(note.unwrap().contains("could not open"), "said");
        assert_eq!(session.admit(), (Admit::Later(CADENCE), None), "said once");
        assert!(matches!(watcher(&l), Watcher::Unknown(_)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

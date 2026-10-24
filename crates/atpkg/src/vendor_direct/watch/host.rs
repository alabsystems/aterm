// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lock-file rendezvous selecting which aterm host runs the vendor head watch.

use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{BUSY_RETRY, CADENCE};
use crate::index_probe::open_private;
use crate::lock::{Flock, RecordLock};
use crate::store::Layout;

const HOSTS_FILE: &str = "vendor-head.hosts";
const SEAT_FILE: &str = "vendor-head.session";
const MAX_SEAT_BYTES: u64 = 64;

/// Which kind of aterm host runs a head watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// The window's package thread: it watches whenever it is up, sharing the store's vendor files
    /// with every other window.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Watching,
    SeatTaken,
    WindowWatches,
    Unavailable,
}

/// One host's claim on the store's rendezvous ([`Host`]).
pub(super) struct HostClaim {
    host: Host,
    prefix: PathBuf,
    hosts: Option<Flock>,
    seat: Option<RecordLock>,
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

    /// Returns whether this host may start a round now and any standing-change note.
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

    fn admit_window(&mut self) -> Admit {
        if self.hosts.is_some() {
            return Admit::Yes;
        }
        let Some(file) = open_private(&self.prefix.join(HOSTS_FILE), true) else {
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

    fn admit_session(&mut self) -> (Admit, Standing) {
        if self.hosts.is_some() {
            return (Admit::Yes, Standing::Watching);
        }
        if self.seat.is_none() {
            let Some(file) = open_private(&self.prefix.join(SEAT_FILE), true) else {
                return (Admit::Later(CADENCE), Standing::Unavailable);
            };
            match RecordLock::try_lock(file, MAX_SEAT_BYTES) {
                Ok(mut held) => {
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
        let Some(file) = open_private(&self.prefix.join(HOSTS_FILE), true) else {
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

    /// Whether this session holds the rendezvous exclusive right now.
    pub(super) fn seated_session(&self) -> bool {
        self.host == Host::Session && self.seat.is_some() && self.hosts.is_some()
    }

    /// Whether this session is seated and waiting only because a window holds the rendezvous.
    pub(super) fn window_watches(&self) -> bool {
        self.host == Host::Session && self.standing == Some(Standing::WindowWatches)
    }

    #[cfg(test)]
    pub(super) fn admit_calls(&self) -> u64 {
        self.admit_calls
    }

    /// A round is over: a session lets the rendezvous go, so a window can take it.
    pub(super) fn end_round(&mut self) {
        if self.host == Host::Session {
            self.hosts = None;
        }
    }
}

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
             rendezvous \u{2014} standing by; the next update pass still finds new releases",
        ),
    }
}

fn write_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

fn read_pid(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.take(MAX_SEAT_BYTES).read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

/// Which host watches this store's vendor heads right now ([`watcher`]).
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

/// Who watches `layout`'s vendor heads, read off the rendezvous without taking the watch.
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
            open_private(&seat_path, false).and_then(|mut file| read_pid(&mut file)),
        ),
        Ok(_) if hosts == Probe::Exclusive => Watcher::Session(None),
        Ok(_) => Watcher::Nobody,
        Err(why) => Watcher::Unknown(why),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Free,
    Shared,
    Exclusive,
}

fn probe(path: &Path) -> Result<Probe, String> {
    let open = || open_private(path, false);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-hosts-{label}-"))
    }

    #[test]
    fn a_window_and_a_session_never_both_hold_the_watch() {
        let l = layout("claims");
        assert_eq!(watcher(&l), Watcher::Nobody, "nothing has ever watched");

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

        let (admit, note) = a.admit();
        assert_eq!(admit, Admit::Later(CADENCE));
        assert!(note.unwrap().contains("an aterm window watches"));
        assert_eq!(a.admit(), (Admit::Later(CADENCE), None));
        let mut second = HostClaim::new(Host::Window, &l);
        assert_eq!(second.admit(), (Admit::Yes, None));
        drop(window);
        assert_eq!(
            a.admit().0,
            Admit::Later(CADENCE),
            "one window still watches"
        );
        drop(second);

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

    #[test]
    fn the_real_claims_refine_the_derived_model() {
        use aterm_models_atpkg::atpkg_head_watch_hosts_model;
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
        let mut steps = Vec::new();
        for (state, path) in &paths {
            for action in &model.actions {
                let mut next = state.clone();
                if model.fire(action.name, &mut next) {
                    steps.push((state, path, action.name, next));
                }
            }
        }
        assert!(
            steps.len() >= 100,
            "every reachable step is taken: {}",
            steps.len()
        );
        crate::testdir::each_side_by_side(&steps, |i, (state, path, action, next)| {
            let l = layout(&format!("refines-{i}"));
            let mut hosts = Hosts::new(&l);
            for step in *path {
                hosts.act(step);
            }
            assert_eq!(&hosts.project(), *state, "reached by {path:?}");
            hosts.act(action);
            assert_eq!(
                &hosts.project(),
                next,
                "{action} from {state:?} (reached by {path:?})"
            );
            drop(hosts);
            let _ = std::fs::remove_dir_all(&l.prefix);
        });

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

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
    let mut text =
        String::with_capacity(usize::try_from(MAX_SEAT_BYTES.saturating_add(1)).unwrap_or(0));
    file.take(MAX_SEAT_BYTES).read_to_string(&mut text).ok()?;
    let s = text.trim();
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
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

/// Who watches `layout`'s vendor heads, read off the rendezvous's locks without taking the watch.
///
/// The hosts file is read first, then the seat. On macOS a read holds no lock; elsewhere it takes
/// each lock for an instant, which can turn away a host that admits in that instant.
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
    // Only a session takes the seat, and only exclusive, so any hold of it is a seated session.
    match probe(&seat_path) {
        Ok(Probe::Free) if hosts == Probe::Exclusive => Watcher::Session(None),
        Ok(Probe::Free) => Watcher::Nobody,
        Ok(Probe::Shared | Probe::Exclusive) => Watcher::Session(
            open_private(&seat_path, false).and_then(|mut file| read_pid(&mut file)),
        ),
        Err(why) => Watcher::Unknown(why),
    }
}

/// How a rendezvous file's lock is held, as one read of it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Free,
    Shared,
    Exclusive,
}

fn probe(path: &Path) -> Result<Probe, String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(Probe::Free);
    }
    #[cfg(target_os = "macos")]
    {
        look(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        sample(path)
    }
}

fn unopened(path: &Path) -> String {
    format!("{} could not be opened", path.display())
}

/// macOS keeps `flock` locks on the list `fcntl` reads, so `F_GETLK` names the kind of the lock
/// that would refuse a writer. That is one read at one instant, holding nothing: a holder that lets
/// go and takes again cannot split it, and no host admitting meanwhile is turned away.
#[cfg(target_os = "macos")]
fn look(path: &Path) -> Result<Probe, String> {
    use std::os::fd::AsRawFd as _;
    let Some(file) = open_private(path, false) else {
        return Err(unopened(path));
    };
    #[cfg(test)]
    probing(path, Step::Reads);
    let mut lock = libc::flock {
        l_start: 0,
        l_len: 0,
        l_pid: 0,
        l_type: libc::F_WRLCK,
        l_whence: libc::SEEK_SET as libc::c_short,
    };
    // SAFETY: `file` stays open for the call, and `lock` is a live `flock` that `F_GETLK` reads
    // the asked range from and writes the blocking lock, if any, into.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } == -1 {
        let e = std::io::Error::last_os_error();
        return Err(format!("{} could not be read ({e})", path.display()));
    }
    match lock.l_type {
        libc::F_UNLCK => Ok(Probe::Free),
        libc::F_RDLCK => Ok(Probe::Shared),
        libc::F_WRLCK => Ok(Probe::Exclusive),
        kind => Err(format!(
            "{} has a lock of unknown kind {kind}",
            path.display()
        )),
    }
}

/// Elsewhere no read names a `flock` holder's kind, so it is sampled by taking the lock for an
/// instant, each take on a fresh open. A refused exclusive take shows somebody holds the file, and a
/// refused shared take that it is held exclusive. A shared take that succeeds shows no exclusive
/// holder at that moment and nothing about a shared one, so a last exclusive take tells a round
/// that has ended (Free) from a window's hold (Shared). The takes are separate instants: a session
/// that lets its round go before the shared take and takes the next before the last reads as a
/// window, and a host admitting while a take holds the file is turned away and asks again later.
#[cfg(any(test, not(target_os = "macos")))]
fn sample(path: &Path) -> Result<Probe, String> {
    let takes = |shared: bool| {
        let Some(file) = open_private(path, false) else {
            return Err(unopened(path));
        };
        #[cfg(test)]
        probing(path, Step::Reads);
        let taken = if shared {
            Flock::try_lock_shared(file)
        } else {
            Flock::try_lock(file)
        };
        match taken {
            Ok(_held) => {
                #[cfg(test)]
                probing(path, Step::Holds);
                Ok(true)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(format!("{} could not be locked ({e})", path.display()))
            }
        }
    };
    if takes(false)? {
        return Ok(Probe::Free);
    }
    if !takes(true)? {
        return Ok(Probe::Exclusive);
    }
    Ok(if takes(false)? {
        Probe::Free
    } else {
        Probe::Shared
    })
}

/// A point in a read of one rendezvous file where a test acts ([`probing`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The read is about to look at the file's lock.
    Reads,
    /// The read holds the lock it took.
    Holds,
}

/// What a test runs at each [`Step`] of a read, given the file read.
#[cfg(test)]
type ProbingHook = Box<dyn FnMut(&Path, Step)>;

#[cfg(test)]
thread_local! {
    static PROBING: std::cell::RefCell<Option<ProbingHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs this thread's probing hook, if a test set one, at `step` of a read of `path`.
#[cfg(test)]
fn probing(path: &Path, step: Step) {
    if let Some(mut hook) = PROBING.with_borrow_mut(Option::take) {
        hook(path, step);
        PROBING.with_borrow_mut(|slot| *slot = Some(hook));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// Each point a read reached, by file name.
    type Seen = Vec<(&'static str, Step)>;

    /// `read()` with `act` run at each point of its rendezvous reads; what it returned and the
    /// points, in order.
    fn probed<T>(
        mut act: impl FnMut(&'static str, Step) + 'static,
        read: impl FnOnce() -> T,
    ) -> (T, Seen) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        PROBING.with_borrow_mut(|slot| {
            *slot = Some(Box::new(move |path: &Path, step| {
                let file = [HOSTS_FILE, SEAT_FILE]
                    .into_iter()
                    .find(|name| path.ends_with(name))
                    .unwrap_or_else(|| panic!("a read of {}", path.display()));
                log.borrow_mut().push((file, step));
                act(file, step);
            }));
        });
        let out = read();
        PROBING.with_borrow_mut(|slot| *slot = None);
        let seen = seen.borrow().clone();
        (out, seen)
    }

    /// A probing hook that runs `act` before the `n`th read.
    fn before_read(
        n: usize,
        mut act: impl FnMut() + 'static,
    ) -> impl FnMut(&'static str, Step) + 'static {
        let mut reads = 0;
        move |_, step| {
            if step == Step::Reads {
                reads += 1;
                if reads == n {
                    act();
                }
            }
        }
    }

    /// A session that holds the seat between rounds.
    fn sitting(l: &Layout) -> HostClaim {
        let mut session = HostClaim::new(Host::Session, l);
        assert_eq!(session.admit().0, Admit::Yes);
        session.end_round();
        session
    }

    /// A seated session lets its round go or takes the next before each of the report's reads, as
    /// a runner step's back-to-back looks can: the report names the session, in either phase.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_round_that_comes_and_goes_mid_report_reads_as_the_session() {
        let l = layout("round-churn");
        let session = Rc::new(RefCell::new(sitting(&l)));
        for in_round in [false, true] {
            if in_round {
                assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
            }
            let churn = Rc::clone(&session);
            let (verdict, seen) = probed(
                move |_, step| {
                    if step == Step::Reads {
                        let mut claim = churn.borrow_mut();
                        if claim.seated_session() {
                            claim.end_round();
                        } else {
                            assert_eq!(claim.admit().0, Admit::Yes, "nothing else holds it");
                        }
                    }
                },
                || watcher(&l),
            );
            assert_eq!(
                verdict,
                Watcher::Session(Some(std::process::id())),
                "starting in a round: {in_round}"
            );
            assert_eq!(
                seen,
                [(HOSTS_FILE, Step::Reads), (SEAT_FILE, Step::Reads)],
                "one read of each file, holding nothing"
            );
        }
        drop(session);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The seat changes hands before each of the report's reads (its sitter exits, or a new one
    /// sits): the report names a seated session iff one sat when the seat was read.
    #[test]
    fn a_seat_that_changes_hands_mid_report_reads_as_it_was_read() {
        let l = layout("seat-churn");
        let sitter = Rc::new(RefCell::new(Some(sitting(&l))));
        for (seated, expect) in [
            (true, Watcher::Session(Some(std::process::id()))),
            (false, Watcher::Nobody),
        ] {
            if !seated {
                drop(sitter.borrow_mut().take());
            }
            let (churn, at) = (Rc::clone(&sitter), (*l).clone());
            let (verdict, _) = probed(
                move |_, step| {
                    if step == Step::Reads {
                        let mut seat = churn.borrow_mut();
                        let next = if seat.is_some() {
                            None
                        } else {
                            Some(sitting(&at))
                        };
                        drop(std::mem::replace(&mut *seat, next));
                    }
                },
                || watcher(&l),
            );
            assert_eq!(verdict, expect, "seated at the start: {seated}");
        }
        drop(sitter);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A session and a window that admit at every point of a report are not turned away by it.
    #[cfg(target_os = "macos")]
    #[test]
    fn hosts_that_admit_mid_report_are_not_turned_away() {
        let l = layout("admit-mid-report");
        let session = Rc::new(RefCell::new(sitting(&l)));
        let refused = Rc::new(RefCell::new(Vec::new()));
        let (admits, log, at) = (Rc::clone(&session), Rc::clone(&refused), (*l).clone());
        let (verdict, _) = probed(
            move |file, step| {
                let mut claim = admits.borrow_mut();
                let session = claim.admit().0;
                claim.end_round();
                let window = HostClaim::new(Host::Window, &at).admit().0;
                if session != Admit::Yes || window != Admit::Yes {
                    log.borrow_mut().push((file, step, session, window));
                }
            },
            || watcher(&l),
        );
        assert!(
            refused.borrow().is_empty(),
            "(file, step, session, window) where the report turned a host away: {:?}",
            refused.borrow()
        );
        assert_eq!(verdict, Watcher::Session(Some(std::process::id())));
        drop(session);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A window opens before the hosts read or between it and the seat's (in a session's round,
    /// which ends to let it in, or between rounds), or one closes at either point: the report
    /// names whoever held the hosts file when it was read.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_window_that_comes_or_goes_mid_report_reads_as_the_hosts_file_was_read() {
        let l = layout("window-churn");
        let session = Rc::new(RefCell::new(sitting(&l)));
        let seated = Watcher::Session(Some(std::process::id()));
        let window = Rc::new(RefCell::new(None::<HostClaim>));
        for (in_round, n, expect) in [
            (false, 1, Watcher::Window),
            (false, 2, seated.clone()),
            (true, 1, Watcher::Window),
            (true, 2, seated.clone()),
        ] {
            if in_round {
                assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
            }
            let (claim, opens, at) = (Rc::clone(&session), Rc::clone(&window), (*l).clone());
            let opening = before_read(n, move || {
                claim.borrow_mut().end_round();
                let mut opened = HostClaim::new(Host::Window, &at);
                assert_eq!(opened.admit(), (Admit::Yes, None));
                *opens.borrow_mut() = Some(opened);
            });
            assert_eq!(
                probed(opening, || watcher(&l)).0,
                expect,
                "in a round: {in_round}, opened before read {n}"
            );
            drop(window.borrow_mut().take());
        }
        for (n, expect) in [(1, seated.clone()), (2, Watcher::Window)] {
            let mut open = HostClaim::new(Host::Window, &l);
            assert_eq!(open.admit(), (Admit::Yes, None));
            *window.borrow_mut() = Some(open);
            let closes = Rc::clone(&window);
            let closing = before_read(n, move || drop(closes.borrow_mut().take()));
            assert_eq!(
                probed(closing, || watcher(&l)).0,
                expect,
                "closed before read {n}"
            );
            drop(window.borrow_mut().take());
        }
        drop(session);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The sample other systems read by: a hold that stays reads as its kind, and one let go
    /// before the last take reads Free.
    #[test]
    fn a_sampled_hold_reads_as_its_kind_or_free_once_let_go() {
        let l = layout("sample");
        let hosts = l.prefix.join(HOSTS_FILE);
        let session = Rc::new(RefCell::new(HostClaim::new(Host::Session, &l)));
        assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
        assert_eq!(sample(&hosts), Ok(Probe::Exclusive), "a round held");
        let claim = Rc::clone(&session);
        let (read, seen) = probed(
            before_read(2, move || claim.borrow_mut().end_round()),
            || sample(&hosts),
        );
        assert_eq!(
            read,
            Ok(Probe::Free),
            "a round let go after the refused exclusive take"
        );
        let (reads, holds) = ((HOSTS_FILE, Step::Reads), (HOSTS_FILE, Step::Holds));
        assert_eq!(seen, [reads, reads, holds, reads, holds]);

        let mut open = HostClaim::new(Host::Window, &l);
        assert_eq!(open.admit(), (Admit::Yes, None));
        assert_eq!(sample(&hosts), Ok(Probe::Shared), "a window's hold");
        let window = Rc::new(RefCell::new(Some(open)));
        let closes = Rc::clone(&window);
        let (read, _) = probed(
            before_read(3, move || drop(closes.borrow_mut().take())),
            || sample(&hosts),
        );
        assert_eq!(
            read,
            Ok(Probe::Free),
            "a window closed after the shared take"
        );
        drop(window);
        drop(session);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// targo's own deadline over connect, IDENTITY and STATUS.
#[cfg(not(test))]
const TARGO_GIVES: Duration = Duration::from_millis(500);

/// Under test, the one-minute hang detector: a unit test's stand-in answers from a thread
/// of the test binary, which a loaded machine can keep from running for longer than targo's
/// 0.5 s, and no test's property is how fast a stand-in answers.
#[cfg(test)]
const TARGO_GIVES: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Path to `trustd`'s per-user Unix domain socket.
#[must_use]
pub fn socket() -> PathBuf {
    let tmp = std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    tmp.join(format!("trustd-runtime-locks-{}", super::proc_tree::euid()))
        .join("trust-memory-jobserver.sock")
}

/// SHA-256 hex digest of `exe` used by `trustd` for `IDENTITY`.
#[must_use]
pub fn build_digest(exe: &Path) -> String {
    crate::durable::sha256_file_with_meta(exe)
        .map(|(h, _)| h)
        .unwrap_or_default()
}

/// What currently serves the `trustd` socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Socket is absent or unconnected.
    None,
    /// Served by this build's `trustd`.
    Ours,
    /// Served by another build's `trustd` or an unrecognized server.
    Foreign,
    /// Connected but did not answer within the timeout.
    Silent,
}

/// Result of probing the `trustd` socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub held: Held,
    pub what: String,
}

impl Probe {
    /// Formats `<none|ours|foreign|silent>\t<detail>` for a check's `.trustd` mark file.
    #[must_use]
    pub fn line(&self) -> String {
        let word = match self.held {
            Held::None => "none",
            Held::Ours => "ours",
            Held::Foreign => "foreign",
            Held::Silent => "silent",
        };
        format!("{word}\t{}", crate::trust_facts::one_line(&self.what))
    }
}

fn ask(s: &mut BufReader<UnixStream>, req: &str) -> std::io::Result<String> {
    const MAX_REPLY: usize = 64 * 1024;
    s.get_mut().write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    if std::io::Read::by_ref(s)
        .take((MAX_REPLY as u64) + 1)
        .read_line(&mut line)?
        == 0
    {
        return Err(std::io::Error::other("it closed the connection"));
    }
    if line.len() > MAX_REPLY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "trustd reply exceeds 64 KiB",
        ));
    }
    Ok(line.trim().to_string())
}

/// Who a trustd says it is: `Ours` when its IDENTITY names `want`.
fn identify(s: &mut BufReader<UnixStream>, sock: &Path, want: &str) -> Probe {
    let silent = |why: String| Probe {
        held: Held::Silent,
        what: format!("the trustd at {} {why}", sock.display()),
    };
    let reply = match ask(s, "IDENTITY") {
        Ok(r) => r,
        Err(e) => return silent(format!("did not answer IDENTITY ({e})")),
    };
    if reply == "ERR busy" {
        return silent("answered ERR busy, every connection taken".into());
    }
    let ident: Option<aterm_json::Value> = aterm_json::from_str(&reply).ok();
    let field = |k: &str| {
        ident
            .as_ref()
            .and_then(|v| v.get(k))
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("")
    };
    let sha = field("executable_sha256");
    let build = format!("trustd {} {}", field("release"), field("commit"));
    if !sha.is_empty() && sha == want {
        return Probe {
            held: Held::Ours,
            what: format!("this build's {build} serves {}", sock.display()),
        };
    }
    Probe {
        held: Held::Foreign,
        what: format!(
            "a trustd of another build serves {} ({build}, sha256 {}; this run's is {})",
            sock.display(),
            sha.get(..12).unwrap_or("none"),
            want.get(..12).unwrap_or("none")
        ),
    }
}

fn probe_once(sock: &Path, want: &str, wait: Duration) -> (Probe, Duration) {
    if !sock.exists() {
        let what = format!("nothing serves {}", sock.display());
        return (
            Probe {
                held: Held::None,
                what,
            },
            Duration::ZERO,
        );
    }
    let t0 = Instant::now();
    // A Unix-domain connect never waits on Apple, so it may hold the spawn lock.
    let stream = match aterm_uds::kept_out(|| UnixStream::connect(sock)) {
        Ok(s) => s,
        Err(e) => {
            let what = format!("nothing serves {} ({e})", sock.display());
            return (
                Probe {
                    held: Held::None,
                    what,
                },
                Duration::ZERO,
            );
        }
    };
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let mut s = BufReader::new(stream);
    let mut p = identify(&mut s, sock, want);
    if p.held == Held::Ours
        && let Err(e) = ask(&mut s, "STATUS")
    {
        p = Probe {
            held: Held::Silent,
            what: format!(
                "the trustd at {} did not answer STATUS ({e})",
                sock.display()
            ),
        };
    }
    (p, t0.elapsed())
}

/// Probes `sock` against expected identity `want` with timeout `wait`.
#[must_use]
pub fn probe(sock: &Path, want: &str, wait: Duration) -> Probe {
    let mut took = Duration::ZERO;
    for _ in 0..3 {
        let (p, t) = probe_once(sock, want, wait);
        if p.held != Held::Ours || t <= TARGO_GIVES {
            return p;
        }
        took = t;
    }
    Probe {
        held: Held::Silent,
        what: format!(
            "the trustd at {} took {:.1} s to answer, where targo gives it 0.5 s",
            sock.display(),
            took.as_secs_f64()
        ),
    }
}

/// How often a wait for the socket looks at it again ([`wait_usable`]).
pub const LOOK_EVERY: Duration = Duration::from_secs(5);

/// What a look at the socket gives each answer it waits for: a trustd that takes longer
/// reads silent, and the wait looks again.
#[cfg(not(test))]
const LOOK_GIVES: Duration = Duration::from_secs(10);

/// Under test, the one-minute hang detector, as [`TARGO_GIVES`]: a unit test's stand-in
/// trustd answers every look from a thread of the test binary, and no test holds one silent
/// for a look.
#[cfg(test)]
const LOOK_GIVES: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The most leases a wait line names.
const LEASES_SAID: usize = 8;

const NEVER: &str = "never signal it: a killed trustd records DIRTY and blocks every check \
                         until `--recover-after-crash` attests no solver survives";

/// A lease a trustd granted, as its STATUS names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub pid: u32,
    /// What its holder said it holds trustd for: the lane's keeper says `trust lane holds
    /// trustd for its run`.
    pub label: String,
}

/// The leases the trustd at `sock` says it has granted (its STATUS `active` list), or
/// `None` when it would not say.
fn leases(sock: &Path, wait: Duration) -> Option<Vec<Lease>> {
    let stream = aterm_uds::kept_out(|| UnixStream::connect(sock)).ok()?;
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let mut s = BufReader::new(stream);
    ask(&mut s, "IDENTITY").ok()?;
    let status: aterm_json::Value = aterm_json::from_str(&ask(&mut s, "STATUS").ok()?).ok()?;
    status
        .get("active")?
        .as_array()?
        .iter()
        .map(|lease| {
            Some(Lease {
                pid: u32::try_from(lease.get("pid")?.as_u64()?).ok()?,
                label: crate::trust_facts::one_line(
                    lease
                        .get("label")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or(""),
                ),
            })
        })
        .collect()
}

/// Whom `leases` names, for a wait line: what keeps a trustd of another build serving,
/// since one retires 300 s after its last grant ends.
#[must_use]
pub fn leased_by(leases: Option<&[Lease]>) -> String {
    let Some(leases) = leases else {
        return String::new();
    };
    if leases.is_empty() {
        return "; it holds no lease, so it retires 300 s after its last grant".into();
    }
    let mut named: Vec<String> = leases
        .iter()
        .take(LEASES_SAID)
        .map(|l| {
            let label = if l.label.is_empty() {
                String::new()
            } else {
                format!(" ({})", l.label)
            };
            let gone = if aterm_uds::process::pid_alive(l.pid) {
                ""
            } else {
                ", which has exited"
            };
            format!("pid {}{label}{gone}", l.pid)
        })
        .collect();
    if leases.len() > LEASES_SAID {
        named.push(format!("and {} more", leases.len() - LEASES_SAID));
    }
    format!("; leased by {}", named.join(", "))
}

/// The clock a wait for the socket keeps ([`wait_on`]): the machine's, or a test's.
pub trait Clock {
    /// How long the wait has lasted.
    fn waited(&self) -> Duration;
    /// Nap `d`: `true` when `stop` ended it early.
    fn nap(&mut self, d: Duration, stop: &dyn Fn() -> bool) -> bool;
}

/// The machine's clock, napping in steps an interrupt ends ([`aterm_toolchain::interrupt::NAP`]).
struct Wall(Instant);

impl Clock for Wall {
    fn waited(&self) -> Duration {
        self.0.elapsed()
    }

    fn nap(&mut self, d: Duration, stop: &dyn Fn() -> bool) -> bool {
        let end = Instant::now() + d;
        loop {
            if stop() {
                return true;
            }
            let now = Instant::now();
            if now >= end {
                return false;
            }
            std::thread::sleep((end - now).min(aterm_toolchain::interrupt::NAP));
        }
    }
}

/// Why a wait for the socket ended without it ([`wait_on`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// `--no-wait`: what holds it.
    Held(String),
    /// An interrupt, or the caller's `stop`: what held it then.
    Stopped(String),
}

/// One look at `sock` against `want`, with the leases of a trustd of another build, made on
/// a thread of its own so that `stop` is heard every [`aterm_toolchain::interrupt::NAP`]
/// while a trustd takes its time to answer; `None` once `stop` says so first.
fn look(sock: &Path, want: &str, stop: &dyn Fn() -> bool) -> Option<(Probe, Option<Vec<Lease>>)> {
    let seen = |sock: &Path, want: &str| {
        let p = probe(sock, want, LOOK_GIVES);
        let leases = (p.held == Held::Foreign)
            .then(|| leases(sock, LOOK_GIVES))
            .flatten();
        (p, leases)
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (owned_sock, owned_want) = (sock.to_path_buf(), want.to_string());
    let spawned = std::thread::Builder::new()
        .name("trustd look".into())
        .spawn(move || {
            let _ = tx.send(seen(&owned_sock, &owned_want));
        });
    if spawned.is_err() {
        return Some(seen(sock, want));
    }
    loop {
        match rx.recv_timeout(aterm_toolchain::interrupt::NAP) {
            Ok(found) => return Some(found),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if stop() => return None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Some(seen(sock, want));
            }
        }
    }
}

/// Waits for `sock` to serve this build's `trustd` or nothing: admission behind whatever
/// holds it, with no deadline (D-TRUSTD-330S). A trustd of another build serves while any
/// run of that build holds a lease on it, which a lane run does for hours, and retires 300 s
/// after its last grant; a fixed wait ended a busy socket COULD NOT RUN. While it waits it
/// says what holds the socket and who holds its leases as it starts and once a minute, and
/// marks the wait where a ceiling reads it ([`aterm_toolchain::waits`]); it looks every
/// [`LOOK_EVERY`]. An interrupt (SIGINT, SIGTERM or SIGHUP, [`aterm_toolchain::interrupt`])
/// or `stop` ends it within a nap, and `--no-wait` ([`aterm_toolchain::named_lock::Wait::No`]) answers at once.
///
/// # Errors
/// What held the socket when `--no-wait` would not wait or the wait was interrupted, and
/// that nothing was verified.
pub fn wait_usable(
    sock: &Path,
    want: &str,
    wait: aterm_toolchain::named_lock::Wait,
    out: &mut dyn std::io::Write,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let watch = aterm_toolchain::interrupt::Watch::enter();
    let stopped = || watch.caught().is_some() || stop();
    let ended = wait_on(sock, want, wait, out, &stopped, &mut Wall(Instant::now()));
    ended.map_err(|e| match e {
        Unusable::Held(what) => format!(
            "{what}: --no-wait does not wait for it, so nothing was verified; leave out \
                 --no-wait to wait until it serves this build or retires; {NEVER}"
        ),
        Unusable::Stopped(what) => format!(
            "interrupted{} while waiting for this build's trustd or none: {what}; nothing \
                 was verified",
            watch.take().map_or_else(String::new, |sig| format!(
                " ({})",
                aterm_toolchain::interrupt::name(sig)
            ))
        ),
    })
}

/// [`wait_usable`]'s wait on `clock`.
///
/// # Errors
/// [`Unusable`]: what held the socket when it would not wait, or when `stop` ended the wait.
pub fn wait_on(
    sock: &Path,
    want: &str,
    wait: aterm_toolchain::named_lock::Wait,
    out: &mut dyn std::io::Write,
    stop: &dyn Fn() -> bool,
    clock: &mut dyn Clock,
) -> Result<(), Unusable> {
    let mut said: Option<Duration> = None;
    let mut mark: Option<aterm_toolchain::waits::Mark> = None;
    let mut held = format!("what serves {} was not read yet", sock.display());
    loop {
        let Some((p, leases)) = look(sock, want, stop) else {
            return Err(Unusable::Stopped(held));
        };
        let waited = clock.waited();
        if matches!(p.held, Held::None | Held::Ours) {
            if said.is_some() {
                let _ = writeln!(
                    out,
                    "trust: the socket is usable after {} min: {}",
                    waited.as_secs() / 60,
                    p.what
                );
            }
            return Ok(());
        }
        held = format!("{}{}", p.what, leased_by(leases.as_deref()));
        if wait == aterm_toolchain::named_lock::Wait::No {
            return Err(Unusable::Held(held));
        }
        let line = match said {
            None => Some(format!(
                "trust: waiting for this build's trustd or none: {held}"
            )),
            Some(t) if waited >= t + aterm_toolchain::named_lock::SAY_EVERY => Some(format!(
                "trust: still waiting for this build's trustd or none ({} min): {held}",
                waited.as_secs() / 60
            )),
            Some(_) => None,
        };
        if let Some(line) = line {
            let _ = writeln!(out, "{line}");
            match &mut mark {
                Some(m) => m.say(&line),
                None => mark = aterm_toolchain::waits::Mark::begin(&line),
            }
            said = Some(waited);
        }
        if clock.nap(LOOK_EVERY, stop) {
            return Err(Unusable::Stopped(held));
        }
    }
}

/// Holds a one-byte lease on this build's `trustd` so it does not idle out mid-run, and
/// counts the leases that ended under it ([`Keeper::lost`]).
pub struct Keeper {
    stop: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
    lost: Arc<AtomicU32>,
    conn: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Keeper {
    #[must_use]
    pub fn start(sock: PathBuf, want: String, grants: PathBuf, seen: PathBuf) -> Keeper {
        let _ = std::fs::remove_file(&grants);
        let _ = std::fs::remove_file(&seen);
        let stop = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicU32::new(0));
        let conn = Arc::new(Mutex::new(None));
        let k = Kept {
            stop: Arc::clone(&stop),
            held: Arc::clone(&held),
            lost: Arc::clone(&lost),
            conn: Arc::clone(&conn),
        };
        let thread = std::thread::spawn(move || keep(&sock, &want, &grants, &seen, &k));
        Keeper {
            stop,
            held,
            lost,
            conn,
            thread: Some(thread),
        }
    }

    #[must_use]
    pub fn held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// How many of its grants trustd ended under it (it closed or failed the lease's
    /// connection: it went away): a check that ran across one lost its prover. A grant
    /// whose connection trustd has already closed counts at once, before the keeper's
    /// thread reads the end, so a check that ended of that loss counts it.
    #[must_use]
    pub fn lost(&self) -> u32 {
        use std::os::fd::AsRawFd as _;
        // Read before the count: the thread counts an end before it lets `held` go, so an
        // end it has not counted yet is the held grant's, and none is missed (one counted
        // twice still counts as lost).
        let ending = self.held()
            && self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|c| {
                    matches!(
                        aterm_uds::hangup::fd_peer_state(c.as_raw_fd()),
                        aterm_uds::hangup::PeerState::Gone
                            | aterm_uds::hangup::PeerState::HalfClosed
                    )
                });
        self.lost
            .load(Ordering::SeqCst)
            .saturating_add(u32::from(ending))
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// The keeper thread's share of its [`Keeper`].
struct Kept {
    stop: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
    /// The grants trustd ended under it ([`Keeper::lost`]).
    lost: Arc<AtomicU32>,
    /// The connection it waits on, for the drop to shut.
    conn: Arc<Mutex<Option<UnixStream>>>,
}

impl Kept {
    /// Registers the cloned connection before waiting on it.
    fn name(&self, stream: std::io::Result<UnixStream>) -> bool {
        let Ok(stream) = stream else { return false };
        let mut c = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.load(Ordering::SeqCst) {
            return false;
        }
        *c = Some(stream);
        true
    }
}

fn keep(sock: &Path, want: &str, grants_out: &Path, seen_out: &Path, k: &Kept) {
    let (stop, held) = (&*k.stop, &*k.held);
    let mut grants = 0u32;
    let tick = Duration::from_secs(2);
    while !stop.load(Ordering::SeqCst) {
        if let Ok(stream) = aterm_uds::kept_out(|| UnixStream::connect(sock)) {
            if !k.name(aterm_uds::kept_out(|| stream.try_clone())) {
                break;
            }
            let _ = stream.set_read_timeout(Some(Duration::from_secs(130)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
            let mut s = BufReader::new(stream);
            let p = identify(&mut s, sock, want);
            if p.held == Held::Ours {
                let req = format!(
                    "RESERVE 1 {} trust lane holds trustd for its run",
                    std::process::id()
                );
                if ask(&mut s, &req).is_ok_and(|r| r.starts_with("GRANTED ")) {
                    grants += 1;
                    let _ = crate::durable::write_fresh(grants_out, format!("{grants}\n"));
                    held.store(true, Ordering::SeqCst);
                    let _ = s.get_ref().set_read_timeout(Some(tick));
                    let mut buf = [0u8; 256];
                    let mut ended = false;
                    while !stop.load(Ordering::SeqCst) {
                        match s.get_mut().read(&mut buf) {
                            Ok(0) => {
                                ended = true;
                                break;
                            }
                            Ok(_) => {}
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(_) => {
                                ended = true;
                                break;
                            }
                        }
                    }
                    // trustd ended the lease, not this keeper's drop: counted before
                    // `held` goes, as [`Keeper::lost`] reads them.
                    if ended && !stop.load(Ordering::SeqCst) {
                        k.lost.fetch_add(1, Ordering::SeqCst);
                    }
                    held.store(false, Ordering::SeqCst);
                }
            } else {
                let _ = crate::durable::write_fresh(seen_out, format!("{}\n", p.line()));
            }
        }
        if !stop.load(Ordering::SeqCst) {
            std::thread::park_timeout(tick);
        }
    }
}

#[cfg(test)]
#[path = "trustd_tests.rs"]
mod tests;

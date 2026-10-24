// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared `fabric_world` test harness for a guarded broker, headless `aterm`, and bridge child.

#![allow(dead_code)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child as OwnedChild, Stdio as OwnedStdio};

use astream_broker::{Broker, BrokerHandle, Client, OpenOptions, Prepared};
use aterm_link::ctl::{Ctl, Reply};

/// The mint secret every test's broker is guarded with.
pub const SECRET: &[u8] = b"a3-bridge-e2e-secret-of-32-bytes";

#[must_use]
pub fn broker_opts(tier: aterm_link::durability::Durability) -> OpenOptions {
    let mut opts = tier.options();
    opts.guard = Some(SECRET.to_vec());
    opts
}

#[must_use]
pub fn open_broker(log: impl AsRef<Path>, opts: &OpenOptions) -> Broker {
    let broker = open_broker_unretained(log, opts);
    aterm_link::broker_rules::retain(&broker).expect("the default retention rules");
    broker
}

#[must_use]
pub fn open_broker_unretained(log: impl AsRef<Path>, opts: &OpenOptions) -> Broker {
    Broker::open_opts(log, opts.clone(), || Ok(Prepared::Nothing)).expect("guarded broker")
}

/// The fleet every test runs in.
pub const FLEET: &str = "f1";
/// The ceiling on every wait.
pub const DEADLINE: Duration = aterm_spec::wait::MUST_HAPPEN;

const MAX_SOCK_PATH: usize = 100;

fn assert_sock_fits(sock: &str) {
    let p = Path::new(sock);
    let dialled = match (p.parent().map(std::fs::canonicalize), p.file_name()) {
        (Some(Ok(dir)), Some(name)) => dir.join(name),
        _ => p.to_path_buf(),
    };
    assert!(
        dialled.as_os_str().len() < MAX_SOCK_PATH,
        "socket path too long for sun_path as dialled: {}",
        dialled.display()
    );
}

/// Wait until `f` answers `Some`, or fail with `what`.
pub fn until<T>(what: &str, f: impl FnMut() -> Option<T>) -> T {
    until_within(DEADLINE, what, f)
}

/// [`until`] with a stated budget, for the one class of observable that has a PERIOD of its own.
pub use aterm_spec::wait::until_within;

/// The budget for something the bridge's periodic backstop publishes.
pub const PERIODIC_DEADLINE: Duration = Duration::from_secs(180);
/// Fast-timers development seam as a [`World::boot_with`] environment entry.
pub const FAST_TIMERS: (&str, &str) = (aterm_types::domain::ENV_DEV_FAST_TIMERS, "1");
/// Bound on one control-verb reply (`2 * DEADLINE`).
pub const VERB_DEADLINE: Duration = Duration::from_secs(2 * DEADLINE.as_secs());

/// Connects to an instance's control socket and authenticates with `bound` read/write timeouts.
pub fn connect_within(sock: &str, token: &str, bound: Duration) -> std::io::Result<Ctl> {
    let ctl = Ctl::connect(sock, token)?;
    ctl.get_ref().set_read_timeout(Some(bound))?;
    ctl.get_ref().set_write_timeout(Some(bound))?;
    Ok(ctl)
}

/// [`connect_within`] at [`VERB_DEADLINE`] — the connection every test verb goes over.
pub fn connect(sock: &str, token: &str) -> std::io::Result<Ctl> {
    connect_within(sock, token, VERB_DEADLINE)
}

fn max_concurrent_boots() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 3).clamp(2, 4))
}

fn boot_gate() -> &'static (std::sync::Mutex<usize>, std::sync::Condvar) {
    static GATE: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()))
}

#[must_use]
pub struct BootPermit;

impl Drop for BootPermit {
    fn drop(&mut self) {
        let (lock, cv) = boot_gate();
        let mut n = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *n = n.saturating_sub(1);
        cv.notify_one();
    }
}

fn max_live_stack_tests() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 11))
}

#[derive(Default)]
struct LiveGate {
    held: usize,
    waiting: std::collections::BTreeSet<(String, u64)>,
    arrivals: u64,
}

fn live_gate() -> &'static (std::sync::Mutex<LiveGate>, std::sync::Condvar) {
    static GATE: std::sync::OnceLock<(std::sync::Mutex<LiveGate>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    GATE.get_or_init(Default::default)
}

struct LiveSlot;

impl Drop for LiveSlot {
    fn drop(&mut self) {
        let (lock, cv) = live_gate();
        let mut gate = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gate.held = gate.held.saturating_sub(1);
        cv.notify_all();
    }
}

thread_local! {
    static LIVE_SLOT: std::cell::RefCell<Option<LiveSlot>> =
        const { std::cell::RefCell::new(None) };
}

struct Queued(Option<(String, u64)>);

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(me) = self.0.take() {
            let (lock, cv) = live_gate();
            let mut gate = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            gate.waiting.remove(&me);
            cv.notify_all();
        }
    }
}

fn queue_for_live_slot() -> Option<Queued> {
    if LIVE_SLOT.with(|s| s.borrow().is_some()) {
        return None;
    }
    let (lock, _) = live_gate();
    let mut gate = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.arrivals += 1;
    let me = (
        std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string(),
        gate.arrivals,
    );
    gate.waiting.insert(me.clone());
    Some(Queued(Some(me)))
}

fn take_live_slot(mut queued: Queued) {
    let me = queued.0.take().expect("a queued test holds its place");
    let (lock, cv) = live_gate();
    let cap = max_live_stack_tests();
    let mut gate = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while gate.held >= cap || gate.waiting.first() != Some(&me) {
        gate = cv
            .wait(gate)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    gate.waiting.remove(&me);
    gate.held += 1;
    drop(gate);
    cv.notify_all();
    LIVE_SLOT.with(|s| *s.borrow_mut() = Some(LiveSlot));
}

/// Take a boot permit, waiting for one if [`max_concurrent_boots`] are already out.
pub fn boot_permit() -> BootPermit {
    if let Some(queued) = queue_for_live_slot() {
        take_live_slot(queued);
    }
    let (lock, cv) = boot_gate();
    let cap = max_concurrent_boots();
    let mut n = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *n >= cap {
        n = cv
            .wait(n)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *n += 1;
    BootPermit
}

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        kill_reaped(&mut self.0);
    }
}

/// `SIGKILL` `child` and reap it within [`DEADLINE`]; one still there then is left unreaped, since whether a program macOS's program check holds takes its signal is unmeasured.
pub fn kill_reaped(child: &mut Child) {
    let _ = child.kill();
    let _ = aterm_spec::wait::within(DEADLINE, Duration::from_millis(10), || {
        child.try_wait().ok().flatten()
    });
}

/// The words a wait on `pid`, a child this test spawned, ends the test COULD NOT RUN with when it ran out at [`DEADLINE`] while macOS's program check holds the child ([`aterm_uds::deadline::held_words`]); asked before the child is killed, so any other silence stays the test's failure.
fn held_at_deadline(pid: u32) -> Option<String> {
    aterm_uds::deadline::held_words(pid, DEADLINE, program_check(pid))
}

/// The next item `rx` carries from the output of `pid`, a child this test spawned, within [`DEADLINE`] of `since`; one that never comes ends the test as [`recv_judged`] does. The child's owner ends it as the test unwinds.
pub fn recv_within<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    since: Instant,
    pid: u32,
    what: impl FnOnce() -> String,
) -> T {
    recv_judged(rx, since, DEADLINE, pid, program_check, what)
}

/// The next item `rx` carries within `hang` of `since`, `hang` a hang detector on `pid`, the child whose output feeds it. At the deadline it asks `held` of the child, before anything kills it: COULD NOT RUN when macOS's program check holds it ([`aterm_uds::deadline::held_words`]), else the test fails naming `what` never came.
pub fn recv_judged<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    since: Instant,
    hang: Duration,
    pid: u32,
    held: impl FnOnce(u32) -> Option<bool>,
    what: impl FnOnce() -> String,
) -> T {
    rx.recv_timeout(hang.saturating_sub(since.elapsed()))
        .unwrap_or_else(|e| {
            if let Some(words) = aterm_uds::deadline::held_words(pid, hang, held(pid)) {
                aterm_uds::host::could_not_run(words);
            }
            panic!("{} ({e:?} after {hang:?})", what())
        })
}

/// Whether macOS's program check holds `pid` ([`aterm_uds::process::at_program_check`]).
fn program_check(pid: u32) -> Option<bool> {
    aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0))
}

fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// Runs `cmd` to completion with its exit and stdio pipes bounded by [`DEADLINE`].
pub fn output_within(cmd: &mut Command, what: &str) -> std::process::Output {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {what}: {e}"));
    wait_output_within(child, what)
}

/// Waits for `child` and collects its output, bounded by [`DEADLINE`].
pub fn wait_output_within(child: Child, what: &str) -> std::process::Output {
    let mut child = Reaped(child);
    let stdout = child.0.stdout.take().map(drain);
    let stderr = child.0.stderr.take().map(drain);
    let status = aterm_spec::wait::within(DEADLINE, Duration::from_millis(10), || {
        child.0.try_wait().ok().flatten()
    })
    .unwrap_or_else(|| {
        if let Some(words) = held_at_deadline(child.0.id()) {
            aterm_uds::host::could_not_run(words);
        }
        panic!("timed out after {DEADLINE:?} waiting for: {what} to exit")
    });
    let read = |rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>, pipe: &str| {
        rx.map_or_else(Vec::new, |rx| {
            rx.recv_timeout(DEADLINE).unwrap_or_else(|_| {
                panic!("{what} exited ({status}) but its {pipe} stayed open past {DEADLINE:?}")
            })
        })
    };
    std::process::Output {
        status,
        stdout: read(stdout, "stdout"),
        stderr: read(stderr, "stderr"),
    }
}

/// Connects a broker client on `sock` with read and write timeouts bounded by [`VERB_DEADLINE`].
pub fn broker_client(sock: impl AsRef<Path>) -> Client {
    let sock = sock.as_ref();
    let s = std::os::unix::net::UnixStream::connect(sock)
        .unwrap_or_else(|e| panic!("connect to the broker at {}: {e}", sock.display()));
    s.set_read_timeout(Some(VERB_DEADLINE))
        .and_then(|()| s.set_write_timeout(Some(VERB_DEADLINE)))
        .expect("bound the broker connection");
    Client::from_stream(s)
}

/// Reads the first line `child` prints on its piped stdout, bounded by [`DEADLINE`]; a child macOS's program check still holds then ends the test COULD NOT RUN ([`aterm_uds::deadline::held_words`]).
pub fn first_line_within(child: &mut OwnedChild, what: &str) -> String {
    use std::io::BufRead;
    let out = child.stdout.take().expect("the child's stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        let held = held_at_deadline(child.id());
        aterm_uds::deadline::kill_within(child, DEADLINE);
        if let Some(words) = held {
            aterm_uds::host::could_not_run(words);
        }
        panic!("timed out after {DEADLINE:?} waiting for: {what}")
    })
}

/// Mint one capability and render it as a `--cap-file` line.
pub fn cap_line(grant: &str) -> String {
    let cap = astream_cap::mint(SECRET, grant).expect("mint");
    let hex: String = cap.tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("{} {hex}\n", cap.filter)
}

/// Returns the capability grant ring for `node` ([`aterm_link::enable::node_grants`]).
pub fn node_grants(node: &str) -> Vec<String> {
    aterm_link::enable::node_grants(FLEET, node)
}

/// Strips inherited `ATERM_*` environment variables and configures test GUI defaults on `cmd`.
pub fn prepare_gui_environment(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if stripped(&name) {
            cmd.env_remove(&name);
        }
    }
    cmd.env(STALE_HORIZON.0, STALE_HORIZON.1.as_millis().to_string());
}

/// Owed-answer horizon (`ATERM_FABRIC_STALE_AFTER_MS`) set by [`prepare_gui_environment`].
pub const STALE_HORIZON: (&str, Duration) = ("ATERM_FABRIC_STALE_AFTER_MS", VERB_DEADLINE);

/// Arms a `--headless` launch with a lifeline held for the life of this test process.
pub fn arm_lifeline(cmd: &mut Command, dir: &Path) {
    use aterm_uds::spawnfd::InheritNothing as _;
    cmd.inherit_nothing();
    aterm_uds::lifeline::arm_for_process(cmd, dir)
        .unwrap_or_else(|e| panic!("arm the instance's lifeline in {}: {e}", dir.display()));
}

#[must_use]
pub struct BrokerChild {
    pub child: OwnedChild,
    pub lifeline: aterm_uds::lifeline::Lifeline,
}

/// Spawns `aterm link broker <args>…` armed with a [`aterm_uds::lifeline::Lifeline`], at test
/// pace ([`FAST_TIMERS`]): it sees a poisoned log or a cut lifeline within its fast wake.
///
/// # Panics
/// The lifeline's FIFO in `dir` could not be set up, or the spawn failed.
pub fn spawn_broker(
    dir: &Path,
    args: &[&str],
    stdio: impl FnOnce(&mut aterm_uds::spawnfd::Command),
) -> BrokerChild {
    let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
    cmd.args(["link", "broker"])
        .args(args)
        .env(FAST_TIMERS.0, FAST_TIMERS.1);
    stdio(&mut cmd);
    let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut cmd, dir)
        .unwrap_or_else(|e| panic!("arm the broker's lifeline in {}: {e}", dir.display()));
    let child = cmd.spawn().expect("spawn the broker");
    BrokerChild { child, lifeline }
}

/// Who started a bridge, read while its aterm lives ([`launcher_of`]), for [`end_bridge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Launcher {
    /// The aterm's identity: where the kernel keeps pid versions (Apple), a bridge whose
    /// launcher version is this aterm's was started by it.
    pub identity: Option<aterm_uds::process::Identity>,
    /// The process `state/pid` named, read as that aterm's own child while the aterm lived: the
    /// proof where the kernel keeps no pid versions (Linux).
    pub child: Option<aterm_uds::process::Identity>,
}

/// `pid`'s [`Launcher`], read while it lives (a process this test spawned and has not reaped,
/// so the number names it): its identity, and the identity of the process `state/pid` names,
/// kept only when that process's parent is `pid` and `pid` reads the same identity after it, so
/// the read named this aterm's own live child.
#[must_use]
pub fn launcher_of(pid: u32, state: &Path) -> Launcher {
    use aterm_uds::process::{read_identity, read_lineage};
    let Ok(pid) = i32::try_from(pid) else {
        return Launcher::default();
    };
    let identity = read_identity(pid);
    let child = std::fs::read_to_string(state.join("pid"))
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .and_then(read_lineage)
        .filter(|lineage| lineage.parent == pid.unsigned_abs())
        .map(|lineage| lineage.identity);
    let still = identity.is_some() && read_identity(pid) == identity;
    Launcher {
        identity,
        child: child.filter(|_| still),
    }
}

/// Whether `lineage`, a process that runs the [`aterm`] under test (`runs_ours`), is the bridge
/// `launcher`'s aterm started: its launcher's pid version is that aterm's (Apple; this also
/// proves a bridge the aterm restarted after `launcher` was read), or it is the very child read
/// while that aterm lived (where the kernel keeps no versions, two missing versions prove
/// nothing).
#[must_use]
pub fn bridge_proven(
    lineage: &aterm_uds::process::Lineage,
    runs_ours: bool,
    launcher: &Launcher,
) -> bool {
    let started = launcher
        .identity
        .is_some_and(|aterm| lineage.launched_by(&aterm) == Some(true));
    runs_ours && (started || launcher.child == Some(lineage.identity))
}

/// Ends the bridge `state/pid` names through [`aterm_uds::process::end_proven`] only if it proves
/// to be the one `launcher`'s aterm started ([`bridge_proven`]) and runs the [`aterm`] under test;
/// otherwise leaves it alone.
pub fn end_bridge(state: &Path, launcher: Launcher) {
    use aterm_uds::process::{END_GRACE, EndControlLive, Identity, end_proven, pid_alive};
    let Some(pid) = std::fs::read_to_string(state.join("pid"))
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
    else {
        return;
    };
    let ours = std::fs::canonicalize(aterm()).ok();
    let proven = |pid: i32| -> Option<Identity> {
        let lineage = aterm_uds::process::read_lineage(pid)?;
        let runs = aterm_uds::process::read_process_path(pid)
            .and_then(|path| std::fs::canonicalize(path).ok());
        let runs_ours = ours.is_some() && runs == ours;
        bridge_proven(&lineage, runs_ours, &launcher).then_some(lineage.identity)
    };
    let Some(token) = proven(pid) else {
        if u32::try_from(pid).is_ok_and(pid_alive) {
            eprintln!(
                "LEFT pid {pid} ({}/pid): not proven the bridge its aterm started",
                state.display()
            );
        }
        return;
    };
    let _ = end_proven(token, END_GRACE, &mut EndControlLive, |id| {
        proven(id.pid) == Some(*id)
    });
}

/// These Fabric fixtures do not exercise update, package or machine configuration.
pub const FIXTURE_GUI_CONFIG: &str = "agents_auto_prime = false\n\n[update]\nenabled = false\nauto_apply = false\n\n[packages]\nenabled = false\n\n[machine]\nuniversal_control = \"leave\"\n";

/// Seed a private config once; an intentional fixture edit survives relaunch.
pub fn prepare_fixture_config(tmp: &Path) {
    use std::io::Write;

    let dir = tmp.join("cfg/aterm");
    std::fs::create_dir_all(&dir).expect("scratch config dir");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("aterm.toml"))
    {
        Ok(mut file) => file
            .write_all(FIXTURE_GUI_CONFIG.as_bytes())
            .expect("write isolated GUI config"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create isolated GUI config: {error}"),
    }
}

/// One booted world: a guarded broker, a headless aterm, and the bridge child aterm launched.
pub type GrantsFor<'a> = &'a dyn Fn(&str) -> Vec<String>;

struct Scratch {
    dir: Option<aterm_tempfile::TempDir>,
    keep: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            eprintln!("KEPT fabric world {}", dir.keep().display());
        }
    }
}

fn child_exit(pid: i32, parent: u32) -> Result<std::os::fd::OwnedFd, String> {
    let birth = aterm_uds::process::read_birth(pid)
        .ok_or_else(|| format!("cannot read child {pid}'s birth"))?;
    if aterm_uds::process::read_process_parent(pid) != Some(parent) {
        return Err(format!(
            "{pid} is not an observed child of instance {parent}"
        ));
    }
    let fd = u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::exitwatch::exit_fd)
        .ok_or_else(|| format!("cannot watch child {pid}'s exit"))?;
    if aterm_uds::process::read_birth(pid) != Some(birth)
        || aterm_uds::process::read_process_parent(pid) != Some(parent)
    {
        return Err(format!(
            "child {pid} changed across exit-watch registration"
        ));
    }
    Ok(fd)
}

fn exited(fd: &std::os::fd::OwnedFd) -> Result<bool, String> {
    use std::os::fd::AsRawFd as _;
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live owned exit descriptor and one writable pollfd; no wait.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        return if e.kind() == std::io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(format!("poll child exit: {e}"))
        };
    }
    if p.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(format!("invalid child exit watch: {}", p.revents));
    }
    Ok(p.revents & (libc::POLLIN | libc::POLLHUP) != 0)
}

fn end_owned_child(child: &mut OwnedChild) -> Result<(), String> {
    let mut killed = false;
    for phase in 0..2 {
        let end = Instant::now() + DEADLINE;
        let mut exit = aterm_uds::deadline::ExitWait::new(child);
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("reap instance: {e}"))?
            {
                return if !killed && status.success() {
                    Ok(())
                } else {
                    Err(format!("instance did not finish a graceful quit: {status}"))
                };
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            exit.wait(left);
        }
        if phase == 0 {
            child
                .kill()
                .map_err(|e| format!("kill owned instance: {e}"))?;
            killed = true;
        }
    }
    Err("owned instance not reaped after SIGKILL".into())
}

pub struct World {
    pub tmp: PathBuf,
    pub broker: Option<Broker>,
    pub handle: Option<BrokerHandle>,
    pub broker_sock: String,
    pub broker_log: String,
    pub gui: Option<OwnedChild>,
    pub ctl_sock: String,
    pub token: String,
    pub node: String,
    pub state: PathBuf,
    pub gui_log: PathBuf,
    /// Broker open options ([`broker_opts`]).
    pub opts: OpenOptions,
    /// Whether this world's broker retains its log by aterm's default rules.
    pub retained: bool,
    watch: std::sync::Mutex<Option<Client>>,
    lifeline: std::sync::Mutex<Option<aterm_uds::lifeline::Lifeline>>,
    stopped: bool,
    scratch: Scratch,
}

impl Drop for World {
    fn drop(&mut self) {
        let ended = self.stop();
        self.watch
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        if ended.is_ok()
            && !std::thread::panicking()
            && !self.scratch.keep.load(std::sync::atomic::Ordering::Relaxed)
            && std::env::var_os("ATERM_LINK_KEEP").is_none()
            && let Some(scratch) = self.scratch.dir.take()
        {
            let path = scratch.keep();
            if let Err(e) = std::fs::remove_dir_all(&path) {
                eprintln!(
                    "KEPT {}: fabric scratch cleanup failed: {e}",
                    path.display()
                );
            }
        }
        if let Err(why) = ended {
            eprintln!(
                "KEPT {}: fabric process cleanup unknown: {why}",
                self.tmp.display()
            );
        }
    }
}

/// `kill(2)`, for the tests that need a real SIGKILL.
pub fn kill(pid: i32, sig: i32) {
    // SAFETY: `kill(2)` with a pid this test owns. No memory is touched.
    unsafe {
        libc_kill(pid, sig);
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[path = "../support/aterm_under_test.rs"]
mod aterm_under_test;

/// Returns the path to the `aterm` binary under test.
pub fn aterm() -> &'static str {
    static ATERM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ATERM.get_or_init(|| {
        aterm_under_test::aterm_executable()
            .into_os_string()
            .into_string()
            .expect("the aterm under test at a UTF-8 path")
    })
}

/// Returns [`aterm`] after warming its first-exec assessment with `--version`.
pub fn aterm_to_launch() -> &'static str {
    let aterm = aterm();
    aterm_spec::wait::first_exec(Path::new(aterm), &["--version"]);
    aterm
}

impl World {
    /// Cut this world's lifeline, reap its direct instance, and wait for its roots to exit.
    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        let mut failures = Vec::new();
        let mut watches = Vec::new();
        if let Some(gui) = &self.gui {
            let observed = (|| {
                let mut pids = Vec::new();
                let bridge = std::fs::read_to_string(self.state.join("pid"))
                    .map_err(|e| format!("read bridge pid: {e}"))?
                    .trim()
                    .parse::<i32>()
                    .map_err(|e| format!("parse bridge pid: {e}"))?;
                pids.push(bridge);
                let mut ctl = connect_within(&self.ctl_sock, &self.token, DEADLINE)
                    .map_err(|e| format!("connect for owned foreground roots: {e}"))?;
                let who = ctl
                    .request("who")
                    .map_err(|e| format!("read foreground roots: {e}"))?;
                if !who.ok() {
                    return Err(format!("foreground roots: {}", who.header()));
                }
                for row in who.rows() {
                    let pid = kv(row, "fgpgid")
                        .and_then(|p| p.parse::<i32>().ok())
                        .filter(|p| *p > 1)
                        .ok_or_else(|| format!("no foreground root in {row}"))?;
                    if !pids.contains(&pid) {
                        pids.push(pid);
                    }
                }
                for pid in pids {
                    watches.push((pid, child_exit(pid, gui.id())?));
                }
                Ok(())
            })();
            if let Err(why) = observed {
                failures.push(why);
            }
        } else {
            failures.push("no owned instance handle for child-exit observations".into());
        }
        self.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(gui) = &mut self.gui {
            match end_owned_child(gui) {
                Ok(()) => {
                    self.gui = None;
                }
                Err(why) => failures.push(why),
            }
        }
        let end = Instant::now() + DEADLINE;
        while !watches.is_empty() {
            let mut pending = Vec::new();
            for (pid, fd) in watches {
                match exited(&fd) {
                    Ok(true) => {}
                    Ok(false) => pending.push((pid, fd)),
                    Err(why) => failures.push(format!("child {pid}: {why}")),
                }
            }
            watches = pending;
            if watches.is_empty() {
                break;
            }
            if Instant::now() >= end {
                failures.push(format!(
                    "child exits unconfirmed: {:?}",
                    watches.iter().map(|(pid, _)| pid).collect::<Vec<_>>()
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if failures.is_empty() {
            self.stopped = true;
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// Boot everything.
    pub fn boot(tag: &str, accept_from: &[&str]) -> Self {
        Self::boot_with(tag, accept_from, &[])
    }

    /// [`World::boot`] with extra environment for the bridge child.
    pub fn boot_with(tag: &str, accept_from: &[&str], env: &[(&str, &str)]) -> Self {
        Self::boot_at(tag, accept_from, env, None)
    }

    /// [`World::boot_with`] with the bridge pointed at `broker_at`.
    pub fn boot_at(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
    ) -> Self {
        Self::boot_full(
            tag,
            accept_from,
            env,
            broker_at,
            &[],
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot`] with extra flags on the bridge's `serve` line (`--presence minimal`, say).
    pub fn boot_flags(tag: &str, extra_serve_flags: &[&str]) -> Self {
        Self::boot_full(
            tag,
            &[],
            &[],
            None,
            extra_serve_flags,
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot_flags`] and [`World::boot_with`] combined.
    pub fn boot_flags_with(tag: &str, extra_serve_flags: &[&str], env: &[(&str, &str)]) -> Self {
        Self::boot_full(
            tag,
            &[],
            env,
            None,
            extra_serve_flags,
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot`] with this world's broker opened with `opts`.
    pub fn boot_opts(tag: &str, opts: OpenOptions) -> Self {
        Self::boot_full(tag, &[], &[], None, &[], None, opts)
    }

    /// Boots a world with custom broker options, retention flag, serve flags, and environment.
    pub fn boot_arm(
        tag: &str,
        opts: OpenOptions,
        retained: bool,
        extra_serve_flags: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::boot_inner(tag, &[], env, None, extra_serve_flags, None, opts, retained)
    }

    /// [`World::boot`] with the node's cap file holding `grants(node)`.
    pub fn boot_with_grants(tag: &str, grants: GrantsFor<'_>) -> Self {
        Self::boot_full(
            tag,
            &[],
            &[],
            None,
            &[],
            Some(grants),
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// Boots a world whose broker keeps no retention rules.
    pub fn without_retention(tag: &str) -> Self {
        Self::boot_arm(
            tag,
            broker_opts(aterm_link::durability::Durability::Strict),
            false,
            &[],
            &[],
        )
    }

    fn boot_full(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
        extra_serve_flags: &[&str],
        grants: Option<GrantsFor<'_>>,
        opts: OpenOptions,
    ) -> Self {
        Self::boot_inner(
            tag,
            accept_from,
            env,
            broker_at,
            extra_serve_flags,
            grants,
            opts,
            true,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one private constructor every boot names"
    )]
    fn boot_inner(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
        extra_serve_flags: &[&str],
        grants: Option<GrantsFor<'_>>,
        opts: OpenOptions,
        retained: bool,
    ) -> Self {
        let _boot = boot_permit();
        let scratch = Scratch {
            dir: Some(
                aterm_tempfile::Builder::new()
                    .prefix("atl-")
                    .tempdir_in("/tmp")
                    .expect("exclusive world scratch"),
            ),
            keep: Default::default(),
        };
        let tmp = scratch
            .dir
            .as_ref()
            .expect("new scratch")
            .path()
            .to_path_buf();
        std::fs::create_dir_all(tmp.join("run/aterm")).expect("scratch runtime dir");
        prepare_fixture_config(&tmp);
        let state = tmp.join("link");
        std::fs::create_dir_all(&state).expect("bridge state dir");

        let node = format!("n-{:016x}", fnv(tag));
        std::fs::write(state.join("node"), format!("{node}\n")).expect("seed the node id");

        let broker_sock = broker_at.map_or_else(
            || tmp.join("b.sock").to_string_lossy().into_owned(),
            str::to_string,
        );
        let broker_log = tmp.join("b.log").to_string_lossy().into_owned();
        let ctl_sock = tmp
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        for p in [&broker_sock, &ctl_sock] {
            assert_sock_fits(p);
        }

        let (broker, handle) = if broker_at.is_some() {
            (None, None)
        } else {
            let broker = if retained {
                open_broker(&broker_log, &opts)
            } else {
                open_broker_unretained(&broker_log, &opts)
            };
            let handle = broker.serve(&broker_sock).expect("serve the broker");
            (Some(broker), Some(handle))
        };

        let cap_path = tmp.join("node.cap");
        let grants = grants.map_or_else(|| node_grants(&node), |f| f(&node));
        let caps: String = grants.iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_path, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} link serve --fleet {FLEET} --broker {broker_sock} --cap-file {} --state {}",
            aterm_to_launch(),
            cap_path.display(),
            state.display()
        );
        if !accept_from.is_empty() {
            fabric_cmd.push_str(&format!(" --accept-from {}", accept_from.join(",")));
        }
        for flag in extra_serve_flags {
            fabric_cmd.push(' ');
            fabric_cmd.push_str(flag);
        }

        std::fs::write(tmp.join("fabric.cmd"), &fabric_cmd).expect("record the fabric command");
        let gui_log = tmp.join("gui.log");
        let out = std::fs::File::create(&gui_log).expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        scratch_spawnfd(&mut cmd, &tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&tmp))
            .env("TMPDIR", &tmp)
            .env("XDG_RUNTIME_DIR", tmp.join("run"))
            .env("XDG_CONFIG_HOME", tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", &fabric_cmd)
            .stdin(OwnedStdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &tmp).expect("the world's lifeline");

        let mut world = World {
            tmp,
            broker,
            handle,
            broker_sock,
            broker_log,
            gui: None,
            ctl_sock,
            token: String::new(),
            node,
            state,
            gui_log,
            opts,
            retained,
            watch: std::sync::Mutex::new(None),
            lifeline: std::sync::Mutex::new(Some(lifeline)),
            stopped: false,
            scratch,
        };
        world.gui = Some(cmd.spawn().expect("launch aterm --headless"));
        let gui = world.gui.as_mut().expect("the gui just spawned");
        world.token = World::wait_for_owned_token_of(&world.ctl_sock, gui, &world.gui_log);
        world
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    /// Run one verb and return the reply.
    pub fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    /// Connects a fleet-root broker client that can publish under any producer id.
    pub fn god(&self) -> Client {
        let mut c = broker_client(&self.broker_sock);
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
    }

    /// Runs one broker read on this world's shared read connection.
    pub fn read<T>(&self, f: impl FnMut(&mut Client) -> std::io::Result<T>) -> std::io::Result<T> {
        read_on(
            &self.watch,
            || {
                let s = std::os::unix::net::UnixStream::connect(&self.broker_sock)?;
                s.set_read_timeout(Some(VERB_DEADLINE))?;
                s.set_write_timeout(Some(VERB_DEADLINE))?;
                let mut c = Client::from_stream(s);
                let cap = read_cap();
                c.attach(&cap.filter, &cap.tag)?;
                Ok(c)
            },
            f,
        )
    }

    /// Reads the retained row body at `subject` over [`World::read`].
    pub fn row(&self, subject: &str) -> Option<String> {
        let page = self.read(|c| c.last(subject, "", 8)).ok()?;
        exact_body(page, subject)
    }

    /// Wait until the bridge is attached and its presence row says `live`.
    pub fn wait_ready(&self) {
        until("aterm to report fabric=connected", || {
            self.verb("status")
                .header()
                .contains("fabric=connected")
                .then_some(())
        });
        let subject = format!("/f/{FLEET}/pub/{}/node/presence", self.node);
        until("the node's presence row to say live", || {
            self.row(&subject)
                .filter(|b| b.contains("state=live"))
                .map(|_| ())
        });
    }

    /// TAKE THE BROKER AWAY, and wait until the bridge has noticed.
    pub fn stop_broker(&mut self) {
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        let _ = std::fs::remove_file(&self.broker_sock);
        until("the bridge to notice the broker gone and redial it", || {
            let fs = self.verb("fabric status").header().to_string();
            (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("no-socket"))
                .then_some(())
        });
    }

    /// BRING THE BROKER BACK on the same log and socket path, and wait for the bridge to reattach.
    pub fn start_broker(&mut self) {
        let _ = std::fs::remove_file(&self.broker_sock);
        let broker = if self.retained {
            open_broker(&self.broker_log, &self.opts)
        } else {
            open_broker_unretained(&self.broker_log, &self.opts)
        };
        let handle = broker
            .serve(&self.broker_sock)
            .expect("serve the broker again");
        self.broker = Some(broker);
        self.handle = Some(handle);
        until("the bridge to reattach to the restarted broker", || {
            self.verb("status")
                .header()
                .contains(" fabric=connected ")
                .then_some(())
        });
    }

    /// `SIGKILL` the bridge, and wait for aterm's supervisor to bring a replacement up, attached.
    pub fn restart_bridge(&self) {
        let pid = self.bridge_pid();
        kill(pid, 9);
        until("the bridge to come back under a new pid", || {
            let back: i32 = std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            (back != pid).then_some(())
        });
        until("the replacement bridge to attach", || {
            self.verb("status")
                .header()
                .contains(" fabric=connected ")
                .then_some(())
        });
    }

    /// Posts a drain-barrier note from `sid` to itself and waits for it to land.
    pub fn drained(&self, sid: &str) {
        let r = self.verb(&format!(
            "@{sid} post to=@{sid} kind=note --wait=60000 drain-barrier"
        ));
        assert!(
            r.ok(),
            "the drain barrier: {} — log:\n{}",
            r.header(),
            self.log_tail()
        );
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        session_rows(&self.verb("sessions"))
    }

    /// The boot session's sid, and one more spawned beside it.
    pub fn two_sessions(&self) -> (String, String) {
        let first = until("aterm's boot session", || {
            self.sessions().first().map(|(_, sid, _)| sid.clone())
        });
        let reply = self.verb("spawn");
        assert!(reply.ok(), "spawn: {}", reply.header());
        let second = reply
            .header()
            .split_whitespace()
            .nth(1)
            .expect("spawn answers OK <sid>")
            .to_string();
        until("the bridge to see both sessions", || {
            (self.sessions().len() >= 2).then_some(())
        });
        (first, second)
    }

    /// Spawns a second headless instance in this world's socket dir without a fabric command.
    pub fn sibling_instance(&mut self) -> Sibling {
        self.sibling_with(None)
    }

    /// Spawns a [`World::sibling_instance`] armed with this world's fabric command.
    pub fn sibling_instance_armed(&mut self) -> Sibling {
        let fabric = std::fs::read_to_string(self.tmp.join("fabric.cmd"))
            .expect("this world's fabric command");
        self.sibling_with(Some(&fabric))
    }

    fn sibling_with(&mut self, fabric: Option<&str>) -> Sibling {
        self.scratch
            .keep
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let own = self.gui.as_ref().expect("this world's gui").id();
        self.ctl_sock = self
            .tmp
            .join(format!("run/aterm/aterm-{own}.sock"))
            .to_string_lossy()
            .into_owned();
        self.token = World::wait_for_token_at(&self.ctl_sock);
        let log = std::fs::File::create(self.tmp.join("sibling.log")).expect("sibling log");
        let err = log.try_clone().expect("sibling log clone");
        let mut cmd = Command::new(aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", self.tmp.join("run"))
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        if let Some(fabric) = fabric {
            cmd.env("ATERM_FABRIC_COMMAND", fabric);
        }
        arm_lifeline(&mut cmd, &self.tmp);
        let mut child = cmd.spawn().expect("launch a sibling aterm --headless");
        let sock = self
            .tmp
            .join(format!("run/aterm/aterm-{}.sock", child.id()))
            .to_string_lossy()
            .into_owned();
        let token = World::wait_for_token_of(&sock, &mut child, &self.tmp.join("sibling.log"));
        Sibling { child, sock, token }
    }

    /// Spawns a successor headless instance in `run-<n>` armed with this world's fabric command.
    pub fn successor_instance(&self, n: usize, env: &[(&str, &str)]) -> Successor {
        self.scratch
            .keep
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let fabric = std::fs::read_to_string(self.tmp.join("fabric.cmd"))
            .expect("this world's fabric command");
        let run = self.tmp.join(format!("run-{n}"));
        std::fs::create_dir_all(run.join("aterm")).expect("the successor's runtime dir");
        let sock = run.join("aterm/aterm.sock").to_string_lossy().into_owned();
        assert_sock_fits(&sock);
        let log_path = self.tmp.join(format!("successor-{n}.log"));
        let log = std::fs::File::create(&log_path).expect("successor log");
        let err = log.try_clone().expect("successor log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        scratch_spawnfd(&mut cmd, &self.tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", &run)
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", fabric.trim())
            .stdin(OwnedStdio::null())
            .stdout(log)
            .stderr(err);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &self.tmp).expect("successor lifeline");
        let mut successor = Successor {
            child: None,
            lifeline: Some(lifeline),
            sock,
            token: String::new(),
            log: log_path,
        };
        successor.child = Some(cmd.spawn().expect("launch a successor aterm --headless"));
        successor.token = World::wait_for_owned_token_of(
            &successor.sock,
            successor.child.as_mut().expect("spawned successor"),
            &successor.log,
        );
        successor
    }

    /// This session's `inbox` rows (`--peek`, so the watermarks do not move).
    pub fn inbox(&self, sid: &str) -> Vec<String> {
        inbox_rows(&self.verb(&format!("@{sid} inbox --peek")))
    }

    /// Posts from `from` to `to` with `--wait=60000` and returns `(header, offset)`.
    pub fn post_landed(
        &self,
        from: &str,
        to: &str,
        kind: &str,
        flags: &str,
        text: &str,
    ) -> (String, u64) {
        let reply = self.verb(&format!(
            "@{from} post to=@{to} kind={kind} {flags} --wait=60000 {text}"
        ));
        let header = reply.header().to_string();
        assert!(
            reply.ok(),
            "@{from}'s post to @{to}: {header} — log:\n{}",
            self.log_tail()
        );
        let off = off_of(&header);
        (header, off)
    }

    /// The bridge child's pid, from its state dir.
    pub fn bridge_pid(&self) -> i32 {
        until("the bridge to write its pid", || {
            std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
    }

    /// Every `ev` record the node has published, newest last — over EVERY `ev` face it owns.
    pub fn ev(&self) -> Vec<String> {
        self.ev_on(&format!("/f/{FLEET}/pub/{}/>", self.node))
    }

    /// Returns every `ev` record under `filter`, newest last.
    pub fn ev_on(&self, filter: &str) -> Vec<String> {
        self.read(|c| ev_rows(c, filter))
            .unwrap_or_else(|e| panic!("read the ev records under {filter}: {e}"))
    }

    /// The tail of aterm's own log, for a failure message.
    pub fn log_tail(&self) -> String {
        log_tail_of(&self.gui_log)
    }
}

fn read_cap() -> astream_cap::Capability {
    astream_cap::mint(SECRET, &format!("ro:/f/{FLEET}/>")).expect("mint the read cap")
}

fn read_on<S: std::io::Read + std::io::Write, T>(
    slot: &std::sync::Mutex<Option<astream_broker::Client<S>>>,
    open: impl FnOnce() -> std::io::Result<astream_broker::Client<S>>,
    mut f: impl FnMut(&mut astream_broker::Client<S>) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut held = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = held.as_mut() {
        if let Ok(v) = f(c) {
            return Ok(v);
        }
        *held = None;
    }
    let mut c = open()?;
    let v = f(&mut c)?;
    *held = Some(c);
    Ok(v)
}

fn exact_body(page: astream_broker::Page, subject: &str) -> Option<String> {
    page.0
        .into_iter()
        .find(|(_, s, _)| s == subject)
        .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
}

fn ev_rows<S: std::io::Read + std::io::Write>(
    conn: &mut astream_broker::Client<S>,
    filter: &str,
) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = conn.fetch(from, filter, 256)?;
        for (_, subject, body) in rows {
            if !subject.ends_with("/ev") {
                continue;
            }
            let (b, _) = aterm_link::body::Body::decode(&body);
            if let Some(ev) = b.unknown.get("ev") {
                out.push(aterm_link::pct::decode(ev));
            }
        }
        if next >= head || next <= from {
            return Ok(out);
        }
        from = next;
    }
}

fn session_rows(reply: &Reply) -> Vec<(u64, String, String)> {
    reply
        .rows()
        .iter()
        .filter_map(|row| {
            let f: Vec<&str> = row.split_whitespace().collect();
            let local = f.first()?.parse().ok()?;
            let sid = (*f.get(1)?).to_string();
            let nonce = f.iter().find_map(|t| t.strip_prefix("nonce="))?.to_string();
            Some((local, sid, nonce))
        })
        .collect()
}

fn inbox_rows(reply: &Reply) -> Vec<String> {
    reply
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg "))
        .cloned()
        .collect()
}

fn log_tail_of(path: &Path) -> String {
    let body = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// `key=` of a whitespace-token line.
pub use aterm_link::fabric::kv;

/// The offset an `OK <id> off=<n> …` header names.
pub fn off_of(header: &str) -> u64 {
    kv(header, "off")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a landing offset in: {header}"))
}

/// Returns the row id of `sid`'s `inbox` row at broker offset `off`.
pub fn id_at_off(w: &World, sid: &str, off: u64) -> Option<u64> {
    w.inbox(sid).into_iter().find_map(|r| {
        (kv(&r, "off") == Some(&off.to_string()))
            .then(|| r.split_whitespace().nth(1)?.parse().ok())
            .flatten()
    })
}

/// One record for [`publish_pipelined`]: `(producer_id, producer_seq, subject, body)`.
pub type Record = (u64, u64, String, Vec<u8>);

/// Publishes `records` pipelined on `conn` and returns each record's `(offset, deduped)`.
pub fn publish_pipelined(conn: &mut Client, records: &[Record]) -> Vec<(u64, bool)> {
    const WINDOW: usize = 128;
    let mut acks = Vec::with_capacity(records.len());
    let mut sent = 0;
    while acks.len() < records.len() {
        while sent < records.len() && sent - acks.len() < WINDOW {
            let (producer, seq, subject, body) = &records[sent];
            conn.send_publish(*producer, *seq, subject, body)
                .unwrap_or_else(|e| panic!("publish record {sent} on {subject}: {e}"));
            sent += 1;
        }
        let i = acks.len();
        let ack = conn
            .recv_publish_ack()
            .unwrap_or_else(|e| panic!("publish record {i} on {}: {e}", records[i].2));
        acks.push(ack);
    }
    acks
}

/// Every record currently on the fleet's broadcast subtree.
pub fn say_records(w: &World) -> Vec<(u64, String, String)> {
    let filter = format!("/f/{FLEET}/pub/*/*/say/>");
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, &filter, 256)?;
            for (off, subject, body) in rows {
                out.push((off, subject, String::from_utf8_lossy(&body).into_owned()));
            }
            if next >= head {
                return Ok(out);
            }
            from = next.max(from + 1);
        }
    })
    .expect("fetch the say subtree")
}

/// The rows of a session's ring that arrived on `topic`.
pub fn topic_rows(w: &World, sid: &str, topic: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(&format!(" topic={topic} ")))
        .collect()
}

/// `Some` once the BRIDGE holds `topic` for `sid`: its persisted cursors, one topic per line.
pub fn bridge_holds(w: &World, sid: &str, topic: &str) -> Option<()> {
    std::fs::read_to_string(w.state.join(format!("topics/{sid}")))
        .ok()?
        .lines()
        .any(|l| l.split(' ').next() == Some(topic))
        .then_some(())
}

/// Returns the `watchers=` count reported by `who` for `sid`.
pub fn watchers(w: &World, sid: &str) -> Option<u64> {
    let reply = w.verb("who");
    if !reply.ok() {
        return None;
    }
    reply
        .rows()
        .iter()
        .find(|row| row.split_whitespace().nth(1) == Some(sid))?
        .split_whitespace()
        .find_map(|t| t.strip_prefix("watchers="))?
        .parse()
        .ok()
}

/// Debug-only push holds armed by `$ATERM_TEST_PUSH_HOLD`.
pub struct Hold(aterm_tempfile::TempDir);

impl Hold {
    /// An exclusively created hold directory for the test `tag`.
    pub fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atl-{tag}-hold-"))
            .tempdir()
            .expect("exclusive hold directory");
        Hold(dir)
    }

    /// The value of `$ATERM_TEST_PUSH_HOLD` that arms it in aterm.
    pub fn env(&self) -> String {
        self.0.path().to_string_lossy().into_owned()
    }

    /// Hold `step`.
    pub fn on(&self, step: &str) {
        std::fs::write(self.0.path().join(step), b"")
            .unwrap_or_else(|e| panic!("hold {step}: {e}"));
    }

    /// Release `step`.
    pub fn off(&self, step: &str) {
        std::fs::remove_file(self.0.path().join(step))
            .unwrap_or_else(|e| panic!("release {step}: {e}"));
    }
}

/// The session's presence row on the bus, as its body.
pub fn presence_body(w: &World, sid: &str) -> Option<String> {
    w.row(&format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node))
}

/// A file's permission bits.
pub fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

#[must_use]
pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 is the liveness probe; it delivers nothing and touches no memory.
    unsafe { libc_kill(pid, 0) == 0 }
}

impl World {
    /// Relaunches `aterm` against the same scratch world.
    pub fn relaunch_gui(w: &World) -> OwnedChild {
        let fabric_cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd"))
            .expect("the boot recorded its fabric command");
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&w.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        scratch_spawnfd(&mut cmd, &w.tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&w.tmp))
            .env("TMPDIR", &w.tmp)
            .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
            .env("XDG_CONFIG_HOME", w.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", fabric_cmd.trim())
            .stdin(OwnedStdio::null())
            .stdout(out)
            .stderr(err);
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &w.tmp).expect("relaunch lifeline");
        *w.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lifeline);
        cmd.spawn().expect("relaunch aterm --headless")
    }

    /// The instance token for a control socket that is coming up.
    pub fn wait_for_token_at(sock: &str) -> String {
        Self::wait_for_token(sock, || {})
    }

    /// Waits for `child` to bind `sock` and return its token, failing early if `child` exits.
    pub fn wait_for_token_of(sock: &str, child: &mut Child, log: &Path) -> String {
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::wait_for_token(sock, || {
                if let Ok(Some(exit)) = child.try_wait() {
                    panic!(
                        "aterm exited ({exit}) before it answered on {sock}; its log:\n{}",
                        log_tail_of(log)
                    );
                }
            })
        }));
        waited.unwrap_or_else(|why| {
            let held = held_at_deadline(child.id());
            kill_reaped(child);
            if let Some(words) = held {
                aterm_uds::host::could_not_run(words);
            }
            std::panic::resume_unwind(why)
        })
    }

    pub fn wait_for_owned_token_of(sock: &str, child: &mut OwnedChild, log: &Path) -> String {
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::wait_for_token(sock, || {
                if let Ok(Some(exit)) = child.try_wait() {
                    panic!(
                        "aterm exited ({exit}) before it answered on {sock}; its log:\n{}",
                        log_tail_of(log)
                    );
                }
            })
        }));
        waited.unwrap_or_else(|why| {
            let held = held_at_deadline(child.id());
            let _ = child.kill();
            if let Err(cleanup) = end_owned_child(child) {
                eprintln!("instance readiness cleanup: {cleanup}");
            }
            if let Some(words) = held {
                aterm_uds::host::could_not_run(words);
            }
            std::panic::resume_unwind(why)
        })
    }

    fn wait_for_token(sock: &str, mut check_child: impl FnMut()) -> String {
        until("aterm to bind and answer on its control socket", || {
            check_child();
            let path = aterm_uds::latest::token_path_for_sock(sock)?;
            let token = std::fs::read_to_string(path).ok()?;
            let token = token.trim().to_string();
            if token.is_empty() {
                return None;
            }
            let mut ctl = connect_within(sock, &token, DEADLINE).ok()?;
            ctl.request("version").ok()?.ok().then_some(token)
        })
    }
}

/// Second instance a [`World`] hosts beside its own ([`World::sibling_instance`]).
pub struct Sibling {
    child: Child,
    /// Its per-pid control socket.
    pub sock: String,
    /// Its Owner token.
    pub token: String,
}

impl Drop for Sibling {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Successor instance ([`World::successor_instance`]), reaped on drop unless taken.
pub struct Successor {
    child: Option<OwnedChild>,
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    /// Its control socket, in its own runtime dir.
    pub sock: String,
    /// Its Owner token.
    pub token: String,
    /// Where its stdout and stderr go (its bridge's too).
    pub log: PathBuf,
}

impl Successor {
    /// A fresh authenticated control connection to it.
    pub fn ctl(&self) -> Ctl {
        connect(&self.sock, &self.token).expect("connect to the successor aterm")
    }

    /// Every session it hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        let reply = self
            .ctl()
            .request("sessions")
            .unwrap_or_else(|e| panic!("sessions on the successor: {e}"));
        session_rows(&reply)
    }

    /// Its `fabric status` header, or empty when it does not answer.
    pub fn fabric_status(&self) -> String {
        connect(&self.sock, &self.token)
            .and_then(|mut c| c.request("fabric status"))
            .map(|r| r.header().to_string())
            .unwrap_or_default()
    }

    /// Installs this successor as `w`'s own instance.
    pub fn take(mut self, w: &mut World) {
        w.gui = self.child.take();
        *w.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.lifeline.take();
        w.stopped = false;
        w.ctl_sock = std::mem::take(&mut self.sock);
        w.token = std::mem::take(&mut self.token);
        w.gui_log = std::mem::take(&mut self.log);
    }
}

impl Drop for Successor {
    fn drop(&mut self) {
        self.lifeline.take();
        if let Some(mut child) = self.child.take()
            && let Err(why) = end_owned_child(&mut child)
        {
            eprintln!("successor cleanup unknown: {why}");
        }
    }
}

fn scratch_home(tmp: &std::path::Path) -> std::path::PathBuf {
    let home = tmp.join("home");
    let _ = std::fs::create_dir_all(&home);
    home
}

/// Configures `cmd` with the scratch directories of `tmp` and strips inherited `ATERM_*` variables.
pub fn scratch_cli<'a>(cmd: &'a mut Command, tmp: &Path) -> &'a mut Command {
    prepare_gui_environment(cmd);
    cmd.envs(scratch_cli_env(tmp))
}

/// [`scratch_cli`] for a child built with `aterm_uds::spawnfd::command`.
pub fn scratch_spawnfd<'a>(
    cmd: &'a mut aterm_uds::spawnfd::Command,
    tmp: &Path,
) -> &'a mut aterm_uds::spawnfd::Command {
    for (name, _) in std::env::vars_os() {
        if stripped(&name) {
            cmd.env_remove(&name);
        }
    }
    cmd.env(STALE_HORIZON.0, STALE_HORIZON.1.as_millis().to_string());
    cmd.envs(scratch_cli_env(tmp))
}

fn stripped(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with("ATERM_") && name != aterm_uds::process::VERIFY_RUN_VAR
}

fn scratch_cli_env(tmp: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("HOME", scratch_home(tmp)),
        ("XDG_RUNTIME_DIR", tmp.join("run")),
        ("XDG_CONFIG_HOME", tmp.join("cfg")),
    ]
}

/// Multi-node test harness with one shared guarded broker in a scratch directory.
pub struct Fleet {
    pub tmp: PathBuf,
    broker: Option<Broker>,
    handle: Option<BrokerHandle>,
    /// The broker's socket path (`<tmp>/b.sock`).
    pub sock: String,
    next: u32,
    watch: std::sync::Mutex<Option<aterm_link::transport::Conn>>,
    scratch: Option<aterm_tempfile::TempDir>,
}

impl Drop for Fleet {
    fn drop(&mut self) {
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        if std::env::var_os("ATERM_LINK_KEEP").is_some()
            && let Some(scratch) = self.scratch.take()
        {
            let _ = scratch.keep();
        }
    }
}

impl Fleet {
    /// Boot the broker under exclusive scratch ownership.
    pub fn boot(tag: &str) -> Self {
        Self::boot_opts(tag, broker_opts(aterm_link::durability::Durability::Strict))
    }

    /// [`Fleet::boot`] with the broker opened with `opts`.
    pub fn boot_opts(_tag: &str, opts: OpenOptions) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("a5-")
            .tempdir_in("/tmp")
            .expect("exclusive fleet scratch");
        let tmp = scratch.path().to_path_buf();
        let broker = open_broker(tmp.join("b.log"), &opts);
        let sock = tmp.join("b.sock").to_string_lossy().into_owned();
        assert_sock_fits(&sock);
        let handle = broker.serve(&sock).expect("serve the broker's socket");
        Self {
            tmp,
            broker: Some(broker),
            handle: Some(handle),
            sock,
            next: 0,
            watch: std::sync::Mutex::new(None),
            scratch: Some(scratch),
        }
    }

    #[must_use]
    pub fn broker(&self) -> &Broker {
        self.broker.as_ref().expect("the fleet serves its broker")
    }

    /// A god-cap client on the socket the nodes use.
    pub fn god(&self) -> aterm_link::transport::Conn {
        let (mut c, _closer) =
            aterm_link::transport::connect(&self.sock, DEADLINE).expect("connect to the broker");
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
    }

    /// Runs one broker read on this fleet's shared read connection.
    pub fn read<T>(
        &self,
        f: impl FnMut(&mut aterm_link::transport::Conn) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        read_on(
            &self.watch,
            || {
                let (mut c, _closer) = aterm_link::transport::connect(&self.sock, DEADLINE)?;
                let cap = read_cap();
                c.attach(&cap.filter, &cap.tag)?;
                Ok(c)
            },
            f,
        )
    }

    /// [`World::row`] over [`Fleet::read`]'s connection.
    pub fn row(&self, subject: &str) -> Option<String> {
        let page = self.read(|c| c.last(subject, "", 8)).ok()?;
        exact_body(page, subject)
    }

    /// Writes a capability file holding `grants` in this fleet's scratch directory.
    pub fn cap_file(&self, name: &str, grants: &[String]) -> PathBuf {
        let path = self.tmp.join(format!("{name}.cap"));
        let caps: String = grants.iter().map(|g| cap_line(g)).collect();
        std::fs::write(&path, caps).expect("write a cap file");
        path
    }

    /// Boots a headless `aterm` node and its `aterm link serve` bridge against this fleet's broker.
    pub fn node(&mut self, tag: &str, accept_from: &[&str]) -> Node {
        self.node_with(tag, accept_from, &[], &[])
    }

    /// [`Fleet::node`] with extra `aterm link serve` flags and environment variables.
    pub fn node_with(
        &mut self,
        tag: &str,
        accept_from: &[&str],
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Node {
        self.next += 1;
        Node::boot(self, tag, accept_from, args, env, self.next)
    }

    /// Run `aterm link ls` against this fleet and answer its stdout.
    pub fn ls(&self) -> String {
        let cap = self.cap_file("ls", &[format!("ro:/f/{FLEET}/pub/>")]);
        let mut cmd = Command::new(aterm_to_launch());
        scratch_cli(&mut cmd, &self.tmp)
            .args(["link", "ls", "--fleet", FLEET, "--broker", &self.sock])
            .args(["--cap-file", cap.to_string_lossy().as_ref()]);
        let out = output_within(&mut cmd, "aterm link ls");
        assert!(
            out.status.success(),
            "aterm link ls failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// One aterm instance on a [`Fleet`], with its own bridge child.
pub struct Node {
    pub tmp: PathBuf,
    pub ctl_sock: String,
    pub token: String,
    pub node: String,
    pub state: PathBuf,
    pub gui_log: PathBuf,
    pub cap_file: PathBuf,
    gui: Option<Child>,
    fabric_cmd: String,
    extra_env: Vec<(String, String)>,
}

impl Drop for Node {
    fn drop(&mut self) {
        let launcher = self
            .gui
            .as_ref()
            .map(|gui| launcher_of(gui.id(), &self.state))
            .unwrap_or_default();
        if let Some(mut gui) = self.gui.take() {
            let _ = gui.kill();
            let _ = gui.wait();
        }
        end_bridge(&self.state, launcher);
    }
}

impl Node {
    fn boot(
        fleet: &Fleet,
        tag: &str,
        accept_from: &[&str],
        args: &[&str],
        env: &[(&str, &str)],
        ordinal: u32,
    ) -> Self {
        let _boot = boot_permit();
        let tmp = fleet.tmp.join(format!("n{ordinal}"));
        std::fs::create_dir_all(tmp.join("run/aterm")).expect("scratch runtime dir");
        prepare_fixture_config(&tmp);
        let state = tmp.join("link");
        std::fs::create_dir_all(&state).expect("bridge state dir");

        let node = format!("n-{:016x}", fnv(tag));
        std::fs::write(state.join("node"), format!("{node}\n")).expect("seed the node id");

        let ctl_sock = tmp
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        assert_sock_fits(&ctl_sock);

        let cap_file = tmp.join("node.cap");
        let caps: String = node_grants(&node).iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_file, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} link serve --fleet {FLEET} --broker {} --cap-file {} --state {}",
            aterm_to_launch(),
            fleet.sock,
            cap_file.display(),
            state.display()
        );
        if !accept_from.is_empty() {
            fabric_cmd.push_str(&format!(" --accept-from {}", accept_from.join(",")));
        }
        for arg in args {
            fabric_cmd.push(' ');
            fabric_cmd.push_str(arg);
        }
        std::fs::write(tmp.join("fabric.cmd"), &fabric_cmd).expect("record the fabric command");

        let gui_log = tmp.join("gui.log");
        let mut me = Self {
            tmp,
            ctl_sock,
            token: String::new(),
            node,
            state,
            gui_log,
            cap_file,
            gui: None,
            fabric_cmd,
            extra_env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        };
        let gui = me.gui.insert(me.spawn_gui());
        me.token = World::wait_for_token_of(&me.ctl_sock, gui, &me.gui_log);
        me
    }

    fn spawn_gui(&self) -> Child {
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = Command::new(aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", self.tmp.join("run"))
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", &self.fabric_cmd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        arm_lifeline(&mut cmd, &self.tmp);
        cmd.spawn().expect("launch aterm --headless")
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    /// Run one verb and return the reply.
    pub fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    /// Wait until the bridge is attached and its presence row says `live`.
    pub fn wait_ready(&self, fleet: &Fleet) {
        until("aterm to report fabric=connected", || {
            self.verb("status")
                .header()
                .contains("fabric=connected")
                .then_some(())
        });
        let subject = format!("/f/{FLEET}/pub/{}/node/presence", self.node);
        until("the node's presence row to say live", || {
            fleet
                .row(&subject)
                .filter(|b| b.contains("state=live"))
                .map(|_| ())
        });
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        session_rows(&self.verb("sessions"))
    }

    /// The boot session's `(sid, nonce)`, once the bridge has seen it.
    pub fn session(&self) -> (String, String) {
        until("aterm's boot session", || {
            self.sessions()
                .first()
                .map(|(_, sid, nonce)| (sid.clone(), nonce.clone()))
        })
    }

    /// This session's `inbox` rows (`--peek`, so the watermarks do not move).
    pub fn inbox(&self, sid: &str) -> Vec<String> {
        inbox_rows(&self.verb(&format!("@{sid} inbox --peek")))
    }

    /// Every `ev` record this node has published, newest last — over EVERY `ev` face it owns.
    pub fn ev(&self, fleet: &Fleet) -> Vec<String> {
        let filter = format!("/f/{FLEET}/pub/{}/>", self.node);
        fleet
            .read(|c| ev_rows(c, &filter))
            .unwrap_or_else(|e| panic!("read the ev records under {filter}: {e}"))
    }

    /// The tail of this node's aterm log, for a failure message.
    pub fn log_tail(&self) -> String {
        log_tail_of(&self.gui_log)
    }

    /// The bridge child's pid, from its state dir.
    pub fn bridge_pid(&self) -> i32 {
        until("the bridge to write its pid", || {
            std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
    }

    /// Quits this node's `aterm` via `invoke Quit` and waits for it and its bridge to exit.
    pub fn quit(&mut self) {
        let bridge = self.bridge_pid();
        let _ = self.ctl().request("invoke Quit");
        assert!(self.gui.is_some(), "this node's aterm is running");
        until("aterm to quit", || {
            self.gui.as_mut()?.try_wait().ok().flatten()
        });
        self.gui = None;
        until("its bridge to exit after it", || {
            (!alive(bridge)).then_some(())
        });
        let _ = std::fs::remove_file(self.state.join("pid"));
    }
}

/// A WAIT THAT RUNS OUT ON A HELD CHILD ENDS COULD NOT RUN, AND ANY OTHER SILENCE FAILS: a channel
/// nothing feeds, judged at 50 ms (the timeout branch under test, not a budget) with a reader that
/// answers for the child's pid alone, ends COULD NOT RUN naming that pid when the reader says it is
/// held, and fails naming what never came when it says not held or cannot tell; an item that came
/// is returned.
#[test]
fn a_wait_that_runs_out_on_a_held_child_ends_could_not_run_and_any_other_silence_fails() {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let pid = 4242;
    let hang = Duration::from_millis(50);
    let ended = |held: Option<bool>| {
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recv_judged(
                &rx,
                Instant::now(),
                hang,
                pid,
                |p| held.filter(|_| p == pid),
                || "the child's readiness line".to_owned(),
            )
        }));
        let why = out.expect_err("nothing came");
        why.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let held = ended(Some(true));
    assert!(
        held.starts_with(aterm_uds::host::COULD_NOT_RUN) && held.contains("(pid 4242)"),
        "expected COULD NOT RUN, got: {held}"
    );
    for not in [Some(false), None] {
        let failed = ended(not);
        assert!(
            failed.starts_with("the child's readiness line"),
            "{not:?}: {failed}"
        );
    }
    tx.send("listening".into()).expect("the channel is open");
    assert_eq!(
        recv_judged(&rx, Instant::now(), hang, pid, |_| Some(true), String::new),
        "listening"
    );
}

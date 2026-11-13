// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated private `aterm` instance runner for `lab instance` and lab proof lanes.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::band::{Band, Job};
use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

#[path = "instance_cleanup.rs"]
mod cleanup;
pub(crate) use cleanup::after_stop;

#[path = "instance_compare.rs"]
mod comparison;

#[path = "instance_handoff.rs"]
mod handoff;

#[cfg(any(test, debug_assertions))]
#[path = "instance_network_away.rs"]
mod network_away;

/// The automatic lanes off.
pub const CONFIG_OFF: &str = "agents_auto_prime = false\n\
                              [update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [harness]\nenabled = false\n\
                              [machine]\nuniversal_control = \"leave\"\n";

/// `config` with the audible bell off (`bell_sound = false` at its head).
#[must_use]
pub fn bell_off(config: &str) -> String {
    match aterm_toml::from_str::<aterm_toml::Value>(config) {
        Ok(doc) if doc.get("bell_sound").is_none() => format!("bell_sound = false\n{config}"),
        _ => config.to_owned(),
    }
}

/// Write a world's `aterm.toml` at `path`, private enough for its reader: every reader of
/// that file refuses a group/world-writable one (`aterm_types::dirs::aterm_config_refusal`),
/// and a plain write takes the host umask's mode — 664 on umask-002 hosts (the Lima guest,
/// most Linux desktops) — so the instance would silently run on defaults, not this text.
/// # Errors
/// The write or the mode fix failed.
pub(crate) fn write_aterm_toml(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut f = opts.open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    // The creating mode is umask-masked and a pre-existing file keeps its own: pin it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.set_len(0)?;
    f.write_all(text.as_bytes())
}

/// One-minute hang detector for startup: a version probe or an instance listening.
pub const BOOT: Duration = Duration::from_secs(60);

/// What a stop gives an instance once it has cut its lifeline: a time to end by itself before the
/// stop signals its group `SIGTERM` (a launchd job's: before it removes the job), then, for this
/// run's child, a time to end on that signal before `SIGKILL`; each within the stop's one-minute
/// deadline ([`BOOT`]).
#[derive(Clone, Copy, Debug)]
struct Graces {
    /// From the lifeline's cut to `SIGTERM` (or the job's removal).
    own: Duration,
    /// From `SIGTERM` to `SIGKILL`.
    term: Duration,
}

/// A lab instance's graces: 8 s to end by itself, 5 s more once signalled.
const STOP_GRACES: Graces = Graces {
    own: Duration::from_secs(8),
    term: Duration::from_secs(5),
};

/// The scratch world an instance runs in.
pub struct World {
    /// The private directory holding everything below.
    pub dir: PathBuf,
    pub home: PathBuf,
    pub cfg: PathBuf,
    /// `XDG_RUNTIME_DIR`: the control socket's directory.
    pub run: PathBuf,
    pub sock: PathBuf,
    keep: bool,
    cleanup_failed: Arc<AtomicBool>,
    #[cfg(debug_assertions)]
    source_fixture_owned: bool,
}

impl World {
    /// Join only the constructor-owned Source proof namespace. The fixture
    /// retains cleanup custody until the host and SDK child have really exited.
    #[cfg(debug_assertions)]
    pub(crate) fn source_proof(
        fixture: &aterm_agent::harness::codex_enrollment::NativeSourceProofWorld,
    ) -> Result<Self, String> {
        let mut world = Self::in_dir(fixture.directory().to_path_buf(), true);
        world.source_fixture_owned = true;
        if fixture.env().home.as_ref() != Some(&world.home)
            || fixture.env().config.as_ref() != Some(&world.cfg.join("aterm/aterm.toml"))
        {
            return Err("source fixture layout changed".into());
        }
        check_world_socket_paths(&world.dir)?;
        for path in [
            &world.dir,
            &world.home,
            &world.cfg,
            &world.cfg.join("aterm"),
        ] {
            aterm_uds::private_file::check_private_dir(path).map_err(|error| error.to_string())?;
        }
        crate::util::create_private_dir_all(&world.run).map_err(|error| error.to_string())?;
        private_home_roots(&world.home).map_err(|error| error.to_string())?;
        for name in [".profile", ".shrc"] {
            crate::util::write_fresh(&world.home.join(name), "PS1='$ '\n")
                .map_err(|error| error.to_string())?;
        }
        Ok(world)
    }

    fn retain_start_failure(&self, error: String) -> String {
        self.cleanup_failed.store(true, Ordering::Release);
        eprintln!(
            "aterm-lab: instance startup failed; keeping {}: {error}",
            self.dir.display()
        );
        error
    }

    /// Lay out a fresh world whose `aterm.toml` is `config` with the audible bell off.
    /// Its short `/tmp` root ends in `.noindex` and uses the shared root's quiet routing.
    /// # Errors
    /// A directory or file could not be made or resolved, or its socket paths are too long.
    pub fn new(config: &str) -> Result<World, String> {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("at")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .map_err(|e| format!("cannot make a scratch directory under /tmp: {e}"))?;
        let dir = std::fs::canonicalize(scratch.path()).map_err(|e| {
            format!(
                "cannot resolve the newly owned scratch directory {}: {e}",
                scratch.path().display()
            )
        })?;
        check_world_socket_paths(&dir)?;
        let _ = scratch.keep();
        Self::lay_out(dir, config, false)
    }

    fn in_dir(dir: PathBuf, keep: bool) -> Self {
        Self {
            home: dir.join("home"),
            cfg: dir.join("cfg"),
            run: dir.join("run"),
            sock: dir.join("run/a.sock"),
            dir,
            keep,
            cleanup_failed: Arc::new(AtomicBool::new(false)),
            #[cfg(debug_assertions)]
            source_fixture_owned: false,
        }
    }

    fn lay_out(dir: PathBuf, config: &str, keep: bool) -> Result<Self, String> {
        let w = Self::in_dir(dir, keep);
        let made = (|| -> std::io::Result<()> {
            for d in [&w.home, &w.cfg.join("aterm"), &w.run] {
                crate::util::create_private_dir_all(d)?;
            }
            write_aterm_toml(&w.cfg.join("aterm/aterm.toml"), &bell_off(config))?;
            private_home_roots(&w.home)?;
            crate::util::write_fresh(&w.home.join(".profile"), "PS1='$ '\n")?;
            crate::util::write_fresh(&w.home.join(".shrc"), "PS1='$ '\n")
        })();
        made.map_err(|e| format!("cannot lay out {}: {e}", w.dir.display()))?;
        Ok(w)
    }

    /// Prepare a fresh private world at `dir`, retaining it for another lab process or host.
    ///
    /// # Errors
    /// The directory already exists, cannot be created privately or resolved, its socket paths
    /// exceed the platform's address bound, or its profile and marker cannot be written.
    pub fn create_at(dir: &Path, config: &str) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        let parent = std::fs::canonicalize(
            dir.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|error| error.to_string())?;
        let name = dir.file_name().ok_or("lab world needs a directory name")?;
        let dir = parent.join(name);
        check_world_socket_paths(&dir)?;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|error| format!("cannot create fresh lab world {}: {error}", dir.display()))?;
        let dir = std::fs::canonicalize(&dir).map_err(|error| error.to_string())?;
        let world = Self::lay_out(dir, config, true)?;
        crate::util::set_dir_mode_nofollow(&world.cfg, 0o700).map_err(|error| error.to_string())?;
        aterm_uds::private_file::write_private(&world.dir, "lab-world", b"aterm-lab-world/1\n")
            .map_err(|error| error.to_string())?;
        Ok(world)
    }

    /// Reopen a prepared world without rewriting its configuration, identity or data.
    /// The world remains on disk when this handle drops.
    ///
    /// # Errors
    /// The root or required profile directories are not private owned directories, the root
    /// cannot be resolved, or its identifying marker is missing or invalid.
    pub fn open_at(dir: &Path) -> Result<Self, String> {
        use aterm_uds::private_file::{check_private_dir, read_private};
        let marker = read_private(dir, "lab-world", 64).map_err(|error| error.to_string())?;
        if marker.as_slice() != b"aterm-lab-world/1\n" {
            return Err("not a prepared lab world".into());
        }
        let dir = std::fs::canonicalize(dir).map_err(|error| error.to_string())?;
        let world = Self::in_dir(dir, true);
        for path in [
            &world.home,
            &world.cfg,
            &world.cfg.join("aterm"),
            &world.run,
        ] {
            check_private_dir(path).map_err(|error| error.to_string())?;
        }
        Ok(world)
    }

    /// Leave the world in place when it drops, and say where.
    pub const fn keep(&mut self) {
        self.keep = true;
    }

    /// Remove the world when it drops after all: a [`World::keep`] taken
    /// back once every program copied into it showed it ran.
    pub const fn release(&mut self) {
        self.keep = false;
    }

    /// Put its `HOME` at a new `home` (made `0700`, with the world's shell profile).
    /// # Errors
    /// The folder already exists, or it or its profile could not be made.
    pub fn rehome(&mut self, home: &Path) -> Result<(), String> {
        let made = (|| -> std::io::Result<PathBuf> {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(home)?;
            crate::util::set_dir_mode_nofollow(home, 0o700)?;
            let home = std::fs::canonicalize(home)?;
            private_home_roots(&home)?;
            crate::util::write_fresh(&home.join(".profile"), "PS1='$ '\n")?;
            crate::util::write_fresh(&home.join(".shrc"), "PS1='$ '\n")?;
            Ok(home)
        })();
        self.home = made.map_err(|e| format!("cannot make the home {}: {e}", home.display()))?;
        Ok(())
    }

    /// The instance's state root (`ATERM_STATE_HOME`): its `logs/` among it.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        private_state_home(&self.home)
    }

    /// The environment an instance in this world gets: nothing inherited.
    #[must_use]
    pub fn env(&self) -> Vec<(String, String)> {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let mut env = vec![
            ("HOME".into(), s(&self.home)),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("ENV".into(), s(&self.home.join(".shrc"))),
            ("XDG_CONFIG_HOME".into(), s(&self.cfg)),
            ("XDG_RUNTIME_DIR".into(), s(&self.run)),
            ("XDG_CACHE_HOME".into(), s(&self.home.join(".cache"))),
            ("XDG_DATA_HOME".into(), s(&self.home.join(".local/share"))),
            ("XDG_STATE_HOME".into(), s(&self.home.join(".local/state"))),
            ("ATERM_STATE_HOME".into(), s(&self.state())),
            ("TMPDIR".into(), s(&self.home.join("tmp"))),
            ("TMP".into(), s(&self.home.join("tmp"))),
            ("TEMP".into(), s(&self.home.join("tmp"))),
            ("HISTFILE".into(), "/dev/null".into()),
            ("xcrun_nocache".into(), "1".into()),
        ];
        env.extend(seams());
        env
    }
}

/// Check the longest socket paths before a fresh world can launch anything.
fn check_world_socket_paths(dir: &Path) -> Result<(), String> {
    for suffix in [
        "run/aterm/aterm-4294967295.sock",
        "cfg/aterm/network-state/observer.sock",
        "cfg/aterm/network-state/waiting.sock",
    ] {
        std::os::unix::net::SocketAddr::from_pathname(dir.join(suffix)).map_err(|_| {
            "lab world socket paths are too long; choose a short directory under /tmp/.ubd"
                .to_string()
        })?;
    }
    Ok(())
}

/// The world `instance` runs in: a fresh scratch one, or the kept one at `at`, laid out there with
/// `config` when it is new. One laid out already keeps its own `aterm.toml`, so `configured` (a
/// `--toml`, `--toml-after`, `--harness` or `--harness-key` given) is refused there, never dropped.
fn instance_world(at: Option<&Path>, config: &str, configured: bool) -> Result<World, Fail> {
    let Some(dir) = at else {
        return Ok(World::new(config)?);
    };
    if !dir.exists() {
        return Ok(World::create_at(dir, config)?);
    }
    if configured {
        return Err(Fail::usage(
            "instance: --world DIR, laid out already, keeps its own aterm.toml; it takes no \
             --toml, --toml-after, --harness or --harness-key",
        ));
    }
    Ok(World::open_at(dir)?)
}

/// A test's parent folder for [`World::create_at`], under `/tmp` so the world's socket paths fit
/// `sun_path`: the targo shim runs tests with `TMPDIR` at `$TMPDIR/.ubd/`, a `/var/folders` path
/// that leaves them too long.
#[cfg(test)]
pub(crate) fn world_parent() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("w")
        .tempdir_in("/tmp")
        .expect("a scratch folder under /tmp")
}

#[cfg(test)]
#[path = "instance_prepared_world_tests.rs"]
mod prepared_world_tests;

/// The development seams every lab world sets ([`World::env`], the lab runs' `lab_vars`, the
/// fleet's `world_env`): the hosted loop records its harness tape ([`crate::tape::seam`]), the
/// instance hears no system memory pressure (`ATERM_DEBUG_SYSTEM_PRESSURE=none`), so only the
/// `pressure` verb sets its level and a loaded host sheds nothing mid-run, and the lab's own
/// program is the one lab stand-in agent the harness may drive ([`standins_seam`]). `--env` and
/// `LAB_INSTANCE_ENV` refuse them (`lab_sets`).
#[must_use]
pub(crate) fn seams() -> [(String, String); 3] {
    [
        crate::tape::seam(),
        ("ATERM_DEBUG_SYSTEM_PRESSURE".into(), "none".into()),
        standins_seam(),
    ]
}

/// The lab's stand-in agents, by the program they all run (`aterm-lab`: the presence and seamless
/// stand-ins, a swarm's shim interpreter): a debug build's harness types into an agent only when
/// the kernel confirms its image, and this seam names the lab's own as one it may
/// ([`aterm_agent::harness::agent_image::STANDINS_SEAM`]). A program merely named `claude` stays
/// observed.
#[must_use]
pub(crate) fn standins_seam() -> (String, String) {
    (
        aterm_agent::harness::agent_image::STANDINS_SEAM.into(),
        crate::paths::paths().exe.to_string_lossy().into_owned(),
    )
}

/// Roots for a newly owned HOME, on its volume even when `rehome` moved it.
fn private_home_roots(home: &Path) -> std::io::Result<()> {
    for path in [
        home.join(".cache"),
        home.join(".local/share"),
        home.join(".local/state"),
        home.join("tmp"),
        private_state_home(home),
    ] {
        crate::util::create_private_dir_all(&path)?;
    }
    Ok(())
}

/// The platform's aterm state root under `home`.
fn private_state_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return home.join("Library/Application Support/aterm");
    #[cfg(not(target_os = "macos"))]
    home.join(".local/state/aterm")
}

impl Drop for World {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        if self.source_fixture_owned {
            return;
        }
        if self.keep || self.cleanup_failed.load(Ordering::Acquire) {
            eprintln!("aterm-lab: kept {}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// How to start an instance.
pub struct Launch {
    pub aterm: PathBuf,
    /// `--headless` (with a lifeline); `false` opens a window.
    pub headless: bool,
    /// Flags and operands after the mode, socket and lifeline flags.
    pub args: Vec<String>,
    /// The whole environment ([`World::env`], adjusted).
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// A `sandbox-exec` profile to start it under.
    pub sandbox: Option<String>,
    /// It reroutes (no `--no-reroute`): its sessions find the reroute stubs
    /// and the `agents/` twins of its HOME's atpkg prefix first on `PATH`
    /// (`--reroute-prefix`, [`crate::reroute_prefix`]).
    pub reroute: bool,
}

/// An instance's process.
enum Proc {
    Child(Child),
    Job(Job),
}

/// How an instance starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Via {
    /// [`Instance::start`]: a child of this lab, in its coalition (the agent band).
    Child,
    /// [`Job::start`]: its own launchd job, a new coalition (the app band).
    Job,
}

impl Via {
    /// How [`Instance::start_in`] starts `l` in `band`: the app band as a job, a windowed
    /// instance too (its keeper is the job's process), and the agent band as a child.
    /// # Errors
    /// A sandboxed `l` in the app band.
    fn of(l: &Launch, band: Band) -> Result<Via, String> {
        match band {
            Band::Agent => Ok(Via::Child),
            Band::App if l.sandbox.is_some() => {
                Err("the app band starts an unsandboxed instance only".into())
            }
            Band::App => Ok(Via::Job),
        }
    }
}

/// The command line that starts `l` `via` a child or a job on the control socket `sock`, program
/// first: a windowed instance's keeper (`keeper`, run as `keeper --`, or as `keeper --disclaim
/// --` when it is a job's process, [`keeper_main`]), then `sandbox-exec` under `l`'s profile,
/// then aterm, a headless one with its lifeline on stdin, and its flags.
fn line(l: &Launch, via: Via, keeper: &Path, sock: &Path) -> Vec<String> {
    let mut line: Vec<String> = Vec::new();
    if !l.headless {
        line.extend([keeper.to_string_lossy().into_owned(), "keeper".to_string()]);
        if via == Via::Job {
            line.push(DISCLAIM.to_string());
        }
        line.push("--".to_string());
    }
    if let Some(profile) = &l.sandbox {
        line.extend(["/usr/bin/sandbox-exec", "-p", profile].map(str::to_string));
    }
    line.push(l.aterm.to_string_lossy().into_owned());
    if l.headless {
        line.extend([
            "--headless".to_string(),
            aterm_uds::lifeline::FLAG.to_string(),
            aterm_uds::lifeline::STDIN.to_string(),
        ]);
    } else {
        line.push("--window".to_string());
    }
    if !l.reroute {
        line.push("--no-reroute".to_string());
    }
    line.extend([
        "--control-sock".to_string(),
        sock.to_string_lossy().into_owned(),
    ]);
    line.extend(l.args.iter().cloned());
    line
}

/// The admission a caller requests for its window. Timing stays the default;
/// functional work must name its memory reservation explicitly.
#[derive(Clone, Copy)]
enum WindowWork {
    Timing,
    Functional { memory_bytes: u64, retain: bool },
}

enum WindowAdmission {
    Timing {
        _claim: aterm_capacity::Claim,
    },
    Retained {
        memory: aterm_capacity::RetainedClaim,
    },
    Functional {
        _claim: aterm_capacity::Claim,
    },
}

impl WindowAdmission {
    fn memory(&self) -> Option<&aterm_capacity::RetainedClaim> {
        match self {
            Self::Retained { memory } => Some(memory),
            Self::Timing { .. } | Self::Functional { .. } => None,
        }
    }
}

/// Take the shared run's window lane, then the window's resource claim before
/// starting any child. Headless work takes neither. Functional admission supplies
/// no timing evidence; ordinary window launches keep their measured Timing guard.
/// # Errors
/// An interrupt ended the wait: why, and the run's interrupt path has it, so the run ends NOT
/// RUN ([`crate::stop::note`]).
fn windowed_admission(l: &Launch, work: WindowWork) -> Result<Option<WindowAdmission>, String> {
    if l.headless {
        return Ok(None);
    }
    crate::admission::window_lane().map_err(|f| f.msg.unwrap_or_default())?;
    let what = format!("a window of {}", l.aterm.display());
    let admitted = match work {
        WindowWork::Timing => crate::admission::quiet(&what)
            .map(|claim| WindowAdmission::Timing { _claim: claim })
            .map_err(|fail| fail.msg.unwrap_or_default()),
        WindowWork::Functional {
            memory_bytes,
            retain,
        } => {
            let claim = crate::admission::functional(&what, memory_bytes)
                .map_err(|fail| fail.msg.unwrap_or_default())?;
            if !retain {
                return Ok(Some(WindowAdmission::Functional { _claim: claim }));
            }
            claim
                .into_retained_guard()
                .map(|memory| WindowAdmission::Retained { memory })
                .map_err(|(original, why)| {
                    drop(original);
                    format!("memory custody refused before native spawn: {why}")
                })
        }
    }?;
    Ok(Some(admitted))
}

#[cfg(test)]
#[path = "instance_window_admission_tests.rs"]
mod window_admission_tests;

/// A running private instance.
pub struct Instance {
    child: Proc,
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    /// The instance's pid; a windowed one's keeper's ([`keeper_main`]).
    pub pid: i32,
    /// It runs under a keeper (windowed): aterm is [`Instance::pid`]'s child.
    kept: bool,
    pub aterm: PathBuf,
    /// The aterm whose `ctl` reads it (the instance's own by default).
    pub client: PathBuf,
    pub sock: PathBuf,
    /// The instance's stdout and stderr.
    pub log: PathBuf,
    env: Vec<(String, String)>,
    /// The window's resource claim, held while it lives ([`crate::admission`]).
    _admission: Option<WindowAdmission>,
    cleanup: cleanup::StopRecord,
    gui: Option<aterm_uds::process::Identity>,
    handoff: Option<handoff::Tracking>,
    /// What its stop gives it before each signal ([`STOP_GRACES`]).
    graces: Graces,
}

/// What one `ctl` client run said.
#[derive(Debug, Default, Clone)]
pub struct Reply {
    /// Its exit status (`124`: a timeout; `-1`: it could not run).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Reply {
    /// The complete absolute path in a successful plain `image` reply (`OK <w> <h> <path>`).
    #[must_use]
    pub fn image_path(&self) -> Option<&str> {
        if self.code != 0 {
            return None;
        }
        let mut fields = self.stdout.lines().next()?.splitn(4, ' ');
        if fields.next()? != "OK" {
            return None;
        }
        let width: u32 = fields.next()?.parse().ok()?;
        let height: u32 = fields.next()?.parse().ok()?;
        let path = fields.next()?;
        (width > 0 && height > 0 && Path::new(path).is_absolute()).then_some(path)
    }

    /// The first line of stdout, else of stderr: an answer's verdict line.
    #[must_use]
    pub fn first(&self) -> String {
        let head = |s: &str| s.lines().next().unwrap_or("").to_string();
        let out = head(&self.stdout);
        if out.is_empty() {
            head(&self.stderr)
        } else {
            out
        }
    }

    /// The `OK …` header `aterm ctl` prints on stderr for a counted reply.
    #[must_use]
    pub fn header(&self) -> String {
        self.stderr
            .lines()
            .find_map(|l| {
                let l = l.strip_prefix("aterm-ctl: ").unwrap_or(l);
                l.starts_with("OK ").then(|| l.to_string())
            })
            .unwrap_or_default()
    }
}

/// A terminal's explicit selector from the instance-local roster.
pub(crate) fn terminal_selector(roster: &str) -> Option<String> {
    roster
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.next()?.parse::<u64>().ok()?;
            let sid = words.next()?;
            if crate::stub::first_sid(sid) != Some(sid) {
                return None;
            }
            words.next()?;
            let live = match words.next()? {
                "alive" | "spawning" => true,
                "exited" => false,
                _ => return None,
            };
            words.next()?;
            let focused = words.clone().any(|word| word == "wfocus=1");
            let active = words.any(|word| word == "active=1");
            Some(((live, focused, active), sid))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, sid)| format!("@{sid}"))
}

pub(crate) fn terminal_request(mut ctl: impl FnMut(&[&str]) -> Reply, args: &[&str]) -> Reply {
    let roster = ctl(&["sessions"]);
    terminal_request_from_roster(ctl, args, roster)
}

fn terminal_request_from_roster(
    mut ctl: impl FnMut(&[&str]) -> Reply,
    args: &[&str],
    roster: Reply,
) -> Reply {
    if roster.code != 0 {
        return roster;
    }
    let Some(selector) = terminal_selector(&roster.stdout) else {
        return Reply {
            code: 1,
            stderr: "instance: no terminal in its session registry".into(),
            ..Reply::default()
        };
    };
    let mut selected = Vec::with_capacity(args.len() + 1);
    selected.push(selector.as_str());
    selected.extend_from_slice(args);
    ctl(&selected)
}

/// The effective environment keeps every required root, and any explicit state root,
/// in this world. A repeated environment name uses its last value, as the child does.
fn ordinary_roots_match(world: &World, env: &[(String, String)]) -> bool {
    let root = |name| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| Path::new(value))
    };
    [
        ("HOME", &world.home),
        ("XDG_CONFIG_HOME", &world.cfg),
        ("XDG_RUNTIME_DIR", &world.run),
    ]
    .into_iter()
    .all(|(name, expected)| root(name) == Some(expected.as_path()))
        && root("ATERM_STATE_HOME").is_none_or(|state| state == world.state())
}

impl Instance {
    /// Follow replacements through the actual retained original window description.
    /// The seed is the GUI observed as this instance's own keeper's child.
    /// # Errors
    /// Retained custody or the bounded private credential is unavailable.
    pub(crate) fn track_handoffs(
        &mut self,
    ) -> Result<Option<aterm_uds::process::Identity>, String> {
        if !self.kept {
            return Err("handoff tracking requires a guarded window".into());
        }
        if self.handoff.is_none() {
            let tracking = crate::admission::window_custody()
                .map_err(|why| {
                    why.msg
                        .unwrap_or_else(|| "original window custody unavailable".into())
                })
                .and_then(|original| handoff::Tracking::new(original, &self.sock, self.gui));
            self.handoff = Some(match tracking {
                Ok(tracking) => tracking,
                Err(why) => {
                    self.cleanup.require_evidence(format!(
                        "handoff ownership could not be captured: {why}"
                    ));
                    return Err(why);
                }
            });
        }
        Ok(self
            .handoff
            .as_ref()
            .expect("tracking installed above")
            .identity())
    }

    /// Start `l` in `world`, its output in `<world>/aterm.log`.
    /// # Errors
    /// The log, the lifeline or the process could not be made.
    pub fn start(world: &World, l: Launch) -> Result<Instance, String> {
        Self::start_on_socket(world, l, &world.sock, WindowWork::Timing, |_| Ok(()))
    }

    /// Start a functional window with a nonzero memory reservation and the
    /// shared run's window lane. This supplies no timing qualification and is
    /// only for checks whose verdict makes no latency or throughput claim.
    /// # Errors
    /// A headless launch, zero reservation, unavailable resources, or a failure
    /// named by [`Self::start`]. No child starts when admission fails.
    pub fn start_functional(
        world: &World,
        l: Launch,
        memory_bytes: u64,
    ) -> Result<Instance, String> {
        Self::start_functional_in(world, l, memory_bytes, Band::Agent)
    }

    /// Functional admission in the selected scheduling band, with the same
    /// original lane custody. App-band handoff completes before GO; neither
    /// route supplies Timing evidence.
    /// # Errors
    /// As [`Self::start_functional`] and [`Self::start_in`].
    pub fn start_functional_in(
        world: &World,
        l: Launch,
        memory_bytes: u64,
        band: Band,
    ) -> Result<Instance, String> {
        Self::start_functional_custody(world, l, memory_bytes, band, false)
    }

    /// Explicit private qualification of a detached original reservation.
    /// Ordinary Functional launches await coordinated reader rollout.
    pub(crate) fn start_retained_functional_in(
        world: &World,
        l: Launch,
        memory_bytes: u64,
        band: Band,
    ) -> Result<Instance, String> {
        Self::start_functional_custody(world, l, memory_bytes, band, true)
    }

    fn start_functional_custody(
        world: &World,
        l: Launch,
        memory_bytes: u64,
        band: Band,
        retain: bool,
    ) -> Result<Instance, String> {
        if l.headless || memory_bytes == 0 {
            return Err("functional window needs a window and a nonzero memory reservation; it started nothing".into());
        }
        Self::start_in_on_socket(
            world,
            l,
            band,
            &world.sock,
            WindowWork::Functional {
                memory_bytes,
                retain,
            },
            |_| Ok(()),
        )
    }

    /// Start an app-band window as its own responsible process through the ordinary
    /// executable entry point, using its default socket beneath this world's private
    /// runtime directory. The caller supplies the private environment and confines
    /// any automatic network lanes the measurement enables. The executable decides
    /// which lanes that environment admits, including guarded debug attachments.
    /// # Errors
    /// A headless launch, mismatched private roots,
    /// or any failure named by [`Self::start_in`].
    pub fn start_ordinary(world: &World, l: Launch) -> Result<Instance, String> {
        Self::start_ordinary_with(world, l, WindowWork::Timing)
    }

    /// Start an ordinary app-band window with a nonzero memory reservation and
    /// the shared window lane, without a quiet-machine wait or timing qualification.
    /// The private roots and default control socket are those of [`Self::start_ordinary`].
    /// # Errors
    /// A headless launch, zero reservation, unavailable resources, or a failure
    /// named by [`Self::start_ordinary`]. No child starts when admission fails.
    pub fn start_ordinary_functional(
        world: &World,
        l: Launch,
        memory_bytes: u64,
    ) -> Result<Instance, String> {
        if l.headless || memory_bytes == 0 {
            return Err("functional window needs a window and a nonzero memory reservation; it started nothing".into());
        }
        Self::start_ordinary_with(
            world,
            l,
            WindowWork::Functional {
                memory_bytes,
                retain: false,
            },
        )
    }

    fn start_ordinary_with(world: &World, l: Launch, work: WindowWork) -> Result<Instance, String> {
        if l.headless || !ordinary_roots_match(world, &l.env) {
            return Err("ordinary instance needs a window and its private home, config and runtime directories".into());
        }
        // Empty means the ordinary per-instance socket, rather than an explicit
        // socket that would suppress Commons startup in the executable.
        let mut instance =
            Self::start_in_on_socket(world, l, Band::App, Path::new(""), work, |_| Ok(()))?;
        instance.sock = world
            .run
            .join("aterm")
            .join(aterm_types::control_socket::LATEST_SOCK_FILE);
        Ok(instance)
    }

    fn start_on_socket(
        world: &World,
        mut l: Launch,
        socket: &Path,
        work: WindowWork,
        prepare: impl FnOnce(&mut Vec<(String, String)>) -> Result<(), String>,
    ) -> Result<Instance, String> {
        let admission = windowed_admission(&l, work)?;
        let log = world.dir.join("aterm.log");
        let out = crate::util::create_stdio_file(&log)
            .map_err(|e| format!("cannot create {}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("cannot share the instance log: {e}"))?;
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.dir)
            .map_err(|e| format!("the lifeline: {e}"))?;
        let mut line = line(&l, Via::Child, &paths().exe, socket);
        let mut command = aterm_uds::spawnfd::command(&line[0]);
        if !l.headless {
            let lane =
                crate::admission::window_descriptor().map_err(|why| why.msg.unwrap_or_default())?;
            let guards = crate::window_guards::WindowGuards::from_original(
                lane,
                admission.as_ref().and_then(WindowAdmission::memory),
            )?;
            let claim = guards.claim().map(str::to_owned);
            command
                .inherit_guards(guards.into_descriptors())
                .map_err(|why| format!("window guard inheritance: {why}"))?;
            let slots: Vec<_> = command.guard_targets().collect();
            let at = line
                .iter()
                .position(|word| word == "--")
                .ok_or("keeper has no command boundary")?;
            line.splice(
                at..at,
                crate::window_guards::WindowGuards::arguments(&slots, claim.as_deref())?,
            );
        }
        let (program, argv) = line.split_first().ok_or("an empty command line")?;
        // Admission and inherited resource guards are settled before time-sensitive inputs
        // are prepared. The callback can add environment inputs, not change the admitted
        // executable, window mode or arguments.
        prepare(&mut l.env)?;
        let child = command
            .env_clear()
            .envs(l.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(argv)
            .current_dir(&l.cwd)
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| world.retain_start_failure(format!("cannot start {program}: {e}")))?;
        let pid = i32::try_from(child.id()).unwrap_or(0);
        Ok(Instance {
            child: Proc::Child(child),
            lifeline: Some(lifeline),
            pid,
            kept: !l.headless,
            client: l.aterm.clone(),
            aterm: l.aterm,
            sock: world.sock.clone(),
            log,
            env: world.env(),
            _admission: admission,
            cleanup: cleanup::StopRecord::new(&world.dir, world.cleanup_failed.clone(), pid),
            gui: None,
            handoff: None,
            graces: STOP_GRACES,
        })
    }

    /// [`Instance::start`] in `band`. In the app band the job's process is a headless aterm, or
    /// a windowed one's keeper, whose aterm runs in the job's coalition as its child and its own
    /// responsible process.
    /// # Errors
    /// As [`Instance::start`]; a sandboxed `l` in the app band; the job could not start.
    pub fn start_in(world: &World, l: Launch, band: Band) -> Result<Instance, String> {
        Self::start_in_on_socket(world, l, band, &world.sock, WindowWork::Timing, |_| Ok(()))
    }

    fn start_in_on_socket(
        world: &World,
        mut l: Launch,
        band: Band,
        socket: &Path,
        work: WindowWork,
        prepare: impl FnOnce(&mut Vec<(String, String)>) -> Result<(), String>,
    ) -> Result<Instance, String> {
        if Via::of(&l, band)? == Via::Child {
            return Self::start_on_socket(world, l, socket, work, prepare);
        }
        let admission = windowed_admission(&l, work)?;
        let log = world.dir.join("aterm.log");
        crate::util::create_stdio_file(&log)
            .map_err(|e| format!("cannot create {}: {e}", log.display()))?;
        let line = line(&l, Via::Job, &paths().exe, socket);
        let job = if l.headless {
            prepare(&mut l.env)?;
            Job::start(&world.dir, &line, &l.env, &l.cwd, &log)
                .map_err(|error| world.retain_start_failure(error))?
        } else {
            let lane =
                crate::admission::window_descriptor().map_err(|why| why.msg.unwrap_or_default())?;
            let guards = crate::window_guards::WindowGuards::from_original(
                lane,
                admission.as_ref().and_then(WindowAdmission::memory),
            )?;
            prepare(&mut l.env)?;
            Job::start_with_guards(&world.dir, &line, &l.env, &l.cwd, &log, guards)
                .map_err(|error| world.retain_start_failure(error))?
        };
        let pid = job.pid;
        Ok(Instance {
            pid,
            child: Proc::Job(job),
            lifeline: None,
            kept: !l.headless,
            client: l.aterm.clone(),
            aterm: l.aterm,
            sock: world.sock.clone(),
            log,
            env: world.env(),
            _admission: admission,
            cleanup: cleanup::StopRecord::new(&world.dir, world.cleanup_failed.clone(), pid),
            gui: None,
            handoff: None,
            graces: STOP_GRACES,
        })
    }

    /// Wait until the control socket answers `version` (at most `limit`). An ordinary launch
    /// waits on its own child process's socket, never a sibling named by the latest alias.
    /// # Errors
    /// Interrupted, the instance exited first, or it never answered (its log tail says why).
    pub fn await_socket(&mut self, limit: Duration) -> Result<(), Fail> {
        let end = stop::deadline_after(limit);
        loop {
            let ordinary = self
                .sock
                .file_name()
                .is_some_and(|name| name == aterm_types::control_socket::LATEST_SOCK_FILE);
            let ready_socket = if ordinary {
                self.aterm_pid().is_some_and(|pid| {
                    self.sock
                        .set_file_name(aterm_types::control_socket::instance_sock_name(pid as u32));
                    true
                })
            } else {
                true
            };
            if ready_socket && self.sock.exists() && self.ctl(&["version"]).code == 0 {
                if self.kept && self.gui.is_none() {
                    self.gui = self
                        .original_aterm_pid()
                        .and_then(aterm_uds::process::read_identity);
                }
                return Ok(());
            }
            if let Some(status) = self.exited() {
                return Err(Fail::die(format!(
                    "the instance exited ({status}) before its socket answered; its log:\n{}",
                    self.log_tail(20)
                )));
            }
            if Instant::now() >= end {
                return Err(Fail::die(format!(
                    "the instance never answered on {} in {} s; its log:\n{}",
                    self.sock.display(),
                    limit.as_secs(),
                    self.log_tail(20)
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    /// Runs `aterm ctl --sock <socket> --timeout 90 <args>` in the world's environment.
    #[must_use]
    pub fn ctl(&self, args: &[&str]) -> Reply {
        let sock = self.sock.to_string_lossy();
        let mut argv = vec!["ctl", "--sock", &sock, "--timeout", "90"];
        argv.extend_from_slice(args);
        self.cli(None, &argv)
    }

    /// Runs `aterm <args>` (the instance's client) in the world's environment, in `cwd` when
    /// given: a read view of its home such as `harness disk`.
    #[must_use]
    pub fn cli(&self, cwd: Option<&Path>, argv: &[&str]) -> Reply {
        let mut cmd = aterm_uds::spawnfd::command(&self.client);
        cmd.env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(argv)
            .stdin(Stdio::null());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output();
        match out {
            Ok(o) => Reply {
                code: o.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => Reply {
                code: -1,
                stdout: String::new(),
                stderr: format!("cannot run {}: {e}", self.client.display()),
            },
        }
    }

    /// `ctl` with the request text piped in (`send --stdin`).
    #[must_use]
    pub fn ctl_stdin(&self, args: &[&str], input: &[u8]) -> Reply {
        use std::io::Write as _;
        let sock = self.sock.to_string_lossy();
        let mut argv = vec!["ctl", "--sock", &sock, "--timeout", "90"];
        argv.extend_from_slice(args);
        let child = aterm_uds::spawnfd::command(&self.client)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            return Reply {
                code: -1,
                ..Reply::default()
            };
        };
        if let Some(mut w) = child.stdin.take() {
            let _ = w.write_all(input);
        }
        match child.wait_with_output() {
            Ok(o) => Reply {
                code: o.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(_) => Reply {
                code: -1,
                ..Reply::default()
            },
        }
    }

    /// One held control connection to it, for a schedule whose requests are milliseconds apart.
    /// # Errors
    /// The socket did not answer.
    pub fn connect(&self) -> Result<aterm_agent::RelayClient<aterm_uds::CtlStream>, String> {
        aterm_agent::RelayClient::connect_local_beside(&self.sock.to_string_lossy())
            .map_err(|e| format!("cannot reach {}: {e}", self.sock.display()))
    }

    /// Whether it still runs.
    pub fn running(&mut self) -> bool {
        self.exited().is_none()
            || self
                .handoff
                .as_mut()
                .is_some_and(|owner| owner.running(&self.sock))
    }

    /// The still-matching proven handoff peer, else the launched process or keeper's child.
    /// Returns `None` when neither an owned peer nor a keeper child can be observed.
    #[must_use]
    pub fn aterm_pid(&self) -> Option<i32> {
        if let Some(pid) = self.handoff.as_ref().and_then(handoff::Tracking::pid) {
            return Some(pid);
        }
        self.original_aterm_pid()
    }

    fn original_aterm_pid(&self) -> Option<i32> {
        if !self.kept {
            return Some(self.pid);
        }
        aterm_uds::process::child_pids(self.pid)?.into_iter().next()
    }

    /// Observe the exact original detached reservation without exporting a copy
    /// to the independent crash verifier.
    #[cfg(any(test, debug_assertions))]
    pub fn memory_for_qualification(
        &self,
    ) -> Result<crate::window_guards::MemoryObservation, String> {
        use std::os::unix::fs::MetadataExt as _;
        let memory = self
            ._admission
            .as_ref()
            .and_then(WindowAdmission::memory)
            .ok_or("window has no original detached memory custody")?;
        let metadata = memory.metadata().map_err(|why| why.to_string())?;
        let record = memory.record();
        if record.accounting != aterm_capacity::Accounting::Detached || record.need.mem == 0 {
            return Err("window memory is not the original detached reservation".into());
        }
        Ok(crate::window_guards::MemoryObservation {
            claim: memory
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("invalid original claim leaf")?
                .to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: record.need.mem,
            pid: record.pid,
            start_ms: record.start_ms,
            sequence: record.seq,
        })
    }

    /// Original launchd job locator for the private custody qualifier.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn qualification_job_label(&self) -> Option<&str> {
        match &self.child {
            Proc::Job(job) => Some(&job.label),
            Proc::Child(_) => None,
        }
    }

    /// Crash the exact original keeper for the maintained native custody qualifier.
    /// Child custody requires its actual SIGKILL wait status; launchd custody requires
    /// authenticated job identity and positive native absence, never a Child receipt.
    /// The GUI is deliberately left alive to test its original guard custody.
    #[cfg(any(test, debug_assertions))]
    pub fn crash_keeper_for_qualification(
        &mut self,
        expected: aterm_uds::process::Identity,
    ) -> Result<(), String> {
        if !self.kept
            || expected.pid != self.pid
            || aterm_uds::process::read_identity(self.pid) != Some(expected)
        {
            return Err(
                "custody qualification requires its exact original native window keeper".into(),
            );
        }
        let child = match &mut self.child {
            Proc::Job(job) => return job.crash_for_qualification(expected),
            Proc::Child(child) => child,
        };
        if child.try_wait().map_err(|why| why.to_string())?.is_some() {
            return Err("the owned keeper ended before the crash cut".into());
        }
        child.kill().map_err(|why| why.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|why| why.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err("one-minute hang detector: owned keeper SIGKILL/reap".into());
            }
            stop::sleep(Duration::from_millis(20))
                .map_err(|why| format!("interrupted: {why:?}"))?;
        };
        use std::os::unix::process::ExitStatusExt as _;
        if status.signal() != Some(libc::SIGKILL) {
            return Err(format!("owned keeper did not die by SIGKILL: {status}"));
        }
        Ok(())
    }

    /// How it ended, once it has.
    fn exited(&mut self) -> Option<String> {
        match &mut self.child {
            Proc::Child(c) => match c.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                _ => None,
            },
            Proc::Job(j) => (!j.running()).then(|| format!("launchd job {}", j.label)),
        }
    }

    /// The last `n` lines of its log.
    #[must_use]
    pub fn log_tail(&self, n: usize) -> String {
        let body = crate::util::read_regular_text(&self.log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }

    /// Cut the lifeline and record one bounded stop attempt for the original process/job
    /// and any replacement whose original-description custody this instance proved.
    /// The signal that stops a run asks for this, so the attempt waits through it
    /// ([`stop::Grace`]). Repeated calls, including Drop, return the recorded result without
    /// another timeout.
    /// # Errors
    /// The child cannot be reaped, the job registration/absence is unknown, an operation
    /// fails or a second signal cuts it short, or the common one-minute deadline expires
    /// (native helper termination/reap has its separate finite grace); the error then says
    /// whether the instance's processes still run ([`standing`]). Failure retains this
    /// Instance's custody while borrowed and keeps its private World and immutable diagnostics.
    pub fn stop(&mut self) -> Result<(), String> {
        let _grace = stop::grace();
        if self.kept && self.gui.is_none() {
            self.gui = self
                .original_aterm_pid()
                .and_then(aterm_uds::process::read_identity);
        }
        self.cleanup.attempt(|| {
            let deadline = Instant::now() + Duration::from_secs(60);
            let original = stop_process(
                &mut self.child,
                &mut self.lifeline,
                self.pid,
                self.kept,
                self.gui,
                self.graces,
            )
            .map_err(|why| {
                format!(
                    "{why}; {}",
                    standing(&mut self.child, self.pid, self.kept, self.gui)
                )
            });
            let handoff = self
                .handoff
                .as_mut()
                .map_or(Ok(()), |owner| owner.stop(&self.sock, deadline));
            match (original, handoff) {
                (Ok(()), result) | (result, Ok(())) => result,
                (Err(original), Err(handoff)) => {
                    Err(format!("{original}; successor cleanup: {handoff}"))
                }
            }
        })
    }
}

/// What one of an instance's processes is now ([`standing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Now {
    Runs,
    /// It has exited and is not yet reaped.
    Exited,
    /// The kernel knows no process by its pid.
    Ended,
    /// Its pid now names another process: it has ended.
    Reused,
    /// Its pid is present, and its identity could not be read.
    Unreadable,
    /// The kernel neither showed its pid nor showed it gone.
    Unseen,
}

impl std::fmt::Display for Now {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Now::Runs => "still runs",
            Now::Exited => "has exited, not yet reaped",
            Now::Ended => "has ended",
            Now::Reused => "has ended, its pid now another process's",
            Now::Unreadable => "is present, its identity unreadable",
            Now::Unseen => "is neither seen nor seen gone",
        })
    }
}

/// What `presence` says of a process, with `same`: whether its pid still names it (`None`: its
/// identity could not be read). A pid no other process can take (this run's unreaped child), or
/// one the stop's own end check reads by number (a launchd job's process), is `Some(true)`.
fn now_of(presence: aterm_uds::process::ProcessPresence, same: Option<bool>) -> Now {
    use aterm_uds::process::ProcessPresence;
    match (presence, same) {
        (ProcessPresence::Absent, _) => Now::Ended,
        (ProcessPresence::Unknown, _) => Now::Unseen,
        (_, Some(false)) => Now::Reused,
        (ProcessPresence::Exited, _) => Now::Exited,
        (ProcessPresence::Present, Some(true)) => Now::Runs,
        (ProcessPresence::Present, None) => Now::Unreadable,
    }
}

/// What a windowed instance's aterm (`gui`, as the stop knew it) is now, from what the kernel
/// says of its pid (`presence`) and the identity that pid has now (`now`), as [`aterm_seen`]
/// reads them.
fn aterm_now(
    gui: aterm_uds::process::Identity,
    presence: aterm_uds::process::ProcessPresence,
    now: Option<aterm_uds::process::Identity>,
) -> Now {
    now_of(presence, now.map(|now| now == gui))
}

#[cfg(test)]
thread_local! {
    /// What a test does once between [`aterm_seen`]'s two reads on this thread: end the aterm.
    static BETWEEN_READS: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// What the kernel says now of the pid of a windowed instance's aterm (`gui`, as the stop knew
/// it): its presence, and the identity the pid names, read first. An exit only moves forward, so
/// an identity unreadable because the aterm's exit had begun is followed by `Exited` or `Absent`,
/// its end, never by `Present` unless the kernel has since given its pid to another process. Read
/// the other way round, a `Present` read just before the aterm's exit began meets an unreadable
/// identity, which would say the aterm changed when it ended.
fn aterm_seen(
    gui: aterm_uds::process::Identity,
) -> (
    aterm_uds::process::ProcessPresence,
    Option<aterm_uds::process::Identity>,
) {
    let now = aterm_uds::process::read_identity(gui.pid);
    #[cfg(test)]
    if let Some(between) = BETWEEN_READS.take() {
        between();
    }
    (aterm_uds::process::process_presence(gui.pid), now)
}

/// What the kernel says now of an instance whose stop did not establish its end, judged by what
/// the stop holds of each process: its own (`pid`, a windowed one's keeper, `kept`) by the
/// child's reaped status where it is this run's child, and a windowed one's aterm (`gui`) by its
/// identity, so a pid the kernel has since given another process is not said to be the
/// instance's.
fn standing(
    child: &mut Proc,
    pid: i32,
    kept: bool,
    gui: Option<aterm_uds::process::Identity>,
) -> String {
    use aterm_uds::process::process_presence;
    let what = if kept {
        "its keeper's"
    } else {
        "the instance's"
    };
    let own = match child {
        Proc::Child(child) => match child.try_wait() {
            Ok(Some(status)) => format!("{what} pid {pid} has ended ({status})"),
            Ok(None) => format!(
                "{what} pid {pid} {}",
                now_of(process_presence(pid), Some(true))
            ),
            Err(why) => format!("{what} pid {pid} cannot be observed: {why}"),
        },
        Proc::Job(_) => format!(
            "{what} pid {pid} {}",
            now_of(process_presence(pid), Some(true))
        ),
    };
    if !kept {
        return own;
    }
    match gui {
        Some(gui) => {
            let (presence, now) = aterm_seen(gui);
            let now = aterm_now(gui, presence, now);
            format!("{own}, its aterm's pid {} {now}", gui.pid)
        }
        None => format!("{own}, its aterm's pid was never read"),
    }
}

/// A cleanup wait's error once a second signal cut it short ([`stop::Grace`]) and a last look
/// did not find the end it waited for.
fn cut_short(phase: &str, Stopped(sig): Stopped) -> String {
    format!("signal {sig} cut the {phase} short")
}

/// Cut the instance's lifeline and wait, under one one-minute deadline, for the end of its
/// process (signalled after each of its `graces` when it is this run's child, its job removed
/// after the first when it is a launchd job's) and of a windowed one's aterm (`kept`, `gui`). A
/// second signal cuts a wait short ([`stop::Grace`]); each wait then looks once more before it
/// gives up, so an end that came with the cut is still established.
fn stop_process(
    child: &mut Proc,
    lifeline: &mut Option<aterm_uds::lifeline::Lifeline>,
    pid: i32,
    kept: bool,
    gui: Option<aterm_uds::process::Identity>,
    graces: Graces,
) -> Result<(), String> {
    use aterm_uds::process::{ProcessPresence, process_presence};
    let deadline = Instant::now() + BOOT;
    if let Some(lifeline) = lifeline.take() {
        lifeline.cut();
    }
    if let Proc::Job(job) = child {
        job.cut();
        // A cut here leaves the job's registration to remove, which no last look can establish.
        let grace = (Instant::now() + graces.own).min(deadline);
        while Instant::now() < grace && job.running() {
            stop::sleep(Duration::from_millis(20))
                .map_err(|cut| cut_short("job lifeline grace", cut))?;
        }
        job.remove_until(deadline).map_err(|why| why.to_string())?;
        let mut cut = None;
        loop {
            match process_presence(pid) {
                ProcessPresence::Absent => return wait_gui_absent(kept, gui, deadline),
                ProcessPresence::Unknown => {
                    return Err("original job process absence remains unknown after removal".into());
                }
                ProcessPresence::Present | ProcessPresence::Exited => {}
            }
            if let Some(cut) = cut {
                return Err(cut_short("job's native absence", cut));
            }
            if Instant::now() >= deadline {
                return Err("one-minute hang detector: original job native absence".into());
            }
            cut = stop::sleep(Duration::from_millis(20)).err();
        }
    }
    let Proc::Child(child) = child else {
        unreachable!()
    };
    for (grace, signal) in [(graces.own, libc::SIGTERM), (graces.term, libc::SIGKILL)] {
        let until = (Instant::now() + grace).min(deadline);
        let mut cut = None;
        loop {
            if child
                .try_wait()
                .map_err(|why| format!("cannot observe original child: {why}"))?
                .is_some()
            {
                return wait_gui_absent(kept, gui, deadline);
            }
            if let Some(cut) = cut {
                return Err(cut_short("child grace", cut));
            }
            if Instant::now() >= until {
                break;
            }
            cut = stop::sleep(Duration::from_millis(20)).err();
        }
        if let Err(error) = crate::sys::kill_group(pid, signal) {
            if child.try_wait().map_err(|why| why.to_string())?.is_some() {
                return wait_gui_absent(kept, gui, deadline);
            }
            return Err(format!("cannot signal original child group: {error}"));
        }
    }
    let mut cut = None;
    loop {
        if child
            .try_wait()
            .map_err(|why| format!("cannot reap original child: {why}"))?
            .is_some()
        {
            return wait_gui_absent(kept, gui, deadline);
        }
        if let Some(cut) = cut {
            return Err(cut_short("child reap", cut));
        }
        if Instant::now() >= deadline {
            return Err("one-minute hang detector: original child reap".into());
        }
        cut = stop::sleep(Duration::from_millis(20)).err();
    }
}

fn gui_closed(
    required: bool,
    known: bool,
    presence: aterm_uds::process::ProcessPresence,
) -> Result<bool, String> {
    use aterm_uds::process::ProcessPresence;
    if !required {
        return Ok(true);
    }
    if !known {
        return Err(
            "original GUI identity was not established; keeper closure proves no GUI closure"
                .into(),
        );
    }
    match presence {
        ProcessPresence::Absent => Ok(true),
        ProcessPresence::Unknown => Err("original GUI native absence remains unknown".into()),
        ProcessPresence::Present | ProcessPresence::Exited => Ok(false),
    }
}

fn wait_gui_absent(
    required: bool,
    gui: Option<aterm_uds::process::Identity>,
    deadline: Instant,
) -> Result<(), String> {
    use aterm_uds::process::ProcessPresence;
    let mut cut = None;
    loop {
        let (presence, now) = gui.map_or((ProcessPresence::Unknown, None), aterm_seen);
        if gui_closed(required, gui.is_some(), presence)? {
            return Ok(());
        }
        if let Some(gui) = gui
            && presence == ProcessPresence::Present
            && now != Some(gui)
        {
            return Err(
                "original GUI identity changed or became unreadable before absence was observed"
                    .into(),
            );
        }
        if let Some(cut) = cut {
            return Err(cut_short("GUI's native absence", cut));
        }
        if Instant::now() >= deadline {
            return Err(
                "one-minute hang detector: original GUI native absence after keeper closure".into(),
            );
        }
        cut = stop::sleep(Duration::from_millis(20)).err();
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // stop records evidence once and says so as it does; a prior explicit failure must not
        // trigger another full cleanup operation from this destructor, nor say it again.
        let _ = self.stop();
    }
}

/// The value of `--flag N` in `args`, removed from them.
/// # Errors
/// The flag has no value.
pub fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, Fail> {
    let Some(at) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(Fail::usage(format!("{flag} needs a value")));
    }
    let v = args.remove(at + 1);
    args.remove(at);
    Ok(Some(v))
}

/// Every value of a repeatable `flag`, in order; removed from `args`.
/// # Errors
/// An occurrence with no value.
pub fn take_values(args: &mut Vec<String>, flag: &str) -> Result<Vec<String>, Fail> {
    let mut out = Vec::new();
    while let Some(v) = take_value(args, flag)? {
        out.push(v);
    }
    Ok(out)
}

/// Whether `flag` is in `args`; removed from them.
pub fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// A positive count, or the usage error naming the flag.
/// # Errors
/// Not a positive number.
pub fn count(flag: &str, v: &str) -> Result<u32, Fail> {
    v.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| v.parse::<u32>().ok())
        .flatten()
        .filter(|n| *n > 0)
        .ok_or_else(|| Fail::usage(format!("{flag} takes a positive count, not '{v}'")))
}

/// The aterm to drive: the `--aterm` value given, else the lab's own pick.
/// # Errors
/// Why there is none to drive: the one given is not executable, or the lab found none.
pub fn resolve_aterm(given: Option<String>) -> Result<PathBuf, String> {
    if let Some(a) = given {
        let a = from_caller(&a);
        if !crate::sys::executable(&a) {
            return Err(format!("--aterm {a} is not an executable"));
        }
        return Ok(PathBuf::from(a));
    }
    paths().need_aterm().map(Path::to_path_buf)
}

fn network_away_delay(args: &mut Vec<String>) -> Result<Option<u64>, Fail> {
    let value = take_value(args, "--network-away-in-ms")?;
    #[cfg(any(test, debug_assertions))]
    {
        value
            .map(|value| network_away::delay(&value))
            .transpose()
            .map_err(Fail::usage)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        if value.is_some() {
            return Err(Fail::usage(
                "instance: --network-away-in-ms requires a debug Lab build",
            ));
        }
        Ok(None)
    }
}

/// `instance`: a private aterm, headless unless `--window`, held until this command ends.
/// # Errors
/// Usage, or the instance could not start or answer.
pub fn cmd_instance(args: &[String]) -> Res {
    if args.first().is_some_and(|arg| arg == "compare") {
        return comparison::run(&args[1..]);
    }
    stop::install();
    let mut a = args.to_vec();
    let program = a
        .iter()
        .position(|x| x == "--")
        .map(|at| a.split_off(at).split_off(1))
        .unwrap_or_default();
    let aterm = take_value(&mut a, "--aterm")?;
    let columns = take_value(&mut a, "--columns")?.unwrap_or_else(|| "110".into());
    let lines = take_value(&mut a, "--lines")?.unwrap_or_else(|| "32".into());
    count("--columns", &columns)?;
    count("--lines", &lines)?;
    let font_px = take_value(&mut a, "--font-px")?;
    let scale = take_value(&mut a, "--scale")?;
    let scene = take_value(&mut a, "--scene")?;
    if scene
        .as_deref()
        .is_some_and(|s| !matches!(s, "band-icons" | "harness-layouts"))
    {
        return Err(Fail::usage(
            "instance: --scene takes band-icons or harness-layouts",
        ));
    }
    let image = take_value(&mut a, "--image")?.map(|p| from_caller(&p));
    let toml = take_values(&mut a, "--toml")?;
    let toml_after = take_values(&mut a, "--toml-after")?;
    let harness_keys = take_values(&mut a, "--harness-key")?;
    let extra_env =
        crate::lab::instance_env(&take_values(&mut a, "--env")?.join(" ")).map_err(Fail::usage)?;
    let network_away = network_away_delay(&mut a)?;
    let home = take_value(&mut a, "--home")?.map(|p| from_caller(&p));
    let reroute = take_value(&mut a, "--reroute-prefix")?.map(|p| from_caller(&p));
    let harness = take_flag(&mut a, "--harness");
    let pre = take_values(&mut a, "--pre")?;
    let post = take_values(&mut a, "--post")?;
    let keep = take_flag(&mut a, "--keep");
    let at = take_value(&mut a, "--world")?.map(|p| from_caller(&p));
    let window = take_flag(&mut a, "--window");
    let band = Band::pick(take_value(&mut a, "--band")?.as_deref())?;
    let cold = match take_value(&mut a, "--pages")?.as_deref() {
        None | Some("warm") => false,
        Some("cold") => true,
        Some(other) => {
            return Err(Fail::usage(format!(
                "instance: --pages takes cold or warm, not '{other}'"
            )));
        }
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "instance: unexpected '{extra}' ({LAB} help)"
        )));
    }
    if home.is_some() && reroute.is_some() {
        return Err(Fail::usage(
            "instance: --reroute-prefix DIR makes DIR the HOME; it takes no --home",
        ));
    }
    if at.is_some() && (home.is_some() || reroute.is_some() || keep) {
        return Err(Fail::usage(
            "instance: --world DIR is kept and keeps its own HOME; it takes no --home, \
             --reroute-prefix or --keep",
        ));
    }
    #[cfg(any(test, debug_assertions))]
    if network_away.is_some()
        && (aterm.is_some()
            || home.is_some()
            || reroute.is_some()
            || at.as_ref().is_some_and(|path| Path::new(path).exists())
            || extra_env
                .iter()
                .any(|(key, _)| key == aterm_netplace::facts::SEAM))
    {
        return Err(Fail::usage(
            "instance: --network-away-in-ms needs the checkout's default debug app, a fresh private world and its own network facts",
        ));
    }
    if !post.is_empty() && image.is_none() {
        return Err(Fail::usage(
            "instance: --post runs after --image's frame; drive a held instance with `aterm ctl` instead",
        ));
    }
    // This fixture requires the original checkout debug artifact, never LAB_ATERM or a
    // release fallback: only a debug app has both the scripted facts seam and A12 lab identity.
    let aterm = resolve_aterm(if network_away.is_some() {
        Some(
            crate::paths::plain_target(&paths().repo)
                .join("debug/aterm")
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        aterm
    })
    .map_err(|e| Fail::exit(3, e))?;
    let config = config_after(&config_with(&toml), harness, &toml_after);
    let configured =
        !toml.is_empty() || !toml_after.is_empty() || harness || !harness_keys.is_empty();
    let mut world = instance_world(
        at.as_deref().map(Path::new),
        &with_harness_keys(&config, &harness_keys),
        configured,
    )?;
    if keep {
        world.keep();
    }
    if let Some(home) = home {
        world.rehome(Path::new(&home))?;
    }
    if let Some(home) = &reroute {
        world.rehome(Path::new(home))?;
        let claude = crate::lab::find_claude().map_err(|e| Fail::exit(3, e))?;
        // An external prefix survives this world's Drop. Hold an owned one until its
        // clone prints from its own path, including when a failed probe returns early.
        let owned = world.home.starts_with(&world.dir);
        let hold = world.dir.join(crate::lab::HELD).join("reroute");
        if owned {
            world.keep();
        }
        let laid = crate::reroute_prefix::lay(
            &world.home,
            Path::new(&claude),
            owned.then_some(hold.as_path()),
        )
        .map_err(|e| Fail::die(format!("instance: --reroute-prefix: {e}")))?;
        if owned && !keep {
            world.release();
        }
        println!(
            "reroute prefix {}: {} -> {} -> {} ({})",
            laid.prefix.display(),
            laid.stub.display(),
            laid.twin.display(),
            laid.exe.display(),
            laid.version
        );
    }
    let cold = if cold {
        Some(cold_copy(&mut world, &aterm)?)
    } else {
        None
    };
    let aterm = cold.as_ref().map_or(aterm, |c| c.exe.clone());
    let mut flags = vec![
        "--columns".to_string(),
        columns,
        "--lines".to_string(),
        lines,
    ];
    if let Some(px) = font_px {
        flags.extend(["--font-px".to_string(), px]);
    }
    if let Some(s) = scale {
        flags.extend(["--scale".to_string(), s]);
    }
    if !program.is_empty() {
        flags.push("-e".to_string());
        flags.extend(program);
    }
    let mut env = world.env();
    env.extend(extra_env);
    if let Some(scene) = scene {
        env.push(("ATERM_DEBUG_SCENE".to_string(), scene));
    }
    #[cfg(any(test, debug_assertions))]
    let prepare = |env: &mut Vec<(String, String)>| -> Result<(), String> {
        if let Some(delay) = network_away {
            let fixture = network_away::prepare(&world, delay, aterm_netplace::unix_ms_now())?;
            println!(
                "instance: network away eligible at unix_ms={}",
                fixture.due_ms
            );
            env.push((
                aterm_netplace::facts::SEAM.into(),
                fixture.facts.to_string_lossy().into_owned(),
            ));
        }
        Ok(())
    };
    #[cfg(not(any(test, debug_assertions)))]
    let prepare = |_: &mut Vec<(String, String)>| -> Result<(), String> { Ok(()) };
    let mut inst = Instance::start_in_on_socket(
        &world,
        Launch {
            aterm,
            headless: !window,
            args: flags,
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: reroute.is_some(),
        },
        band,
        &world.sock,
        WindowWork::Timing,
        prepare,
    )?;
    // From here the instance's end is this verb's to wait for and say: a second signal, even
    // one that comes with the first, cuts that wait short instead of ending the run unsaid.
    let _ahead = stop::cleanup_ahead();
    let work = drive(
        &mut inst,
        &world,
        &Drive {
            pre: &pre,
            image,
            post: &post,
            band,
            window,
            cold: cold.as_ref(),
        },
    );
    end(&mut inst, work)
}

/// What `lab instance` does with its instance once it has started.
struct Drive<'a> {
    pre: &'a [String],
    image: Option<String>,
    post: &'a [String],
    band: Band,
    window: bool,
    cold: Option<&'a ColdCopy>,
}

/// Bring `inst` up and use it as `run` says: one frame (`--image`), or hold it until a signal.
/// # Errors
/// It did not come up, a control command or the frame failed, it exited while held, or a signal came first
/// ([`Stopped`]).
fn drive(inst: &mut Instance, world: &World, run: &Drive<'_>) -> Res {
    inst.await_socket(BOOT)?;
    let ready = terminal_request(|args| inst.ctl(args), &["ready"]);
    // A signal can end the client too (Ctrl-C reaches its group): its reply then says nothing.
    stop::check()?;
    if ready.code != 0 {
        return Err(Fail::die(format!("instance: not ready: {}", ready.first())));
    }
    if run.window {
        inst.track_handoffs().map_err(Fail::die)?;
    }
    run_verbs(run.pre, |args| inst.ctl(args))?;
    if let Some(dest) = &run.image {
        let shot = request_settled_image(|args| inst.ctl(args))?;
        let Some(path) = shot.image_path() else {
            return Err(Fail::die(format!("instance: image: {}", shot.first())));
        };
        crate::util::copy_regular_file(path, dest)
            .map_err(|e| format!("instance: cannot write {dest}: {e}"))?;
        println!("{} -> {dest}", shot.first());
        run_verbs(run.post, |args| inst.ctl(args))?;
        return Ok(0);
    }
    let aterm_pid = inst.aterm_pid();
    let read = |figure: Option<String>| figure.unwrap_or_else(|| "?".to_string());
    let coalition = read(
        aterm_pid
            .and_then(crate::sys::coalition_ids)
            .map(|[resource, _]| resource.to_string()),
    );
    let role = read(aterm_pid.and_then(|p| crate::sys::darwin_role(p).ok().map(|r| r.to_string())));
    let main_pri = read(
        aterm_pid
            .and_then(crate::sys::main_thread_priority)
            .map(|p| p.to_string()),
    );
    let kept = if run.window {
        format!(" aterm_pid={}", read(aterm_pid.map(|p| p.to_string())))
    } else {
        String::new()
    };
    let pages = run.cold.map_or_else(String::new, |c| {
        crate::sys::resident_pages(&c.root).map_or_else(
            |e| format!(" pages=cold resident_at_ready=? ({e})"),
            |(resident, pages)| format!(" pages=cold resident_at_ready={resident}/{pages}"),
        )
    });
    // asked after the resident pages are read, so the question touches none of them
    let version = inst.ctl(&["version"]);
    stop::check()?;
    let build = read(
        (version.code == 0)
            .then(|| crate::lab::build_named(&version.stdout))
            .flatten(),
    );
    println!(
        "instance pid={} sock={} build={build} band={} coalition={coalition} role={role} main_pri={main_pri}{kept}{pages}\n  drive it: {} ctl --sock {} <verb>   (image, text, send --stdin, metrics percentiles, …)\n  \
         world:    {}{}\n  it ends when this command does: Ctrl-C or SIGTERM ends it and waits for its end \
         (a second signal cuts that wait short)",
        inst.pid,
        inst.sock.display(),
        run.band.name(),
        inst.aterm.display(),
        inst.sock.display(),
        world.dir.display(),
        if world.keep {
            " (kept)"
        } else {
            " (removed once the instance has ended)"
        }
    );
    hold(inst)
}

/// Hold a running instance until it exits or a signal asks for its end: `Ok(0)` on the signal.
/// # Errors
/// It exited by itself (its log's tail says why).
fn hold(inst: &mut Instance) -> Res {
    loop {
        if !inst.running() {
            return Err(Fail::die(format!(
                "instance: it exited; its log:\n{}",
                inst.log_tail(20)
            )));
        }
        if stop::sleep(Duration::from_millis(500)).is_err() {
            return Ok(0);
        }
    }
}

/// `lab instance`'s end once its instance has started: the instance stopped, and `work`'s
/// outcome with what came of that. A signal during `work` asked for the stop, which waits through
/// it for the instance's end ([`Instance::stop`]): the end said when it came, else the run fails
/// saying what still stands. Without a signal, a stop that fails fails the run ([`after_stop`]).
/// # Errors
/// `work`'s, or the stop's.
fn end(inst: &mut Instance, work: Res) -> Res {
    // Read on this thread before the stop takes its grace: the run's own stop, if one came.
    let signal = stop::check().err();
    let stopped = inst.stop();
    let Some(Stopped(sig)) = signal else {
        return after_stop(work, stopped);
    };
    match stopped {
        Ok(()) => {
            println!("instance: ended (signal {sig})");
            work
        }
        Err(why) => {
            let said = format!("instance: signal {sig}, and its end is not established: {why}");
            Err(Fail::die(match work {
                Err(Fail {
                    msg: Some(msg),
                    stopped: None,
                    ..
                }) => format!("{msg}; {said}"),
                _ => said,
            }))
        }
    }
}

/// `instance --pages cold`'s copy of the aterm it launches.
struct ColdCopy {
    /// The executable to launch, inside [`ColdCopy::root`].
    exe: PathBuf,
    /// The copy: the bundle's when the aterm was a bundle's executable, else the file.
    root: PathBuf,
}

/// Copies `aterm` into the world, runs the copy once with `--version`, and evicts the copy's
/// pages (`--pages cold`).
/// # Errors
/// The copy, its run or its eviction failed, or a page of it stayed resident (or it had none).
/// A copy whose run gave no proof keeps the world: macOS may still be assessing it.
fn cold_copy(world: &mut World, aterm: &Path) -> Result<ColdCopy, Fail> {
    let aterm = std::fs::canonicalize(aterm)
        .map_err(|e| format!("instance: cannot resolve {}: {e}", aterm.display()))?;
    let aterm = aterm.as_path();
    let bundle = aterm
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"));
    let source = bundle.unwrap_or(aterm);
    let name = source
        .file_name()
        .ok_or_else(|| format!("instance: {} has no file name", source.display()))?;
    let root = world.dir.join(name);
    copy_tree(source, &root)
        .map_err(|e| format!("instance: cannot copy {}: {e}", source.display()))?;
    let exe = match bundle {
        Some(bundle) => root.join(aterm.strip_prefix(bundle).unwrap_or(aterm)),
        None => root.clone(),
    };
    prove_cold_version(world, &exe, |cmd| {
        aterm_uds::deadline::output_within(cmd, BOOT)
    })?;
    crate::sys::evict_pages(&root)
        .map_err(|e| format!("instance: cannot evict {}: {e}", root.display()))?;
    match crate::sys::resident_pages(&root) {
        Ok((0, pages)) if pages > 0 => Ok(ColdCopy { exe, root }),
        Ok((0, _)) => Err(Fail::die(format!(
            "instance: the copy {} holds no page to put out of the page cache",
            root.display()
        ))),
        Ok((resident, pages)) => Err(Fail::die(format!(
            "instance: {resident} of {pages} pages of {} stayed in the page cache",
            root.display()
        ))),
        Err(e) => Err(Fail::die(format!(
            "instance: cannot read {}'s pages: {e}",
            root.display()
        ))),
    }
}

/// Require a completed version probe before allowing this world's copy to be removed.
fn prove_cold_version(
    world: &mut World,
    exe: &Path,
    run: impl FnOnce(
        &mut aterm_uds::spawnfd::Command,
    ) -> Result<std::process::Output, aterm_uds::deadline::RunError>,
) -> Result<(), Fail> {
    let requested_keep = world.keep;
    world.keep();
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg("--version")
        .current_dir(&world.dir)
        .env_clear()
        .envs(world.env())
        .stdin(Stdio::null());
    let ran = run(&mut cmd);
    if !ran
        .as_ref()
        .is_ok_and(|out| out.status.success() && out.stdout.starts_with(b"aterm "))
    {
        return Err(Fail::die(format!(
            "instance: the copy {} did not complete its version probe ({}); the world is kept",
            exe.display(),
            ran.map_or_else(
                |e| e.to_string(),
                |out| format!(
                    "{}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            )
        )));
    }
    world.keep = requested_keep;
    Ok(())
}

/// Copy `from` to the new path `to`: a directory's tree, its symbolic links as links.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    copy_tree_kind(from, to, std::fs::symlink_metadata(from)?.file_type())
}

fn copy_tree_kind(from: &Path, to: &Path, kind: std::fs::FileType) -> std::io::Result<()> {
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if kind.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            copy_tree_kind(&entry.path(), &to.join(entry.file_name()), ft)?;
        }
        Ok(())
    } else if kind.is_file() {
        crate::util::copy_regular_file(from, to).map(drop)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing nonregular file {}", from.display()),
        ))
    }
}

/// [`CONFIG_OFF`] headed by `top`.
#[must_use]
pub fn config_with(top: &[String]) -> String {
    let mut config = String::new();
    for line in top {
        config.push_str(line);
        config.push('\n');
    }
    config.push_str(CONFIG_OFF);
    config
}

/// Enables the harness host when `harness` is set and appends `after` lines.
#[must_use]
pub fn config_after(config: &str, harness: bool, after: &[String]) -> String {
    let mut out = if harness {
        config.replace(
            "[harness]\nenabled = false\n",
            "[harness]\nenabled = true\n",
        )
    } else {
        config.to_owned()
    };
    for line in after {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Inserts `keys` under the root `[harness]` table after its `enabled` line.
#[must_use]
pub fn with_harness_keys(config: &str, keys: &[String]) -> String {
    let Some(table) = config.find("[harness]\n") else {
        return config.to_owned();
    };
    let head = table + "[harness]\n".len();
    let at = config[head..]
        .find('\n')
        .map_or(config.len(), |n| head + n + 1);
    let mut out = config[..at].to_owned();
    for key in keys {
        out.push_str(key);
        out.push('\n');
    }
    out.push_str(&config[at..]);
    out
}

/// Runs each control verb and prints its reply, stopping at the first failure.
/// # Errors
/// A control client failed; no later verb runs.
/// A signal came ([`Stopped`]): the verbs after it are not run, and the reply of the one it
/// came during, which it may have ended, is not printed.
fn run_verbs(verbs: &[String], mut ctl: impl FnMut(&[&str]) -> Reply) -> Result<(), Fail> {
    for verb in verbs {
        let words: Vec<&str> = verb.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let reply = ctl(&words);
        stop::check()?;
        let answer = if reply.stdout.trim().is_empty() {
            reply.stderr.trim_end()
        } else {
            reply.stdout.trim_end()
        };
        println!("ctl {verb} -> {answer}");
        if reply.code != 0 {
            return Err(Fail::die(format!(
                "instance: ctl {verb} failed (exit {}): {}",
                reply.code,
                reply.first()
            )));
        }
    }
    Ok(())
}

/// Request a frame after its terminal settles, or a successful roster proves none exists.
/// # Errors
/// A signal interrupted the request, the terminal did not settle, or the image command failed.
fn request_settled_image(mut ctl: impl FnMut(&[&str]) -> Reply) -> Result<Reply, Fail> {
    let roster = ctl(&["sessions"]);
    stop::check()?;
    let empty = roster.code == 0
        && roster.stdout.trim().is_empty()
        && roster
            .header()
            .strip_prefix("OK ")
            .and_then(|body| body.split_whitespace().next())
            == Some("0");
    if !empty {
        let idle = terminal_request_from_roster(
            &mut ctl,
            &["await", "idle", "500", "timeout=60000"],
            roster,
        );
        stop::check()?;
        if idle.code != 0 {
            return Err(Fail::die(format!(
                "instance: terminal did not settle (exit {}): {}",
                idle.code,
                idle.first()
            )));
        }
    }
    let image = ctl(&["image", "frame.png"]);
    stop::check()?;
    if image.code != 0 {
        return Err(Fail::die(format!("instance: image: {}", image.first())));
    }
    Ok(image)
}

#[cfg(test)]
#[path = "instance_control_verb_tests.rs"]
mod control_verb_tests;

#[cfg(test)]
#[path = "instance_image_reply_tests.rs"]
mod image_reply_tests;

/// Why a keeper ends its aterm.
enum KeeperEnd {
    /// The lifeline read end of file (or could not be read): the launcher is gone.
    Cut,
    /// `SIGTERM`, `SIGINT` or `SIGHUP` reached the keeper.
    Signal(i32),
}

/// `keeper`'s flag that starts its program as its own responsible process.
const DISCLAIM: &str = "--disclaim";

/// The keeper of a windowed instance (`aterm-lab keeper [--disclaim] -- <aterm> ARGS…`).
/// `--disclaim`, an app-band job's keeper, starts aterm as its own responsible process, as
/// launchd starts aterm.app, so macOS asks aterm's privacy questions of aterm and not of this
/// keeper; without it aterm is charged to whatever this keeper is charged to.
#[must_use]
pub fn keeper_main(args: &[String]) -> i32 {
    let split = args.iter().position(|a| a == "--").and_then(|at| {
        let mut flags: Vec<std::ffi::OsString> = args[..at].iter().map(Into::into).collect();
        let guard = aterm_types::lab_window_guard::take(&mut flags).ok()?;
        let disclaim = match flags.as_slice() {
            [] => false,
            [flag] if flag == DISCLAIM => true,
            _ => return None,
        };
        args[at + 1..]
            .split_first()
            .map(|(prog, rest)| (disclaim, guard, prog, rest))
    });
    let Some((disclaim, offered_guard, prog, rest)) = split else {
        eprintln!(
            "aterm-lab keeper: usage: keeper [{DISCLAIM}] -- PROGRAM ARGS (the lifeline on stdin)"
        );
        return 2;
    };
    let window_at = rest.iter().position(|word| word == "--window");
    if offered_guard.is_some() != window_at.is_some() {
        eprintln!("aterm-lab keeper: an explicit window requires its original lane description");
        return 2;
    }
    let window_guard = match offered_guard {
        Some(bundle) => {
            let expected = match crate::admission::window_path() {
                Ok(path) => path,
                Err(why) => {
                    eprintln!("aterm-lab keeper: window lane: {why}");
                    return 2;
                }
            };
            // SAFETY: keeper dispatch is before application threads/spawns. This is the
            // explicitly inherited, exclusively offered descriptor from the original run
            // (or its authenticated launchd transfer); that run retains genuine close-only
            // custody. The canonical account path is independently resolved. Adoption
            // consumes only a validated original slot and marks its retained copy CLOEXEC.
            #[allow(unsafe_code)]
            match unsafe {
                aterm_toolchain::record_lock::CloseOnlyLock::adopt_inherited(bundle.lane, &expected)
            } {
                Ok(guard) => {
                    let memory = if let Some(memory) = bundle.memory {
                        let ledger = match expected.parent() {
                            Some(directory) => aterm_capacity::Ledger::at(directory),
                            None => {
                                eprintln!(
                                    "aterm-lab keeper: original memory ledger is unavailable"
                                );
                                return 2;
                            }
                        };
                        // SAFETY: the exact second role was offered exclusively by the same
                        // original native handoff before threads, with continuous genuine
                        // detached claim custody. Core verifies its record, not argv budget.
                        #[allow(unsafe_code)]
                        match unsafe {
                            aterm_capacity::RetainedClaim::adopt_inherited(
                                memory.descriptor,
                                &ledger,
                                &memory.claim,
                            )
                        } {
                            Ok(memory) => Some(memory),
                            Err(why) => {
                                eprintln!("aterm-lab keeper: memory guard adoption: {why}");
                                return 2;
                            }
                        }
                    } else {
                        None
                    };
                    Some((guard, memory))
                }
                Err(why) => {
                    eprintln!("aterm-lab keeper: window guard adoption: {why}");
                    return 2;
                }
            }
        }
        None => None,
    };
    let lifeline = match aterm_uds::lifeline::adopt(aterm_uds::lifeline::STDIN) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("aterm-lab keeper: stdin is not a lifeline: {e}");
            return 2;
        }
    };
    let _ = aterm_uds::lifeline::close_inherited_copies(&lifeline);
    let signals =
        match crate::sys::SignalPipe::install(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aterm-lab keeper: cannot catch signals: {e}");
                return 1;
            }
        };
    let mut command = aterm_uds::spawnfd::command(prog);
    if let Some((lane, memory)) = &window_guard {
        // The GUI owns an independent lifetime after either controller dies. In the
        // app band, launchd retires the keeper's process group on death/restart; leave
        // that group before executing the GUI while retaining the exact original FDs.
        // Normal keeper teardown still signals and reaps its actual GUI Child below.
        command.setsid(true);
        let forwarded = lane
            .try_clone_fd()
            .map_err(|why| why.to_string())
            .and_then(|lane| {
                crate::window_guards::WindowGuards::from_original(lane, memory.as_ref())
            });
        let guards = match forwarded {
            Ok(guards) => guards,
            Err(why) => {
                eprintln!("aterm-lab keeper: original guard duplication: {why}");
                return 1;
            }
        };
        let claim = guards.claim().map(str::to_owned);
        if let Err(why) = command.inherit_guards(guards.into_descriptors()) {
            eprintln!("aterm-lab keeper: GUI guard inheritance: {why}");
            return 1;
        }
        let slots: Vec<_> = command.guard_targets().collect();
        let arguments =
            match crate::window_guards::WindowGuards::arguments(&slots, claim.as_deref()) {
                Ok(arguments) => arguments,
                Err(why) => {
                    eprintln!("aterm-lab keeper: GUI guard roles: {why}");
                    return 1;
                }
            };
        let at = window_at.expect("guarded window boundary was checked");
        command.args(&rest[..at]).args(arguments).args(&rest[at..]);
    } else {
        command.args(rest);
    }
    let mut child = match command
        .stdin(Stdio::null())
        .disclaim_responsibility(disclaim)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-lab keeper: cannot start {prog}: {e}");
            return 1;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let cut = tx.clone();
    std::thread::spawn(move || {
        let _ = aterm_uds::lifeline::wait_for_cut(lifeline);
        let _ = cut.send(KeeperEnd::Cut);
    });
    std::thread::spawn(move || {
        let sig = signals.wait().first().copied().unwrap_or(libc::SIGTERM);
        let _ = tx.send(KeeperEnd::Signal(sig));
    });
    let end = loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status.code().unwrap_or(1);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(why) => break why,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break KeeperEnd::Cut,
        }
    };
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let _ = crate::sys::kill(pid, libc::SIGTERM);
    let grace = Instant::now() + Duration::from_secs(5);
    while Instant::now() < grace && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    match end {
        KeeperEnd::Cut => 0,
        KeeperEnd::Signal(sig) => 128 + sig,
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "instance_owned_world_store_tests.rs"]
mod owned_world_store_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "instance_cold_copy_tests.rs"]
mod cold_copy_tests;

#[cfg(test)]
#[path = "instance_terminal_selection_tests.rs"]
mod terminal_selection_tests;

#[cfg(test)]
#[path = "instance_launch_line_tests.rs"]
mod launch_line_tests;

#[cfg(test)]
#[path = "instance_world_tests.rs"]
mod world_tests;

#[cfg(test)]
#[path = "instance_bell_off_tests.rs"]
mod bell_off_tests;

#[cfg(test)]
#[path = "instance_harness_key_tests.rs"]
mod harness_key_tests;

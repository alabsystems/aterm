// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private headless `aterm` worlds for `fleet-sim`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Command, Stdio};

use crate::procs::{self, alive};
use crate::stop;

/// One-minute hang detector for operations that must complete.
pub const HANG: Duration = Duration::from_secs(60);

/// Soft and hard file descriptor limit for each simulated instance and reader.
pub const NOFILE: usize = 256;

/// Terminal columns for each simulated instance.
pub const COLUMNS: u16 = 100;
/// Terminal lines for each simulated instance.
pub const LINES: u16 = 30;

/// The aterm.toml every instance starts with: the live tests' isolation table
/// (`crates/aterm/tests/support/launch_isolation.rs` `CONFIG_OFF`): updates,
/// packages and the agent harness off, so nothing types into a fixture and
/// nothing reaches the network.
const CONFIG: &str = "agents_auto_prime = false\n\
                      [update]\nenabled = false\nauto_apply = false\n\
                      [packages]\nenabled = false\n\
                      [machine]\nuniversal_control = \"leave\"\n\
                      [harness]\nenabled = false\n";

/// What a session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A shell at its prompt.
    Shell,
    /// A shell running `build-loop`, which prints progress lines.
    Build,
    /// The `claude` fixture in one of its states.
    Claude(crate::fleet_fixture::Mode),
}

impl Kind {
    /// The `program=` the survey must name.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Kind::Shell => "zsh",
            Kind::Build => "build-loop",
            Kind::Claude(_) => "claude",
        }
    }

    /// The `agent=` word the survey must name.
    #[must_use]
    pub fn agent(self) -> &'static str {
        match self {
            Kind::Claude(mode) => mode.agent_word(),
            _ => "-",
        }
    }
}

/// One session the simulation made, and what it runs.
#[derive(Clone, Debug)]
pub struct Sess {
    /// Its sid.
    pub sid: String,
    /// What it runs.
    pub kind: Kind,
    /// The `<n>` its fixture was started with (its words); 0 for the shell.
    pub n: usize,
}

/// One instance.
pub struct Inst {
    /// Its index (`m00` is the fleet's issuer and the reader's machine).
    pub i: usize,
    /// Its scratch world.
    pub root: PathBuf,
    /// Its control socket.
    pub sock: String,
    /// Its fleet port on 127.0.0.1.
    pub port: u16,
    /// The instance's pid (the launcher shell exec'd into it).
    pub pid: i32,
    /// Recorded `(pid, start_time)` pairs of descendants for teardown.
    pub seen: Vec<(i32, String)>,
    /// Its sessions, the boot session first.
    pub sessions: Vec<Sess>,
    /// SIGSTOPped by the fault phase (continued before the teardown).
    pub stopped: bool,
    /// Killed by the fault phase.
    pub killed: bool,
    /// The lifeline the instance watches; cut when dropped.
    _lifeline: aterm_uds::lifeline::Lifeline,
}

impl Inst {
    /// `home/`.
    #[must_use]
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// `state/`.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `cfg/aterm/aterm.toml`.
    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.root.join("cfg/aterm/aterm.toml")
    }

    /// The boot session's sid.
    #[must_use]
    pub fn boot_sid(&self) -> &str {
        self.sessions.first().map_or("", |s| s.sid.as_str())
    }

    /// Records current descendant PIDs and start times so teardown can clean up orphans.
    pub fn note_descendants(&mut self) {
        for pid in procs::descendants(self.pid) {
            let started = procs::start_time(pid);
            if !started.is_empty() && !self.seen.iter().any(|(p, _)| *p == pid) {
                self.seen.push((pid, started));
            }
        }
    }
}

/// Builds a command that execs `program` under a soft and hard descriptor limit of [`NOFILE`].
#[must_use]
pub fn limited(name: &str, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg(format!("ulimit -n {NOFILE} && exec \"$@\""))
        .arg(name)
        .arg(program);
    cmd
}

/// What a client process said.
#[derive(Debug, Default, Clone)]
pub struct Out {
    /// Its exit status (`-1`: none, killed by a signal).
    pub code: i32,
    /// Its standard output.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

/// The simulation's machines.
pub struct World {
    /// `/tmp/af.XXXXXX`.
    pub base: PathBuf,
    /// `<base>/bin` directory holding `aterm` and fixture symlinks.
    pub bin: PathBuf,
    /// The aterm under test.
    pub aterm: PathBuf,
    /// The instances.
    pub insts: Vec<Inst>,
    /// Keep `<base>` after the teardown (`--keep`).
    pub keep: bool,
}

/// Returns the isolated environment variables for an instance world rooted at `root`.
#[must_use]
pub fn world_env(root: &Path, bin: &Path) -> Vec<(String, String)> {
    let p = crate::paths::paths();
    let r = |d: &str| root.join(d).to_string_lossy().into_owned();
    vec![
        ("HOME".into(), r("home")),
        ("XDG_CONFIG_HOME".into(), r("cfg")),
        ("XDG_RUNTIME_DIR".into(), r("run")),
        ("XDG_CACHE_HOME".into(), r("cache")),
        ("XDG_DATA_HOME".into(), r("data")),
        ("XDG_STATE_HOME".into(), r("state")),
        ("ATERM_STATE_HOME".into(), r("state")),
        ("TMPDIR".into(), r("tmp")),
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
        ),
        ("SHELL".into(), "/bin/zsh".into()),
        ("USER".into(), p.user.clone()),
        ("LOGNAME".into(), p.user.clone()),
        ("LANG".into(), "en_US.UTF-8".into()),
        ("xcrun_nocache".into(), "1".into()),
    ]
}

/// Creates the private directory layout and `aterm.toml` for a world at `root`.
///
/// # Errors
/// A directory or the config could not be made.
pub fn make_world(root: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for d in [
        "",
        "home",
        "cfg",
        "cfg/aterm",
        "run",
        "run/aterm",
        "cache",
        "data",
        "state",
        "tmp",
    ] {
        let dir = root.join(d);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        crate::instance::bell_off(CONFIG),
    )
}

/// Allocates `n` distinct ephemeral loopback TCP ports.
///
/// # Errors
/// A bind failed.
pub fn free_ports(n: usize) -> std::io::Result<Vec<u16>> {
    let held: Vec<std::net::TcpListener> = (0..n)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0"))
        .collect::<Result<_, _>>()?;
    held.iter()
        .map(|l| l.local_addr().map(|a| a.port()))
        .collect()
}

impl World {
    /// A fresh base with its `bin/`.
    ///
    /// # Errors
    /// The base or a link could not be made.
    pub fn new(aterm: &Path, keep: bool) -> Result<World, String> {
        let base = crate::sys::make_temp_dir("/tmp/af.XXXXXX")
            .map(PathBuf::from)
            .map_err(|e| format!("cannot make the simulation's base under /tmp: {e}"))?;
        let bin = base.join("bin");
        let me = &crate::paths::paths().exe;
        let made = std::fs::create_dir_all(&bin).and_then(|()| {
            for name in ["claude", "build-loop"] {
                std::os::unix::fs::symlink(me, bin.join(name))?;
            }
            std::os::unix::fs::symlink(aterm, bin.join("aterm"))
        });
        let world = World {
            base,
            bin,
            aterm: aterm.to_path_buf(),
            insts: Vec::new(),
            keep,
        };
        made.map_err(|e| format!("cannot lay out {}: {e}", world.bin.display()))?;
        Ok(world)
    }

    /// `<base>/mNN`.
    #[must_use]
    pub fn root_of(&self, i: usize) -> PathBuf {
        self.base.join(format!("m{i:02}"))
    }

    /// Starts instance `i` on fleet port `port` and waits until its control socket listens.
    ///
    /// # Errors
    /// Returns an error if the world cannot be created, the instance fails to start, or its socket does not listen within [`HANG`].
    pub fn start(&self, i: usize, port: u16) -> Result<Inst, String> {
        let root = self.root_of(i);
        make_world(&root).map_err(|e| format!("m{i:02}: cannot make its world: {e}"))?;
        let sock = root.join("run/aterm/aterm.sock");
        let log = root.join("gui.log");
        let out = std::fs::File::create(&log).map_err(|e| format!("m{i:02}: its log: {e}"))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let home = root.join("home");
        let mut cmd = limited("fleet-sim-instance", &self.aterm);
        cmd.env_clear()
            .envs(world_env(&root, &self.bin))
            .args(["--headless", "--no-reroute"])
            .arg("--control-sock")
            .arg(&sock)
            .arg("-d")
            .arg(&home)
            .args([
                "--columns",
                &COLUMNS.to_string(),
                "--lines",
                &LINES.to_string(),
            ])
            .current_dir(&home)
            .setsid(true)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut cmd, &root)
            .map_err(|e| format!("m{i:02}: the lifeline: {e}"))?;
        let pid = procs::spawn_kept(&mut cmd).map_err(|e| format!("m{i:02}: cannot start: {e}"))?;
        let inst = Inst {
            i,
            sock: sock.to_string_lossy().into_owned(),
            root,
            port,
            pid,
            seen: Vec::new(),
            sessions: Vec::new(),
            stopped: false,
            killed: false,
            _lifeline: lifeline,
        };
        let end = Instant::now() + HANG;
        loop {
            if !alive(pid) {
                return Err(format!(
                    "m{i:02} (pid {pid}) exited before its control socket listened; its log:\n{}",
                    tail(&log, 12)
                ));
            }
            if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                break;
            }
            if Instant::now() >= end {
                return Err(format!(
                    "m{i:02}: no control socket listened within {HANG:?}; its log:\n{}",
                    tail(&log, 12)
                ));
            }
            stop::sleep(Duration::from_millis(50)).map_err(|_| "interrupted".to_string())?;
        }
        let logs = inst.state().join("logs");
        let end = Instant::now() + HANG;
        while !logs.is_dir() {
            if Instant::now() >= end {
                return Err(format!(
                    "m{i:02} did not take ATERM_STATE_HOME ({} was built without the dev \
                     seams): targo --unverified build -p aterm --features dev-seams",
                    self.aterm.display()
                ));
            }
            stop::sleep(Duration::from_millis(50)).map_err(|_| "interrupted".to_string())?;
        }
        Ok(inst)
    }

    /// Runs `aterm <args>` in instance world `root`'s environment with optional `stdin`.
    #[must_use]
    pub fn aterm_in(&self, root: &Path, args: &[&str], stdin: Option<&str>) -> Out {
        let mut cmd = aterm_uds::spawnfd::command(&self.aterm);
        cmd.env_clear()
            .envs(world_env(root, &self.bin))
            .args(args)
            .current_dir(root.join("home"));
        finish(&mut cmd, stdin)
    }

    /// `aterm ctl --sock <sock> -- <args>` against `inst`.
    #[must_use]
    pub fn ctl(&self, inst: &Inst, args: &[&str]) -> Out {
        let mut all = vec!["ctl", "--sock", inst.sock.as_str(), "--"];
        all.extend_from_slice(args);
        self.aterm_in(&inst.root, &all, None)
    }

    /// [`World::ctl`] that must exit 0: its stdout.
    ///
    /// # Errors
    /// It did not.
    pub fn ctl_ok(&self, inst: &Inst, args: &[&str]) -> Result<String, String> {
        let out = self.ctl(inst, args);
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(format!(
                "m{:02}: aterm ctl {args:?} exited {}: {}{}",
                inst.i,
                out.code,
                out.stdout.trim(),
                out.stderr.trim()
            ))
        }
    }

    /// Type `line` into `sid` of `inst` and press Enter.
    ///
    /// # Errors
    /// A client call failed.
    pub fn type_line(&self, inst: &Inst, sid: &str, line: &str) -> Result<(), String> {
        let at = format!("@{sid}");
        self.ctl_ok(inst, &[&at, "send", line])?;
        self.ctl_ok(inst, &[&at, "key", "enter"]).map(|_| ())
    }

    /// The `fleet=` word of `inst`'s boot session's `status`.
    #[must_use]
    pub fn fleet_word(&self, inst: &Inst) -> String {
        let at = format!("@{}", inst.boot_sid());
        let out = self.ctl(inst, &[&at, "status"]);
        out.stdout
            .split_whitespace()
            .find_map(|w| w.strip_prefix("fleet="))
            .unwrap_or("?")
            .to_owned()
    }

    /// Wait (the hang detector) until `inst`'s listener reads `fleet=listening`.
    ///
    /// # Errors
    /// It did not, or the run was interrupted.
    pub fn await_listening(&self, inst: &Inst) -> Result<(), String> {
        let end = Instant::now() + HANG;
        let mut last = String::new();
        while Instant::now() < end {
            last = self.fleet_word(inst);
            if last == "listening" {
                return Ok(());
            }
            stop::sleep(Duration::from_millis(200)).map_err(|_| "interrupted".to_string())?;
        }
        Err(format!(
            "m{:02}: fleet={last}, never `listening` within {HANG:?}",
            inst.i
        ))
    }

    /// Terminates all instances and descendants by PID and removes `<base>` unless `--keep`.
    pub fn teardown(&mut self) {
        let table = procs::table();
        let mut every: Vec<(i32, String)> = Vec::new();
        for inst in &self.insts {
            if inst.stopped {
                let _ = crate::sys::kill(inst.pid, libc::SIGCONT);
            }
            if !inst.killed {
                for pid in procs::descendants_in(&table, inst.pid) {
                    every.push((pid, procs::start_time(pid)));
                }
            }
            every.extend(inst.seen.iter().cloned());
        }
        for inst in self.insts.iter().filter(|i| !i.killed) {
            if alive(inst.pid) {
                let _ = crate::sys::kill(inst.pid, libc::SIGTERM);
            }
        }
        for inst in self.insts.iter().filter(|i| !i.killed) {
            if !procs::wait_gone(inst.pid, Duration::from_secs(5)) {
                let _ = crate::sys::kill(inst.pid, libc::SIGKILL);
                let _ = procs::wait_gone(inst.pid, Duration::from_secs(5));
            }
        }
        every.sort();
        every.dedup();
        for (pid, started) in every {
            if !started.is_empty() && crate::sys::alive(pid) && procs::start_time(pid) == started {
                let _ = crate::sys::kill(pid, libc::SIGKILL);
            }
        }
        self.insts.clear();
        if !self.keep && self.base.to_string_lossy().starts_with("/tmp/af.") {
            for _ in 0..40 {
                if std::fs::remove_dir_all(&self.base).is_ok() || !self.base.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Runs `cmd` to completion with optional `stdin` and captures stdout and stderr.
#[must_use]
pub fn finish(cmd: &mut Command, stdin: Option<&str>) -> Out {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Out {
                code: -1,
                stderr: format!("could not start: {e}"),
                ..Out::default()
            };
        }
    };
    if let Some(text) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        use std::io::Write as _;
        let _ = pipe.write_all(text.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Out {
            code: -1,
            stderr: format!("could not be read: {e}"),
            ..Out::default()
        },
    }
}

/// The last `n` lines of the file at `path`.
#[must_use]
pub fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Each of `pids`' resident set in KiB (`ps -o pid=,rss=`), one call.
#[must_use]
pub fn rss_kib(pids: &[i32]) -> std::collections::BTreeMap<i32, u64> {
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    let out = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "pid=,rss=", "-p", &list.join(",")])
        .stdin(Stdio::null())
        .output();
    let mut map = std::collections::BTreeMap::new();
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut w = line.split_whitespace();
            if let (Some(Ok(pid)), Some(Ok(rss))) =
                (w.next().map(str::parse), w.next().map(str::parse))
            {
                map.insert(pid, rss);
            }
        }
    }
    map
}

/// Returns each PID's physical footprint in KiB ([`crate::sys::phys_footprint`]).
#[must_use]
pub fn footprint_kib(pids: &[i32]) -> std::collections::BTreeMap<i32, u64> {
    pids.iter()
        .filter_map(|&pid| crate::sys::phys_footprint(pid).map(|b| (pid, b / 1024)))
        .collect()
}

/// `vmmap --summary` `Malloc Large (empty)` row values.
#[derive(Clone, Debug)]
pub struct LargeEmpty {
    /// Its virtual size.
    pub virt: String,
    /// What of it is resident.
    pub resident: String,
    /// What of it is dirty.
    pub dirty: String,
}

/// Reads the `Malloc Large (empty)` row from `vmmap --summary <pid>`, if present.
#[must_use]
pub fn malloc_large_empty(pid: i32) -> Option<LargeEmpty> {
    let out = aterm_uds::spawnfd::command("/usr/bin/vmmap")
        .args(["--summary", &pid.to_string()])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let row = text
        .lines()
        .find_map(|l| l.strip_prefix("Malloc Large (empty)"))?;
    let mut w = row.split_whitespace().map(str::to_owned);
    Some(LargeEmpty {
        virt: w.next()?,
        resident: w.next()?,
        dirty: w.next()?,
    })
}

/// Counts open numbered file descriptors for each PID in `pids` via `lsof`.
#[must_use]
pub fn open_fds(pids: &[i32]) -> std::collections::BTreeMap<i32, usize> {
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    let out = aterm_uds::spawnfd::command("/usr/sbin/lsof")
        .args([
            "-n",
            "-P",
            "-w",
            "-a",
            "-p",
            &list.join(","),
            "-d",
            "0-65535",
        ])
        .stdin(Stdio::null())
        .output();
    let mut map = std::collections::BTreeMap::new();
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
            if let Some(Ok(pid)) = line.split_whitespace().nth(1).map(str::parse::<i32>) {
                *map.entry(pid).or_insert(0) += 1;
            }
        }
    }
    map
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated private `aterm` instance runner for `lab instance` and lab proof lanes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::band::{Band, Job};
use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

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

/// How long an instance may take to listen.
pub const BOOT: Duration = Duration::from_secs(60);

/// The scratch world an instance runs in.
pub struct World {
    /// The `/tmp/at.*` directory holding everything below.
    pub dir: PathBuf,
    pub home: PathBuf,
    pub cfg: PathBuf,
    /// `XDG_RUNTIME_DIR`: the control socket's directory.
    pub run: PathBuf,
    pub sock: PathBuf,
    keep: bool,
}

impl World {
    /// Lay out a fresh world whose `aterm.toml` is `config` with the audible bell off.
    /// # Errors
    /// A directory or file could not be made.
    pub fn new(config: &str) -> Result<World, String> {
        let dir = crate::sys::make_temp_dir("/tmp/at.XXXXXX")
            .map_err(|e| format!("cannot make a scratch directory under /tmp: {e}"))?;
        let dir = PathBuf::from(dir);
        let dir = std::fs::canonicalize(&dir).map_err(|e| {
            let _ = std::fs::remove_dir(&dir);
            format!(
                "cannot resolve the newly owned scratch directory {}: {e}",
                dir.display()
            )
        })?;
        let w = World {
            home: dir.join("home"),
            cfg: dir.join("cfg"),
            run: dir.join("run"),
            sock: dir.join("run/a.sock"),
            dir,
            keep: false,
        };
        let made = (|| -> std::io::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;
            for d in [&w.home, &w.cfg.join("aterm"), &w.run] {
                std::fs::create_dir_all(d)?;
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
            }
            std::fs::write(w.cfg.join("aterm/aterm.toml"), bell_off(config))?;
            private_home_roots(&w.home)?;
            std::fs::write(w.home.join(".profile"), "PS1='$ '\n")?;
            std::fs::write(w.home.join(".shrc"), "PS1='$ '\n")
        })();
        made.map_err(|e| format!("cannot lay out {}: {e}", w.dir.display()))?;
        Ok(w)
    }

    /// Leave the world in place when it drops, and say where.
    pub const fn keep(&mut self) {
        self.keep = true;
    }

    /// Put its `HOME` at a new `home` (made `0700`, with the world's shell profile).
    /// # Errors
    /// The folder already exists, or it or its profile could not be made.
    pub fn rehome(&mut self, home: &Path) -> Result<(), String> {
        let made = (|| -> std::io::Result<PathBuf> {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::create_dir(home)?;
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
            let home = std::fs::canonicalize(home)?;
            private_home_roots(&home)?;
            std::fs::write(home.join(".profile"), "PS1='$ '\n")?;
            std::fs::write(home.join(".shrc"), "PS1='$ '\n")?;
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
        vec![
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
            crate::tape::seam(),
        ]
    }
}

/// Roots for a newly owned HOME, on its volume even when `rehome` moved it.
fn private_home_roots(home: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for path in [
        home.join(".cache"),
        home.join(".local/share"),
        home.join(".local/state"),
        home.join("tmp"),
        private_state_home(home),
    ] {
        std::fs::create_dir_all(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Keep the platform's existing state location explicit.
fn private_state_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return home.join("Library/Application Support/aterm");
    #[cfg(not(target_os = "macos"))]
    home.join(".local/state/aterm")
}

impl Drop for World {
    fn drop(&mut self) {
        if self.keep {
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
}

/// An instance's process.
enum Proc {
    Child(Child),
    Job(Job),
}

/// A running private instance.
pub struct Instance {
    child: Proc,
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    /// The instance's pid; a windowed one's keeper's ([`keeper_main`]).
    pub pid: i32,
    pub aterm: PathBuf,
    /// The aterm whose `ctl` reads it (the instance's own by default).
    pub client: PathBuf,
    pub sock: PathBuf,
    /// The instance's stdout and stderr.
    pub log: PathBuf,
    env: Vec<(String, String)>,
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
fn terminal_selector(roster: &str) -> Option<String> {
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

fn terminal_request(mut ctl: impl FnMut(&[&str]) -> Reply, args: &[&str]) -> Reply {
    let roster = ctl(&["sessions"]);
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

impl Instance {
    /// Start `l` in `world`, its output in `<world>/aterm.log`.
    /// # Errors
    /// The log, the lifeline or the process could not be made.
    pub fn start(world: &World, l: Launch) -> Result<Instance, String> {
        let log = world.dir.join("aterm.log");
        let out = std::fs::File::create(&log)
            .map_err(|e| format!("cannot create {}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("cannot share the instance log: {e}"))?;
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.dir)
            .map_err(|e| format!("the lifeline: {e}"))?;
        let mut line: Vec<String> = Vec::new();
        if !l.headless {
            let exe = paths().exe.to_string_lossy().into_owned();
            line.extend([exe, "keeper".to_string(), "--".to_string()]);
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
        }
        line.extend([
            "--no-reroute".to_string(),
            "--control-sock".to_string(),
            world.sock.to_string_lossy().into_owned(),
        ]);
        line.extend(l.args);
        let (program, argv) = line.split_first().ok_or("an empty command line")?;
        let child = aterm_uds::spawnfd::command(program)
            .env_clear()
            .envs(l.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(argv)
            .current_dir(&l.cwd)
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("cannot start {program}: {e}"))?;
        let pid = i32::try_from(child.id()).unwrap_or(0);
        Ok(Instance {
            child: Proc::Child(child),
            lifeline: Some(lifeline),
            pid,
            client: l.aterm.clone(),
            aterm: l.aterm,
            sock: world.sock.clone(),
            log,
            env: world.env(),
        })
    }

    /// [`Instance::start`] in `band`.
    /// # Errors
    /// As [`Instance::start`]; a windowed or sandboxed `l` in the app band; the job could not
    /// start.
    pub fn start_in(world: &World, l: Launch, band: Band) -> Result<Instance, String> {
        if band == Band::Agent {
            return Instance::start(world, l);
        }
        if !l.headless || l.sandbox.is_some() {
            return Err("the app band starts a headless, unsandboxed instance only".into());
        }
        let log = world.dir.join("aterm.log");
        std::fs::File::create(&log).map_err(|e| format!("cannot create {}: {e}", log.display()))?;
        let mut line = vec![
            l.aterm.to_string_lossy().into_owned(),
            "--headless".to_string(),
            aterm_uds::lifeline::FLAG.to_string(),
            aterm_uds::lifeline::STDIN.to_string(),
            "--no-reroute".to_string(),
            "--control-sock".to_string(),
            world.sock.to_string_lossy().into_owned(),
        ];
        line.extend(l.args);
        let job = Job::start(&world.dir, &line, &l.env, &l.cwd, &log)?;
        Ok(Instance {
            pid: job.pid,
            child: Proc::Job(job),
            lifeline: None,
            client: l.aterm.clone(),
            aterm: l.aterm,
            sock: world.sock.clone(),
            log,
            env: world.env(),
        })
    }

    /// Wait until the control socket answers `version` (at most `limit`).
    /// # Errors
    /// Interrupted, the instance exited first, or it never answered (its log tail says why).
    pub fn await_socket(&mut self, limit: Duration) -> Result<(), Fail> {
        let end = Instant::now() + limit;
        loop {
            if self.sock.exists() && self.ctl(&["version"]).code == 0 {
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
        let out = aterm_uds::spawnfd::command(&self.client)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(&argv)
            .stdin(Stdio::null())
            .output();
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
        let body = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }

    /// End it: cut its lifeline.
    pub fn stop(&mut self) {
        if let Some(l) = self.lifeline.take() {
            l.cut();
        }
        if let Proc::Job(j) = &mut self.child {
            j.cut();
            let end = Instant::now() + Duration::from_secs(8);
            while Instant::now() < end && j.running() {
                std::thread::sleep(Duration::from_millis(50));
            }
            j.remove();
            return;
        }
        for (grace, sig) in [(8, libc::SIGTERM), (5, libc::SIGKILL)] {
            let end = Instant::now() + Duration::from_secs(grace);
            while Instant::now() < end && self.running() {
                std::thread::sleep(Duration::from_millis(50));
            }
            if !self.running() {
                break;
            }
            let _ = crate::sys::kill_group(self.pid, sig);
        }
        match &mut self.child {
            Proc::Child(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            Proc::Job(j) => j.remove(),
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop();
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
    v.parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| Fail::usage(format!("{flag} takes a positive count, not '{v}'")))
}

/// The aterm to drive: the `--aterm` value given, else the lab's own pick.
/// # Errors
/// Why there is none to drive.
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

/// `instance`: a private aterm, headless unless `--window`, held until this command ends.
/// # Errors
/// Usage, or the instance could not start or answer.
pub fn cmd_instance(args: &[String]) -> Res {
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
    let image = take_value(&mut a, "--image")?.map(|p| from_caller(&p));
    let toml = take_values(&mut a, "--toml")?;
    let toml_after = take_values(&mut a, "--toml-after")?;
    let harness_keys = take_values(&mut a, "--harness-key")?;
    let extra_env =
        crate::lab::instance_env(&take_values(&mut a, "--env")?.join(" ")).map_err(Fail::usage)?;
    let home = take_value(&mut a, "--home")?.map(|p| from_caller(&p));
    let harness = take_flag(&mut a, "--harness");
    let pre = take_values(&mut a, "--pre")?;
    let post = take_values(&mut a, "--post")?;
    let keep = take_flag(&mut a, "--keep");
    let window = take_flag(&mut a, "--window");
    let band = Band::pick(take_value(&mut a, "--band")?.as_deref(), window)?;
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
    if !post.is_empty() && image.is_none() {
        return Err(Fail::usage(
            "instance: --post runs after --image's frame; drive a held instance with `aterm ctl` instead",
        ));
    }
    let aterm = resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    let config = config_after(&config_with(&toml), harness, &toml_after);
    let mut world = World::new(&with_harness_keys(&config, &harness_keys))?;
    if keep {
        world.keep();
    }
    if let Some(home) = home {
        world.rehome(Path::new(&home))?;
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
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm,
            headless: !window,
            args: flags,
            env,
            cwd: world.home.clone(),
            sandbox: None,
        },
        band,
    )?;
    inst.await_socket(BOOT)?;
    let ready = terminal_request(|args| inst.ctl(args), &["ready"]);
    if ready.code != 0 {
        return Err(Fail::die(format!("instance: not ready: {}", ready.first())));
    }
    run_verbs(&inst, &pre);
    if let Some(dest) = image {
        await_band(&inst)?;
        let _ = terminal_request(
            |args| inst.ctl(args),
            &["await", "idle", "500", "timeout=60000"],
        );
        let shot = inst.ctl(&["image", "frame.png"]);
        let path = shot
            .stdout
            .split_whitespace()
            .nth(3)
            .unwrap_or_default()
            .to_string();
        if shot.code != 0 || path.is_empty() {
            return Err(Fail::die(format!("instance: image: {}", shot.first())));
        }
        std::fs::copy(&path, &dest).map_err(|e| format!("instance: cannot write {dest}: {e}"))?;
        println!("{} -> {dest}", shot.first());
        run_verbs(&inst, &post);
        return Ok(0);
    }
    let coalition = crate::sys::coalition_ids(inst.pid)
        .map_or_else(|| "?".to_string(), |[resource, _]| resource.to_string());
    let role =
        crate::sys::darwin_role(inst.pid).map_or_else(|_| "?".to_string(), |r| r.to_string());
    let main_pri = crate::sys::main_thread_priority(inst.pid)
        .map_or_else(|| "?".to_string(), |p| p.to_string());
    let pages = cold.as_ref().map_or_else(String::new, |c| {
        crate::sys::resident_pages(&c.root).map_or_else(
            |e| format!(" pages=cold resident_at_ready=? ({e})"),
            |(resident, pages)| format!(" pages=cold resident_at_ready={resident}/{pages}"),
        )
    });
    println!(
        "instance pid={} sock={} band={} coalition={coalition} role={role} main_pri={main_pri}{pages}\n  drive it: {} ctl --sock {} <verb>   (image, text, send --stdin, metrics percentiles, …)\n  \
         world:    {}{}\n  it ends when this command does (Ctrl-C, SIGTERM)",
        inst.pid,
        inst.sock.display(),
        band.name(),
        inst.aterm.display(),
        inst.sock.display(),
        world.dir.display(),
        if keep {
            " (kept)"
        } else {
            " (removed at the end)"
        }
    );
    loop {
        if !inst.running() {
            return Err(Fail::die(format!(
                "instance: it exited; its log:\n{}",
                inst.log_tail(20)
            )));
        }
        if let Err(Stopped(sig)) = stop::sleep(Duration::from_millis(500)) {
            println!("instance: ended (signal {sig})");
            return Ok(0);
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
    let ran = aterm_uds::spawnfd::command(&exe)
        .arg("--version")
        .env_clear()
        .envs(world.env())
        .stdin(Stdio::null())
        .output();
    if !ran
        .as_ref()
        .is_ok_and(|out| out.stdout.starts_with(b"aterm "))
    {
        world.keep();
        return Err(Fail::die(format!(
            "instance: the copy {} never printed its version ({}); the world is kept",
            exe.display(),
            ran.map_or_else(
                |e| e.to_string(),
                |out| String::from_utf8_lossy(&out.stderr).trim().to_string()
            )
        )));
    }
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

/// Copy `from` to the new path `to`: a directory's tree, its symbolic links as links.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(drop)
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

/// Whether a `messages live` reply names a row with `reveal=pending`.
fn reveal_pending(rows: &str) -> bool {
    rows.lines()
        .any(|row| row.starts_with("message ") && row.split(' ').any(|w| w == "reveal=pending"))
}

/// The band settled: no live row still in its reveal's grace, read through `messages live`.
/// # Errors
/// A row still pending after a minute, or the run interrupted.
fn await_band(inst: &Instance) -> Result<(), Fail> {
    let start = Instant::now();
    loop {
        let rows = inst.ctl(&["messages", "live"]);
        if rows.code != 0 || !reveal_pending(&rows.stdout) {
            return Ok(());
        }
        if start.elapsed() >= BOOT {
            return Err(Fail::die(format!(
                "instance: a band row is still in its reveal's grace after {}s: {}",
                BOOT.as_secs(),
                rows.first()
            )));
        }
        stop::sleep(Duration::from_millis(100))
            .map_err(|Stopped(sig)| Fail::die(format!("instance: ended (signal {sig})")))?;
    }
}

/// Runs each control verb against `inst` and prints its reply.
fn run_verbs(inst: &Instance, verbs: &[String]) {
    for verb in verbs {
        let words: Vec<&str> = verb.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let reply = inst.ctl(&words);
        let answer = if reply.stdout.trim().is_empty() {
            reply.stderr.trim_end()
        } else {
            reply.stdout.trim_end()
        };
        println!("ctl {verb} -> {answer}");
    }
}

/// Why a keeper ends its aterm.
enum KeeperEnd {
    /// The lifeline read end of file (or could not be read): the launcher is gone.
    Cut,
    /// `SIGTERM`, `SIGINT` or `SIGHUP` reached the keeper.
    Signal(i32),
}

/// The keeper of a windowed instance (`aterm-lab keeper -- <aterm> ARGS…`).
#[must_use]
pub fn keeper_main(args: &[String]) -> i32 {
    let Some((prog, rest)) = args
        .iter()
        .position(|a| a == "--")
        .and_then(|at| args[at + 1..].split_first())
    else {
        eprintln!("aterm-lab keeper: usage: keeper -- PROGRAM ARGS (the lifeline on stdin)");
        return 2;
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
    let mut child = match aterm_uds::spawnfd::command(prog)
        .args(rest)
        .stdin(Stdio::null())
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
mod owned_world_store_tests {
    use super::*;
    use aterm_agent::supervise::tape::store;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn rehoming_preserves_an_existing_home_and_initializes_only_a_new_one() {
        let mut world = World::new(CONFIG_OFF).expect("fresh private world");
        let original_home = world.home.clone();
        std::fs::write(original_home.join(".profile"), b"owner profile").unwrap();
        std::fs::write(original_home.join(".shrc"), b"owner shell configuration").unwrap();
        std::fs::set_permissions(&original_home, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(world.rehome(&original_home).is_err());
        assert_eq!(world.home, original_home);
        assert_eq!(
            std::fs::read(original_home.join(".profile")).unwrap(),
            b"owner profile"
        );
        assert_eq!(
            std::fs::read(original_home.join(".shrc")).unwrap(),
            b"owner shell configuration"
        );
        assert_eq!(
            std::fs::metadata(&original_home)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        let new_home = world.dir.join("new-home");
        world.rehome(&new_home).expect("new private home");
        assert_eq!(world.home, new_home);
        assert_eq!(
            std::fs::read(new_home.join(".profile")).unwrap(),
            b"PS1='$ '\n"
        );
        for (name, value) in world.env() {
            if matches!(
                name.as_str(),
                "TMPDIR"
                    | "TMP"
                    | "TEMP"
                    | "XDG_CACHE_HOME"
                    | "XDG_DATA_HOME"
                    | "XDG_STATE_HOME"
                    | "ATERM_STATE_HOME"
            ) {
                let path = Path::new(&value);
                assert!(path.starts_with(&new_home), "{name} follows the new HOME");
                assert!(path.is_dir(), "{name} already exists");
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
    }

    #[test]
    fn a_new_world_publishes_a_home_that_its_hosted_store_can_admit() {
        let world = World::new(CONFIG_OFF).expect("fresh private world");
        let env = world.env();
        let home = PathBuf::from(
            &env.iter()
                .find(|(name, _)| name == "HOME")
                .expect("private HOME")
                .1,
        );
        assert_eq!(home, world.home);
        for name in [
            "TMPDIR",
            "TMP",
            "TEMP",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "ATERM_STATE_HOME",
        ] {
            let path = PathBuf::from(
                &env.iter()
                    .find(|(key, _)| key == name)
                    .expect("explicit private root")
                    .1,
            );
            assert!(path.starts_with(&home), "{name} belongs to the owned HOME");
            assert!(path.is_dir(), "{name} already exists");
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let state = PathBuf::from(
            &env.iter()
                .find(|(name, _)| name == "ATERM_STATE_HOME")
                .unwrap()
                .1,
        );
        let root = store::store_under(&state);
        let session = store::session_dir(&root, Some("s-world"));
        std::fs::create_dir_all(&session).expect("fresh owned store");
        for dir in [&root, &session] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .expect("private fixture directory");
        }
        let tape = session.join("owned.atape");
        let mut file = store::create(&tape).expect("new private tape");
        file.write_all(b"owned diagnostic bytes")
            .expect("fixture bytes");
        drop(file);
        let admitted = store::try_list(&root, None);
        assert!(
            admitted.as_ref().is_ok_and(|tapes| tapes.len() == 1
                && tapes[0].path == tape
                && tapes[0].bytes == 22),
            "a newly owned Lab World must publish a HOME whose hosted store admits its private tape: {admitted:?}"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod cold_copy_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_cold_copy_of_a_link_is_the_file_it_names_with_no_page_resident() {
        let world_source = World::new(CONFIG_OFF).expect("fixture world");
        let real = world_source.dir.join("aterm-real");
        let script = b"#!/bin/sh\necho 'aterm 0.0.0'\n";
        std::fs::write(&real, script).expect("stand-in");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let link = world_source.dir.join("aterm");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let mut world = World::new(CONFIG_OFF).expect("fresh private world");
        let copy = cold_copy(&mut world, &link).expect("a cold copy");
        let meta = std::fs::symlink_metadata(&copy.exe).expect("the copy");
        assert!(meta.is_file(), "a file, not a link: {meta:?}");
        assert!(copy.exe.starts_with(&world.dir), "{}", copy.exe.display());
        assert_eq!(std::fs::read(&copy.exe).expect("read"), script);
    }
}

#[cfg(test)]
mod terminal_selection_tests {
    use super::{Reply, terminal_request, terminal_selector};

    #[test]
    fn terminal_waits_name_the_registry_session_when_a_native_tab_is_front() {
        for wait in [
            &["ready"][..],
            &["await", "idle", "500", "timeout=60000"][..],
        ] {
            let mut calls = Vec::new();
            let reply = terminal_request(
                |args| {
                    calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
                    match args {
                        ["sessions"] => Reply {
                            stdout: "OK 2\n0 s-aa - exited previous active=0 wfocus=1\n7 s-bb - alive shell active=0 wfocus=1\n".into(),
                            ..Reply::default()
                        },
                        ["@s-bb", rest @ ..] if rest == wait => Reply::default(),
                        _ => Reply {
                            code: 1,
                            stderr: "ERR no active terminal".into(),
                            ..Reply::default()
                        },
                    }
                },
                wait,
            );
            assert_eq!(reply.code, 0);
            assert_eq!(calls.len(), 2, "discovery and one selected request");
            assert_eq!(calls[1][0], "@s-bb");
            assert_eq!(&calls[1][1..], wait);
        }
    }

    #[test]
    fn terminal_selection_tracks_the_focused_session_and_keeps_exited_commands_readable() {
        let roster = "OK 3\n0 s-aa - alive active=1 active=0 wfocus=1\n1 s-bb - spawning shell active=1 wfocus=1\n2 s-cc - alive shell active=1 wfocus=0\n";
        assert_eq!(terminal_selector(roster).as_deref(), Some("@s-bb"));
        assert_eq!(
            terminal_selector("0 s-aa - exited finished active=1 wfocus=1\n").as_deref(),
            Some("@s-aa")
        );
        assert_eq!(terminal_selector("OK 0\n"), None);
        assert_eq!(terminal_selector("0 native - native SystemState\n"), None);
    }

    #[test]
    fn terminal_wait_errors_propagate_without_retrying_or_following_the_front() {
        let mut calls = 0;
        let reply = terminal_request(
            |args| {
                calls += 1;
                if args == ["sessions"] {
                    Reply {
                        stdout: "0 s-aa - exited finished active=1 wfocus=1\n".into(),
                        ..Reply::default()
                    }
                } else {
                    assert_eq!(args, ["@s-aa", "ready"]);
                    Reply {
                        code: 3,
                        stderr: "ERR selected session unavailable".into(),
                        ..Reply::default()
                    }
                }
            },
            &["ready"],
        );
        assert_eq!(reply.code, 3);
        assert_eq!(calls, 2);
    }
}

#[cfg(test)]
mod band_tests {
    use super::reveal_pending;

    #[test]
    fn a_row_in_its_grace_holds_the_frame_and_nothing_else_does() {
        let row = |tail: &str| {
            format!(
                "message 7 at=1 ago_ms=5 tag=harness sev=warn origin=host state=live glass=- \
                 rep=0 key=k title=t detail=d actions=-{tail}"
            )
        };
        assert!(reveal_pending(&row(" reveal=pending need=now")));
        assert!(!reveal_pending(&row(" need=now")));
        assert!(!reveal_pending(""));
        assert!(!reveal_pending(
            &row(" need=now").replace("title=t", "title=reveal=pending")
        ));
    }
}

#[cfg(test)]
mod bell_off_tests {
    use super::*;

    fn root_bell(text: &str) -> Option<bool> {
        aterm_toml::from_str::<aterm_toml::Value>(text)
            .ok()?
            .get("bell_sound")
            .and_then(aterm_toml::Value::as_bool)
    }

    #[test]
    fn a_lab_world_starts_with_the_audible_bell_off() {
        assert_eq!(
            root_bell(CONFIG_OFF),
            None,
            "the base sets no bell_sound: aterm's default (on) would rule it",
        );
        let world = World::new(CONFIG_OFF).expect("fresh private world");
        let text = std::fs::read_to_string(world.cfg.join("aterm/aterm.toml")).expect("written");
        assert_eq!(root_bell(&text), Some(false), "{text}");
        let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).expect("one document");
        assert_eq!(
            doc.get("update")
                .and_then(|t| t.get("enabled"))
                .and_then(aterm_toml::Value::as_bool),
            Some(false),
            "the lanes stay off: {text}",
        );
    }

    #[test]
    fn a_bell_the_caller_sets_stands_alone() {
        let asked = config_with(&["bell_sound = true".to_string()]);
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&format!("bell_sound = false\n{asked}"))
                .is_err(),
            "a key written twice is no document",
        );
        assert_eq!(root_bell(&bell_off(&asked)), Some(true));
        assert_eq!(bell_off(&asked), asked, "the caller's file, unchanged");
    }

    #[test]
    fn a_bell_sound_inside_a_table_is_not_the_root_key() {
        let text = bell_off("[x]\nbell_sound = true\n");
        assert_eq!(root_bell(&text), Some(false), "{text}");
        let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).expect("one document");
        assert_eq!(
            doc.get("x")
                .and_then(|t| t.get("bell_sound"))
                .and_then(aterm_toml::Value::as_bool),
            Some(true),
        );
    }
}

#[cfg(test)]
mod harness_key_tests {
    use aterm_agent::supervise::{Occupancy, SupervisorConfig};

    use super::*;

    #[test]
    fn a_harness_key_lands_under_the_root_harness_table() {
        let key = vec!["occupancy = \"prompt\"".to_string()];
        let config = config_after(&config_with(&[]), true, &[]);
        let keyed = with_harness_keys(&config, &key);
        assert!(
            keyed.contains("[harness]\nenabled = true\noccupancy = \"prompt\"\n"),
            "{keyed}"
        );
        let (cfg, notes) = SupervisorConfig::from_aterm_toml(&keyed);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(cfg.occupancy, Occupancy::Prompt, "{keyed}");
        assert!(cfg.enabled);
        for unkeyed in [
            with_harness_keys(&config, &[]),
            config_after(&config_with(&[]), true, &key),
        ] {
            let (cfg, _) = SupervisorConfig::from_aterm_toml(&unkeyed);
            assert_ne!(cfg.occupancy, Occupancy::Prompt, "{unkeyed}");
        }
        assert_eq!(with_harness_keys("[update]\n", &key), "[update]\n");
    }
}

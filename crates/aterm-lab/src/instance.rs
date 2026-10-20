// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private aterm outside any lab's jail, for checking a change on a real
//! instance (`instance`), and the instances `presence-flap` and
//! `codex-alab-proof` drive.
//!
//! Its [`World`] is one `0700` directory under `/tmp` (a socket path must fit
//! `sun_path`) holding a scratch `HOME`, an `XDG_CONFIG_HOME` whose
//! `aterm.toml` turns the automatic lanes off, and the `XDG_RUNTIME_DIR` its
//! control socket and token live in. Nothing of the caller's environment
//! reaches it, and the control socket is its only door: every `ctl` call
//! names that socket. It ends by its process group (`SIGTERM`, then
//! `SIGKILL`), never by name, and it holds a lifeline (a headless aterm reads
//! it, a windowed one's [`keeper_main`] does), so a launcher killed any way at
//! all takes it along. The world is removed when it drops.
//!
//! `instance [--window] [--columns N] [--lines N] [--font-px N] [--scale N]
//! [--toml LINE]… [--pre VERB]… [--image PNG] [--post VERB]… [--keep]
//! [-- PROGRAM ARGS…]` starts one headless (windowed with `--window`) and
//! prints its socket; drive it with `aterm ctl --sock <sock> <verb>` (`image`,
//! `text`, `send --stdin`, `metrics percentiles`, …) until this command ends
//! (Ctrl-C or `SIGTERM`). A headless instance never presents (its pixels are
//! offscreen captures), so what only a present publishes (`present_*`,
//! `gpu_time_*`, `passes_last`) needs `--window`. `--toml` lines head its
//! `aterm.toml` (top-level keys, such as `theme = "Aterm Day"`); each `--pre`
//! verb runs once it is ready (`open app settings /themes`), each `--post`
//! after the frame, and each reply is printed. With `--image`, it captures
//! one frame once the screen has settled, writes it there and ends.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The automatic lanes off: no update check, no package loop, no agent
/// priming or supervision, the machine's own preferences left alone.
pub const CONFIG_OFF: &str = "agents_auto_prime = false\n\
                              [update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [harness]\nenabled = false\n\
                              [machine]\nuniversal_control = \"leave\"\n";

/// How long an instance may take to listen: a hang detector, never a budget
/// (a debug build on a loaded machine can take well past ten seconds).
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
    /// Lay out a fresh world whose `aterm.toml` is `config`.
    ///
    /// # Errors
    /// A directory or file could not be made.
    pub fn new(config: &str) -> Result<World, String> {
        let dir = crate::sys::make_temp_dir("/tmp/at.XXXXXX")
            .map_err(|e| format!("cannot make a scratch directory under /tmp: {e}"))?;
        let dir = PathBuf::from(dir);
        // This is the root just made exclusively above, not an arbitrary store
        // supplied by a caller. Publish its physical spelling before deriving
        // HOME and state paths: the hosted store refuses linked ancestors.
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
            std::fs::write(w.cfg.join("aterm/aterm.toml"), config)?;
            // A two-column `/bin/sh` prompt, so typed text never wraps on a long host name.
            std::fs::write(w.home.join(".profile"), "PS1='$ '\n")?;
            std::fs::write(w.home.join(".shrc"), "PS1='$ '\n")
        })();
        made.map_err(|e| format!("cannot lay out {}: {e}", w.dir.display()))?;
        Ok(w)
    }

    /// Leave the world in place when it drops, and say where.
    pub fn keep(&mut self) {
        self.keep = true;
    }

    /// Put its `HOME` at a new `home` (made `0700`, with the world's shell
    /// profile): a scratch volume's folder, so what the instance measures and
    /// removes is that volume's (`disk-lane`, `instance --home`).
    ///
    /// # Errors
    /// The folder already exists, or it or its profile could not be made.
    pub fn rehome(&mut self, home: &Path) -> Result<(), String> {
        let made = (|| -> std::io::Result<PathBuf> {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::create_dir(home)?;
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
            let home = std::fs::canonicalize(home)?;
            std::fs::write(home.join(".profile"), "PS1='$ '\n")?;
            std::fs::write(home.join(".shrc"), "PS1='$ '\n")?;
            Ok(home)
        })();
        self.home = made.map_err(|e| format!("cannot make the home {}: {e}", home.display()))?;
        Ok(())
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
            ("HISTFILE".into(), "/dev/null".into()),
            ("xcrun_nocache".into(), "1".into()),
            // Its hosted loops recorded to harness tapes, as a lab's are
            // (`crate::tape`).
            crate::tape::seam(),
        ]
    }
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

/// A running private instance. Dropping it stops it.
pub struct Instance {
    child: Child,
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

impl Instance {
    /// Start `l` in `world`, its output in `<world>/aterm.log`.
    ///
    /// # Errors
    /// The log, the lifeline or the process could not be made.
    pub fn start(world: &World, l: Launch) -> Result<Instance, String> {
        let log = world.dir.join("aterm.log");
        let out = std::fs::File::create(&log)
            .map_err(|e| format!("cannot create {}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("cannot share the instance log: {e}"))?;
        // The lifeline: a headless aterm reads it itself; a windowed one has
        // no such flag, so the lab's keeper holds it and ends the window when
        // it reads end of file.
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.dir)
            .map_err(|e| format!("the lifeline: {e}"))?;
        // The command line, program first: the keeper (windowed), then the
        // sandbox, then the aterm and its flags.
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
            child,
            lifeline: Some(lifeline),
            pid,
            client: l.aterm.clone(),
            aterm: l.aterm,
            sock: world.sock.clone(),
            log,
            env: world.env(),
        })
    }

    /// Wait until the control socket answers `version` (at most `limit`).
    ///
    /// # Errors
    /// Interrupted, the instance exited first, or it never answered (its log tail says why).
    pub fn await_socket(&mut self, limit: Duration) -> Result<(), Fail> {
        let end = Instant::now() + limit;
        loop {
            if self.sock.exists() && self.ctl(&["version"]).code == 0 {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
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

    /// `aterm ctl --sock <its socket> --timeout 90 <args>`, its environment
    /// the world's, never the caller's.
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

    /// One held control connection to it, for a schedule whose requests are
    /// milliseconds apart (a client process per request costs more than that).
    ///
    /// # Errors
    /// The socket did not answer.
    pub fn connect(&self) -> Result<aterm_agent::RelayClient<aterm_uds::CtlStream>, String> {
        aterm_agent::RelayClient::connect_local_beside(&self.sock.to_string_lossy())
            .map_err(|e| format!("cannot reach {}: {e}", self.sock.display()))
    }

    /// Whether it still runs.
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The last `n` lines of its log.
    #[must_use]
    pub fn log_tail(&self, n: usize) -> String {
        let body = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }

    /// End it: cut its lifeline, then `SIGTERM` and `SIGKILL` to its process
    /// group, each after a grace; reaped either way. The group (it is a
    /// `setsid` leader) holds a windowed instance's keeper and its aterm, so
    /// the window goes even when the keeper cannot end it.
    pub fn stop(&mut self) {
        if let Some(l) = self.lifeline.take() {
            l.cut();
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The value of `--flag N` in `args`, removed from them.
///
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
///
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
///
/// # Errors
/// Not a positive number.
pub fn count(flag: &str, v: &str) -> Result<u32, Fail> {
    v.parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| Fail::usage(format!("{flag} takes a positive count, not '{v}'")))
}

/// The aterm to drive: the `--aterm` value given, else the lab's own pick
/// ([`crate::paths::Paths::need_aterm`]). Read after every argument is
/// parsed, so a usage error is never reported as a missing build.
///
/// # Errors
/// Why there is none to drive: nothing was found, or the one given is not
/// executable (the verbs' NOT RUN, never a usage error).
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

/// `instance`: a private aterm, headless unless `--window`, held until this
/// command ends.
///
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
    let home = take_value(&mut a, "--home")?.map(|p| from_caller(&p));
    let harness = take_flag(&mut a, "--harness");
    let pre = take_values(&mut a, "--pre")?;
    let post = take_values(&mut a, "--post")?;
    let keep = take_flag(&mut a, "--keep");
    let window = take_flag(&mut a, "--window");
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
    let mut world = World::new(&config_after(&config_with(&toml), harness, &toml_after))?;
    if keep {
        world.keep();
    }
    if let Some(home) = home {
        world.rehome(Path::new(&home))?;
    }
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
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: !window,
            args: flags,
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
        },
    )?;
    inst.await_socket(BOOT)?;
    let ready = inst.ctl(&["ready"]);
    if ready.code != 0 {
        return Err(Fail::die(format!("instance: not ready: {}", ready.first())));
    }
    run_verbs(&inst, &pre);
    if let Some(dest) = image {
        // The screen settles (half a second without output) before the frame is taken.
        let _ = inst.ctl(&["await", "idle", "500", "timeout=60000"]);
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
    println!(
        "instance pid={} sock={}\n  drive it: {} ctl --sock {} <verb>   (image, text, send --stdin, metrics percentiles, …)\n  \
         world:    {}{}\n  it ends when this command does (Ctrl-C, SIGTERM)",
        inst.pid,
        inst.sock.display(),
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

/// [`CONFIG_OFF`] headed by `top`: top-level keys must come before the
/// first table, or TOML files them under it.
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

/// `config` (a [`config_with`]) with the harness host ON when `harness` (its
/// disk watch runs only while the host does; supervision still reaches no
/// agent, none being primed) and each of `after` appended after its tables
/// (`[disk]` lines, `instance --toml-after`).
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

/// Run each control verb (split on whitespace: a quoted multi-word value is
/// not one word) against `inst`, printing its reply.
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

/// The keeper of a windowed instance (`aterm-lab keeper -- <aterm> ARGS…`):
/// it starts the aterm and holds the lifeline on its stdin, which a windowed
/// aterm cannot read itself. When the lifeline is cut, or `SIGTERM`, `SIGINT`
/// or `SIGHUP` reaches the keeper, it ends the aterm by pid (`SIGTERM`, then
/// `SIGKILL` after 5 s), so a launcher ended any way at all takes its window
/// along. It ends with the aterm.
///
/// The lifeline is waited on by a blocking read on a thread of its own: on
/// macOS `poll(2)` never reports a FIFO whose last writer closed.
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
    // A copy of the held end leaked into this process would keep the cut from ever coming.
    let _ = aterm_uds::lifeline::close_inherited_copies(&lifeline);
    // Caught before the aterm starts, so no signal can end the keeper and orphan it.
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
        #[cfg(target_os = "macos")]
        let state = home.join("Library/Application Support/aterm");
        #[cfg(target_os = "linux")]
        let state = home.join(".local/state/aterm");
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `seamless`: drives a windowed private instance across a seamless update
//! (`ATERM_DEBUG_SEAMLESS_REEXEC=1` and `aterm ctl update apply`) and verifies session-log readers
//! across it, then judges present latency on fresh instances against a `--baseline` aterm.
//!
//! Its privacy leg plants a sentinel in each kind of text one instance holds, has the outgoing
//! process keep a copy of every file its update writes (`ATERM_DEBUG_SEAMLESS_KEEP`), and sweeps
//! the copies ([`crate::seamless_carry`]).

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::bus::Fabric;
use crate::instance::{self, Instance, Launch, Reply, World};
use crate::screen::field;
use crate::stop;
use crate::{Fail, LAB, Res};

const HANG: Duration = Duration::from_secs(60);

const APPLY: Duration = Duration::from_secs(120);

const TOPIC: &str = "seamless.t";

const GAP_TOPIC: &str = "seamless.g";

const ROLL_PAST: u64 = 200;

const ROLL_MAX: u32 = 4000;

const BURST_GAP_MS: u64 = 30_000;

const RUNS: usize = 4;

const LINES: &str = "for i in $(seq 1 300); do echo \"line $i of the shell tab\"; sleep 0.02; done";

/// One leg's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    Pass(String),
    Fail(String),
    CouldNotRun(String),
}

impl Verdict {
    fn word(&self) -> &'static str {
        match self {
            Verdict::Pass(_) => "PASS",
            Verdict::Fail(_) => "FAIL",
            Verdict::CouldNotRun(_) => "COULD NOT RUN",
        }
    }

    fn said(&self) -> &str {
        match self {
            Verdict::Pass(s) | Verdict::Fail(s) | Verdict::CouldNotRun(s) => s,
        }
    }
}

/// What `metrics percentiles` said of presents and input.
#[derive(Clone, Debug, Default, PartialEq)]
struct Figures {
    n_present: Option<u64>,
    /// Hundredths of a millisecond, the precision `metrics percentiles` reports.
    present: [Option<u64>; 3],
    n_input: Option<u64>,
    now_rows: Vec<String>,
}

impl Figures {
    fn read(percentiles: &str) -> Figures {
        let num = |k: &str| {
            field(percentiles, k)
                .as_deref()
                .and_then(reported_hundredths)
        };
        let count = |k: &str| field(percentiles, k).and_then(|v| v.parse::<u64>().ok());
        Figures {
            n_present: count("n_present"),
            present: [
                num("present_p50_ms"),
                num("present_p95_ms"),
                num("present_p99_ms"),
            ],
            n_input: count("n_input"),
            now_rows: Vec::new(),
        }
    }

    fn line(&self) -> String {
        let ms = |v: Option<u64>| {
            v.map_or_else(
                || "-".to_string(),
                |v| format!("{}.{:02}", v / 100, v % 100),
            )
        };
        let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
        format!(
            "n_present={} present_p50/p95/p99_ms={}/{}/{} n_input={} now_rows={}",
            n(self.n_present),
            ms(self.present[0]),
            ms(self.present[1]),
            ms(self.present[2]),
            n(self.n_input),
            self.now_rows.len()
        )
    }
}

fn reported_hundredths(value: &str) -> Option<u64> {
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(whole, fraction)| (whole, Some(fraction)));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let fraction = match fraction {
        None => 0,
        Some(f) if (1..=2).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            f.parse::<u64>().ok()? * if f.len() == 1 { 10 } else { 1 }
        }
        Some(_) => return None,
    };
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(fraction)
}

fn events_gaps(lines: &[String], from: usize) -> Vec<(String, u64)> {
    lines
        .iter()
        .skip(from)
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            if w.next() != Some("GAP") {
                return None;
            }
            let target = w.next()?.to_string();
            let n = w
                .find_map(|t| t.strip_prefix("events-dropped="))?
                .parse()
                .ok()?;
            Some((target, n))
        })
        .collect()
}

fn mail_answers(log: &str) -> usize {
    marks(log, "human")
        .filter(|w| w.contains(&"why=mail"))
        .count()
}

fn human_marks(log: &str) -> usize {
    marks(log, "human").count()
}

fn mail_count(log: &str) -> usize {
    marks(log, "mail").filter(|w| w.contains(&"dir=in")).count()
}

fn without_text(reply: &str) -> String {
    reply
        .lines()
        .map(|l| {
            l.split_whitespace()
                .filter(|t| !t.starts_with("text="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn delivery_marks(log: &str, off: u64) -> Vec<(u64, Option<u64>)> {
    let at = format!("off={off}");
    marks(log, "mail")
        .filter(|w| w.contains(&"dir=in") && w.contains(&at.as_str()))
        .filter_map(|w| {
            let seq = w.first()?.parse().ok()?;
            let t = w.iter().find_map(|t| t.strip_prefix("t=")?.parse().ok());
            Some((seq, t))
        })
        .collect()
}

fn mail_off(log: &str) -> Option<u64> {
    marks(log, "mail")
        .filter(|w| w.contains(&"dir=in"))
        .filter_map(|w| w.iter().find_map(|t| t.strip_prefix("off=")?.parse().ok()))
        .last()
}

fn marks<'a>(log: &'a str, kind: &'a str) -> impl Iterator<Item = Vec<&'a str>> + 'a {
    log.lines().filter_map(move |l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        (w.get(1) == Some(&"mark") && w.get(3) == Some(&kind)).then_some(w)
    })
}

fn brief_links(brief: &str) -> Vec<u64> {
    brief
        .split_whitespace()
        .filter_map(|t| t.strip_prefix("mail=@")?.strip_suffix('~')?.parse().ok())
        .collect()
}

fn server_pid(sock: &Path) -> Option<i32> {
    let stream = std::os::unix::net::UnixStream::connect(sock).ok()?;
    aterm_uds::fdpass::peer_pid(&stream)
        .ok()
        .and_then(|p| i32::try_from(p).ok())
}

fn end_pid(pid: i32) -> bool {
    if !crate::sys::alive(pid) {
        return true;
    }
    let _ = crate::sys::kill(pid, libc::SIGTERM);
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && crate::sys::alive(pid) {
        std::thread::sleep(Duration::from_millis(50));
    }
    if crate::sys::alive(pid) {
        let _ = crate::sys::kill(pid, libc::SIGKILL);
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && crate::sys::alive(pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    !crate::sys::alive(pid)
}

struct Watch {
    child: Child,
    out: Arc<Mutex<Vec<String>>>,
    err: Arc<Mutex<Vec<String>>>,
    what: String,
}

impl Watch {
    fn lines(&self) -> Vec<String> {
        self.out.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn errs(&self) -> Vec<String> {
        self.err.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn ended(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }

    fn wait_end(&mut self, within: Duration) -> Result<Option<i32>, Fail> {
        let end = Instant::now() + within;
        loop {
            if let Some(code) = self.ended() {
                return Ok(Some(code));
            }
            if Instant::now() >= end {
                return Ok(None);
            }
            stop::sleep(Duration::from_millis(50))?;
        }
    }

    fn end(&mut self) {
        if self.ended().is_none()
            && let Ok(pid) = i32::try_from(self.child.id())
        {
            let _ = crate::sys::kill(pid, libc::SIGCONT);
            let _ = crate::sys::kill(pid, libc::SIGTERM);
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end && self.ended().is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.end();
    }
}

struct Desk {
    client: PathBuf,
    env: Vec<(String, String)>,
    sock: PathBuf,
}

impl Desk {
    fn ctl_within(&self, timeout: Duration, args: &[&str]) -> Reply {
        let sock = self.sock.to_string_lossy();
        let secs = timeout.as_secs().to_string();
        let mut argv = vec!["ctl", "--sock", &sock, "--timeout", &secs];
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

    fn ctl(&self, args: &[&str]) -> Reply {
        self.ctl_within(Duration::from_secs(90), args)
    }

    fn ok(&self, args: &[&str]) -> Result<Reply, String> {
        let r = self.ctl(args);
        if r.code == 0 {
            Ok(r)
        } else {
            Err(format!(
                "`{}` answered {}: {} {}",
                args.join(" "),
                r.code,
                r.stdout.trim(),
                r.stderr.trim()
            ))
        }
    }

    fn watch(&self, what: &str, args: &[&str]) -> Result<Watch, String> {
        let sock = self.sock.to_string_lossy().into_owned();
        let mut argv = vec!["ctl".to_string(), "--sock".into(), sock];
        argv.extend(args.iter().map(|a| (*a).to_string()));
        let mut child = aterm_uds::spawnfd::command(&self.client)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run `ctl {}`: {e}", args.join(" ")))?;
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        for (pipe, into) in [
            (
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
                Arc::clone(&out),
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
                Arc::clone(&err),
            ),
        ] {
            let Some(pipe) = pipe else { continue };
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut g) = into.lock() {
                        g.push(line);
                    }
                }
            });
        }
        Ok(Watch {
            child,
            out,
            err,
            what: what.to_string(),
        })
    }

    fn status_word(&self, sid: &str, key: &str) -> Option<String> {
        field(&self.ctl(&[&format!("@{sid}"), "status"]).stdout, key)
    }

    fn cursor(&self, sid: &str) -> Result<(String, String), String> {
        let st = self.ctl(&[&format!("@{sid}"), "status"]).stdout;
        match (field(&st, "log_seq"), field(&st, "log_ep")) {
            (Some(seq), Some(ep)) => Ok((format!("since={seq}"), format!("ep={ep}"))),
            _ => Err(format!("@{sid} status names no log_seq= and log_ep=: {st}")),
        }
    }

    fn watchers(&self, sid: &str) -> Option<u64> {
        self.ctl(&["who"])
            .stdout
            .lines()
            .find(|row| row.split_whitespace().nth(1) == Some(sid))?
            .split_whitespace()
            .find_map(|t| t.strip_prefix("watchers="))?
            .parse()
            .ok()
    }

    fn sessions(&self) -> Vec<(String, String)> {
        self.ctl(&["sessions"])
            .stdout
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let local = w.next()?;
                local.parse::<u64>().ok()?;
                Some((local.to_string(), w.next()?.to_string()))
            })
            .collect()
    }
}

fn until<T>(within: Duration, mut f: impl FnMut() -> Option<T>) -> Result<Option<T>, Fail> {
    let end = Instant::now() + within;
    loop {
        if let Some(v) = f() {
            return Ok(Some(v));
        }
        if Instant::now() >= end {
            return Ok(None);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn set_mode(file: &Path, mode: crate::fleet_fixture::Mode) -> Result<(), String> {
    std::fs::write(file, mode.word()).map_err(|e| format!("{}: {e}", file.display()))
}

fn measure(desk: &Desk, a: &str, b: &str, mode: &Path) -> Result<Figures, Fail> {
    use crate::fleet_fixture::Mode;
    let at_a = format!("@{a}");
    let at_b = format!("@{b}");
    desk.ok(&["metrics", "reset"]).map_err(Fail::die)?;
    desk.ok(&["raise", a]).map_err(Fail::die)?;
    desk.ok(&[&at_a, "ctrl", "u"]).map_err(Fail::die)?;
    desk.ok(&[&at_a, "send", LINES]).map_err(Fail::die)?;
    desk.ok(&[&at_a, "key", "enter"]).map_err(Fail::die)?;
    desk.ok(&[&at_a, "await", "idle", "1000", "timeout=60000"])
        .map_err(Fail::die)?;
    desk.ok(&["raise", b]).map_err(Fail::die)?;
    let (since, ep) = desk.cursor(b).map_err(Fail::die)?;
    set_mode(mode, Mode::Busy).map_err(Fail::die)?;
    desk.ok(&[
        &at_b,
        "await",
        "log",
        "open=turn",
        &since,
        &ep,
        "timeout=60000",
    ])
    .map_err(Fail::die)?;
    stop::sleep(Duration::from_secs(6))?;
    let (since, ep) = desk.cursor(b).map_err(Fail::die)?;
    set_mode(mode, Mode::Prompt).map_err(Fail::die)?;
    desk.ok(&[&at_b, "await", "log", "need", &since, &ep, "timeout=60000"])
        .map_err(Fail::die)?;
    desk.ok(&["open", "tab-menu"]).map_err(Fail::die)?;
    let menu = desk.ctl(&["controls", "tab-menu"]);
    let _ = desk.ctl(&["key", "escape"]);
    let (since, ep) = desk.cursor(b).map_err(Fail::die)?;
    set_mode(mode, Mode::Busy).map_err(Fail::die)?;
    stop::sleep(Duration::from_secs(3))?;
    set_mode(mode, Mode::Idle).map_err(Fail::die)?;
    desk.ok(&[
        &at_b,
        "await",
        "log",
        "close=turn",
        &since,
        &ep,
        "timeout=60000",
    ])
    .map_err(Fail::die)?;
    let mut figures = Figures::read(
        &desk
            .ok(&["metrics", "percentiles"])
            .map_err(Fail::die)?
            .stdout,
    );
    figures.now_rows = menu
        .stdout
        .lines()
        .filter(|l| l.contains("now:"))
        .map(str::to_string)
        .collect();
    Ok(figures)
}

fn report(legs: &[(&str, Verdict)]) -> i32 {
    println!("seamless: the legs");
    for (name, v) in legs {
        println!("  {:<8} {:<13} {}", name, v.word(), v.said());
    }
    if legs.iter().any(|(_, v)| matches!(v, Verdict::Fail(_))) {
        1
    } else if legs
        .iter()
        .any(|(_, v)| matches!(v, Verdict::CouldNotRun(_)))
    {
        3
    } else {
        0
    }
}

/// `seamless --baseline PATH [--aterm PATH] [--runs N] [--keep]`: OPENS A WINDOW, one at a time.
/// # Errors
/// Usage, or an instance could not be laid out, started or reached.
pub fn cmd_seamless(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let baseline = instance::take_value(&mut a, "--baseline")?;
    let runs = match instance::take_value(&mut a, "--runs")? {
        None => RUNS,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if n >= 2 => n,
            _ => {
                return Err(Fail::usage(format!(
                    "seamless: --runs takes a count of 2 or more, not '{n}'"
                )));
            }
        },
    };
    let keep = instance::take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "seamless: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let Some(baseline) = baseline.map(|p| PathBuf::from(crate::paths::from_caller(&p))) else {
        return Err(Fail::usage(format!(
            "seamless: --baseline names an aterm built from 6B-6's tree, for the present leg's same-day runs ({LAB} help)"
        )));
    };
    if !baseline.is_file() {
        return Err(Fail::exit(
            3,
            format!("seamless: no baseline aterm at {}", baseline.display()),
        ));
    }
    let aterm = instance::resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    let mut legs = across_update(&aterm, keep)?;
    legs.push(("present", present(&aterm, &baseline, runs, keep)?));
    Ok(report(&legs))
}

fn across_update(aterm: &Path, keep: bool) -> Result<Vec<(&'static str, Verdict)>, Fail> {
    let mut world = World::new(&instance::config_with(&["tab_strip_rows = 1".to_string()]))?;
    if keep {
        world.keep();
    }
    let mut fabric = Fabric::start(aterm, &world.dir.join("fabric"))
        .map_err(|e| Fail::die(format!("seamless: cannot start the lab's bus: {e}")))?;
    fabric
        .name_in(&world.cfg.join("aterm/aterm.toml"))
        .map_err(|e| Fail::die(format!("seamless: {e}")))?;
    let bin = world.dir.join("bin");
    let mode = world.dir.join("standin.mode");
    let updroot = world.dir.join("updroot");
    let keep_root = world.dir.join("keep");
    let laid = (|| -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        for d in [&bin, &updroot, &keep_root] {
            std::fs::create_dir_all(d)?;
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
        std::os::unix::fs::symlink(&crate::paths::paths().exe, bin.join("claude"))?;
        std::fs::write(&mode, "idle")
    })();
    laid.map_err(|e| {
        Fail::die(format!(
            "seamless: cannot lay out {}: {e}",
            world.dir.display()
        ))
    })?;
    let mut env = world.env();
    env.push(("ATERM_DEBUG_SEAMLESS_REEXEC".into(), "1".into()));
    env.push((
        "ATERM_UPDATE_ROOT".into(),
        updroot.to_string_lossy().into_owned(),
    ));
    env.push((
        "ATERM_DEBUG_SEAMLESS_KEEP".into(),
        keep_root.to_string_lossy().into_owned(),
    ));
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args: ["--columns", "120", "--lines", "40"]
                .map(str::to_string)
                .to_vec(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    inst.await_socket(instance::BOOT)?;
    let desk = Desk {
        client: aterm.to_path_buf(),
        env: world.env(),
        sock: world.sock.clone(),
    };
    let mut pids = Vec::new();
    let lay = Lay {
        mode: &mode,
        bin: &bin,
        dir: &world.dir,
        keep: &keep_root,
    };
    let ran = legs(&desk, &mut inst, &fabric, &lay, &mut pids);
    pids.extend(bridge_pid(&fabric.state));
    pids.sort_unstable();
    pids.dedup();
    let mut ended: Vec<String> = pids
        .into_iter()
        .map(|pid| {
            let _ = crate::sys::kill(pid, libc::SIGCONT);
            format!(
                "{pid}:{}",
                if end_pid(pid) { "gone" } else { "STILL ALIVE" }
            )
        })
        .collect();
    let keeper = inst.pid;
    inst.stop();
    ended.push(format!(
        "keeper {keeper}:{}",
        if crate::sys::alive(keeper) {
            "STILL ALIVE"
        } else {
            "gone"
        }
    ));
    fabric.stop();
    println!("seamless: ended by pid {}", ended.join(" "));
    println!(
        "seamless: world {}{}",
        world.dir.display(),
        if keep { " (kept)" } else { " (removed)" }
    );
    ran
}

fn two_tabs(desk: &Desk, mode: &Path, bin: &Path) -> Result<(String, String), Fail> {
    let die = |e: String| Fail::die(format!("seamless: {e}"));
    let Some((_, a)) = desk.sessions().into_iter().next() else {
        return Err(die("the instance lists no session".into()));
    };
    desk.ok(&[&format!("@{a}"), "ready"]).map_err(die)?;
    let spawned = desk.ok(&["spawn"]).map_err(die)?;
    let Some(b) = spawned
        .first()
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
    else {
        return Err(die(format!("spawn answered {}", spawned.first())));
    };
    let at_b = format!("@{b}");
    desk.ok(&[&at_b, "ready"]).map_err(die)?;
    let standin = format!("{} follow {}", bin.join("claude").display(), mode.display());
    desk.ok(&[&at_b, "send", &standin]).map_err(die)?;
    desk.ok(&[&at_b, "key", "enter"]).map_err(die)?;
    if until(HANG, || {
        desk.status_word(&b, "program").filter(|p| p == "claude")
    })?
    .is_none()
    {
        return Err(die(format!(
            "the stand-in never read as Claude Code: @{b} status program={}",
            desk.status_word(&b, "program").unwrap_or_default()
        )));
    }
    Ok((a, b))
}

fn present(aterm: &Path, baseline: &Path, runs: usize, keep: bool) -> Result<Verdict, Fail> {
    let mut base = Vec::new();
    let mut tested = Vec::new();
    for run in 1..=runs {
        for (which, bin, into) in [
            ("baseline", baseline, &mut base),
            ("tested", aterm, &mut tested),
        ] {
            let f = match present_once(bin, keep) {
                Ok(f) => f,
                Err(e) if e.stopped.is_some() => return Err(e),
                Err(e) => {
                    return Ok(Verdict::Fail(format!(
                        "the {which} run {run} of {} did not finish: {}",
                        bin.display(),
                        e.msg.unwrap_or_default()
                    )));
                }
            };
            println!("seamless: present {which} run {run}: {}", f.line());
            into.push(f);
        }
    }
    Ok(judge_present(&base, &tested))
}

fn present_once(aterm: &Path, keep: bool) -> Result<Figures, Fail> {
    let mut world = World::new(&instance::config_with(&["tab_strip_rows = 1".to_string()]))?;
    if keep {
        world.keep();
    }
    let bin = world.dir.join("bin");
    let mode = world.dir.join("standin.mode");
    let laid = (|| -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(&bin)?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))?;
        std::os::unix::fs::symlink(&crate::paths::paths().exe, bin.join("claude"))?;
        std::fs::write(&mode, "idle")
    })();
    laid.map_err(|e| {
        Fail::die(format!(
            "seamless: cannot lay out {}: {e}",
            world.dir.display()
        ))
    })?;
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args: ["--columns", "120", "--lines", "40"]
                .map(str::to_string)
                .to_vec(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    inst.await_socket(instance::BOOT)?;
    let desk = Desk {
        client: aterm.to_path_buf(),
        env: world.env(),
        sock: world.sock.clone(),
    };
    let server = server_pid(&desk.sock);
    let measured = two_tabs(&desk, &mode, &bin).and_then(|(a, b)| measure(&desk, &a, &b, &mode));
    let keeper = inst.pid;
    inst.stop();
    let gone = server.is_none_or(end_pid) && !crate::sys::alive(keeper);
    println!(
        "seamless: present instance {} (keeper {keeper}) {}",
        server.map_or_else(|| "-".to_string(), |p| p.to_string()),
        if gone { "gone" } else { "STILL ALIVE" }
    );
    if !gone {
        return Err(Fail::die(format!(
            "seamless: the present instance of {} outlived its stop",
            aterm.display()
        )));
    }
    measured
}

fn bridge_pid(state: &Path) -> Option<i32> {
    std::fs::read_to_string(state.join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p > 1)
}

/// Where the update world's files are: the stand-in's mode file, its `bin`, the world itself
/// (the privacy leg's scripts) and the folder the keep seam copies the carry into.
struct Lay<'a> {
    mode: &'a Path,
    bin: &'a Path,
    dir: &'a Path,
    keep: &'a Path,
}

/// Every leg across the update, in order.
#[allow(clippy::too_many_lines)]
fn legs(
    desk: &Desk,
    inst: &mut Instance,
    fabric: &Fabric,
    lay: &Lay<'_>,
    ended: &mut Vec<i32>,
) -> Result<Vec<(&'static str, Verdict)>, Fail> {
    let (mode, bin) = (lay.mode, lay.bin);
    let die = |e: String| Fail::die(format!("seamless: {e}"));
    let Some(parent) = server_pid(&desk.sock) else {
        return Err(die("no pid answers the control socket".into()));
    };
    let fabric_up = until(HANG, || {
        field(&desk.ctl(&["fabric", "status"]).first(), "state").filter(|w| w == "connected")
    })?;
    if fabric_up.is_none() {
        return Err(die(format!(
            "the bridge never connected: fabric status {}",
            desk.ctl(&["fabric", "status"]).first()
        )));
    }
    let (a, b) = two_tabs(desk, mode, bin)?;
    println!(
        "seamless: aterm pid={parent} sock={} shell tab {a}, stand-in tab {b}",
        desk.sock.display()
    );
    let planted = plant(desk, lay.dir)?;
    match &planted {
        Ok(p) => println!(
            "seamless: privacy tabs {} (local {}, every sentinel) and {} (its mail)",
            p.p, p.local_p, p.q
        ),
        Err(why) => println!("seamless: a privacy plant did not land: {why}"),
    }
    let at_a = format!("@{a}");
    let at_b = format!("@{b}");

    desk.ok(&["raise", &a]).map_err(die)?;
    let topic = desk.ok(&[&at_a, "topic", "add", TOPIC]).map_err(die)?;
    let old_bridge = bridge_pid(&fabric.state);
    let base = desk.watchers(&a).unwrap_or(0);
    let mut events = desk
        .watch("subscribe events", &["subscribe", &at_a, "events"])
        .map_err(die)?;
    if until(HANG, || desk.watchers(&a).filter(|n| *n > base))?.is_none() {
        return Err(die(format!(
            "the events watch never armed: @{a} watchers={:?}, base {base}",
            desk.watchers(&a)
        )));
    }
    desk.ok(&[
        &at_a,
        "post",
        &format!("to=@{b}"),
        "kind=task",
        "seamless mail",
    ])
    .map_err(die)?;
    let delivered = until(HANG, || {
        mail_off(&desk.ctl(&[&at_b, "log", "kinds=mail"]).stdout)
    })?;
    let Some(mail) = delivered else {
        return Err(die(format!(
            "the mail never reached @{b}'s log as mail dir=in (topic add said {})",
            topic.first()
        )));
    };
    let marked = delivery_marks(&desk.ctl(&[&at_b, "log", "kinds=mail"]).stdout, mail);
    let mail_at = Instant::now();
    let mut need = desk
        .watch(
            "await log need",
            &[&at_a, "await", "log", "need", WATCH_TIMEOUT],
        )
        .map_err(die)?;
    let mut need_close = desk
        .watch(
            "await log need,close=turn",
            &[&at_a, "await", "log", "need,close=turn", WATCH_TIMEOUT],
        )
        .map_err(die)?;
    if until(HANG, || desk.watchers(&a).filter(|n| *n >= base + 3))?.is_none() {
        return Err(die(format!(
            "the three watches never armed: @{a} watchers={:?}, base {base}",
            desk.watchers(&a)
        )));
    }

    // The bridge is stopped from here until the successor answers, so the plants' second post
    // waits in P's outbox across the update; the run's end lets it go if a leg fails first.
    let planted = planted.and_then(|p| plant_waiting(desk, &p, old_bridge).map(|()| p));
    if let Err(why) = &planted {
        println!("seamless: a privacy plant did not land: {why}");
    }
    let applied = desk.ctl_within(APPLY, &["update", "apply"]);
    println!(
        "seamless: update apply answered {}: {}",
        applied.code,
        applied.first()
    );
    ended.extend(old_bridge);
    let (next, seen) = successor_of(
        APPLY,
        parent,
        || server_pid(&desk.sock),
        || desk.ctl(&["version"]).code == 0,
    )?;
    ended.extend(seen.iter().copied());
    let Some(next) = next else {
        return Err(die(format!(
            "no successor answered within {} s of the apply (a pid other than {parent} on the socket: {seen:?}); the instance's log:\n{}",
            APPLY.as_secs(),
            inst.log_tail(30)
        )));
    };
    let parent_gone = until(HANG, || (!crate::sys::alive(parent)).then_some(()))?.is_some();
    let said = std::fs::read_to_string(&inst.log).unwrap_or_default();
    let handed = said
        .lines()
        .filter(|l| l.contains("SEAMLESS update") || l.contains("handed off"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" | ");
    println!(
        "seamless: successor pid={next} {} s after the mail (the outgoing {parent} {}); {handed}",
        mail_at.elapsed().as_secs(),
        if parent_gone { "ended" } else { "STILL RUNS" }
    );
    if let Ok(p) = &planted {
        // The plants' post waited and their hold stood across the update so the carry holds
        // them; the old bridge goes on, and a standing hold refuses the window verbs (`hwkey`)
        // across the instance, so both end here.
        if let Some(bridge) = old_bridge {
            let _ = crate::sys::kill(bridge, libc::SIGCONT);
        }
        let lifted = desk.ctl(&["hold", &p.p, "off"]);
        println!(
            "seamless: the old bridge {old_bridge:?} let go; the plants' hold on @{} lifted: {}",
            p.p,
            lifted.first()
        );
    }

    let latch = mail_link(desk, &b, mode, mail, &marked, mail_at)?;
    let redial_close = redial_ends(&mut need_close)?;
    let redial_need = redial_resumes(desk, &a, &mut need, next)?;
    let topics = topic_after(
        desk,
        fabric,
        &a,
        &b,
        TOPIC,
        "the successor's bridge read it",
    )?;
    let local_a = desk
        .sessions()
        .into_iter()
        .find(|(_, s)| *s == a)
        .map(|(l, _)| l);
    let (gap, gap_topics) = slow_readers(desk, fabric, &a, &b, local_a.as_deref(), &mut events)?;
    let person = person_keys(desk, &a, &b)?;
    events.end();
    ended.extend(bridge_pid(&fabric.state));
    if let Ok(p) = &planted {
        let at_q = format!("@{}", p.q);
        println!(
            "seamless: after the update @{}'s log holds {} mail dir=in (the delivered post, then the waiting one)",
            p.q,
            mail_count(&desk.ctl(&[&at_q, "log", "kinds=mail"]).stdout)
        );
    }
    let privacy = carry_privacy(planted, lay.keep, parent)?;
    Ok(vec![
        ("gap", gap),
        ("topics", topics),
        ("gaptopic", gap_topics),
        ("redial", redial_need),
        ("redial75", redial_close),
        ("latch", latch),
        ("hwkey", person),
        ("privacy", privacy),
    ])
}

/// The privacy leg's tabs: `p`, which holds every sentinel, and `q`, which `p` mails.
struct Planted {
    p: String,
    local_p: String,
    q: String,
}

/// A `sh` script that reports, through the shell's own marks (OSC 133 and 633;E, signed with the
/// tab's nonce when it has one), a command whose second word is `secret`, and prints only
/// `ran-42`: the command line reaches the terminal's marks and the session log, never the screen.
fn marked_script(secret: &str) -> String {
    format!(
        "#!/bin/sh\n\
         n=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007$ \\033]133;B%s\\007\\033]633;E;echo {secret} 42%s\\007\\r\\n\\033]133;C%s\\007ran-%s\\r\\n\\033]133;D;0%s\\007' \
         \"$n\" \"$n\" \"$n\" \"$n\" \"$((40+2))\" \"$n\"\n"
    )
}

/// THE PRIVACY LEG'S PLANTS (§5 leg 7, the places one instance holds): on a fresh tab P, a
/// command's later word twice, once as the shell's marks report it (`QZP6SECRET1`, through
/// [`marked_script`], which prints none of it) and once typed (`echo QZP6SECRET1 42`, the path a
/// command line normally takes: echoed onto the screen); a title (2, from a script, so the typed
/// line holds none); an attention text (3); a topic name (4); and a mail to a second tab Q with
/// its body (6), `key=` (8) and `via=` hop (9), delivered. A second post and the hold, which
/// stand across the update, are [`plant_waiting`]'s. `Err` names a plant that did not land (the
/// leg fails with it); a tab that cannot be made at all ends the run.
fn plant(desk: &Desk, dir: &Path) -> Result<Result<Planted, String>, Fail> {
    let die = |e: String| Fail::die(format!("seamless: {e}"));
    let spawn = || -> Result<String, Fail> {
        let spawned = desk.ok(&["spawn"]).map_err(die)?;
        let sid = spawned
            .first()
            .split_whitespace()
            .nth(1)
            .map(str::to_string)
            .ok_or_else(|| die(format!("spawn answered {}", spawned.first())))?;
        desk.ok(&[&format!("@{sid}"), "ready"]).map_err(die)?;
        Ok(sid)
    };
    let (p, q) = (spawn()?, spawn()?);
    let at_p = format!("@{p}");
    let marked = dir.join("marked.sh");
    let title = dir.join("title.sh");
    let written = std::fs::write(&marked, marked_script("QZP6SECRET1"))
        .and_then(|()| std::fs::write(&title, "printf '\\033]0;QZP6SECRET2\\007'\n"));
    if let Err(e) = written {
        return Ok(Err(format!("the plants' scripts: {e}")));
    }
    let typed = |script: &Path| -> Result<(), String> {
        desk.ok(&[&at_p, "send", &format!("sh {}", script.display())])?;
        desk.ok(&[&at_p, "key", "enter"]).map(|_| ())
    };
    let held_by = |args: &[&str], digit: u8| {
        crate::seamless_carry::sentinels(desk.ctl(args).stdout.as_bytes()).contains(&digit)
    };
    if let Err(e) = typed(&marked) {
        return Ok(Err(e));
    }
    let ran = until(HANG, || {
        desk.ctl(&[&at_p, "log"])
            .stdout
            .lines()
            .any(|l| l.contains(" cmd ") && l.contains("detail=echo"))
            .then_some(())
    })?;
    if ran.is_none() {
        return Ok(Err(format!(
            "@{p}'s log holds no `cmd` run with detail=echo a minute after the marked command"
        )));
    }
    if let Err(e) = typed(&title) {
        return Ok(Err(e));
    }
    if until(HANG, || {
        held_by(&[&at_p, "log", "tier=content"], 2).then_some(())
    })?
    .is_none()
    {
        return Ok(Err(format!(
            "@{p}'s log holds no title mark at the content tier a minute after the title script"
        )));
    }
    if let Err(e) = desk
        .ok(&[&at_p, "send", TYPED_LINE])
        .and_then(|_| desk.ok(&[&at_p, "key", "enter"]))
    {
        return Ok(Err(e));
    }
    if until(HANG, || {
        desk.ctl(&[&at_p, "text"])
            .stdout
            .lines()
            .any(|l| l.trim_end() == TYPED_ECHO)
            .then_some(())
    })?
    .is_none()
    {
        return Ok(Err(format!(
            "@{p}'s screen shows no `{TYPED_ECHO}` a minute after the typed `{TYPED_LINE}`"
        )));
    }
    for line in [
        vec![at_p.as_str(), "meta", "set", "attention", "QZP6SECRET3"],
        vec![at_p.as_str(), "topic", "add", "qzp6secret4"],
    ] {
        if let Err(e) = desk.ok(&line) {
            return Ok(Err(e));
        }
    }
    let to = format!("to=@{q}");
    if let Err(e) = desk.ok(&[
        &at_p,
        "post",
        &to,
        "kind=note",
        "key=QZP6SECRET8",
        "via=h-qzp6secret9",
        "QZP6SECRET6",
    ]) {
        return Ok(Err(e));
    }
    let at_q = format!("@{q}");
    if until(HANG, || {
        (mail_count(&desk.ctl(&[&at_q, "log", "kinds=mail"]).stdout) > 0).then_some(())
    })?
    .is_none()
    {
        return Ok(Err(format!(
            "@{p}'s mail never reached @{q}'s log as mail dir=in"
        )));
    }
    let Some(local_p) = desk
        .sessions()
        .into_iter()
        .find(|(_, s)| *s == p)
        .map(|(l, _)| l)
    else {
        return Ok(Err(format!("`sessions` lists no local id for @{p}")));
    };
    Ok(Ok(Planted { p, local_p, q }))
}

/// The privacy leg's typed command line: its second word is the sentinel `1`.
const TYPED_LINE: &str = "echo QZP6SECRET1 42";

/// What the shell prints for [`TYPED_LINE`].
const TYPED_ECHO: &str = "QZP6SECRET1 42";

/// THE WAITING POST AND THE HOLD (§5 leg 7's mail body and hold reason, across the update): the
/// bridge `bridge` is stopped (`SIGSTOP`, by its pid) so a post from P to Q (its body 6, `key=` 8
/// and `via=` hop 9, each beside a `w`, a key of its own) stays in P's outbox, then P is held
/// with the reason 5. Both stand across the update so its carry holds them; [`legs`] lets the
/// bridge go (`SIGCONT`) and lifts the hold once the successor answers, since a standing hold
/// refuses `hwkey` across the instance. On `Err`, which names what did not land, the bridge is
/// let go here.
fn plant_waiting(desk: &Desk, planted: &Planted, bridge: Option<i32>) -> Result<(), String> {
    let Some(bridge) = bridge else {
        return Err("no bridge pid to stop, so no post can wait across the update".to_string());
    };
    let _ = crate::sys::kill(bridge, libc::SIGSTOP);
    let at_p = format!("@{}", planted.p);
    let to = format!("to=@{}", planted.q);
    let made = desk
        .ok(&[
            &at_p,
            "post",
            &to,
            "kind=note",
            "key=QZP6SECRET8w",
            "via=h-qzp6secret9w",
            "QZP6SECRET6 waits",
        ])
        .and_then(|_| desk.ok(&["hold", &planted.p, "on", "reason=QZP6SECRET5"]));
    if made.is_err() {
        let _ = crate::sys::kill(bridge, libc::SIGCONT);
    }
    made.map(|_| ())
}

/// The attempt `parent` kept under `keep` (`<parent>-<nonce>/`), if it kept exactly one.
fn kept_folder(keep: &Path, parent: i32) -> Result<PathBuf, String> {
    let prefix = format!("{parent}-");
    let folders: Vec<PathBuf> = std::fs::read_dir(keep)
        .map_err(|e| format!("{}: {e}", keep.display()))?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect();
    match folders.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "the outgoing process {parent} kept no carry under {} (is the aterm a debug or dev-seams build? the keep seam is ATERM_DEBUG_SEAMLESS_KEEP)",
            keep.display()
        )),
        many => Err(format!("the outgoing process kept {} attempts", many.len())),
    }
}

/// The files `parent`'s handoffs left in `dir` (`seamless-<parent>-…`).
fn left_by(dir: &Path, parent: i32) -> Vec<String> {
    let prefix = format!("seamless-{parent}-");
    let mut left: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    left.sort();
    left
}

/// THE PRIVACY LEG (§5 leg 7, the carry): the files the outgoing process `parent` kept, swept by
/// [`crate::seamless_carry::verdict`]; and none of its handoff's files may still be in the control
/// directory a minute after it ended (the successor consumes the manifest, the layout, the grids
/// and the stamps; the outgoing process retires the sidecars after the proof).
fn carry_privacy(
    planted: Result<Planted, String>,
    keep: &Path,
    parent: i32,
) -> Result<Verdict, Fail> {
    let planted = match planted {
        Ok(p) => p,
        Err(why) => return Ok(Verdict::Fail(format!("a plant did not land: {why}"))),
    };
    let kept = match kept_folder(keep, parent).and_then(|f| crate::seamless_carry::read_kept(&f)) {
        Ok(kept) => kept,
        Err(why) => return Ok(Verdict::Fail(why)),
    };
    let left = match &kept.dir {
        Some((_, dir)) => {
            let dir = Path::new(dir);
            until(HANG, || left_by(dir, parent).is_empty().then_some(()))?;
            left_by(dir, parent)
        }
        None => Vec::new(),
    };
    Ok(
        match crate::seamless_carry::verdict(&kept, &planted.local_p, &left).rendered() {
            Ok(said) => Verdict::Pass(format!(
                "@{} (local {}): {said}",
                planted.p, planted.local_p
            )),
            Err(why) => Verdict::Fail(format!("@{} (local {}): {why}", planted.p, planted.local_p)),
        },
    )
}

const WATCH_TIMEOUT: &str = "timeout=600000";

fn successor_of(
    within: Duration,
    parent: i32,
    mut pid_now: impl FnMut() -> Option<i32>,
    mut answers: impl FnMut() -> bool,
) -> Result<(Option<i32>, Vec<i32>), Fail> {
    let mut seen = Vec::new();
    let next = until(within, || {
        let pid = pid_now().filter(|p| *p != parent)?;
        if !seen.contains(&pid) {
            seen.push(pid);
        }
        answers().then_some(pid)
    })?;
    Ok((next, seen))
}

fn mail_link(
    desk: &Desk,
    b: &str,
    mode: &Path,
    mail: u64,
    marked: &[(u64, Option<u64>)],
    mail_at: Instant,
) -> Result<Verdict, Fail> {
    use crate::fleet_fixture::Mode;
    let at_b = format!("@{b}");
    let die = |e: String| Fail::die(format!("seamless: {e}"));
    let (since, ep) = desk.cursor(b).map_err(die)?;
    set_mode(mode, Mode::Busy).map_err(die)?;
    let opened = desk.ctl(&[
        &at_b,
        "await",
        "log",
        "open=turn",
        &since,
        &ep,
        "timeout=60000",
    ]);
    let open_s = mail_at.elapsed().as_secs();
    let brief_open = desk.ctl(&[&at_b, "brief"]).stdout;
    let (since, ep) = desk.cursor(b).map_err(die)?;
    set_mode(mode, Mode::Idle).map_err(die)?;
    let closed = desk.ctl(&[
        &at_b,
        "await",
        "log",
        "close=turn",
        &since,
        &ep,
        "timeout=60000",
    ]);
    let brief_closed = desk.ctl(&[&at_b, "brief"]).stdout;
    let links: Vec<u64> = brief_links(&brief_open)
        .into_iter()
        .chain(brief_links(&brief_closed))
        .collect();
    let turn = if opened.code != 0 || closed.code != 0 {
        Err(format!(
            "the stand-in's turn on the successor did not open and close: {} / {}",
            opened.first(),
            closed.first()
        ))
    } else {
        Ok(open_s)
    };
    let rows = desk.ctl(&[&at_b, "log", "kinds=mail"]).stdout;
    Ok(latch_verdict(
        turn,
        &links,
        mail,
        marked,
        &delivery_marks(&rows, mail),
        &format!(
            "@{b}'s mail rows: {}; brief: {}",
            without_text(&rows),
            without_text(&brief_closed)
        ),
    ))
}

fn latch_verdict(
    turn: Result<u64, String>,
    links: &[u64],
    mail: u64,
    before: &[(u64, Option<u64>)],
    after: &[(u64, Option<u64>)],
    rows: &str,
) -> Verdict {
    let open_s = match turn {
        Ok(s) => s,
        Err(why) => return Verdict::Fail(why),
    };
    let linked = links.contains(&mail);
    match (before, linked) {
        ([one], true) if after == [*one] => Verdict::Pass(format!(
            "the mail at off={mail}, delivered before the update (seq {}), links the successor's first turn (opened {open_s} s after it): brief says mail=@{mail}~, and the successor's log holds that one delivery mark",
            one.0
        )),
        ([_], true) => Verdict::Fail(format!(
            "brief says mail=@{mail}~, but the link is not the carry's: the outgoing process marked the delivery {before:?} (seq, t) and the successor's log marks it {after:?}; {rows}"
        )),
        ([_], false) => {
            let twice = if after.len() > 1 {
                format!(
                    "; the log marks the mail at off={mail} delivered {} times (the successor's bridge delivered it again), so two waiting mails share the turn's window",
                    after.len()
                )
            } else {
                String::new()
            };
            Verdict::Fail(format!(
                "no turn line says mail=@{mail}~ (links {links:?}; the turn opened {open_s} s after the mail){twice}; {rows}"
            ))
        }
        _ => Verdict::Fail(format!(
            "the outgoing process marked the mail at off={mail} delivered {before:?} (seq, t), not once; {rows}"
        )),
    }
}

fn ran_out(lines: &[String]) -> bool {
    lines.iter().any(|l| l.trim() == "OK timeout")
}

fn watch_ended(what: &str, code: i32, lines: &[String], errs: &[String], instead: &str) -> Verdict {
    if ran_out(lines) {
        Verdict::Fail(format!(
            "`{what}` ran out of its own timeout= before the leg read it (the lab's bound, not the redial): {}",
            lines.join(" ")
        ))
    } else {
        Verdict::Fail(format!(
            "`{what}` ended {code} {instead}: {} {}",
            lines.join(" "),
            errs.join(" ")
        ))
    }
}

fn redial_ends(need_close: &mut Watch) -> Result<Verdict, Fail> {
    Ok(match need_close.wait_end(HANG)? {
        Some(75) => {
            let errs = need_close.errs().join(" ");
            if errs.contains("since=<log_seq> ep=<log_ep> base=<log_base>") {
                Verdict::Pass(format!("`{}` ended 75 and named since=", need_close.what))
            } else {
                Verdict::Fail(format!(
                    "`{}` ended 75 without the since= note: {errs}",
                    need_close.what
                ))
            }
        }
        Some(code) => watch_ended(
            &need_close.what,
            code,
            &need_close.lines(),
            &need_close.errs(),
            "(not 75)",
        ),
        None => Verdict::Fail(format!(
            "`{}` still waits a minute after the update",
            need_close.what
        )),
    })
}

fn redial_resumes(desk: &Desk, a: &str, need: &mut Watch, next: i32) -> Result<Verdict, Fail> {
    let at_a = format!("@{a}");
    if let Some(code) = need.ended() {
        return Ok(watch_ended(
            &need.what,
            code,
            &need.lines(),
            &need.errs(),
            "at the update instead of being asked again",
        ));
    }
    let marked = desk.ctl(&[&at_a, "mark", "-", "need", "word=help"]);
    let verdict = match need.wait_end(HANG)? {
        Some(0) if need.lines().iter().any(|l| l.contains(" need ")) => Verdict::Pass(format!(
            "`{}` was asked again of {next} and answered its need: {}",
            need.what,
            need.lines().join(" ")
        )),
        Some(code) => Verdict::Fail(format!(
            "`{}` ended {code} after the need mark ({}): {} {}",
            need.what,
            marked.first(),
            need.lines().join(" "),
            need.errs().join(" ")
        )),
        None => Verdict::Fail(format!(
            "`{}` did not answer the need mark ({}) within a minute",
            need.what,
            marked.first()
        )),
    };
    let _ = desk.ctl(&[&at_a, "mark", "-", "clear", "word=help"]);
    Ok(verdict)
}

fn topic_after(
    desk: &Desk,
    fabric: &Fabric,
    a: &str,
    b: &str,
    topic: &str,
    how: &str,
) -> Result<Verdict, Fail> {
    let posted = desk.ctl(&[
        &format!("@{b}"),
        "post",
        &format!("to=say:{topic}"),
        "kind=note",
        "seamless topic",
    ]);
    let needle = format!(" topic={topic} ");
    let row = until(HANG, || {
        desk.ctl(&[&format!("@{a}"), "inbox", "--peek"])
            .stdout
            .lines()
            .find(|r| r.contains(&needle))
            .map(str::to_string)
    })?;
    let held = bridge_holds(&fabric.state, a, topic);
    Ok(topic_verdict(
        how,
        topic,
        a,
        row.as_deref(),
        held,
        &posted.first(),
    ))
}

fn topic_verdict(
    how: &str,
    topic: &str,
    a: &str,
    row: Option<&str>,
    held: bool,
    posted: &str,
) -> Verdict {
    match row {
        Some(row) => Verdict::Pass(format!(
            "{how}: a say: post on {topic} reached @{a} ({}); the bridge's cursor file holds it: {held}",
            without_text(row)
        )),
        None => Verdict::Fail(format!(
            "no {topic} row reached @{a}'s inbox within a minute of the post ({posted}); the bridge's cursor file holds it: {held}"
        )),
    }
}

fn bridge_holds(state: &Path, sid: &str, topic: &str) -> bool {
    std::fs::read_to_string(state.join(format!("topics/{sid}")))
        .unwrap_or_default()
        .lines()
        .any(|l| l.split(' ').next() == Some(topic))
}

/// THE GAP, AS THE RING SAID IT (T22): two slow readers of the successor, the lab's own `subscribe @<a> events` client and the bridge, each stopped (`SIGSTOP`, by its pid) while `a`'s log rolls past where they read, then let go (`SIGCONT`): each must be told `GAP <local> events-dropped=<n>`. The bridge says so on the bus (`ev gap …`, found in the broker's log) and reads the session's topics again: one added while it was stopped reaches the inbox after.
#[allow(clippy::too_many_lines)]
fn slow_readers(
    desk: &Desk,
    fabric: &Fabric,
    a: &str,
    b: &str,
    local_a: Option<&str>,
    events: &mut Watch,
) -> Result<(Verdict, Verdict), Fail> {
    let at_a = format!("@{a}");
    let watch_pid = i32::try_from(events.child.id()).unwrap_or(0);
    let bridge = bridge_pid(&fabric.state).filter(|p| crate::sys::alive(*p));
    let broker_log = fabric.state.parent().map(|d| d.join("b.log"));
    let said_gaps = || {
        broker_log
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .map_or(0, |bytes| count_of(&bytes, b"events-dropped="))
    };
    let gaps_before = said_gaps();
    if events.ended().is_some() || watch_pid <= 1 {
        return Ok((
            Verdict::Fail(format!(
                "the events watch ended before its gap leg: {:?} {:?}",
                events.lines(),
                events.errs()
            )),
            Verdict::Fail("the events watch ended before its gap leg".into()),
        ));
    }
    let _ = crate::sys::kill(watch_pid, libc::SIGSTOP);
    if let Some(p) = bridge {
        let _ = crate::sys::kill(p, libc::SIGSTOP);
    }
    let resume = |bridge: Option<i32>| {
        let _ = crate::sys::kill(watch_pid, libc::SIGCONT);
        if let Some(p) = bridge {
            let _ = crate::sys::kill(p, libc::SIGCONT);
        }
    };
    let stopped_at = desk
        .status_word(a, "log_seq")
        .and_then(|s| s.parse::<u64>().ok());
    let added = desk.ctl(&[&at_a, "topic", "add", GAP_TOPIC]);
    let filler = "seamlessgap".repeat(80);
    let mut rolled = None;
    let mut wrote = 0u32;
    if let Some(s0) = stopped_at {
        for i in 0..ROLL_MAX {
            if let Err(stopped) = stop::sleep(Duration::ZERO) {
                resume(bridge);
                return Err(Fail::from(stopped));
            }
            let r = desk.ctl(&[&at_a, "meta", "set", "description", &format!("{filler}{i}")]);
            if r.code != 0 {
                break;
            }
            wrote += 1;
            if i % 20 == 19 {
                let oldest = field(&desk.ctl(&[&at_a, "log", "n=1"]).header(), "oldest")
                    .and_then(|o| o.parse::<u64>().ok());
                if oldest.is_some_and(|o| o > s0 + ROLL_PAST) {
                    rolled = oldest;
                    break;
                }
            }
        }
    }
    let mark = events.lines().len();
    resume(bridge);
    let Some(oldest) = rolled else {
        return Ok((
            Verdict::Fail(format!(
                "@{a}'s log never rolled {ROLL_PAST} records past seq {stopped_at:?} in {wrote} description sets"
            )),
            Verdict::Fail("the log never rolled past the stopped bridge".into()),
        ));
    };
    let gaps = until(HANG, || {
        let gaps = events_gaps(&events.lines(), mark);
        (!gaps.is_empty()).then_some(gaps)
    })?;
    let gap = match gaps {
        Some(gaps) if gaps.iter().any(|(t, _)| Some(t.as_str()) == local_a) => {
            Verdict::Pass(format!(
                "the stopped watch was told {gaps:?} once let go (the shell tab is local {}; {wrote} description sets rolled the log from seq {} to oldest={oldest})",
                local_a.unwrap_or("?"),
                stopped_at.unwrap_or(0)
            ))
        }
        Some(gaps) => Verdict::Fail(format!(
            "GAPs for others than the shell tab (local {local_a:?}): {gaps:?}"
        )),
        None => Verdict::Fail(format!(
            "no GAP events-dropped= within a minute of letting the watch go ({wrote} sets, oldest={oldest} past seq {}); it said: {:?}; stderr {:?}",
            stopped_at.unwrap_or(0),
            events.lines().get(mark..).unwrap_or_default(),
            events.errs()
        )),
    };
    let Some(bridge) = bridge else {
        return Ok((
            gap,
            Verdict::Fail(format!(
                "no live bridge pid in {}",
                fabric.state.join("pid").display()
            )),
        ));
    };
    let held = until(HANG, || {
        bridge_holds(&fabric.state, a, GAP_TOPIC).then_some(())
    })?;
    let delivered = topic_after(
        desk,
        fabric,
        a,
        b,
        GAP_TOPIC,
        "the bridge, let go, read the topic added while it was stopped",
    )?;
    let gaps_after = said_gaps();
    let same = bridge_pid(&fabric.state) == Some(bridge) && crate::sys::alive(bridge);
    let gap_topics = match (held, delivered, gaps_after > gaps_before, same) {
        (Some(()), Verdict::Pass(said), true, true) => Verdict::Pass(format!(
            "bridge {bridge} said events-dropped= on the bus ({gaps_before} -> {gaps_after} in the broker's log); {said} (topic add: {})",
            added.first()
        )),
        (held, delivered, told, same) => Verdict::Fail(format!(
            "bridge {bridge}: topic held {}, the bus told of a gap {told} ({gaps_before} -> {gaps_after}), the same bridge runs {same}; {} (topic add: {})",
            held.is_some(),
            delivered.said(),
            added.first()
        )),
    };
    Ok((gap, gap_topics))
}

fn count_of(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn person_keys(desk: &Desk, a: &str, b: &str) -> Result<Verdict, Fail> {
    let verdict = keys_on(desk, a, b);
    let _ = desk.ctl(&[&format!("@{a}"), "ctrl", "u"]);
    verdict
}

fn keys_on(desk: &Desk, a: &str, b: &str) -> Result<Verdict, Fail> {
    let at_a = format!("@{a}");
    if let Err(why) = crate::meta_keys::await_idle()? {
        return Ok(Verdict::CouldNotRun(format!(
            "hwkey presses keys at an idle desk: {why}"
        )));
    }
    let _ = desk.ctl(&["raise", a]);
    let read = || desk.ctl(&[&at_a, "log", "kinds=mail,human"]).stdout;
    let seq = || {
        desk.status_word(a, "human_seq")
            .and_then(|s| s.parse::<u64>().ok())
    };
    let press = |key: &str| -> Result<String, Verdict> {
        let r = desk.ctl(&["hwkey", key]);
        if r.code == 0 && r.stdout.starts_with("OK ") {
            Ok(r.first())
        } else {
            let refusal = format!("{} {}", r.stdout.trim(), r.stderr.trim());
            Err(press_refused(key, &refusal, &desk.ctl(&["privacy"]).stdout))
        }
    };
    let (h0, m0) = (human_marks(&read()), mail_answers(&read()));
    let s0 = seq();
    let first = match press("a") {
        Ok(said) => said,
        Err(v) => return Ok(v),
    };
    let burst_at = Instant::now();
    let opened = until(HANG, || (human_marks(&read()) > h0).then_some(()))?;
    let s1 = seq();
    if opened.is_none() {
        return Ok(Verdict::Fail(format!(
            "{first}: no human mark within a minute (human_seq {s0:?} -> {s1:?})"
        )));
    }
    let had_mail = mail_count(&read());
    let _ = desk.ctl(&[
        &format!("@{b}"),
        "post",
        &format!("to=@{a}"),
        "kind=task",
        "seamless key",
    ]);
    if until(HANG, || (mail_count(&read()) > had_mail).then_some(()))?.is_none() {
        return Ok(Verdict::Fail(format!(
            "{first}; the second mail never reached @{a}'s log"
        )));
    }
    if burst_at.elapsed().as_millis() >= u128::from(BURST_GAP_MS) {
        return Ok(Verdict::CouldNotRun(format!(
            "the mail took {} s, past the person's 30 s burst, so the next key opens a burst of its own",
            burst_at.elapsed().as_secs()
        )));
    }
    let second = match press("b") {
        Ok(said) => said,
        Err(v) => return Ok(v),
    };
    let answered = until(HANG, || (mail_answers(&read()) > m0).then_some(()))?;
    let s2 = seq();
    let third = match press("c") {
        Ok(said) => said,
        Err(v) => return Ok(v),
    };
    let counted = until(HANG, || (seq() > s2).then_some(()))?.is_some();
    let s3 = seq();
    let (h3, m3) = (human_marks(&read()), mail_answers(&read()));
    Ok(keys_verdict(
        Keys {
            answered: answered.is_some(),
            third_counted: counted,
            human: (h0, h3),
            why_mail: (m0, m3),
        },
        &[&first, &second, &third],
        &format!("human_seq {s0:?} -> {s1:?} -> {s2:?} -> {s3:?}"),
    ))
}

/// What the hwkey leg read after its three keys.
#[derive(Clone, Copy, Debug)]
struct Keys {
    answered: bool,
    third_counted: bool,
    human: (usize, usize),
    why_mail: (usize, usize),
}

fn keys_verdict(k: Keys, said: &[&str; 3], seqs: &str) -> Verdict {
    let [first, second, third] = said;
    let (h0, h3) = k.human;
    let (m0, m3) = k.why_mail;
    if !k.answered {
        Verdict::Fail(format!(
            "{second}: no human why=mail mark within a minute ({seqs})"
        ))
    } else if !k.third_counted {
        Verdict::Fail(format!(
            "{third}: the third key never reached the session within a minute ({seqs}), so its mark-free row proves nothing"
        ))
    } else if m3 == m0 + 1 && h3 == h0 + 2 {
        Verdict::Pass(format!(
            "{first} opened a burst (one human mark); a mail inside it; {second} wrote one human why=mail mark; {third}, counted, wrote none ({seqs})"
        ))
    } else {
        Verdict::Fail(format!(
            "human marks {h0} -> {h3}, why=mail {m0} -> {m3} after three keys ({first} / {second} / {third}; {seqs})"
        ))
    }
}

fn press_refused(key: &str, refusal: &str, privacy: &str) -> Verdict {
    let said = refusal.to_ascii_lowercase();
    let wall = privacy
        .split_whitespace()
        .filter_map(|t| t.split_once('='))
        .find(|(grant, state)| {
            matches!(*state, "denied" | "missing" | "no")
                && said.contains(&grant.replace('_', " ").to_ascii_lowercase())
        });
    match wall {
        Some((grant, state)) => Verdict::CouldNotRun(format!(
            "hwkey {key}: {refusal}; privacy says {grant}={state}"
        )),
        None => Verdict::Fail(format!(
            "hwkey {key} was refused, and no grant privacy reports missing explains it: {refusal}"
        )),
    }
}

fn median_twice(v: &[u64]) -> Option<u128> {
    let mut v = v.to_vec();
    v.sort_unstable();
    let n = v.len();
    match n {
        0 => None,
        _ => Some(u128::from(v[(n - 1) / 2]) + u128::from(v[n / 2])),
    }
}

fn figure_text(twice: u128, milliseconds: bool) -> String {
    let (whole, fraction, places) = if milliseconds {
        (twice / 200, (twice % 200) * 5, 3)
    } else {
        (twice / 2, (twice % 2) * 5, 1)
    };
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:0places$}")
            .trim_end_matches('0')
            .to_string()
    }
}

fn judge_present(baseline: &[Figures], tested: &[Figures]) -> Verdict {
    if baseline.is_empty() || tested.is_empty() {
        return Verdict::Fail(format!(
            "no runs to judge ({} baseline, {} tested)",
            baseline.len(),
            tested.len()
        ));
    }
    for (which, runs) in [("baseline", baseline), ("tested", tested)] {
        for (i, run) in runs.iter().enumerate() {
            for (key, count) in [("n_present", run.n_present), ("n_input", run.n_input)] {
                if count.is_none_or(|n| n == 0) {
                    return Verdict::Fail(format!(
                        "{key} missing or zero in {which} run {}: no samples to judge",
                        i + 1
                    ));
                }
            }
        }
    }
    let clause_off: Vec<String> = [("baseline", baseline), ("tested", tested)]
        .iter()
        .flat_map(|(which, runs)| {
            runs.iter()
                .enumerate()
                .filter(|(_, f)| f.now_rows.is_empty())
                .map(move |(i, _)| format!("{which} run {}", i + 1))
        })
        .collect();
    if !clause_off.is_empty() {
        return Verdict::Fail(format!(
            "no `now:` row in the tab menu of {}: the clause was off there, so those are not clause-on figures",
            clause_off.join(", ")
        ));
    }
    type Of = fn(&Figures) -> Option<u64>;
    let figures: [(&str, bool, Of); 4] = [
        ("n_present", false, |f| f.n_present),
        ("present_p50_ms", true, |f| f.present[0]),
        ("present_p95_ms", true, |f| f.present[1]),
        ("present_p99_ms", true, |f| f.present[2]),
    ];
    let mut said = Vec::new();
    let mut ok = true;
    for (key, milliseconds, of) in figures {
        let base: Option<Vec<u64>> = baseline.iter().map(of).collect();
        let test: Option<Vec<u64>> = tested.iter().map(of).collect();
        let (Some(base), Some(test)) = (base, test) else {
            said.push(format!("{key} missing from a run"));
            ok = false;
            continue;
        };
        let (Some(lo), Some(hi), Some(mid)) = (
            base.iter().copied().min(),
            base.iter().copied().max(),
            median_twice(&test),
        ) else {
            continue;
        };
        let lo = u128::from(lo) * 2;
        let hi = u128::from(hi) * 2;
        let runs = |v: &[u64]| {
            v.iter()
                .map(|x| figure_text(u128::from(*x) * 2, milliseconds))
                .collect::<Vec<_>>()
                .join("/")
        };
        let (word, fine) = if !milliseconds {
            if (lo..=hi).contains(&mid) {
                ("within", true)
            } else {
                ("outside", false)
            }
        } else if mid > hi {
            ("OVER", false)
        } else if mid < lo {
            ("under", true)
        } else {
            ("within", true)
        };
        ok &= fine;
        let mid = figure_text(mid, milliseconds);
        let lo = figure_text(lo, milliseconds);
        let hi = figure_text(hi, milliseconds);
        said.push(format!(
            "{key} tested median {mid} ({}) {word} baseline {lo}..{hi} ({})",
            runs(&test),
            runs(&base)
        ));
    }
    let text = said.join("; ");
    if ok {
        Verdict::Pass(text)
    } else {
        Verdict::Fail(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gap_is_read_only_past_where_the_watch_was_armed() {
        let lines: Vec<String> = [
            "sub 1 s-a",
            "GAP 1 events-dropped=3",
            "EVENT 1 meta field=title value=x",
            "EVENT * bye handoff",
            "sub 1 s-a",
            "GAP 1 events-dropped=65536",
            "GAP 1 log-lost=2 why=handoff resync_to=9 ep=3",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            events_gaps(&lines, 0),
            [("1".to_string(), 3), ("1".to_string(), 65536)]
        );
        assert_eq!(events_gaps(&lines, 3), [("1".to_string(), 65536)]);
        assert!(
            events_gaps(&lines, 6).is_empty(),
            "a log gap is not an events gap"
        );
    }

    #[test]
    fn the_log_parsers_read_marks_by_their_kind_word() {
        let log = "OK 5 v=2 as_of=10 ep=3 oldest=1\n\
                   7 mark - mail dir=in kind=task off=12 by=bridge src=fabric conf=exact\n\
                   8 mark - human by=human src=input conf=strong\n\
                   9 mark - human why=mail by=human src=input conf=strong\n\
                   10 mark - note text=why%3Dmail by=ctl src=mark conf=claim\n\
                   11 mark - mail dir=in kind=note off=15 by=bridge src=fabric conf=exact\n\
                   12 mark - mail dir=out kind=task off=16 by=bridge src=fabric conf=exact\n";
        assert_eq!(
            mail_answers(log),
            1,
            "only a human mark that says why=mail answers mail"
        );
        assert_eq!(human_marks(log), 2);
        assert_eq!(mail_count(log), 2, "dir=out is not delivered mail");
        assert_eq!(
            mail_off(log),
            Some(15),
            "the newest delivered mail's offset"
        );
        assert_eq!(mail_off("8 mark - human by=human\n"), None);
        assert_eq!(delivery_marks(log, 12), [(7, None)]);
        assert!(
            delivery_marks(log, 16).is_empty(),
            "a mail sent is not delivered"
        );
        assert!(
            delivery_marks(log, 1).is_empty(),
            "off=1 is not off=12's prefix"
        );
        assert_eq!(
            delivery_marks(
                "21 mark - mail dir=in kind=task off=78 inbox=1 t=1700\n\
                 65565 mark - mail dir=in kind=task off=78 inbox=4098 t=1911\n",
                78
            ),
            [(21, Some(1700)), (65565, Some(1911))],
            "each delivery's seq and t="
        );
        assert_eq!(
            human_marks("OK 1 v=2 human=3\n8 open t1 human\n"),
            0,
            "a mark is `<seq> mark <node> <kind>`"
        );
        assert_eq!(
            without_text("7 mark - mail dir=in text=QZP6SECRET1 off=12\n8 x"),
            "7 mark - mail dir=in off=12 | 8 x"
        );
    }

    #[test]
    fn a_brief_links_a_turn_only_with_the_tilde() {
        let brief = "s-x idle\n  t2 closed by=agent mail=@90~\n  t1 closed mail=@12 cause=@5\n";
        assert_eq!(brief_links(brief), [90]);
        assert!(brief_links("t1 open mail=@x~").is_empty());
    }

    #[test]
    fn the_broker_log_count_is_of_whole_needles() {
        assert_eq!(
            count_of(
                b"gap 1 events-dropped=3 .. gap 1 events-dropped=9",
                b"events-dropped="
            ),
            2
        );
        assert_eq!(count_of(b"events-dropped", b"events-dropped="), 0);
        assert_eq!(count_of(b"abc", b""), 0);
    }

    fn figures(n: u64, p: [f64; 3]) -> Figures {
        let mut figures = Figures::read(&format!(
            "OK n_present={n} present_p50_ms={:.2} present_p95_ms={:.2} present_p99_ms={:.2} n_input=2",
            p[0], p[1], p[2]
        ));
        figures
            .now_rows
            .push("tab-menu item text=\"now: approval\"".into());
        figures
    }

    #[test]
    fn the_present_leg_judges_the_tested_median_against_the_baselines_runs() {
        let base = [
            figures(340, [1.0, 1.5, 6.0]),
            figures(344, [1.2, 1.7, 9.0]),
            figures(342, [1.1, 1.4, 5.0]),
        ];
        let same = [
            figures(341, [1.1, 1.6, 7.0]),
            figures(343, [1.3, 1.9, 2.0]),
            figures(342, [1.0, 1.2, 6.0]),
        ];
        assert!(
            matches!(judge_present(&base, &same), Verdict::Pass(_)),
            "{:?}",
            judge_present(&base, &same)
        );
        let faster = vec![figures(342, [0.2, 0.3, 1.0]); 3];
        assert!(
            matches!(judge_present(&base, &faster), Verdict::Pass(ref s) if s.contains("under")),
            "a latency under the baseline is no regression"
        );
        let slow_p50 = [
            figures(342, [1.3, 1.5, 6.0]),
            figures(342, [1.4, 1.5, 6.0]),
            figures(342, [0.9, 1.5, 6.0]),
        ];
        let v = judge_present(&base, &slow_p50);
        assert!(
            matches!(&v, Verdict::Fail(s) if s.contains("present_p50_ms tested median 1.3 ") && s.contains("OVER")),
            "a median over the baseline's highest run is OVER, p50 included: {v:?}"
        );
        assert_eq!(
            v.said().matches("OVER").count(),
            1,
            "only that figure: {v:?}"
        );
        let more = vec![figures(360, [1.0, 1.4, 6.0]); 3];
        assert!(
            matches!(judge_present(&base, &more), Verdict::Fail(ref s) if s.contains("n_present tested median 360 ") && s.contains("outside")),
            "n_present is held inside the baseline's runs"
        );
        let mut off = same.clone();
        off[1].now_rows.clear();
        assert!(
            matches!(judge_present(&base, &off), Verdict::Fail(ref s) if s.contains("tested run 2")),
            "a run with the clause off is not a clause-on figure"
        );
        let mut base_off = base.clone();
        base_off[0].now_rows.clear();
        assert!(
            matches!(judge_present(&base_off, &same), Verdict::Fail(ref s) if s.contains("baseline run 1")),
            "nor is a baseline run with it off"
        );
        let mut missing = same.clone();
        missing[0].present[2] = None;
        assert!(matches!(judge_present(&base, &missing), Verdict::Fail(_)));
        assert!(matches!(judge_present(&[], &same), Verdict::Fail(_)));
        assert_eq!(median_twice(&[3, 1, 2]), Some(4));
        assert_eq!(median_twice(&[4, 1, 2, 3]), Some(5));
        assert_eq!(median_twice(&[]), None);
    }

    #[test]
    fn a_reported_even_median_at_the_baseline_boundary_is_not_over() {
        let read = |n: u64, p: [&str; 3]| {
            let mut figures = Figures::read(&format!(
                "OK n_present={n} present_p50_ms={} present_p95_ms={} present_p99_ms={} n_input=3",
                p[0], p[1], p[2]
            ));
            figures
                .now_rows
                .push("tab-menu item text=\"now: approval\"".into());
            figures
        };
        let baseline = [
            read(347, ["1.84", "4.72", "6.82"]),
            read(349, ["1.97", "4.72", "6.29"]),
            read(348, ["1.97", "5.77", "9.44"]),
            read(348, ["1.84", "4.72", "7.34"]),
        ];
        let tested = [
            read(349, ["1.97", "5.77", "10.49"]),
            read(347, ["1.97", "5.77", "11.53"]),
            read(347, ["1.97", "5.24", "6.29"]),
            read(348, ["1.84", "4.72", "8.39"]),
        ];
        // The inputs are reported to 0.01 ms, so an even median can be halfway
        // between those units. Neither a full unit nor a half-unit overshoot
        // may be rounded back inside the baseline.
        for upper_middle in ["10.51", "10.50"] {
            let mut over = tested.clone();
            over[0] = read(349, ["1.97", "5.77", upper_middle]);
            let verdict = judge_present(&baseline, &over);
            assert!(
                matches!(&verdict, Verdict::Fail(_)),
                "a genuine p99 median above 9.44 ms must fail ({upper_middle}): {verdict:?}"
            );
        }
        // The retained live run reported middle p99 values 8.39 and 10.49:
        // their exact reported median is 9.44, the baseline's upper bound.
        let verdict = judge_present(&baseline, &tested);
        assert!(
            matches!(&verdict, Verdict::Pass(_)),
            "an equal reported p99 boundary must pass without a binary-float overshoot: {verdict:?}"
        );
    }

    #[test]
    fn the_latch_passes_only_on_the_outgoing_processs_own_mark() {
        let before = [(21, Some(1700))];
        let rows = "rows";
        assert!(matches!(
            latch_verdict(Ok(0), &[78], 78, &before, &before, rows),
            Verdict::Pass(_)
        ));
        let again = [(21, Some(1700)), (65565, Some(1911))];
        assert!(
            matches!(latch_verdict(Ok(0), &[], 78, &before, &again, rows), Verdict::Fail(ref s) if s.contains("delivered 2 times")),
            "a second delivery mark is named"
        );
        let own = [(65565, Some(1911))];
        assert!(
            matches!(latch_verdict(Ok(0), &[78], 78, &before, &own, rows), Verdict::Fail(ref s) if s.contains("not the carry's")),
            "a link to the successor's own mark (a carry that lost the waiting mail, or a log that started over) is not the latch's crossing"
        );
        assert!(
            matches!(
                latch_verdict(Ok(0), &[78], 78, &before, &again, rows),
                Verdict::Fail(_)
            ),
            "nor is a link beside a second mark"
        );
        assert!(matches!(
            latch_verdict(Ok(0), &[77], 78, &before, &before, rows),
            Verdict::Fail(_)
        ));
        assert!(matches!(
            latch_verdict(Ok(0), &[78], 78, &again, &again, rows),
            Verdict::Fail(ref s) if s.contains("not once")
        ));
        assert!(matches!(
            latch_verdict(Err("no turn".into()), &[78], 78, &before, &before, rows),
            Verdict::Fail(ref s) if s == "no turn"
        ));
    }

    #[test]
    fn the_third_key_counts_only_once_it_reached_the_session() {
        let said = ["k1", "k2", "k3"];
        let keys = |answered, third_counted, h3| Keys {
            answered,
            third_counted,
            human: (4, h3),
            why_mail: (1, 2),
        };
        assert!(matches!(
            keys_verdict(keys(true, true, 6), &said, "seqs"),
            Verdict::Pass(_)
        ));
        assert!(
            matches!(keys_verdict(keys(true, false, 6), &said, "seqs"), Verdict::Fail(ref s) if s.contains("never reached")),
            "a third key that never arrived leaves the same marks, and proves nothing"
        );
        assert!(matches!(
            keys_verdict(keys(true, true, 7), &said, "seqs"),
            Verdict::Fail(_)
        ));
        assert!(matches!(
            keys_verdict(keys(false, true, 6), &said, "seqs"),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn a_refused_hwkey_is_a_desk_wall_only_when_privacy_names_its_grant_missing() {
        let privacy = "OK 3\nschema=1\nfull_disk_access=granted probe=ok\naccessibility=denied\n";
        for refusal in [
            "ERR hwkey: no frontmost AppKit window to post into",
            "ERR hwkey: a sudo password sheet is up",
            "ERR hwkey: refused while a tab menu is open",
        ] {
            assert!(
                matches!(press_refused("a", refusal, privacy), Verdict::Fail(ref s) if s.contains(refusal)),
                "{refusal}: a FAIL that quotes it"
            );
        }
        let walled = "ERR hwkey: the Accessibility grant is missing";
        assert!(matches!(
            press_refused("a", walled, privacy),
            Verdict::CouldNotRun(ref s) if s.contains("accessibility=denied")
        ));
        assert!(
            matches!(
                press_refused("a", walled, "accessibility=granted"),
                Verdict::Fail(_)
            ),
            "a grant privacy holds is no wall"
        );
        assert!(matches!(
            press_refused("a", "ERR hwkey: Full Disk Access", privacy),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn a_topic_verdict_says_the_row_without_its_words() {
        let row = "msg 7 off=40 t=1 from=s-x@n-y kind=note trust=agent topic=seamless.t len=11 text=QZP6SECRET1";
        let v = topic_verdict("how", "seamless.t", "s-a", Some(row), true, "OK 1");
        assert!(
            matches!(&v, Verdict::Pass(s) if s.contains("off=40") && !s.contains("QZP6SECRET1") && !s.contains("text=")),
            "{v:?}"
        );
        assert!(matches!(
            topic_verdict("how", "seamless.t", "s-a", None, false, "OK 1"),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn a_watch_that_ran_out_of_its_own_timeout_is_not_called_a_redial_failure() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let timed = watch_ended("w", 0, &lines(&["OK timeout"]), &[], "(not 75)");
        assert!(
            matches!(&timed, Verdict::Fail(s) if s.contains("the lab's bound")),
            "{timed:?}"
        );
        let ended = watch_ended("w", 1, &lines(&["ERR exited"]), &lines(&["x"]), "(not 75)");
        assert!(
            matches!(&ended, Verdict::Fail(s) if s.contains("ended 1 (not 75)") && !s.contains("bound")),
            "{ended:?}"
        );
    }

    #[test]
    fn a_successor_that_never_answers_is_still_named_for_its_end() {
        let mut probes = [Some(10), Some(10), Some(42)].into_iter();
        let (next, seen) = successor_of(Duration::ZERO, 10, || probes.next().flatten(), || false)
            .expect("no stop");
        assert_eq!(next, None);
        assert!(seen.is_empty(), "the parent is not a successor: {seen:?}");
        let (next, seen) =
            successor_of(Duration::ZERO, 10, || Some(42), || false).expect("no stop");
        assert_eq!(
            (next, seen),
            (None, vec![42]),
            "seen though it never answered, so it is ended by its pid"
        );
        let (next, seen) = successor_of(Duration::ZERO, 10, || Some(42), || true).expect("no stop");
        assert_eq!((next, seen), (Some(42), vec![42]));
    }

    #[test]
    fn the_figures_are_read_from_their_own_keys() {
        let f = Figures::read(
            "OK n_present=338 present_p50_ms=1.18 present_p95_ms=1.44 present_p99_ms=5.77 \
             n_input=2 input_p50_ms=3.00",
        );
        assert_eq!(f.n_present, Some(338));
        assert_eq!(f.present, [Some(118), Some(144), Some(577)]);
        assert_eq!(f.n_input, Some(2));
        assert_eq!(Figures::read("OK n_input=2").n_present, None);
    }

    #[test]
    fn every_present_run_has_present_and_input_samples_before_it_can_be_judged() {
        let read = |counts: &str| {
            let mut figures = Figures::read(&format!(
                "OK {counts} present_p50_ms=0.00 present_p95_ms=0.00 present_p99_ms=0.00"
            ));
            figures
                .now_rows
                .push("tab-menu item text=\"now: approval\"".into());
            figures
        };
        let populated = vec![read("n_present=1 n_input=1"); 3];
        assert!(
            matches!(judge_present(&populated, &populated), Verdict::Pass(_)),
            "one sample is valid, even when its reported latency rounds to zero"
        );

        let mut accepted = Vec::new();
        for cohort in ["baseline", "tested"] {
            for run in 0..populated.len() {
                for (missing, counts) in [
                    ("missing n_present", "n_input=1"),
                    ("zero n_present", "n_present=0 n_input=1"),
                    ("missing n_input", "n_present=1"),
                    ("zero n_input", "n_present=1 n_input=0"),
                ] {
                    let mut baseline = populated.clone();
                    let mut tested = populated.clone();
                    let runs = if cohort == "baseline" {
                        &mut baseline
                    } else {
                        &mut tested
                    };
                    runs[run] = read(counts);
                    let verdict = judge_present(&baseline, &tested);
                    if !matches!(verdict, Verdict::Fail(_)) {
                        accepted.push(format!("{cohort} run {} {missing}: {verdict:?}", run + 1));
                    }
                }
            }
        }
        assert!(
            accepted.is_empty(),
            "an invalid run cannot hide among populated runs: {}",
            accepted.join("; ")
        );
    }

    #[test]
    fn the_present_judge_rejects_invalid_wire_units_and_keeps_large_counts_exact() {
        let read = |n: u64, p99: &str| {
            let mut figures = Figures::read(&format!(
                "OK n_present={n} present_p50_ms=1.00 present_p95_ms=1.00 present_p99_ms={p99} n_input=2"
            ));
            figures
                .now_rows
                .push("tab-menu item text=\"now: approval\"".into());
            figures
        };
        for value in ["1", "1.2", "1.20", "0.00", "184467440737095516.15"] {
            let f = read(342, value);
            assert!(
                matches!(
                    judge_present(std::slice::from_ref(&f), std::slice::from_ref(&f)),
                    Verdict::Pass(_)
                ),
                "valid reported value: {value}"
            );
        }
        for value in [
            "NaN",
            "inf",
            "-0.01",
            "+1",
            "1e0",
            "1.234",
            "1.",
            ".5",
            "",
            "184467440737095516.16",
            "184467440737095517",
        ] {
            let verdict = judge_present(&[read(342, "1.20")], &[read(342, value)]);
            assert!(
                matches!(&verdict, Verdict::Fail(s) if s.contains("present_p99_ms missing")),
                "invalid reported value {value:?}: {verdict:?}"
            );
        }
        let largest = u64::MAX;
        let pair = [read(largest - 1, "1.20"), read(largest, "1.20")];
        assert!(matches!(judge_present(&pair, &pair), Verdict::Pass(_)));
        assert!(matches!(
            judge_present(&[read(largest - 1, "1.20")], &pair),
            Verdict::Fail(_)
        ));
        assert!(matches!(
            judge_present(&[read(largest - 1, "1.20")], &[read(largest, "1.20")]),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn a_failed_leg_outranks_one_that_could_not_run() {
        let pass = ("a", Verdict::Pass(String::new()));
        let cnr = ("b", Verdict::CouldNotRun(String::new()));
        let fail = ("c", Verdict::Fail(String::new()));
        assert_eq!(report(std::slice::from_ref(&pass)), 0);
        assert_eq!(report(&[pass.clone(), cnr.clone()]), 3);
        assert_eq!(report(&[cnr, fail, pass]), 1);
    }
}

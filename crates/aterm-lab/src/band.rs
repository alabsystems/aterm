// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The scheduling band a measured instance runs in (`instance --band`).

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::Fail;
use crate::paths::paths;

/// Where an instance runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// A child of this lab, inside the caller's coalition, at the caller's role.
    Agent,
    /// Its own launchd job: a new coalition, role `UI_FOCAL` (aterm.app's band).
    App,
}

impl Band {
    /// `--band` as given, else the default: the app band for a headless instance.
    /// # Errors
    /// An unknown band, or the app band asked of a window.
    pub fn pick(given: Option<&str>, window: bool) -> Result<Band, Fail> {
        match (given, window) {
            (None, true) | (Some("agent"), _) => Ok(Band::Agent),
            (None | Some("app"), false) => Ok(Band::App),
            (Some("app"), true) => Err(Fail::usage(
                "instance: --band app is headless-only (a windowed instance runs under its keeper)",
            )),
            (Some(other), _) => Err(Fail::usage(format!(
                "instance: --band takes app or agent, not '{other}'"
            ))),
        }
    }

    /// The band's word, as `--band` takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Band::Agent => "agent",
            Band::App => "app",
        }
    }
}

/// The darwin role the app band runs at: `PRIO_DARWIN_ROLE_UI_FOCAL`, what LaunchServices gives the frontmost app.
pub const UI_FOCAL: u64 = 1;

/// `PRIO_DARWIN_ROLE_UI`: the role aterm-pty gives each shell before its exec.
const UI: i32 = 2;

/// The world's entries a job reads: its environment (NUL-separated
/// `KEY=VALUE`), the lifeline FIFO, and the pid it reports.
const ENV_FILE: &str = "band.env";
const LIFELINE: &str = "band.lifeline";
const PID_FILE: &str = "band.pid";
const PLIST: &str = "band.plist";

const LAUNCHCTL: &str = "/bin/launchctl";

/// An instance running as its own launchd job.
pub struct Job {
    pub label: String,
    /// aterm's pid (the job's process: [`band_main`] execs it).
    pub pid: i32,
    /// The lifeline's held end; dropping it cuts the lifeline.
    held: Option<OwnedFd>,
}

impl Job {
    /// Start `line` (program first: aterm and its flags, `--lifeline-fd 0` among them) as its own launchd job ([`launch`]) with exactly `env`, in `cwd`, its output appended to `log`, and wait for it to report its pid.
    /// # Errors
    /// The world's entries could not be made, `launchctl` refused the job, or the job never reported (its log says why; the job is removed).
    pub fn start(
        dir: &Path,
        line: &[String],
        env: &[(String, String)],
        cwd: &Path,
        log: &Path,
    ) -> Result<Job, String> {
        let fifo = dir.join(LIFELINE);
        crate::sys::make_fifo(&fifo).map_err(|e| format!("the lifeline FIFO: {e}"))?;
        let held: OwnedFd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .map_err(|e| format!("the lifeline's held end: {e}"))?
            .into();
        let mut vars = Vec::new();
        for (k, v) in env {
            vars.extend_from_slice(format!("{k}={v}").as_bytes());
            vars.push(0);
        }
        std::fs::write(dir.join(ENV_FILE), vars)
            .map_err(|e| format!("cannot write the job's environment: {e}"))?;
        let world = dir.file_name().map_or_else(String::new, |n| {
            n.to_string_lossy()
                .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        });
        let label = format!("aterm-lab.instance.{world}.{}", std::process::id());
        launch(dir, &label, line, cwd, log)?;
        let end = Instant::now() + crate::instance::BOOT;
        loop {
            if let Some(pid) = std::fs::read_to_string(dir.join(PID_FILE))
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                return Ok(Job {
                    label,
                    pid,
                    held: Some(held),
                });
            }
            if Instant::now() >= end {
                remove(&label);
                return Err(format!(
                    "the instance's launchd job {label} never started in {} s",
                    crate::instance::BOOT.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether its process still runs (launchd reaps it; a restart has another pid).
    #[must_use]
    pub fn running(&self) -> bool {
        crate::sys::alive(self.pid)
    }

    /// Cut its lifeline: a headless aterm ends at the end of file.
    pub fn cut(&mut self) {
        self.held = None;
    }

    /// Remove the job by its label, so launchd never starts it again.
    pub fn remove(&self) {
        remove(&self.label);
    }
}

/// The job `label` named by the domain [`launch`] loads it in, this user's
/// GUI domain: `gui/<uid>/<label>`.
fn target(label: &str) -> String {
    format!("gui/{}/{label}", crate::sys::getuid())
}

/// Unload the job `label` (`launchctl bootout gui/<uid>/<label>`; it ends the
/// job's process if one still runs).
fn remove(label: &str) {
    let _ = aterm_uds::spawnfd::command(LAUNCHCTL)
        .arg("bootout")
        .arg(target(label))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The job's definition: `args` (its first process's argv, [`band_main`]'s),
/// `ProcessType` `App` (no QoS clamp, an APP-type task, as launchd starts an
/// app), started at load and kept alive (a restart after the program ends
/// removes the job, [`band_main`]), restarted 1 s after an end rather than
/// launchd's default 10 s: that restart only removes the job, so nothing
/// gains by the wait.
fn job_plist(label: &str, args: &[String]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST \
         1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n",
    );
    out.push_str(&format!(
        "<key>Label</key><string>{}</string>\n",
        esc(label)
    ));
    out.push_str("<key>ProcessType</key><string>App</string>\n");
    out.push_str("<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n");
    out.push_str("<key>ThrottleInterval</key><integer>1</integer>\n");
    out.push_str("<key>ProgramArguments</key><array>\n");
    for a in args {
        out.push_str(&format!("<string>{}</string>\n", esc(a)));
    }
    out.push_str("</array>\n</dict></plist>\n");
    out
}

/// Load `line` (program first) as the launchd job `label` in this user's GUI domain, its first process `aterm-lab band` ([`band_main`]) over `dir`'s world (its environment file and lifeline FIFO, which the caller made), in `cwd`, its output appended to `log`.
/// # Errors
/// A path is not UTF-8 (a property list cannot carry it), the definition could not be written, or `launchctl` refused it.
pub fn launch(
    dir: &Path,
    label: &str,
    line: &[String],
    cwd: &Path,
    log: &Path,
) -> Result<(), String> {
    let utf8 = |p: &Path| {
        p.to_str().map(str::to_string).ok_or_else(|| {
            format!(
                "{} is not UTF-8, which the job's definition cannot carry; `--band agent` starts \
                 the instance as this command's child instead",
                p.display()
            )
        })
    };
    let mut args = vec![
        utf8(&paths().exe)?,
        "band".to_string(),
        utf8(dir)?,
        label.to_string(),
        "--cwd".to_string(),
        utf8(cwd)?,
        "--log".to_string(),
        utf8(log)?,
        "--".to_string(),
    ];
    args.extend_from_slice(line);
    let plist = dir.join(PLIST);
    std::fs::write(&plist, job_plist(label, &args))
        .map_err(|e| format!("cannot write the job's definition: {e}"))?;
    let out = aterm_uds::spawnfd::command(LAUNCHCTL)
        .arg("bootstrap")
        .arg(format!("gui/{}", crate::sys::getuid()))
        .arg(&plist)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {LAUNCHCTL}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "launchctl bootstrap refused the instance ({}): {}; `--band agent` starts it as \
             this command's child instead",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// `aterm-lab band-launch <dir> <label> --cwd DIR --log FILE -- PROGRAM ARGS…`: [`launch`] alone, over a world the caller made (`tests/band.rs`).
#[must_use]
pub fn launch_main(args: &[String]) -> i32 {
    let usage = || {
        eprintln!(
            "aterm-lab band-launch: usage: band-launch <dir> <label> --cwd DIR --log FILE -- PROGRAM ARGS"
        );
        2
    };
    let Some(at) = args.iter().position(|a| a == "--") else {
        return usage();
    };
    let (head, line) = (&args[..at], &args[at + 1..]);
    let [dir, label, cwd_flag, cwd, log_flag, log] = head else {
        return usage();
    };
    if cwd_flag != "--cwd" || log_flag != "--log" || line.is_empty() {
        return usage();
    }
    match launch(Path::new(dir), label, line, Path::new(cwd), Path::new(log)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aterm-lab band-launch: {e}");
            1
        }
    }
}

/// `aterm-lab band <dir> <label> --cwd DIR --log FILE -- PROGRAM ARGS…`: the first process of an app-band job ([`Job::start`]).
#[must_use]
pub fn band_main(args: &[String]) -> i32 {
    let usage = || {
        eprintln!(
            "aterm-lab band: usage: band <dir> <label> --cwd DIR --log FILE -- PROGRAM ARGS (a launchd job's first process)"
        );
        2
    };
    let Some(at) = args.iter().position(|a| a == "--") else {
        return usage();
    };
    let (head, program) = (&args[..at], &args[at + 1..]);
    let ([dir, label, cwd_flag, cwd, log_flag, log], Some((prog, _))) =
        (head, program.split_first())
    else {
        return usage();
    };
    if cwd_flag != "--cwd" || log_flag != "--log" {
        return usage();
    }
    let dir = PathBuf::from(dir);
    let marker = dir.join(format!("band.{label}.ran"));
    let first = dir.is_dir()
        && OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .is_ok();
    if !first {
        remove(label);
        return 0;
    }
    let env: Vec<(String, String)> = match std::fs::read(dir.join(ENV_FILE)) {
        Ok(bytes) => bytes
            .split(|b| *b == 0)
            .filter_map(|kv| {
                let kv = String::from_utf8_lossy(kv);
                let (k, v) = kv.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect(),
        Err(e) => {
            eprintln!("aterm-lab band: the job's environment: {e}");
            return 127;
        }
    };
    let lifeline = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(dir.join(LIFELINE))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aterm-lab band: the lifeline: {e}");
            return 127;
        }
    };
    let _ = std::fs::remove_file(dir.join(LIFELINE));
    if let Err(e) = crate::sys::set_blocking(lifeline.as_raw_fd()) {
        eprintln!("aterm-lab band: the lifeline: {e}");
        return 127;
    }
    let out = match OpenOptions::new().create(true).append(true).open(log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aterm-lab band: the log {log}: {e}");
            return 127;
        }
    };
    if let Err(e) = write_pid(&dir) {
        eprintln!("aterm-lab band: the pid file: {e}");
        return 127;
    }
    let placed = crate::sys::dup_onto(lifeline.as_raw_fd(), 0)
        .and_then(|()| crate::sys::dup_onto(out.as_raw_fd(), 1))
        .and_then(|()| crate::sys::dup_onto(out.as_raw_fd(), 2))
        .and_then(|()| std::env::set_current_dir(cwd));
    if let Err(e) = placed {
        eprintln!("aterm-lab band: the job's stdio or folder: {e}");
        return 127;
    }
    let e = crate::sys::exec_as_app(prog, program, &env, UI_FOCAL);
    eprintln!("aterm-lab band: exec {prog}: {e}");
    127
}

/// `aterm-lab band-standin <file>`: the program `tests/band.rs` runs in an app-band job, in aterm's place.
#[must_use]
pub fn standin_main(args: &[String]) -> i32 {
    let [file] = args else {
        eprintln!("aterm-lab band-standin: usage: band-standin <file> (an app-band job's program)");
        return 2;
    };
    let pid = i32::try_from(std::process::id()).unwrap_or(0);
    let role = crate::sys::darwin_role(pid).map_or_else(|e| format!("({e})"), |r| r.to_string());
    let set =
        crate::sys::set_darwin_role(pid, UI).map_or_else(|e| format!("({e})"), |()| "ok".into());
    let lands = crate::sys::ui_request_lands().map_or_else(|e| format!("({e})"), |p| p.to_string());
    let line = format!("{role} {set} {lands}\n");
    let wrote = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = wrote {
        eprintln!("aterm-lab band-standin: {file}: {e}");
        return 1;
    }
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    0
}

/// This process's pid into the world's pid file, whole or not at all.
fn write_pid(dir: &Path) -> std::io::Result<()> {
    let tmp = dir.join(format!("{PID_FILE}.tmp"));
    let mut f = File::create(&tmp)?;
    writeln!(f, "{}", std::process::id())?;
    drop(f);
    std::fs::rename(tmp, dir.join(PID_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_instance_defaults_to_the_app_band_and_a_window_to_the_agent_band() {
        assert_eq!(Band::pick(None, false).ok(), Some(Band::App));
        assert_eq!(Band::pick(None, true).ok(), Some(Band::Agent));
        assert_eq!(Band::pick(Some("agent"), false).ok(), Some(Band::Agent));
        assert_eq!(Band::pick(Some("app"), false).ok(), Some(Band::App));
        assert!(Band::pick(Some("app"), true).is_err());
        assert!(Band::pick(Some("own"), false).is_err());
    }
}

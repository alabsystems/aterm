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

#[path = "band_cleanup.rs"]
mod cleanup;
#[path = "band_guard.rs"]
mod guard;

pub use cleanup::CleanupError;
#[cfg(test)]
use cleanup::{Registration, decode_registration};

/// Where an instance runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// A child of this lab, inside the caller's coalition, at the caller's role.
    Agent,
    /// Its own launchd job: a new coalition, role `UI_FOCAL` (aterm.app's band). A windowed
    /// instance's keeper is the job's process, and its aterm a child in the job's coalition.
    App,
}

impl Band {
    /// `--band` as given, else the default: the app band.
    /// # Errors
    /// An unknown band.
    pub fn pick(given: Option<&str>) -> Result<Band, Fail> {
        match given {
            None | Some("app") => Ok(Band::App),
            Some("agent") => Ok(Band::Agent),
            Some(other) => Err(Fail::usage(format!(
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

const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// An instance running as its own launchd job.
pub struct Job {
    pub label: String,
    /// The job's process's pid ([`band_main`] execs it): a headless aterm's, or a windowed one's
    /// keeper's.
    pub pid: i32,
    /// The lifeline's held end; dropping it cuts the lifeline.
    held: Option<OwnedFd>,
}

impl Job {
    /// Start `line` (program first: a headless aterm and its flags, `--lifeline-fd 0` among them, or a windowed one's keeper, which takes the lifeline on stdin) as its own launchd job ([`launch`]) with exactly `env`, in `cwd`, its output appended to `log`, wait for it to report its pid, and then give it the go ([`band_main`]).
    /// # Errors
    /// The world's entries could not be made, `launchctl` refused the job, the job never reported (its log says why), or its go could not be written; the job is removed after either of the last two.
    pub fn start(
        dir: &Path,
        line: &[String],
        env: &[(String, String)],
        cwd: &Path,
        log: &Path,
    ) -> Result<Job, String> {
        Self::start_inner(dir, line, env, cwd, log, None)
    }

    /// Start a window keeper with the original account lane description handed over before
    /// GO. The private socket authenticates both processes and acknowledges exact custody;
    /// the inherited keeper validates and forwards the same lane and optional original
    /// Functional memory reservation to the GUI. This supplies no new admission or Timing evidence.
    /// # Errors
    /// As [`Self::start`], or the keeper line, original guard or authenticated handoff refuses.
    pub fn start_with_guards(
        dir: &Path,
        line: &[String],
        env: &[(String, String)],
        cwd: &Path,
        log: &Path,
        guards: crate::window_guards::WindowGuards,
    ) -> Result<Job, String> {
        keeper_line(line)?;
        Self::start_inner(dir, line, env, cwd, log, Some(guards))
    }

    fn start_inner(
        dir: &Path,
        line: &[String],
        env: &[(String, String)],
        cwd: &Path,
        log: &Path,
        guard: Option<crate::window_guards::WindowGuards>,
    ) -> Result<Job, String> {
        let offer = guard
            .map(|guard| guard::Offer::new(dir, guard))
            .transpose()
            .map_err(|error| format!("the app-band window guard: {error}"))?;
        // A kept world reused (`instance --world`) holds what its last job left: its pid, no pid
        // of the job this launch starts, which alone may write the file read below, and a
        // lifeline no job opened when that launch failed before its job started.
        for name in [PID_FILE, LIFELINE] {
            match std::fs::remove_file(dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("the last job's {name}: {e}")),
            }
        }
        let fifo = dir.join(LIFELINE);
        crate::sys::make_fifo(&fifo).map_err(|e| format!("the lifeline FIFO: {e}"))?;
        let held: OwnedFd = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&fifo)
            .map_err(|e| format!("the lifeline's held end: {e}"))?
            .into();
        let mut vars = Vec::new();
        for (k, v) in env {
            vars.extend_from_slice(format!("{k}={v}").as_bytes());
            vars.push(0);
        }
        crate::util::atomic_write(&dir.join(ENV_FILE), &vars)
            .map_err(|e| format!("cannot write the job's environment: {e}"))?;
        let label = label_for(dir, std::process::id());
        let end = Instant::now() + crate::instance::BOOT;
        launch_inner(dir, &label, line, cwd, log, offer.as_ref(), end)?;
        loop {
            if let Some(pid) = crate::util::read_regular_text(&dir.join(PID_FILE))
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                if let Some(offer) = &offer {
                    let transferred = u32::try_from(pid)
                        .map_err(|_| std::io::Error::other("invalid app-band job PID"))
                        .and_then(|pid| offer.transfer(pid, &label, end));
                    if let Err(error) = transferred {
                        return Err(start_failure(
                            &label,
                            format!("the app-band window guard: {error}"),
                        ));
                    }
                }
                let mut held = File::from(held);
                if let Err(e) = held.write_all(&[GO]) {
                    return Err(start_failure(
                        &label,
                        format!("the go to the instance's launchd job {label}: {e}"),
                    ));
                }
                return Ok(Job {
                    label,
                    pid,
                    held: Some(held.into()),
                });
            }
            if Instant::now() >= end {
                return Err(start_failure(
                    &label,
                    format!(
                        "the instance's launchd job {label} never started in {} s",
                        crate::instance::BOOT.as_secs()
                    ),
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

    /// Cut its lifeline: a headless aterm ends at the end of file, as a keeper ends its aterm.
    pub fn cut(&mut self) {
        self.held = None;
    }

    /// Remove the owned job, establishing its registration is absent under a common
    /// operation deadline and finite native kill/reap grace.
    /// # Errors
    /// A command fails, registration stays unknown/present, or observation is interrupted.
    pub fn remove(&self) -> Result<(), CleanupError> {
        self.remove_until(Instant::now() + Duration::from_secs(60))
    }

    /// Remove this owned job within the caller's existing operation deadline. Native
    /// timeout cleanup has the same finite kill/reap grace as [`Self::remove`].
    /// # Errors
    /// As [`Self::remove`], including an already expired caller deadline.
    pub fn remove_until(&self, deadline: Instant) -> Result<(), CleanupError> {
        remove_until(&self.label, deadline)
    }

    /// Deliberately crash this actual app-band keeper without directly signaling its GUI or
    /// removing its job. Only native process absence establishes launchd reaped the keeper;
    /// this produces no owned-Child exit-status receipt.
    /// # Errors
    /// The expected native identity or current launchd job PID differs, SIGKILL refuses,
    /// process presence becomes unknown, interruption occurs, or absence is not established
    /// within the one-minute hang detector.
    #[cfg(any(test, debug_assertions))]
    pub fn crash_for_qualification(
        &mut self,
        expected: aterm_uds::process::Identity,
    ) -> Result<(), String> {
        use aterm_uds::process::{
            EndControl as _, EndControlLive, ProcessPresence, process_presence,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut live = EndControlLive;
        if expected.pid != self.pid || live.identity(self.pid) != Some(expected) {
            return Err("the original app-band keeper identity changed before its crash".into());
        }
        let pid = guard::launchd_pid(&self.label, deadline).map_err(|why| why.to_string())?;
        if i32::try_from(pid).ok() != Some(self.pid) || live.identity(self.pid) != Some(expected) {
            return Err("launchd no longer names the original app-band keeper".into());
        }
        live.signal(&expected, libc::SIGKILL)
            .map_err(|why| format!("cannot SIGKILL the original app-band keeper: {why}"))?;
        loop {
            match process_presence(self.pid) {
                ProcessPresence::Absent => return Ok(()),
                ProcessPresence::Unknown => {
                    return Err("app-band keeper absence is unknown after SIGKILL".into());
                }
                ProcessPresence::Present | ProcessPresence::Exited => {}
            }
            if live.identity(self.pid).is_some_and(|now| now != expected) {
                return Err(
                    "the app-band keeper PID was reused before absence was observed".into(),
                );
            }
            if Instant::now() >= deadline {
                return Err("one-minute hang detector: launchd keeper native absence".into());
            }
            crate::stop::sleep(Duration::from_millis(20)).map_err(|why| {
                format!("interrupted waiting for app-band keeper absence: {why:?}")
            })?;
        }
    }
}

/// The exact label a lab process owns for its private world.
#[must_use]
pub(crate) fn label_for(dir: &Path, owner: u32) -> String {
    let world = dir.file_name().map_or_else(String::new, |name| {
        name.to_string_lossy()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
    });
    format!("aterm-lab.instance.{world}.{owner}")
}

#[cfg(any(test, debug_assertions))]
fn checked_owned_label(dir: &Path, owner: u32, label: &str) -> Result<(), String> {
    if owner <= 1 || dir.file_name().is_none() || label != label_for(dir, owner) {
        return Err("cleanup must name this original owner's exact private-world job".into());
    }
    Ok(())
}

/// Observe this original owner's job disappearing without removing or signaling it. The
/// custody oracle uses this to wait through launchd's one-use restart while its GUI lives.
/// # Errors
/// The label or private world differs, a bounded probe cannot positively establish the
/// registration state, interruption occurs, or the job remains past the one-minute hang
/// detector.
#[cfg(any(test, debug_assertions))]
pub(crate) fn await_owned_absent_for_qualification(
    dir: &Path,
    owner: u32,
    label: &str,
) -> Result<(), String> {
    checked_owned_label(dir, owner, label)?;
    aterm_uds::private_file::check_private_dir(dir).map_err(|why| why.to_string())?;
    cleanup::await_absent(label, Instant::now() + Duration::from_secs(60))
        .map_err(|why| why.to_string())
}

/// Remove only the job derived from this original owner and private world, including after
/// that owner was SIGKILLed. Successful cleanup requires a positive missing-registration
/// observation; a failed launchctl command alone never proves absence.
/// # Errors
/// The label or private world differs, a bounded command fails without positive absence,
/// interruption occurs, or registration remains past the one-minute hang detector.
#[cfg(any(test, debug_assertions))]
pub(crate) fn remove_owned_for_qualification(
    dir: &Path,
    owner: u32,
    label: &str,
) -> Result<(), String> {
    checked_owned_label(dir, owner, label)?;
    aterm_uds::private_file::check_private_dir(dir).map_err(|why| why.to_string())?;
    remove(label).map_err(|why| why.to_string())
}

/// The job `label` named by the domain [`launch`] loads it in, this user's
/// GUI domain: `gui/<uid>/<label>`.
fn target(label: &str) -> String {
    format!("gui/{}/{label}", crate::sys::getuid())
}

/// Unload the job `label` (`launchctl bootout gui/<uid>/<label>`; it ends the
/// job's process if one still runs).
fn remove(label: &str) -> Result<(), CleanupError> {
    remove_until(label, Instant::now() + Duration::from_secs(60))
}

fn remove_until(label: &str, deadline: Instant) -> Result<(), CleanupError> {
    cleanup::remove(label, deadline)
}

fn start_failure(label: &str, original: String) -> String {
    match remove(label) {
        Ok(()) => original,
        Err(cleanup) => format!("{original}; cleanup did not establish job removal: {cleanup}"),
    }
}

/// The job's definition: `args` (its first process's argv, [`band_main`]'s),
/// `ProcessType` `App` (no QoS clamp, an APP-type task, as launchd starts an
/// app), started at load and kept alive (a restart after the program ends
/// removes the job, [`band_main`]), restarted 1 s after an end rather than
/// launchd's default 10 s: that restart only removes the job, so nothing
/// gains by the wait.
fn job_plist(label: &str, args: &[String]) -> String {
    let esc = aterm_codec::xml::escape;
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
    launch_inner(
        dir,
        label,
        line,
        cwd,
        log,
        None,
        Instant::now() + crate::instance::BOOT,
    )
}

fn launch_inner(
    dir: &Path,
    label: &str,
    line: &[String],
    cwd: &Path,
    log: &Path,
    guard: Option<&guard::Offer>,
    deadline: Instant,
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
    ];
    if let Some(guard) = guard {
        args.extend(guard.arguments());
    }
    args.push("--".to_string());
    args.extend_from_slice(line);
    let plist = dir.join(PLIST);
    crate::util::atomic_write(&plist, job_plist(label, &args).as_bytes())
        .map_err(|e| format!("cannot write the job's definition: {e}"))?;
    let mut command = aterm_uds::spawnfd::command(LAUNCHCTL);
    command
        .arg("bootstrap")
        .arg(format!("gui/{}", crate::sys::getuid()))
        .arg(&plist)
        .stdin(Stdio::null());
    let out =
        cleanup::output("bootstrap", &mut command, deadline).map_err(|why| why.to_string())?;
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

/// The byte [`Job::start`] writes into the lifeline once it has read the job's pid: the go.
const GO: u8 = b'g';

/// What [`band_main`] read from the lifeline while it waited for the go.
#[derive(Debug)]
enum Go {
    /// A byte: the launcher still held its writer after the job's open of the lifeline.
    Given,
    /// End of file: every writer is gone, the launcher's among them.
    Gone,
    /// Neither, within the wait's bound.
    Late,
    /// The read failed.
    Failed(std::io::Error),
}

/// Wait at most `within` for the go on `lifeline`, a non-blocking read end, polling every 20 ms
/// rather than blocking in a read, so the wait ends by `within` whatever the lifeline answers.
fn await_go(mut lifeline: &File, within: Duration) -> Go {
    let end = Instant::now() + within;
    loop {
        match lifeline.read(&mut [0_u8; 1]) {
            Ok(0) => return Go::Gone,
            Ok(_) => return Go::Given,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= end {
                    return Go::Late;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Go::Failed(e),
        }
    }
}

/// `aterm-lab band <dir> <label> --cwd DIR --log FILE -- PROGRAM ARGS…`: the first process of an app-band job ([`Job::start`]).
///
/// A launchd job inherits no descriptor, so it opens the lifeline FIFO by its name, and on macOS a
/// FIFO reader whose open returns just as the only writer closes can miss the end of file and
/// never end (measured 2026-10-29 under load): a launcher killed at that moment could leave the
/// job's program running for good. So the job writes its pid only once its open has returned,
/// and starts its program only on the go, which [`Job::start`] writes once it has read that pid:
/// the launcher held its writer after the open, so no close of it can meet the open. A launcher
/// gone before the go leaves the job to end without its program, at the end of file or, should
/// that be lost, once [`crate::instance::BOOT`] has passed; launchd's restart then removes it.
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
    let Some((prog, _)) = program.split_first() else {
        return usage();
    };
    let (base, guard) = match head {
        [_, _, _, _, _, _] => (head, None),
        [_, _, _, _, _, _, parent_flag, parent, nonce_flag, nonce]
            if parent_flag == "--guard-parent" && nonce_flag == "--guard-nonce" =>
        {
            match guard::Request::parse(parent, nonce, None) {
                Ok(request) => (&head[..6], Some(request)),
                Err(error) => {
                    eprintln!("aterm-lab band: the guard request: {error}");
                    return 2;
                }
            }
        }
        [
            _,
            _,
            _,
            _,
            _,
            _,
            parent_flag,
            parent,
            nonce_flag,
            nonce,
            memory_flag,
            memory,
        ] if parent_flag == "--guard-parent"
            && nonce_flag == "--guard-nonce"
            && memory_flag == "--guard-memory-leaf" =>
        {
            match guard::Request::parse(parent, nonce, Some(memory)) {
                Ok(request) => (&head[..6], Some(request)),
                Err(error) => {
                    eprintln!("aterm-lab band: the memory guard request: {error}");
                    return 2;
                }
            }
        }
        _ => return usage(),
    };
    let [dir, label, cwd_flag, cwd, log_flag, log] = base else {
        return usage();
    };
    if cwd_flag != "--cwd" || log_flag != "--log" {
        return usage();
    }
    if guard.is_some() && keeper_line(program).is_err() {
        return usage();
    }
    let dir = PathBuf::from(dir);
    let marker = dir.join(format!("band.{label}.ran"));
    let first = dir.is_dir() && crate::util::create_new_regular_file(&marker, 0o600).is_ok();
    if !first {
        return match remove(label) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!("aterm-lab band: one-use job cleanup: {why}");
                1
            }
        };
    }
    let env: Vec<(String, String)> = match crate::util::read_regular_file(&dir.join(ENV_FILE)) {
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(dir.join(LIFELINE))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aterm-lab band: the lifeline: {e}");
            return 127;
        }
    };
    let _ = std::fs::remove_file(dir.join(LIFELINE));
    let out = match OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(log)
        .and_then(|f| {
            if f.metadata()?.is_file() {
                Ok(f)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not a regular file",
                ))
            }
        }) {
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
    let guard = match guard
        .map(|request| request.receive(&dir, Instant::now() + crate::instance::BOOT))
        .transpose()
    {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("aterm-lab band: the window guard: {error}; {prog} does not run");
            return 127;
        }
    };
    match await_go(&lifeline, crate::instance::BOOT) {
        Go::Given => {}
        Go::Gone => {
            eprintln!("aterm-lab band: the launcher was gone before its go; {prog} does not run");
            return 1;
        }
        Go::Late => {
            eprintln!(
                "aterm-lab band: no go from the launcher in {} s; {prog} does not run",
                crate::instance::BOOT.as_secs()
            );
            return 1;
        }
        Go::Failed(e) => {
            eprintln!("aterm-lab band: the lifeline: {e}");
            return 127;
        }
    }
    if let Err(e) = crate::sys::set_blocking(lifeline.as_raw_fd()) {
        eprintln!("aterm-lab band: the lifeline: {e}");
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
    let e = match &guard {
        Some(guard) => {
            let mut guarded = program.to_vec();
            let at = match keeper_line(&guarded) {
                Ok(at) => at,
                Err(error) => {
                    eprintln!("aterm-lab band: {error}");
                    return 127;
                }
            };
            let descriptors = guard.borrowed();
            let slots: Vec<_> = descriptors
                .iter()
                .map(|descriptor| descriptor.as_raw_fd())
                .collect();
            let arguments =
                match crate::window_guards::WindowGuards::arguments(&slots, guard.claim()) {
                    Ok(arguments) => arguments,
                    Err(error) => {
                        eprintln!("aterm-lab band: native guard roles: {error}");
                        return 127;
                    }
                };
            guarded.splice(at..at, arguments);
            crate::sys::exec_as_app_with_guards(prog, &guarded, &env, UI_FOCAL, &descriptors)
        }
        None => crate::sys::exec_as_app(prog, program, &env, UI_FOCAL),
    };
    eprintln!("aterm-lab band: exec {prog}: {e}");
    127
}

/// An app-band window guard is passed only into our keeper's prefix. Never inject a guard
/// into an arbitrary program, shell payload, or an already guarded command.
fn keeper_line(line: &[String]) -> Result<usize, String> {
    let at = line
        .iter()
        .position(|arg| arg == "--")
        .ok_or_else(|| "window guard requires the original keeper separator".to_owned())?;
    if at < 2
        || line.first().map(Path::new) != Some(paths().exe.as_path())
        || line.get(1).map(String::as_str) != Some("keeper")
        || !match &line[2..at] {
            [] => true,
            [flag] => flag == "--disclaim",
            _ => false,
        }
        || line.get(at + 1).is_none()
    {
        return Err("window guard requires the original keeper command".into());
    }
    Ok(at)
}

/// `aterm-lab band-standin <file> [--kept]`: the program `tests/band.rs` runs in an app-band
/// job, in aterm's place; `--kept`, a keeper's, in a windowed aterm's: it prints `kept <pid>`,
/// then appends its pid to `<file>`, and runs until a signal ends it.
#[must_use]
pub fn standin_main(args: &[String]) -> i32 {
    let usage = || {
        eprintln!(
            "aterm-lab band-standin: usage: band-standin <file> [--kept] (an app-band job's program, or its keeper's)"
        );
        2
    };
    let file = match args {
        [file] => file,
        [file, kept] if kept == "--kept" => return kept_main(file),
        _ => return usage(),
    };
    let pid = i32::try_from(std::process::id()).unwrap_or(0);
    let role = crate::sys::darwin_role(pid).map_or_else(|e| format!("({e})"), |r| r.to_string());
    let set =
        crate::sys::set_darwin_role(pid, UI).map_or_else(|e| format!("({e})"), |()| "ok".into());
    let lands = crate::sys::ui_request_lands().map_or_else(|e| format!("({e})"), |p| p.to_string());
    let line = format!("{role} {set} {lands}\n");
    let wrote = crate::util::append_regular_file(std::path::Path::new(file))
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = wrote {
        eprintln!("aterm-lab band-standin: {file}: {e}");
        return 1;
    }
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    0
}

/// `band-standin <file> --kept`: this process's pid printed and then appended to `file` (so the
/// file's line says the printed one was written), then held until a signal ends it (a keeper's
/// `SIGTERM`, or the test's).
fn kept_main(file: &str) -> i32 {
    let pid = std::process::id();
    let mut out = std::io::stdout().lock();
    if let Err(e) = writeln!(out, "kept {pid}").and_then(|()| out.flush()) {
        eprintln!("aterm-lab band-standin: stdout: {e}");
        return 1;
    }
    drop(out);
    let wrote = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(file)
        .and_then(|mut f| f.write_all(format!("{pid}\n").as_bytes()));
    if let Err(e) = wrote {
        eprintln!("aterm-lab band-standin: {file}: {e}");
        return 1;
    }
    loop {
        std::thread::park();
    }
}

/// This process's pid into the world's pid file, whole or not at all.
fn write_pid(dir: &Path) -> std::io::Result<()> {
    let tmp = dir.join(format!("{PID_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    if let Err(e) = writeln!(f, "{}", std::process::id()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(f);
    std::fs::rename(tmp, dir.join(PID_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lifeline as [`band_main`] holds it, a non-blocking read end of a FIFO, beside the
    /// launcher's held read-write end, as [`Job::start`] opens them.
    fn lifeline() -> (aterm_tempfile::TempDir, File, File) {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let fifo = dir.path().join(LIFELINE);
        crate::sys::make_fifo(&fifo).expect("mkfifo");
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .expect("the held end");
        let job = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("the job's end");
        (dir, held, job)
    }

    /// The go is a byte; a launcher gone without it is the end of file; one that holds its writer
    /// and writes nothing leaves the job waiting until the bound.
    #[test]
    fn the_job_waits_for_its_go_and_sees_a_launcher_gone_without_it() {
        let (_dir, mut held, job) = lifeline();
        held.write_all(&[GO]).expect("the go");
        assert!(
            matches!(await_go(&job, crate::instance::BOOT), Go::Given),
            "the go"
        );

        let (_dir, held, job) = lifeline();
        drop(held);
        assert!(
            matches!(await_go(&job, crate::instance::BOOT), Go::Gone),
            "a launcher gone"
        );

        let (_dir, _held, job) = lifeline();
        assert!(
            matches!(await_go(&job, Duration::ZERO), Go::Late),
            "a launcher that holds its writer and writes nothing"
        );
    }

    #[test]
    fn an_instance_defaults_to_the_app_band_and_takes_either_band_by_name() {
        assert_eq!(Band::pick(None).ok(), Some(Band::App));
        assert_eq!(Band::pick(Some("agent")).ok(), Some(Band::Agent));
        assert_eq!(Band::pick(Some("app")).ok(), Some(Band::App));
        assert!(Band::pick(Some("own")).is_err());
    }

    #[test]
    fn qualification_cleanup_binds_the_original_owner_and_world() {
        let world = Path::new("/private/tmp/.ubd/at_original.noindex");
        let label = label_for(world, 42);
        assert!(checked_owned_label(world, 42, &label).is_ok());
        for (dir, owner, label) in [
            (world, 43, label.as_str()),
            (
                Path::new("/private/tmp/.ubd/at_other.noindex"),
                42,
                label.as_str(),
            ),
            (world, 42, "other-job"),
            (world, 0, label.as_str()),
        ] {
            assert!(checked_owned_label(dir, owner, label).is_err());
            // Refusal happens before filesystem or launchd access, even for a nonexistent
            // path; an arbitrary label never supplies permission to remove a job.
            assert!(remove_owned_for_qualification(dir, owner, label).is_err());
        }
    }

    #[test]
    fn missing_registration_is_exact_to_the_job_domain_and_native_result() {
        use std::os::unix::process::ExitStatusExt as _;
        let label = "aterm-lab.instance.original.42";
        let missing = format!(
            "Bad request.\nCould not find service \"{label}\" in domain for user gui: 501\n"
        );
        let output = |code: i32, stdout: &[u8], stderr: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        assert_eq!(
            decode_registration(&output(113, b"", missing.as_bytes()), label, 501).unwrap(),
            Registration::Absent
        );
        assert_eq!(
            decode_registration(&output(0, b"job = {\n\tpid = 42\n}\n", b""), label, 501).unwrap(),
            Registration::Present
        );
        for (observed, job, uid) in [
            (output(1, b"", missing.as_bytes()), label, 501),
            (output(113, b"foreign", missing.as_bytes()), label, 501),
            (output(113, b"", b"permission denied\n"), label, 501),
            (output(113, b"", missing.as_bytes()), "other-job", 501),
            (output(113, b"", missing.as_bytes()), label, 502),
            (
                output(113, b"", format!("{missing}foreign\n").as_bytes()),
                label,
                501,
            ),
        ] {
            assert!(decode_registration(&observed, job, uid).is_err());
        }
    }

    #[test]
    fn a_guard_is_injected_only_into_the_original_keeper_prefix() {
        let own_program = paths().exe.to_string_lossy().into_owned();
        let line = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            keeper_line(&line(&[&own_program, "keeper", "--", "gui", "--window"])).unwrap(),
            2
        );
        assert_eq!(
            keeper_line(&line(&[
                &own_program,
                "keeper",
                "--disclaim",
                "--",
                "gui",
                "--window"
            ]))
            .unwrap(),
            3
        );
        for args in [
            vec!["--", "keeper"],
            vec!["other-program", "keeper", "--", "gui"],
            vec!["lab", "other", "--", "gui"],
            vec!["lab", "keeper", "--"],
            vec!["lab", "keeper", "--lab-window-guard", "4", "--", "gui"],
        ] {
            assert!(keeper_line(&line(&args)).is_err());
        }
    }
}

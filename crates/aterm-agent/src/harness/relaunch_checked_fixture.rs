// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private-world source observations for the real checked relaunch producer.
//! The caller supplies the actual GUI authorizer and native operation endpoint.

use super::super::native_act::{Binding, Prepare, Process, Reply, Request};
use super::*;
use std::io::{BufRead as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

const SESSION: &str = "bbccdd00-0000-0000-0000-000000000001";
const TOKEN: &str = "checked-relaunch-private-fixture";
const PATIENCE: Duration = Duration::from_secs(60);
type Delegate = Box<dyn Fn(&str) -> String + Send>;

struct Observations {
    screen: String,
    human: u64,
    revision: u64,
    background: Vec<String>,
    native: Option<Delegate>,
    requests: Vec<String>,
}

/// Scripted observations, never an authorization or a process identity factory.
#[derive(Clone)]
pub struct Controls {
    observations: Arc<Mutex<Observations>>,
    child: Arc<Mutex<aterm_uds::spawnfd::Child>>,
}

impl Controls {
    pub fn screen(&self, rows: &[&str], cursor_row: usize, cursor_col: usize) {
        let rows = rows
            .iter()
            .map(|row| aterm_json::Value::String((*row).into()).to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.observations.lock().unwrap().screen = format!(
            r#"{{"rows":[{rows}],"cursor":{{"row":{cursor_row},"col":{cursor_col}}},"seq":77,"first":0,"human_ms":null,"human_seq":0}}"#
        );
    }

    pub fn human_sequence(&self, sequence: u64) {
        self.observations.lock().unwrap().human = sequence;
    }

    pub fn revision(&self, revision: u64) {
        self.observations.lock().unwrap().revision = revision;
    }

    pub fn background(&self, names: &[&str]) {
        self.observations.lock().unwrap().background =
            names.iter().map(|name| (*name).into()).collect();
    }

    pub fn requests(&self) -> Vec<String> {
        self.observations.lock().unwrap().requests.clone()
    }

    /// Retire only the child this world spawned, with its positive owning wait.
    pub fn retire_source(&self) -> std::io::Result<std::process::ExitStatus> {
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        child.kill()?;
        child.wait()
    }
}

struct SourceKernel {
    controls: Controls,
    process: Process,
    shell: Process,
}

impl Kernel for SourceKernel {
    fn shell_dialect(
        &self,
        shell: u32,
        _: &[super::super::upgrade_drive::ProcRow],
    ) -> Option<Dialect> {
        // Rendering is an injected platform fact; this test process is not a physical Bash.
        (shell == self.shell.pid).then_some(Dialect::Bash)
    }
    fn job(&self, pid: u32) -> Option<(Job, u32)> {
        (pid == self.process.pid).then_some((Job::Foreground, self.shell.pid))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        Process::observe(pid).map(|process| process.parent)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        // Only terminal ownership is injected; parent/group/session/image are real.
        (pid == self.process.pid).then_some((self.shell.pid, "aterm-fixture".into()))
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        kernel_start(pid).map(|start| squash(&start))
    }

    fn restart_background(&self, pid: u32) -> Vec<String> {
        if pid == self.process.pid {
            self.controls
                .observations
                .lock()
                .unwrap()
                .background
                .clone()
        } else {
            vec![super::super::upgrade_drive::UNREADABLE_TABLE.into()]
        }
    }

    /// Unread: this fixture's lane is the restart in place, which never moves over an agent's
    /// waits, and an unread row is never a wait.
    fn restart_held(&self, _: u32) -> Option<Vec<super::super::upgrade::Held>> {
        None
    }
}

/// A genuine source process and private record namespace, available before GUI Cold setup.
pub struct World {
    root: aterm_tempfile::TempDir,
    opts: Opts,
    snapshot: Snapshot,
    process: Process,
    shell: Process,
    controls: Controls,
    _input: std::process::ChildStdin,
    endpoint: Endpoint,
}

impl World {
    pub fn new(tab: &str) -> Result<Self, String> {
        let root = aterm_tempfile::Builder::new()
            .prefix("rc-")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .map_err(|error| error.to_string())?;
        let home = root.path().join("home");
        let state = root.path().join("state");
        let records = home.join(".claude");
        let executable =
            std::fs::canonicalize(aterm_uds::deadline::stand_in(Path::new("/bin/cat")))
                .map_err(|error| error.to_string())?;
        let mut child = aterm_uds::spawnfd::command(&executable)
            .process_group(0)
            .env("HOME", &home)
            .env("ATERM_PARENT_SESSION_ID", tab)
            .env_remove(accounts::LOGIN_ENV)
            .env_remove(accounts::HOME_ENV)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        let child = Arc::new(Mutex::new(child));
        let controls = Controls {
            observations: Arc::new(Mutex::new(Observations {
                screen: String::new(),
                human: 0,
                revision: 0,
                background: Vec::new(),
                native: None,
                requests: Vec::new(),
            })),
            child,
        };
        // From this point every fallible setup path retains an owning child guard.
        let mut owned = SetupChild(Some(controls.clone()));
        let pid = controls.child.lock().unwrap().id();
        let process = Process::observe(pid).ok_or("source process unread")?;
        let shell = Process::observe(std::process::id()).ok_or("source parent unread")?;
        if process.parent != shell.pid
            || process.group != process.pid
            || process.session != shell.session
        {
            return Err("source process topology changed".into());
        }
        controls.screen(
            &["────────────────────", "❯ ", "────────────────────"],
            1,
            2,
        );
        std::fs::create_dir_all(records.join("sessions")).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(records.join("projects/p")).map_err(|error| error.to_string())?;
        std::fs::write(home.join(".claude.json"), "{}").map_err(|error| error.to_string())?;
        let start = kernel_start(pid).ok_or("source start unread")?;
        let record = format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        );
        std::fs::write(records.join(format!("sessions/{pid}.json")), record)
            .map_err(|error| error.to_string())?;
        std::fs::write(records.join(format!("projects/p/{SESSION}.jsonl")), "{\"type\":\"assistant\",\"timestamp\":\"2099-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-6\",\"content\":[{\"type\":\"text\",\"text\":\"Done.\"}]}}\n").map_err(|error| error.to_string())?;
        let socket = root.path().join("control.sock");
        let endpoint = Endpoint::new(&socket, tab.to_owned(), controls.clone())
            .map_err(|error| error.to_string())?;
        let mut timings = crate::harness::Timings::FAST;
        timings.exited = Duration::ZERO;
        timings.prompt_back = Duration::ZERO;
        timings.held = Duration::ZERO;
        let opts = Opts {
            home: home.clone(),
            state,
            account_roster: Some(root.path().join("accounts.toml")),
            sock: Some(socket.display().to_string()),
            token: Some(super::super::upgrade_drive::HarnessToken::new(TOKEN)),
            only_sid: Some(tab.into()),
            dry_run: false,
            human_grace_s: 0,
            hand_back: false,
            background: false,
            aterm_state: None,
            ask: false,
            usage_tap: None,
            instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
            agents_off: Vec::new(),
            land: false,
            timings,
            tell: super::super::upgrade_drive::Tell::Off,
            press: None,
        };
        let snapshot = Snapshot {
            tab: tab.into(),
            pid,
            start: squash(&start),
            shell: shell.pid,
            program: executable.clone(),
            argv: vec![executable.display().to_string()],
            session: Some(SESSION.into()),
            cwd: "/".into(),
            version: Some("1.0.0".into()),
            run: AgentRun::Claude,
            dialect: Some(Dialect::Bash),
            login: None,
            home: Some(home.display().to_string()),
            claude_dir: Some(records.clone()),
            route: accounts::AuthRoute::Outside,
            auth_observed: true,
            launch_env: None,
            profile: Some(super::super::profile::Context {
                plain: home.join(".claude.json"),
                records,
                selection: super::super::profile::Selection::Standard,
                home: Some(home.display().to_string()),
                config: None,
                login: None,
            }),
        };
        owned.0 = None;
        Ok(Self {
            root,
            opts,
            snapshot,
            process,
            shell,
            controls,
            _input: input,
            endpoint,
        })
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn process(&self) -> &Process {
        &self.process
    }
    pub fn shell(&self) -> &Process {
        &self.shell
    }
    pub fn home(&self) -> &Path {
        &self.opts.home
    }
    pub fn state(&self) -> &Path {
        &self.opts.state
    }
    pub fn root(&self) -> &Path {
        self.root.path()
    }
    pub fn session(&self) -> &str {
        SESSION
    }
    pub fn tab(&self) -> &str {
        &self.snapshot.tab
    }
    pub fn controls(&self) -> Controls {
        self.controls.clone()
    }

    /// Retire the fixture's endpoint and real child before asserting captured outcomes.
    pub fn close(&mut self) -> Result<(), String> {
        let endpoint = self.endpoint.stop();
        let delegate = self.controls.observations.lock().unwrap().native.take();
        drop(delegate);
        let child = self
            .controls
            .retire_source()
            .map_err(|error| error.to_string());
        endpoint.and(child.map(|_| ()))
    }

    /// Consume a real server completion using the existing client terminal reducer.
    pub fn retain_completion(
        &self,
        prepare: &Prepare,
        binding: &Binding,
        reply: &Reply,
    ) -> Result<(), String> {
        if prepare.id != binding.id {
            return Err("completion binding mismatch".into());
        }
        let super::super::native_act::NativePlan::Restart {
            process,
            shell,
            cold: Some(_),
            ..
        } = &prepare.plan
        else {
            return Err("fixture predecessor must be a genuine cold preparation".into());
        };
        let mut state = St {
            tab: self.tab().into(),
            pid: process.pid,
            shell: shell.pid,
            from: "1.0.0".into(),
            to: "1.0.0".into(),
            native: Some(binding.clone()),
            native_prepare: Request::prepare(prepare.clone())
                .encode()
                .ok_or("prepare unencodable")?,
            ..St::default()
        };
        state.bind(
            process.pid,
            &super::super::native_restart::vendor_start(process),
        );
        state.keep_home(
            &Records::of_snapshot(&self.opts.home, &self.snapshot),
            &self.opts.home,
        );
        if super::super::native_restart::terminal_reply(&mut state, reply)
            != Some(super::super::native_restart::Recovery::Completed)
        {
            return Err("server reply is not a completed operation".into());
        }
        save_checked(&self.opts, SESSION, &state).map_err(|error| error.to_string())
    }

    pub fn run(
        &mut self,
        why: &Restart,
        ask: &RestartAsk,
        authorizer: &dyn RelaunchAuthorizer,
        native: impl Fn(&str) -> String + Send + 'static,
    ) -> Restarted {
        let previous = self
            .controls
            .observations
            .lock()
            .unwrap()
            .native
            .replace(Box::new(native));
        drop(previous);
        let kernel = SourceKernel {
            controls: self.controls.clone(),
            process: self.process.clone(),
            shell: self.shell.clone(),
        };
        restart_fenced(
            &self.opts,
            why,
            ask,
            &accounts::Roster { rows: Vec::new() },
            Ok(self.snapshot.clone()),
            None,
            &kernel,
            &|| true,
            Some(authorizer),
        )
    }
}

struct SetupChild(Option<Controls>);
impl Drop for SetupChild {
    fn drop(&mut self) {
        if let Some(controls) = &self.0 {
            let _ = controls.retire_source();
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.endpoint.stop();
        let delegate = self
            .controls
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .native
            .take();
        drop(delegate);
        let _ = self.controls.retire_source();
    }
}

struct Endpoint {
    stop: Option<std::io::PipeWriter>,
    active: Arc<Mutex<Option<UnixStream>>>,
    join: Option<std::thread::JoinHandle<()>>,
    completed: std::sync::mpsc::Receiver<()>,
}

impl Endpoint {
    fn new(path: &Path, tab: String, controls: Controls) -> std::io::Result<Self> {
        let listener = aterm_uds::kept_out(|| UnixListener::bind(path))?;
        let (cancelled, cancel) = std::io::pipe()?;
        let active = Arc::new(Mutex::new(None::<UnixStream>));
        let serving = active.clone();
        let (done, completed) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("checked-relaunch-ctl".into())
            .spawn(move || {
                struct Finished(std::sync::mpsc::SyncSender<()>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        let _ = self.0.send(());
                    }
                }
                let _finished = Finished(done);
                loop {
                    let mut ready = [
                        libc::pollfd {
                            fd: listener.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: cancelled.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // SAFETY: descriptors remain owned for the entire blocking poll.
                    let result = unsafe { libc::poll(ready.as_mut_ptr(), 2, -1) };
                    if result < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    if result < 0 || ready[1].revents != 0 {
                        break;
                    }
                    let Ok((stream, _)) = aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) else {
                        break;
                    };
                    let _ = stream.set_read_timeout(Some(PATIENCE));
                    let _ = stream.set_write_timeout(Some(PATIENCE));
                    *serving.lock().unwrap() = stream.try_clone().ok();
                    let Ok(mut writer) = stream.try_clone() else {
                        break;
                    };
                    let mut reader = std::io::BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err()
                        || line.trim_end() != format!("AUTH {TOKEN}")
                    {
                        continue;
                    }
                    loop {
                        line.clear();
                        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
                            break;
                        }
                        let request = line.trim_end();
                        let reply = answer(&tab, &controls, request);
                        if writer.write_all(reply.as_bytes()).is_err()
                            || (!reply.ends_with('\n') && writer.write_all(b"\n").is_err())
                        {
                            break;
                        }
                    }
                    *serving.lock().unwrap() = None;
                }
            })?;
        Ok(Self {
            stop: Some(cancel),
            active,
            join: Some(join),
            completed,
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        if let Some(mut stop) = self.stop.take() {
            let _ = stop.write_all(&[1]);
        }
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = active.shutdown(std::net::Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            match self.completed.recv_timeout(PATIENCE) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    join.join()
                        .map_err(|_| "checked source endpoint panicked".to_string())?;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    return Err("checked source endpoint did not retire".into());
                }
            }
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn answer(tab: &str, controls: &Controls, line: &str) -> String {
    let mut observations = controls.observations.lock().unwrap();
    observations.requests.push(line.into());
    let verb = line
        .split_whitespace()
        .find(|word| !word.starts_with('@'))
        .unwrap_or("");
    match verb {
        "version" => "OK checked-source-fixture".into(),
        "sessions" => format!("OK 1\nlocal {tab} 0 idle claude active=1 wfocus=1"),
        "text" => format!("OK {}", observations.screen),
        "cell" => "OK %20 d0d0d0 111318 none".into(),
        "status" => format!(
            "OK schema=1 hold=0 hand=- input=clear input_bytes=0 human_ms=- human_seq={} supervisor_revision={}",
            observations.human, observations.revision
        ),
        "help" if line.contains("native-act") => "OK 0".into(),
        "lease" if line.contains(" lease release ") => "OK lease released".into(),
        "lease" => "OK lease acquired fixture=1".into(),
        "native-act" => {
            // Do not hold the observation mutex while the real GUI callback changes controls.
            let native = observations.native.take();
            drop(observations);
            let reply = native
                .as_ref()
                .map_or_else(|| "ERR no native delegate".into(), |native| native(line));
            controls.observations.lock().unwrap().native = native;
            reply
        }
        "signal" => {
            let pid = line
                .split_whitespace()
                .find_map(|word| word.strip_prefix("pid="))
                .and_then(|pid| pid.parse::<u32>().ok());
            let owned = controls.child.lock().unwrap().id();
            drop(observations);
            if pid == Some(owned)
                && line.contains(" signal term ")
                && controls.retire_source().is_ok()
            {
                "OK signal term".into()
            } else {
                "ERR fixture signal refused".into()
            }
        }
        _ => "ERR unsupported fixture request".into(),
    }
}

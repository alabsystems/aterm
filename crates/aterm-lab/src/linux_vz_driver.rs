// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's vz arm (`docs/DESIGN-vz-guest-2026-10-23.md` §4, §5.1, §7, §8, §11 and
//! §12; slice V4): the lane's own VM, reached through `aterm-vz-host`'s session and the guest
//! agent, behind [`crate::linux_driver::Driver::Vz`].
//!
//! [`open`] is the arm's `up`: the gates, the helper and the agent built and placed, a
//! session on the live helper or on one it starts ([`aterm_vz::client::Lab::session`]), and,
//! only when that session found no VM, the kernel laid, the disks prepared and the boot
//! (V3's layers; a bake's boot where no baked disk is sealed); then the agent's `Ready`, and
//! the `aterm` share mounted. The lane's own steps then run through the agent exactly as they
//! run through lima: provisioning in place, the Rosetta cache reset, the build-output claim.
//!
//! Each command is one `Exec` on a command connection of its own, as the lane user, with
//! §8's environment; the lane's own variables, which its argv carries as `env K=V …` for
//! lima, join that environment ([`exec_parts`]), so the agent starts the program itself and
//! a missing one is `NotStarted`, never `env`'s 127. No read waits on a timer: a hung guest is the helper's to detect (its heartbeat), which
//! stops the VM and so ends every connection, and a hung command is its row's own detector's,
//! as today. A connection that ends before its `Exit` is `Lost`, with the session's words.
//!
//! The state lives under `$LAB_VZ_STATE`, else this user's `vz.noindex`: the helper's and
//! the agent's content-addressed copies, the kernel, seed, baked and live layers, the run
//! files and the read-only `in` share `copy_dir_in` stages through.

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_vz::client::{ClientError, Detached, Lab, Session, UpError};
use aterm_vz::layers::{Base, Layers, PartialBake, Prepared, System, VM};
use aterm_vz::plan::{COMMAND_LINE, CPUS, Disk, MEMORY, Plan, ShareEntry, mac_for};
use aterm_vz::proto::{Attach, PROTO, Phase};
use aterm_vz::record::{Kernel, Paths, Record};
use aterm_vz_agent::wire::{self, Exec, Exit, Frame, Hello, Share, Stdin};

use crate::linux::{Out, not_run};
use crate::linux_driver::HostDisk;
use crate::linux_vz_host::{build_host, client_fail, layer_fail, place_host, virtualization_gate};
use crate::{Fail, LAB};

/// The variable that moves the arm's whole state (the tests' and the measures' scratch).
pub const STATE_ENV: &str = "LAB_VZ_STATE";

/// The share directory `copy_dir_in` stages through, under the state.
pub const IN: &str = "in";

/// Where the guest sees the `aterm` share ([`aterm_vz_agent::mounts::AT`]).
pub const SHARE_AT: &str = aterm_vz_agent::mounts::AT;

/// The most bytes [`Vz::copy_dir_out`] unpacks from one guest directory.
pub const COPY_CAP: u64 = 8 << 30;

/// The arm's state directory: `$LAB_VZ_STATE` (absolute), else this user's `vz.noindex`.
///
/// # Errors
/// A relative `$LAB_VZ_STATE` (usage), or no `HOME`.
pub fn root() -> Result<PathBuf, Fail> {
    root_of(std::env::var_os(STATE_ENV).as_deref().map(Path::new))
}

/// [`root`] for the variable's value `named`.
///
/// # Errors
/// As [`root`].
pub fn root_of(named: Option<&Path>) -> Result<PathBuf, Fail> {
    match named.filter(|p| !p.as_os_str().is_empty()) {
        Some(p) if p.is_absolute() => Ok(p.to_path_buf()),
        Some(p) => Err(Fail::usage(format!(
            "{STATE_ENV}={} must be an absolute path",
            p.display()
        ))),
        None => aterm_uds::host::host_user_cache("vz")
            .ok_or_else(|| Fail::die("linux: no HOME, so no vz.noindex")),
    }
}

/// The lane lock under the arm's state (§11: `vz.noindex/aterm-linux.lane.lock`).
#[must_use]
pub fn lane_lock_path(root: &Path) -> PathBuf {
    root.join(format!("{VM}.lane.lock"))
}

/// The lane user in the guest: the host's name and uid (§8, the seed's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestUser {
    /// The login name.
    pub name: String,
    /// The uid.
    pub uid: u32,
}

/// Ubuntu's `/etc/environment` `PATH`.
pub const GUEST_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin";

impl GuestUser {
    /// `/home/<name>.guest` (lima's convention, fact 20).
    #[must_use]
    pub fn home(&self) -> String {
        format!("/home/{}.guest", self.name)
    }

    /// `/run/user/<uid>`.
    #[must_use]
    pub fn runtime(&self) -> String {
        format!("/run/user/{}", self.uid)
    }

    /// Every `Exec`'s environment (§8): HOME, USER, LOGNAME, SHELL, PATH, LANG,
    /// `XDG_RUNTIME_DIR` and the session bus; the lane adds its own in the argv.
    #[must_use]
    pub fn env(&self) -> Vec<(String, String)> {
        let runtime = self.runtime();
        vec![
            ("HOME".into(), self.home()),
            ("USER".into(), self.name.clone()),
            ("LOGNAME".into(), self.name.clone()),
            ("SHELL".into(), "/bin/bash".into()),
            ("PATH".into(), GUEST_PATH.into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={runtime}/bus"),
            ),
            ("XDG_RUNTIME_DIR".into(), runtime),
        ]
    }
}

/// What the arm asks of its helper's session; the tests' arm answers on socket pairs.
pub trait Link: Send {
    /// A command connection to the agent: its id and descriptor.
    ///
    /// # Errors
    /// The helper refused it (its words).
    fn connect(&mut self) -> Result<(u64, OwnedFd), String>;

    /// The lab closed connection `id`.
    fn release(&mut self, id: u64);

    /// Why a command connection ended before its `Exit`: the session's `Lost`, or the VM's
    /// phase.
    fn why_lost(&mut self) -> String;

    /// Set `keep`.
    ///
    /// # Errors
    /// The helper did not answer.
    fn keep(&mut self) -> Result<(), String>;

    /// Whether `keep` is set.
    fn kept(&self) -> bool;

    /// Swap the running `aterm` share to `entries` (§5.1); the lab then asks the agent to
    /// rebind ([`Vz::mount`]).
    ///
    /// # Errors
    /// The helper could not (its words) or did not answer.
    fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String>;

    /// Wait for the VM to stop on its own (the bake's guest, asked to power off on a command
    /// connection): the helper's words once it stopped, [`aterm_vz::helper::POWERED_OFF`]
    /// for a guest that powered itself off.
    ///
    /// # Errors
    /// The VM failed, the session ended with no word, or the helper said nothing for a span.
    fn wait_stopped(&mut self) -> Result<String, String>;

    /// Close the session: the release rule runs (§5.1).
    fn detach(self: Box<Self>);
}

impl Link for Session {
    fn connect(&mut self) -> Result<(u64, OwnedFd), String> {
        Self::connect(self, aterm_vz_agent::COMMAND_PORT).map_err(|e| e.to_string())
    }

    fn release(&mut self, id: u64) {
        let _ = Self::release(self, id);
    }

    fn why_lost(&mut self) -> String {
        Self::why_lost(self)
    }

    fn keep(&mut self) -> Result<(), String> {
        Self::keep(self, true).map_err(|e| e.to_string())
    }

    fn kept(&self) -> bool {
        self.keep
    }

    fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String> {
        Self::share(self, entries).map_err(|e| e.to_string())
    }

    fn wait_stopped(&mut self) -> Result<String, String> {
        Self::wait_stopped(self).map_err(|e| e.to_string())
    }

    fn detach(self: Box<Self>) {
        Session::detach(*self);
    }
}

/// How a command's connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// With the agent's `Exit`.
    Exit(Exit),
    /// Before it: why.
    Lost(String),
}

/// What one `Exec` did.
#[derive(Clone, Debug, PartialEq)]
pub struct Ran {
    /// How it ended.
    pub ended: Ended,
    /// Its stdout (and stderr, merged).
    pub out: Vec<u8>,
    /// Its stderr.
    pub err: Vec<u8>,
    /// Its wall time.
    pub secs: f64,
}

/// The words a signal's end adds to an [`Out`]'s text, as lima's `ended_on_signal` names a
/// host signal (§10).
#[must_use]
pub fn signal_words(signal: i32) -> String {
    format!("\n(the guest command ended on signal {signal})\n")
}

/// The lane's [`Out`] of `ran` (§10's four outcomes, each kept apart): `Exited(n)` is
/// `code: Some(n)`; `Signaled(s)` is `code: None`, the signal named; `NotStarted(why)` is
/// `not_started: Some(why)`; a connection that ended first is `lost: Some(why)`. The text is
/// stdout then stderr, as lima's.
#[must_use]
pub fn out_of(ran: &Ran) -> Out {
    let mut text = String::from_utf8_lossy(&ran.out).into_owned();
    text.push_str(&String::from_utf8_lossy(&ran.err));
    let mut out = Out {
        code: None,
        text,
        secs: ran.secs,
        signal: None,
        not_started: None,
        lost: None,
    };
    match &ran.ended {
        Ended::Exit(Exit::Exited(code)) => out.code = Some(*code),
        Ended::Exit(Exit::Signaled(signal)) => {
            out.signal = Some(*signal);
            out.text.push_str(&signal_words(*signal));
        }
        Ended::Exit(Exit::NotStarted(why)) => out.not_started = Some(why.clone()),
        Ended::Lost(why) => out.lost = Some(why.clone()),
    }
    out
}

/// The `Exec` parts of the lane's `argv` under the lane user's environment `base`: a
/// leading `env NAME=VALUE… PROGRAM…` ([`crate::linux_guest::with_env`], lima's way to add
/// the lane's variables) has its assignments taken into the environment, each over any
/// earlier value of its name, and `PROGRAM…` is the argv, so the agent starts the program
/// itself on that environment's `PATH`, as `env` would, and a missing or unexecutable one
/// is `NotStarted` with its reason, never `env`'s 127 or 126 (§10). An `env` with an option
/// first, or with no program after its assignments, runs as given.
#[must_use]
pub fn exec_parts(
    base: &[(String, String)],
    argv: &[String],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut env = base.to_vec();
    if argv.first().map(String::as_str) != Some("env") {
        return (env, argv.to_vec());
    }
    let mut set = Vec::new();
    let mut at = 1;
    while let Some(word) = argv.get(at) {
        match word.split_once('=') {
            Some((name, value)) if !name.is_empty() && !word.starts_with('-') => {
                set.push((name.to_string(), value.to_string()));
                at += 1;
            }
            _ => break,
        }
    }
    match argv.get(at) {
        Some(program) if !program.starts_with('-') => {
            for (name, value) in set {
                env.retain(|(k, _)| *k != name);
                env.push((name, value));
            }
            (env, argv[at..].to_vec())
        }
        _ => (env, argv.to_vec()),
    }
}

/// The lane user's `Exec` of `argv` in `cwd` with exactly `env`.
#[must_use]
pub fn exec_of(
    argv: &[String],
    cwd: &str,
    env: &[(String, String)],
    stdin: bool,
    merge: bool,
) -> Exec {
    Exec {
        argv: argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
        cwd: cwd.as_bytes().to_vec(),
        env: env
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect(),
        stdin: if stdin { Stdin::Pipe } else { Stdin::Null },
        merge,
        // The host's clock, which the agent steps the guest's to when they differ (§6).
        clock_unix_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX)),
    }
}

/// The largest `Stdin` frame the arm writes.
const STDIN_CHUNK: usize = 64 << 10;

/// Hands each whole line of a stream to `line`, keeping the rest.
#[derive(Debug, Default)]
pub struct Lines {
    partial: Vec<u8>,
}

impl Lines {
    /// Take `bytes`, handing on each line they complete.
    pub fn feed(&mut self, bytes: &[u8], line: &mut dyn FnMut(&str)) {
        self.partial.extend_from_slice(bytes);
        while let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
            let rest = self.partial.split_off(at + 1);
            let whole = std::mem::replace(&mut self.partial, rest);
            line(String::from_utf8_lossy(&whole[..at]).trim_end_matches('\r'));
        }
    }

    /// The stream ended: hand on what is left as a last line.
    pub fn finish(&mut self, line: &mut dyn FnMut(&str)) {
        if !self.partial.is_empty() {
            let rest = std::mem::take(&mut self.partial);
            line(&String::from_utf8_lossy(&rest));
        }
    }
}

/// The lane's VM, through its helper's session.
pub struct Vz {
    link: Mutex<Option<Box<dyn Link>>>,
    /// The lane user.
    pub user: GuestUser,
    /// The run files.
    pub paths: Paths,
    /// `copy_dir_in`'s staging share directory.
    pub in_dir: PathBuf,
    /// The helper's content address.
    pub helper_sha256: String,
    /// The agent's hello.
    pub hello: Hello,
}

impl std::fmt::Debug for Vz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vz")
            .field("user", &self.user)
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl Vz {
    /// The arm over `link`.
    #[must_use]
    pub fn new(
        link: Box<dyn Link>,
        user: GuestUser,
        paths: Paths,
        helper_sha256: String,
        hello: Hello,
    ) -> Self {
        let in_dir = paths.root().join(IN);
        Self {
            link: Mutex::new(Some(link)),
            user,
            paths,
            in_dir,
            helper_sha256,
            hello,
        }
    }

    fn with_link<T>(&self, f: impl FnOnce(&mut dyn Link) -> T) -> Option<T> {
        let mut guard = self
            .link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_mut().map(|l| f(l.as_mut()))
    }

    /// Run `exec` on a connection of its own, writing `stdin` when given, handing each
    /// chunk of its stdout to `on_out`.
    pub fn run(&self, exec: &Exec, stdin: Option<&[u8]>, on_out: &mut dyn FnMut(&[u8])) -> Ran {
        let t0 = Instant::now();
        let lost = |why: String, out: Vec<u8>, err: Vec<u8>| Ran {
            ended: Ended::Lost(why),
            out,
            err,
            secs: t0.elapsed().as_secs_f64(),
        };
        let (id, fd) = match self.with_link(|l| l.connect()) {
            Some(Ok(c)) => c,
            Some(Err(why)) => return lost(format!("no command connection: {why}"), vec![], vec![]),
            None => return lost("the session is closed".into(), vec![], vec![]),
        };
        let mut stream = UnixStream::from(fd);
        let mut exec = exec.clone();
        exec.stdin = if stdin.is_some() {
            Stdin::Pipe
        } else {
            Stdin::Null
        };
        if let Err(e) = wire::write_frame(&mut stream, &Frame::Exec(exec)) {
            self.with_link(|l| l.release(id));
            let why = self.with_link(|l| l.why_lost()).unwrap_or_default();
            return lost(format!("Exec: {e}; {why}"), vec![], vec![]);
        }
        let writer = stdin.map(|bytes| {
            let bytes = bytes.to_vec();
            let out = stream.try_clone();
            std::thread::spawn(move || {
                let Ok(mut out) = out else { return };
                for chunk in bytes.chunks(STDIN_CHUNK) {
                    if wire::write_frame(&mut out, &Frame::Stdin(chunk.to_vec())).is_err() {
                        return;
                    }
                }
                let _ = wire::write_frame(&mut out, &Frame::StdinEof);
            })
        });
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ended = loop {
            match wire::read_frame(&mut stream) {
                Ok(Some(Frame::Out(b))) => {
                    on_out(&b);
                    out.extend_from_slice(&b);
                }
                Ok(Some(Frame::Err(b))) => err.extend_from_slice(&b),
                Ok(Some(Frame::Exit(exit))) => break Ok(exit),
                Ok(Some(other)) => {
                    break Err(format!(
                        "the agent sent a frame of kind {:#04x} on a command connection",
                        other.kind()
                    ));
                }
                Ok(None) => break Err("the connection ended before its Exit".into()),
                Err(e) => break Err(format!("the command connection: {e}")),
            }
        };
        let _ = stream.shutdown(std::net::Shutdown::Both);
        if let Some(w) = writer {
            let _ = w.join();
        }
        self.with_link(|l| l.release(id));
        match ended {
            Ok(exit) => Ran {
                ended: Ended::Exit(exit),
                out,
                err,
                secs: t0.elapsed().as_secs_f64(),
            },
            Err(how) => {
                let why = self.with_link(|l| l.why_lost()).unwrap_or_default();
                lost(format!("{how}: {why}"), out, err)
            }
        }
    }

    /// As [`crate::linux_driver::Driver::shell`].
    #[must_use]
    pub fn shell(&self, workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
        let (env, argv) = exec_parts(&self.user.env(), argv);
        let exec = exec_of(&argv, workdir, &env, stdin.is_some(), false);
        out_of(&self.run(&exec, stdin.map(str::as_bytes), &mut |_| {}))
    }

    /// As [`crate::linux_driver::Driver::shell_streamed`]: one pipe for stdout and stderr
    /// in the guest (`merge`), each line shown here as it comes.
    #[must_use]
    pub fn shell_streamed(&self, workdir: &str, argv: &[String], prefix: &str) -> Out {
        let (env, argv) = exec_parts(&self.user.env(), argv);
        let exec = exec_of(&argv, workdir, &env, false, true);
        let stdout = std::io::stdout();
        let mut lines = Lines::default();
        let mut show = |line: &str| {
            let mut lock = stdout.lock();
            let _ = writeln!(lock, "{prefix}{line}");
        };
        let ran = self.run(&exec, None, &mut |b| lines.feed(b, &mut show));
        lines.finish(&mut show);
        out_of(&ran)
    }

    /// As [`crate::linux_driver::Driver::copy_out`]: `cat -- <guest>`, its stdout written
    /// to the host file; the `Out`'s text is its stderr.
    #[must_use]
    pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
        let argv = ["cat".to_string(), "--".into(), guest_path.into()];
        let exec = exec_of(&argv, "/", &self.user.env(), false, false);
        let ran = self.run(&exec, None, &mut |_| {});
        written(&ran, |bytes| {
            std::fs::write(host_path, bytes).map_err(|e| format!("{}: {e}", host_path.display()))
        })
    }

    /// As [`crate::linux_driver::Driver::copy_dir_out`]: `tar -czf -` of the directory in
    /// the guest, unpacked here by atpkg's vetted extraction (§7, fact 51), so no member
    /// lands outside `host_parent/<name>`.
    #[must_use]
    pub fn copy_dir_out(&self, guest_dir: &str, host_parent: &Path) -> Out {
        let guest = Path::new(guest_dir);
        let (Some(parent), Some(name)) = (guest.parent(), guest.file_name()) else {
            return refused(format!("{guest_dir} names no directory to copy"));
        };
        let argv = [
            "tar".to_string(),
            "-C".into(),
            parent.to_string_lossy().into_owned(),
            "-czf".into(),
            "-".into(),
            "--".into(),
            name.to_string_lossy().into_owned(),
        ];
        let exec = exec_of(&argv, "/", &self.user.env(), false, false);
        let ran = self.run(&exec, None, &mut |_| {});
        written(&ran, |bytes| {
            unpack_into(bytes, host_parent, Path::new(name))
        })
    }

    /// As [`crate::linux_driver::Driver::copy_dir_in`]: the directory cloned into the run's
    /// read-only `in` share (APFS `clonefile`, else a copy), then `cp -a` from the share in
    /// the guest; the staged copy is removed after.
    #[must_use]
    pub fn copy_dir_in(&self, host_dir: &Path, guest_parent: &str) -> Out {
        let Some(name) = host_dir.file_name() else {
            return refused(format!("{} names no directory", host_dir.display()));
        };
        let staged = self.in_dir.join(name);
        if let Err(e) = remove_tree(&staged).and_then(|()| copy_tree(host_dir, &staged)) {
            return refused(format!(
                "staging {} in the in share: {e}",
                host_dir.display()
            ));
        }
        let from = format!("{SHARE_AT}/{IN}/{}", name.to_string_lossy());
        let argv = [
            "cp".to_string(),
            "-a".into(),
            "--".into(),
            from,
            format!("{}/", guest_parent.trim_end_matches('/')),
        ];
        let exec = exec_of(&argv, "/", &self.user.env(), false, false);
        let out = out_of(&self.run(&exec, None, &mut |_| {}));
        let _ = remove_tree(&staged);
        out
    }

    /// Mount the `aterm` share in the guest and bind its `shares` at their host paths (the
    /// agent's `Mount`); after a swap, it rebinds.
    ///
    /// # Errors
    /// The agent refused it, or the connection failed.
    pub fn mount(&self, shares: &[ShareEntry]) -> Result<(), String> {
        let shares: Vec<Share> = shares
            .iter()
            .filter(|s| s.name != IN)
            .map(|s| Share {
                name: s.name.clone(),
                path: s.path.as_os_str().as_encoded_bytes().to_vec(),
            })
            .collect();
        let (id, fd) = match self.with_link(|l| l.connect()) {
            Some(c) => c?,
            None => return Err("the session is closed".into()),
        };
        let mut stream = UnixStream::from(fd);
        let sent = wire::write_frame(&mut stream, &Frame::Mount(shares));
        let answer = sent
            .map_err(|e| format!("Mount: {e}"))
            .and_then(|()| wire::read_frame(&mut stream).map_err(|e| format!("Mount: {e}")));
        self.with_link(|l| l.release(id));
        match answer? {
            Some(Frame::Exit(Exit::Exited(0))) => Ok(()),
            Some(Frame::Exit(Exit::NotStarted(why))) => Err(format!("the agent's Mount: {why}")),
            other => Err(format!("the agent answered Mount with {other:?}")),
        }
    }

    /// Swap the running share to `entries` and rebind it in the guest.
    ///
    /// # Errors
    /// The helper or the agent refused it.
    pub fn swap(&self, entries: &[ShareEntry]) -> Result<(), String> {
        match self.with_link(|l| l.share(entries)) {
            Some(r) => r?,
            None => return Err("the session is closed".into()),
        }
        self.mount(entries)
    }

    /// Ask the agent to power the guest off (the bake's end, §4).
    ///
    /// # Errors
    /// The connection failed.
    pub fn poweroff(&self) -> Result<(), String> {
        let (_id, fd) = match self.with_link(|l| l.connect()) {
            Some(c) => c?,
            None => return Err("the session is closed".into()),
        };
        aterm_vz::agent::poweroff(fd)
    }

    /// As [`crate::linux_driver::Driver::host_disk`]: the live disk's allocated bytes.
    #[must_use]
    pub fn host_disk(&self) -> Option<HostDisk> {
        use std::os::unix::fs::MetadataExt as _;
        let path = self.paths.live_disk();
        let meta = std::fs::metadata(&path).ok()?;
        Some(HostDisk {
            bytes: meta.blocks() * 512,
            path,
        })
    }

    /// As [`crate::linux_driver::Driver::version`].
    #[must_use]
    pub fn version(&self) -> String {
        format!(
            "aterm-vz-host {}, agent {}, Ubuntu {} kernel {}, boot {}",
            short(&self.helper_sha256),
            short(&aterm_codec::hex::encode_lower(&self.hello.agent_sha256)),
            aterm_vz::pins::RELEASE,
            self.hello.kernel,
            self.hello.boot_id
        )
    }

    /// As [`crate::linux_driver::Driver::end`].
    pub fn end(self, booted_here: bool, keep: bool) {
        let link = self
            .link
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut link) = link else { return };
        if keep {
            match link.keep() {
                Ok(()) => println!("linux: {VM} stays up (keep set; `{LAB} linux down` stops it)"),
                Err(e) => eprintln!("aterm-lab: linux: keep was not set: {e}"),
            }
        }
        let kept = link.kept();
        let record = Record::read(&self.paths.record());
        link.detach();
        if kept {
            return;
        }
        if let Some(r) = record {
            match wait_ended(&self.paths, &r) {
                Ok(()) if booted_here => println!("linux: VM stopped (this run booted it)"),
                Ok(()) => println!("linux: VM stopped (no session holds it, and keep is unset)"),
                Err(why) => eprintln!("aterm-lab: linux: {why}"),
            }
        }
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn refused(why: String) -> Out {
    Out {
        code: Some(1),
        text: why,
        secs: 0.0,
        signal: None,
        not_started: None,
        lost: None,
    }
}

/// `ran`'s `Out`, its stdout handed to `keep` once it exited 0; the text is its stderr.
fn written(ran: &Ran, keep: impl FnOnce(&[u8]) -> Result<(), String>) -> Out {
    let mut out = out_of(&Ran {
        out: Vec::new(),
        ..ran.clone()
    });
    if out.ok()
        && let Err(why) = keep(&ran.out)
    {
        out.code = Some(1);
        out.text.push_str(&why);
    }
    out
}

/// Unpack the `.tar.gz` `bytes` (one member tree, `name`) into `host_parent/name`, through
/// an empty staging directory beside it, by atpkg's vetted extraction.
///
/// # Errors
/// The staging failed, an entry was refused, or `host_parent/name` exists.
pub fn unpack_into(bytes: &[u8], host_parent: &Path, name: &Path) -> Result<(), String> {
    let dest = host_parent.join(name);
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(format!(
            "{} exists: nothing is copied over it",
            dest.display()
        ));
    }
    let staging = host_parent.join(format!(".vz-copy-{}", std::process::id()));
    let _ = remove_tree(&staging);
    std::fs::create_dir_all(staging.join("tree"))
        .map_err(|e| format!("{}: {e}", staging.display()))?;
    let archive = staging.join("archive.tar.gz");
    let unpacked = std::fs::write(&archive, bytes)
        .map_err(|e| format!("{}: {e}", archive.display()))
        .and_then(|()| {
            atpkg::extract::extract_tree(
                &archive,
                &staging.join("tree"),
                atpkg::extract::Container::TarGz,
                COPY_CAP,
                atpkg::extract::ExtractOptions::default(),
            )
            .map_err(|e| format!("the guest's archive of {}: {e}", name.display()))
        })
        .and_then(|()| {
            let landed = staging.join("tree").join(name);
            std::fs::rename(&landed, &dest)
                .map_err(|e| format!("{} -> {}: {e}", landed.display(), dest.display()))
        });
    let _ = remove_tree(&staging);
    unpacked
}

/// Remove the tree at `path`, if any.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    }
}

/// Copy the tree `from` to the new path `to`: each file an APFS clone where the volume
/// can, else a copy; links as links.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(from)?, to);
    }
    if meta.is_file() {
        return aterm_vz::clone::clone_file(from, to)
            .or_else(|_| std::fs::copy(from, to).map(|_| ()));
    }
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// How long the wait for a helper's end may see neither its log nor the console grow: one
/// span past the helper's own power-off grace, since a guest that does not power off is
/// quiet for that whole grace before the helper stops it hard and says so (measured: a
/// bake's guest whose PID 1 could not execute its shutdown binary).
const QUIET: Duration = aterm_vz::helper::STOP_GRACE.saturating_add(aterm_vz::detect::SPAN);

/// Wait for the helper `record` names to end once its last session closed: a one-minute
/// no-progress detector over its log and the console (the guest's power-off writes both).
///
/// # Errors
/// It ran on with neither growing for a minute; it is not signalled (it stops its VM itself).
pub fn wait_ended(paths: &Paths, record: &Record) -> Result<(), String> {
    let size = || {
        [paths.helper_log(), paths.console_log()]
            .iter()
            .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
            .sum::<u64>()
    };
    let (mut last, mut since) = (size(), Instant::now());
    while aterm_vz::record::is_live(record, &Kernel) {
        let now = size();
        if now != last {
            (last, since) = (now, Instant::now());
        }
        if since.elapsed() >= QUIET {
            return Err(format!(
                "helper {} still runs, and wrote nothing for {} s; it stops its VM itself \
                 (`{LAB} linux status`)",
                record.pid,
                QUIET.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// What [`open`] brought up.
pub struct Opened {
    /// The arm.
    pub vz: Vz,
    /// Whether this run booted the VM.
    pub booted_here: bool,
    /// A bake under way: its disk, to seal once the guest is provisioned and powered off.
    pub bake: Option<PartialBake>,
    /// What it took.
    pub steps: Vec<(String, f64)>,
}

/// The share entry's name for the directory `path` of `kind` (`repo`, `git`):
/// `<kind>-<16 hex>`, the FNV-1a of its path and its identity (device, inode, birth time),
/// so each checkout's entries have names of their own (§7). A swap to another checkout then
/// binds names the share's mount never looked up. Virtiofs keeps one superblock per device
/// while anything references it, so a guest process still inside the old checkout keeps
/// that superblock and its cached entries, and the fresh mount after a swap shares them: a
/// bind through the swapped name read the old directory (measured in V4). A directory made
/// again at the same path is another identity, so another name.
#[must_use]
pub fn entry_name(kind: &str, path: &Path) -> String {
    use astream_wire::hash::{FNV1A_64_INIT, fnv1a_64_update};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let mut h = fnv1a_64_update(FNV1A_64_INIT, path.as_os_str().as_bytes());
    if let Ok(meta) = std::fs::metadata(path) {
        h = fnv1a_64_update(h, &meta.dev().to_le_bytes());
        h = fnv1a_64_update(h, &meta.ino().to_le_bytes());
        if let Some(born) = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        {
            h = fnv1a_64_update(h, &born.as_nanos().to_le_bytes());
        }
    }
    format!("{kind}-{h:016x}")
}

/// The `aterm` share of a run from the checkout `repo`: `repo`, the git directory a linked
/// worktree's `.git` names outside it, each under its [`entry_name`], and the `in` staging
/// directory, each read-only.
#[must_use]
pub fn shares(repo: &Path, in_dir: &Path) -> Vec<ShareEntry> {
    let mut out = vec![ShareEntry {
        name: entry_name("repo", repo),
        path: repo.to_path_buf(),
    }];
    if let Some(git) = crate::linux::outside_git_dir(repo) {
        out.push(ShareEntry {
            name: entry_name("git", &git),
            path: git,
        });
    }
    out.push(ShareEntry {
        name: IN.into(),
        path: in_dir.to_path_buf(),
    });
    out
}

/// The arm's `up`: the gates (Apple silicon, Rosetta for Linux, the `virtualization` row),
/// the helper and the agent built and placed under the state, then a session, and only when
/// it found no VM the kernel laid, the disks prepared and the boot; then the agent's
/// `Ready` and the share mounted. A bake's boot comes back with its disk ([`Opened::bake`]).
///
/// # Errors
/// NOT RUN where a gate, a build, the helper or the boot says so.
pub fn open(repo: &Path) -> Result<Opened, Fail> {
    if !cfg!(target_arch = "aarch64") {
        return Err(not_run("the vz lane needs Apple silicon"));
    }
    if !Path::new("/Library/Apple/usr/libexec/oah/RosettaLinux").exists() {
        return Err(not_run(
            "Rosetta for Linux is not installed (softwareupdate --install-rosetta)",
        ));
    }
    virtualization_gate(aterm_sysprobe::hv_support())?;
    let root = root()?;
    let paths = Paths::at(&root);
    paths.checked_socket().map_err(not_run)?;
    let mut steps = Vec::new();
    let t0 = Instant::now();
    let built = build_host(repo)?;
    let placed = place_host(&built, &root, &paths)?;
    let agent = crate::linux_vz::build_agent(repo, &root)?;
    steps.push(("helper and agent".to_string(), t0.elapsed().as_secs_f64()));
    let boot_share = agent
        .path
        .parent()
        .ok_or_else(|| Fail::die("linux: the agent has no directory"))?
        .to_path_buf();
    let seed_user =
        aterm_vz::seed::User::host().ok_or_else(|| Fail::die("linux: this user has no name"))?;
    let user = GuestUser {
        name: seed_user.name.clone(),
        uid: seed_user.uid,
    };
    let in_dir = root.join(IN);
    std::fs::create_dir_all(&in_dir)
        .map_err(|e| Fail::die(format!("{}: {e}", in_dir.display())))?;
    let shares = shares(repo, &in_dir);
    let attach = Attach {
        proto: PROTO,
        helper_sha256: placed.sha256.clone(),
        agent_sha256: agent.sha256.clone(),
        shares: shares.clone(),
        keep: false,
    };
    let mut lab = Lab::new(paths.clone(), Detached::new(placed.exe.clone()));
    let t0 = Instant::now();
    let mut session = lab
        .session(&attach)
        .map_err(|e| client_fail(e, &placed.exe, lab.replaced.as_ref()))?;
    let booted_here = session.phase == Phase::Absent;
    println!(
        "linux: a session on helper {} in {:.2} s; {VM} is {}",
        Record::read(&paths.record()).map_or(0, |r| r.pid),
        t0.elapsed().as_secs_f64(),
        session.phase.name()
    );
    let mut bake = None;
    let prepare = || -> Result<Plan, Fail> {
        let (plan, partial) = plan(&paths, &seed_user, &boot_share, &agent.sha256, &shares)?;
        bake = partial;
        Ok(plan)
    };
    let t0 = Instant::now();
    let hello = session
        .bring_up(prepare, &mut |p| {
            println!("linux: {:7.2} s {p}", t0.elapsed().as_secs_f64());
        })
        .map_err(|e| match e {
            UpError::Prepare(fail) => fail,
            UpError::Client(e) => client_fail(e, &placed.exe, lab.replaced.as_ref()),
        })?;
    if booted_here {
        steps.push((
            if bake.is_some() { "bake boot" } else { "boot" }.to_string(),
            t0.elapsed().as_secs_f64(),
        ));
    }
    let vz = Vz::new(Box::new(session), user, paths, placed.sha256, hello);
    if let Err(why) = vz.mount(&shares) {
        vz.end(booted_here, false);
        return Err(not_run(format!(
            "the share was not mounted in the guest: {why}"
        )));
    }
    Ok(Opened {
        vz,
        booted_here,
        bake,
        steps,
    })
}

/// The boot's plan: the pinned kernel laid, the disks prepared (V3's layers), and, where no
/// baked disk is sealed, a bake's disks (its partial disk and the seed).
fn plan(
    paths: &Paths,
    user: &aterm_vz::seed::User,
    boot_share: &Path,
    agent_sha256: &str,
    shares: &[ShareEntry],
) -> Result<(Plan, Option<PartialBake>), Fail> {
    let lookup = aterm_uds::host::Lookup::host;
    let layers = Layers::at(paths.root());
    let (vmlinuz, initrd) =
        aterm_vz::pins::kernel_files(&lookup(aterm_vz::pins::kernel_tool())).map_err(layer_fail)?;
    let kernel = aterm_vz::kernel::lay(
        &layers.kernel_dir(),
        &vmlinuz,
        &initrd,
        &aterm_vz::kernel::Want::PINNED,
    )
    .map_err(layer_fail)?;
    let base = Base::pinned(&lookup(aterm_vz::pins::root_tool())).map_err(layer_fail)?;
    let seed = aterm_vz::seed::Seed::new(user).map_err(not_run)?;
    let prepared = layers
        .prepare(&System, &base, &seed.fingerprint(), &mut |s| {
            println!("linux: {s}")
        })
        .map_err(layer_fail)?;
    let (disks, bake) = match prepared {
        Prepared::Live { disk, .. } => (
            vec![Disk {
                path: disk,
                read_only: false,
            }],
            None,
        ),
        Prepared::Bake(partial) => {
            let iso = layers.seed_iso(&System, &seed).map_err(layer_fail)?;
            println!(
                "linux: no baked disk for this base and seed: this boot bakes {}",
                partial.sealed_path().display()
            );
            (
                vec![
                    Disk {
                        path: partial.path().to_path_buf(),
                        read_only: false,
                    },
                    Disk {
                        path: iso,
                        read_only: true,
                    },
                ],
                Some(partial),
            )
        }
    };
    Ok((
        Plan {
            name: VM.into(),
            cpus: CPUS,
            memory_bytes: MEMORY,
            kernel: kernel.image,
            initrd: kernel.initrd,
            command_line: COMMAND_LINE.into(),
            disks,
            mac: mac_for(VM),
            boot_share: boot_share.to_path_buf(),
            shares: shares.to_vec(),
            rosetta: true,
            console_log: paths.console_log(),
            agent_sha256: agent_sha256.to_string(),
        },
        bake,
    ))
}

/// The bake's last steps once the guest is provisioned (§4 steps 4 and 5): [`end_bake`],
/// then the seal.
///
/// # Errors
/// As [`end_bake`] (the partial disk stays for the next bring-up to remove), or the seal
/// failed.
pub fn finish_bake(vz: Vz, partial: PartialBake) -> Result<PathBuf, Fail> {
    end_bake(vz)?;
    partial.seal().map_err(layer_fail)
}

/// The bake's guest finished: cloud-init's off switch, a trim, systemd re-executed and the
/// disk synced, the power-off through the agent, the session held until the helper says the
/// VM stopped, then the helper's end. Only a guest that powered itself off
/// ([`aterm_vz::helper::POWERED_OFF`]) ends a bake that seals: one a detector or a hard stop
/// ended was never cleanly unmounted (§4: a detector that fires stops before the seal).
///
/// A PID 1 left on its old binary by provisioning's systemd upgrade is re-executed first,
/// a hardening against a power-off that could not execute its shutdown binary (seen once in
/// V4, its cause not established); the sync keeps the disk whole up to the power-off.
///
/// # Errors
/// A step failed, or the guest did not power itself off: NOT RUN, nothing sealed.
pub fn end_bake(vz: Vz) -> Result<(), Fail> {
    for (label, argv) in [
        (
            "cloud-init disabled",
            ["sudo", "touch", "/etc/cloud/cloud-init.disabled"].as_slice(),
        ),
        ("trim", ["sudo", "fstrim", "/"].as_slice()),
        (
            "systemd re-executed",
            ["sudo", "systemctl", "daemon-reexec"].as_slice(),
        ),
        ("sync", ["sudo", "sync"].as_slice()),
    ] {
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
        let out = vz.shell("/", &argv, None);
        if !out.ok() {
            vz.end(true, false);
            return Err(not_run(format!(
                "the bake's step '{label}' failed ({}): {}",
                out.status(),
                out.tail(5)
            )));
        }
    }
    let record = Record::read(&vz.paths.record());
    let paths = vz.paths.clone();
    if let Err(why) = vz.poweroff() {
        vz.end(true, false);
        return Err(not_run(format!("the bake's power-off: {why}")));
    }
    // The guest powers off; the session hears how the VM stopped, then the helper ends.
    let link = vz
        .link
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stopped = match link {
        Some(mut link) => {
            let stopped = link.wait_stopped();
            link.detach();
            stopped
        }
        None => Err("the session was closed".into()),
    };
    if let Some(r) = record {
        wait_ended(&paths, &r).map_err(not_run)?;
    }
    match stopped {
        Ok(why) if why == aterm_vz::helper::POWERED_OFF => {
            println!("linux: the bake's guest powered itself off");
            Ok(())
        }
        Ok(why) => Err(not_run(format!(
            "the bake's guest did not power itself off ({why}): its disk is not sealed, and the \
             next bring-up bakes again"
        ))),
        Err(why) => Err(not_run(format!(
            "the bake's power-off was not seen ({why}): its disk is not sealed, and the next \
             bring-up bakes again"
        ))),
    }
}

/// The helper copy a record names, under the state's `host/`.
fn helper_exe(root: &Path, record: &Record) -> PathBuf {
    root.join("host")
        .join(&record.helper_sha256)
        .join(aterm_vz::sign::EXE)
}

/// A session joined on the live helper `record` names, for `down`'s clean: its helper and
/// the agent its guest said hello with, and this checkout's share. Nothing is started; a
/// helper that does not answer is escalated ([`ClientError::Hung`]).
fn join(root: &Path, paths: &Paths, record: &Record, repo: &Path) -> Result<Vz, ClientError> {
    let agent = record.agent_sha256.clone().ok_or_else(|| {
        ClientError::Error(format!(
            "helper {}'s guest has not said hello yet",
            record.pid
        ))
    })?;
    let user = aterm_vz::seed::User::host()
        .ok_or_else(|| ClientError::Error("this user has no name".into()))?;
    let in_dir = root.join(IN);
    let attach = Attach {
        proto: PROTO,
        helper_sha256: record.helper_sha256.clone(),
        agent_sha256: agent,
        shares: shares(repo, &in_dir),
        keep: false,
    };
    let mut lab = Lab::new(paths.clone(), Detached::new(helper_exe(root, record)));
    let mut session = lab.join(&attach)?;
    let hello = session.wait_ready(&mut |p| println!("linux: {p}"))?;
    Ok(Vz::new(
        Box::new(session),
        GuestUser {
            name: user.name,
            uid: user.uid,
        },
        paths.clone(),
        record.helper_sha256.clone(),
        hello,
    ))
}

/// The vz arm's `down`: the guest's build output removed and the disk trimmed (`clean`,
/// through a session joined on the live helper), then `Stop`, and the wait for the helper's
/// end (§5.1); a dead helper's run files are removed. Nothing is started.
///
/// # Errors
/// The helper did not answer (NOT RUN, naming it, once it was SIGKILLed).
pub fn down(
    clean: impl Fn(&crate::linux_driver::Driver, &str) -> Result<(), String>,
) -> Result<i32, Fail> {
    let root = root()?;
    let paths = Paths::at(&root);
    let record = match aterm_vz::record::holder(&paths, &Kernel) {
        aterm_vz::record::Holder::Live(r) => r,
        aterm_vz::record::Holder::Dead(r) => {
            aterm_vz::record::remove_leftovers(&paths, &r);
            println!(
                "linux: helper {} was gone; its run files were removed",
                r.pid
            );
            return Ok(0);
        }
        aterm_vz::record::Holder::None => {
            println!("linux: no helper runs {VM} at {}", root.display());
            return Ok(0);
        }
    };
    let repo = PathBuf::from(crate::linux::repo());
    let exe = helper_exe(&root, &record);
    let joined = join(&root, &paths, &record, &repo);
    down_after(joined, clean, record.pid, || {
        Lab::new(paths.clone(), Detached::new(exe.clone()))
            .down()
            .map_err(|e| client_fail(e, &exe, None))
    })
}

/// `down` once its join on helper `pid` came back: the clean through a joined session, then
/// `stop` (`Stop`, and the wait for the helper's end). A join the helper did not answer was
/// escalated, so no `Stop` can be answered: that is NOT RUN with the escalation's words,
/// never a stop. Any other join failure keeps the build output and still stops the VM.
///
/// # Errors
/// The join or the stop was escalated, or the stop failed (NOT RUN).
pub fn down_after(
    joined: Result<Vz, ClientError>,
    clean: impl Fn(&crate::linux_driver::Driver, &str) -> Result<(), String>,
    pid: u32,
    stop: impl FnOnce() -> Result<(), Fail>,
) -> Result<i32, Fail> {
    match joined {
        Ok(vz) => {
            let home = vz.user.home();
            let guest = crate::linux_driver::Driver::Vz(Box::new(vz));
            match clean(&guest, &home) {
                Ok(()) => println!("linux: the guest's build output removed, disk trimmed"),
                Err(e) => eprintln!("aterm-lab: linux: build output NOT removed: {e}"),
            }
            guest.end(false, false);
        }
        Err(e @ ClientError::Hung(_)) => {
            eprintln!("aterm-lab: linux: build output kept: the helper did not answer");
            return Err(client_fail(e, Path::new(""), None));
        }
        Err(why) => eprintln!("aterm-lab: linux: build output kept: {why}"),
    }
    let t0 = Instant::now();
    stop()?;
    println!(
        "linux: {VM} stopped (helper {pid} ended {:.2} s after Stop, or with its last session)",
        t0.elapsed().as_secs_f64()
    );
    Ok(0)
}

/// Each layer file's allocated bytes, as `status` lists them.
fn allocated(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.blocks() * 512)
}

/// The vz arm's `status`: the live helper's state (no session opens, nothing is signalled:
/// a session would swap the share under a run from another checkout), each layer file's
/// allocated bytes and the volume's free space. Clones share blocks, which each file's count
/// includes.
///
/// # Errors
/// The helper did not answer (NOT RUN, naming it; it runs on).
pub fn status() -> Result<i32, Fail> {
    let root = root()?;
    let paths = Paths::at(&root);
    println!("driver    vz (state {})", root.display());
    let mut lab = Lab::new(paths.clone(), Detached::new(PathBuf::new()));
    match lab
        .status()
        .map_err(|e| client_fail(e, Path::new(""), None))?
    {
        None => println!("vm        none running (`{LAB} linux up` boots it)"),
        Some(s) => {
            println!(
                "vm        {VM} {} — helper {} (unique id {}), keep {}, {} session(s)",
                s.phase.name(),
                s.pid,
                s.uniq,
                s.keep,
                s.sessions
            );
            println!(
                "guest     boot {}, agent {}, beat {}{}",
                if s.boot_id.is_empty() {
                    "-"
                } else {
                    &s.boot_id
                },
                if s.agent_sha256.is_empty() {
                    "-"
                } else {
                    short(&s.agent_sha256)
                },
                s.beat_ms_ago
                    .map_or_else(|| "-".to_string(), |ms| format!("{ms} ms ago")),
                if s.failed_units.is_empty() {
                    String::new()
                } else {
                    format!(", failed units: {}", s.failed_units.join(" "))
                }
            );
        }
    }
    let layers = Layers::at(&root);
    let mut files = vec![layers.live_disk()];
    if let Ok(dir) = std::fs::read_dir(layers.baked_dir()) {
        let mut baked: Vec<PathBuf> = dir.flatten().map(|e| e.path()).collect();
        baked.sort();
        files.extend(baked);
    }
    for f in files {
        if let Some(bytes) = allocated(&f) {
            println!(
                "host disk {} allocated in {}",
                crate::linux::gib(bytes),
                f.display()
            );
        }
    }
    if let Some(r) = layers.record() {
        println!(
            "live      cloned from {} at {} (Unix seconds)",
            r.baked, r.cloned
        );
    }
    match aterm_vz::space::read(&root) {
        Ok(free) => println!(
            "volume    {} free on {} (clones share blocks, which each file above counts)",
            aterm_vz::space::gib(free.bytes),
            free.volume
        ),
        Err(e) => println!("volume    unread: {e}"),
    }
    println!(
        "provision `{LAB} linux exec -- cat {}` reads the guest's stamp",
        crate::linux_guest::STAMP
    );
    Ok(0)
}

/// The vz arm's `destroy` (§4): `Stop` first, then the live disk, its record and every baked
/// disk deleted once no process holds one open. Base stays in the tooling cache, as lima
/// keeps its image cache, so the next `up` bakes again.
///
/// # Errors
/// The helper did not answer and still runs, a disk stayed held, or a file could not be
/// removed.
pub fn destroy() -> Result<i32, Fail> {
    let root = root()?;
    let paths = Paths::at(&root);
    let mut lab = Lab::new(paths.clone(), Detached::new(PathBuf::new()));
    let stopped = lab.down();
    let runs = matches!(
        aterm_vz::record::holder(&paths, &Kernel),
        aterm_vz::record::Holder::Live(_)
    );
    let layers = Layers::at(&root);
    let mut doomed = vec![layers.live_disk(), layers.record_path()];
    if let Ok(dir) = std::fs::read_dir(layers.baked_dir()) {
        doomed.extend(dir.flatten().map(|e| e.path()));
    }
    let gone = destroy_files(
        stopped,
        runs,
        &doomed,
        &mut holders_of,
        aterm_vz::detect::SPAN,
    )?;
    if gone.is_empty() {
        println!("linux: there is no {VM} disk at {}", root.display());
    }
    for f in gone {
        println!("linux: deleted {}", f.display());
    }
    Ok(0)
}

/// The processes of this user, other than this one, that hold `path` open.
///
/// # Errors
/// The process table could not be read.
fn holders_of(path: &Path) -> Result<Vec<u32>, String> {
    match aterm_uds::holders::scan_file_holders(path, &[std::process::id()]) {
        Ok(scan) => Ok(scan.holders),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported
            ) =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(format!("who holds {}: {e}", path.display())),
    }
}

/// `destroy`'s deletions once `stopped` came back from its `Stop`: a helper that did not
/// answer and still `runs` keeps every file (NOT RUN); one the escalation ended goes on, as
/// §5.1's `destroy` does. Each file is unlinked only once no process `holders` names holds
/// any of them, since a SIGKILLed helper's VM process outlives it for a moment (fact 44)
/// and no disk is unlinked under a running VM: a one-minute hang detector.
///
/// # Errors
/// The helper runs on, a file stayed held for `span`, or a removal failed: nothing more is
/// deleted.
pub fn destroy_files(
    stopped: Result<(), ClientError>,
    runs: bool,
    doomed: &[PathBuf],
    holders: &mut dyn FnMut(&Path) -> Result<Vec<u32>, String>,
    span: Duration,
) -> Result<Vec<PathBuf>, Fail> {
    if let Err(e) = stopped {
        if runs {
            return Err(client_fail(e, Path::new(""), None));
        }
        let words = e.to_string();
        let words = words
            .strip_prefix(aterm_vz::client::NOT_RUN)
            .unwrap_or(&words);
        eprintln!("aterm-lab: linux destroy: {words}; the helper is gone, so the disks go");
    }
    let t0 = Instant::now();
    loop {
        let mut held = Vec::new();
        for f in doomed.iter().filter(|f| f.exists()) {
            for pid in holders(f).map_err(not_run)? {
                held.push(format!("pid {pid} holds {}", f.display()));
            }
        }
        if held.is_empty() {
            break;
        }
        if t0.elapsed() >= span {
            return Err(not_run(format!(
                "linux destroy: {} after {} s; nothing was deleted",
                held.join(", "),
                span.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut gone = Vec::new();
    for f in doomed {
        match std::fs::remove_file(f) {
            Ok(()) => gone.push(f.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Fail::die(format!("linux destroy: {}: {e}", f.display()))),
        }
    }
    Ok(gone)
}

/// One row of `vz-smoke`.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    /// What it checks.
    pub name: String,
    /// Why it failed, or `None`.
    pub failed: Option<String>,
    /// Its wall time.
    pub secs: f64,
    /// What it measured.
    pub note: String,
}

fn check(name: &str, t0: Instant, result: Result<String, String>) -> Check {
    let secs = t0.elapsed().as_secs_f64();
    let (failed, note) = match result {
        Ok(note) => (None, note),
        Err(why) => (Some(why), String::new()),
    };
    let c = Check {
        name: name.into(),
        failed,
        secs,
        note,
    };
    println!(
        "vz-smoke: {:<5} {:7.2} s  {}{}",
        if c.failed.is_none() { "PASS" } else { "FAIL" },
        c.secs,
        c.name,
        match (&c.failed, c.note.is_empty()) {
            (Some(why), _) => format!(": {why}"),
            (None, false) => format!(" ({})", c.note),
            (None, true) => String::new(),
        }
    );
    c
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The checks `vz-smoke` runs on a VM that is up: §10's four outcomes, stdin, the merged
/// order, the environment, the three copies, the read-only checkout, a share swap and
/// rebind, Rosetta's registration and, where the guest has the x86_64 cross compiler, its
/// translation cache.
#[must_use]
pub fn smoke_checks(up: &crate::linux::Up, repo: &str, scratch: &Path) -> Vec<Check> {
    let g = &up.guest;
    let home = up.home.as_str();
    let mut rows = Vec::new();
    let t = Instant::now();
    let out = g.shell(home, &argv(&["sh", "-c", "exit 3"]), None);
    rows.push(check(
        "an exit code is its own",
        t,
        (out.code == Some(3))
            .then(String::new)
            .ok_or_else(|| out.status()),
    ));
    let t = Instant::now();
    let out = g.shell(home, &argv(&["sh", "-c", "kill -KILL $$"]), None);
    rows.push(check(
        "a signal is named, never an exit code",
        t,
        (out.code.is_none() && out.signal == Some(9) && out.text.contains("signal 9"))
            .then(String::new)
            .ok_or_else(|| format!("{}: {}", out.status(), out.tail(2))),
    ));
    let t = Instant::now();
    let out = g.shell(home, &argv(&["/nonexistent/aterm-vz-smoke"]), None);
    rows.push(check(
        "a missing program is not started, never exit 127",
        t,
        out.not_started.clone().ok_or_else(|| out.status()),
    ));
    let t = Instant::now();
    let out = g.shell(
        home,
        &crate::linux_guest::with_env(
            &[("LANE".into(), "1".into())],
            &argv(&["/nonexistent/aterm-vz-smoke"]),
        ),
        None,
    );
    rows.push(check(
        "a missing program under the lane's variables is not started, never env's 127",
        t,
        out.not_started.clone().ok_or_else(|| out.status()),
    ));
    let t = Instant::now();
    let out = g.shell(home, &argv(&["cat"]), Some("one\ntwo\n"));
    rows.push(check(
        "stdin reaches the command",
        t,
        (out.ok() && out.text == "one\ntwo\n")
            .then(String::new)
            .ok_or_else(|| format!("{}: {:?}", out.status(), out.text)),
    ));
    let t = Instant::now();
    let out = g.shell_streamed(
        home,
        &argv(&["sh", "-c", "echo out-1; echo err-2 >&2; echo out-3"]),
        "  | ",
    );
    rows.push(check(
        "stdout and stderr merged in the guest's order",
        t,
        (out.ok() && out.text == "out-1\nerr-2\nout-3\n")
            .then(String::new)
            .ok_or_else(|| format!("{}: {:?}", out.status(), out.text)),
    ));
    let t = Instant::now();
    let out = g.shell(
        "/",
        &argv(&[
            "sh",
            "-c",
            "printf '%s\\n' \"$HOME\" \"$USER\" \"$LOGNAME\" \"$SHELL\" \"$XDG_RUNTIME_DIR\" \"$DBUS_SESSION_BUS_ADDRESS\" \"$(id -u)\" \"$(pwd)\"",
        ]),
        None,
    );
    rows.push(check(
        "the lane user's environment",
        t,
        if out.ok() && out.text.lines().next() == Some(home) {
            Ok(out.text.trim().replace('\n', " "))
        } else {
            Err(format!("{}: {:?}", out.status(), out.text))
        },
    ));
    // copy_out: a binary file, held to its digest on both sides.
    let t = Instant::now();
    let made = g.shell(
        home,
        &argv(&[
            "sh",
            "-c",
            "head -c 1048576 /dev/urandom > /tmp/vz-smoke.bin && sha256sum /tmp/vz-smoke.bin",
        ]),
        None,
    );
    let local = scratch.join("vz-smoke.bin");
    let copied = g.copy_out("/tmp/vz-smoke.bin", &local);
    rows.push(check(
        "copy_out keeps every byte",
        t,
        match (made.ok(), copied.ok(), aterm_vz_agent::sha256_of(&local)) {
            (true, true, Ok(sha)) => {
                let ours = aterm_codec::hex::encode_lower(&sha);
                if made.text.starts_with(&ours) {
                    Ok(format!("1 MiB, sha256 {}", short(&ours)))
                } else {
                    Err(format!(
                        "the guest read {}, this Mac {ours}",
                        made.text.trim()
                    ))
                }
            }
            (m, c, sha) => Err(format!(
                "made {m} ({}), copied {c} ({}), read {sha:?}",
                made.tail(2),
                copied.tail(2)
            )),
        },
    ));
    // copy_dir_out: a tree with a nested file, through tar and atpkg's extraction.
    let t = Instant::now();
    let made = g.shell(
        home,
        &argv(&[
            "sh",
            "-c",
            "rm -rf /tmp/vz-smoke-dir && mkdir -p /tmp/vz-smoke-dir/a/b && echo nested > /tmp/vz-smoke-dir/a/b/c.txt && echo top > /tmp/vz-smoke-dir/top.txt",
        ]),
        None,
    );
    let copied = g.copy_dir_out("/tmp/vz-smoke-dir", scratch);
    let read = std::fs::read_to_string(scratch.join("vz-smoke-dir/a/b/c.txt"));
    rows.push(check(
        "copy_dir_out lands the tree under its own name",
        t,
        match (made.ok(), copied.ok(), read) {
            (true, true, Ok(text)) if text == "nested\n" => Ok(String::new()),
            (m, c, r) => Err(format!(
                "made {m}, copied {c} ({}), read {r:?}",
                copied.tail(3)
            )),
        },
    ));
    // copy_dir_in: a host tree through the in share.
    let t = Instant::now();
    let tree = scratch.join("vz-smoke-in");
    let _ = std::fs::create_dir_all(tree.join("sub"));
    let _ = std::fs::write(tree.join("sub/file.txt"), "from the Mac\n");
    let _ = g.shell(home, &argv(&["rm", "-rf", "/tmp/vz-smoke-in"]), None);
    let copied = g.copy_dir_in(&tree, "/tmp");
    let read = g.shell(home, &argv(&["cat", "/tmp/vz-smoke-in/sub/file.txt"]), None);
    rows.push(check(
        "copy_dir_in lands the tree under its own name",
        t,
        (copied.ok() && read.ok() && read.text == "from the Mac\n")
            .then(String::new)
            .ok_or_else(|| format!("{} / {}", copied.tail(3), read.tail(3))),
    ));
    let t = Instant::now();
    let seen = g.shell(
        home,
        &argv(&["test", "-f", &format!("{repo}/Cargo.toml")]),
        None,
    );
    let wrote = g.shell(home, &argv(&["touch", &format!("{repo}/.vz-smoke")]), None);
    rows.push(check(
        "the checkout is mounted read-only at its own path",
        t,
        (seen.ok() && !wrote.ok())
            .then(|| wrote.tail(1))
            .ok_or_else(|| format!("seen {}, written {}", seen.ok(), wrote.ok())),
    ));
    rows.push(swap_check(up, repo, scratch));
    let t = Instant::now();
    let reg = g.shell(
        home,
        &argv(&["cat", "/proc/sys/fs/binfmt_misc/rosetta"]),
        None,
    );
    rows.push(check(
        "Rosetta is registered",
        t,
        (reg.ok() && reg.text.contains("enabled"))
            .then(|| {
                reg.text
                    .lines()
                    .find(|l| l.starts_with("interpreter"))
                    .unwrap_or("")
                    .to_string()
            })
            .ok_or_else(|| reg.tail(3)),
    ));
    rows.push(rosetta_cache_check(up));
    rows
}

/// The share swapped to another directory and back, each rebound by the agent, timed
/// (the `VZMultipleDirectoryShare` swap measure), while a lane unit sits in the checkout as
/// its working directory: a process left inside the old checkout keeps the share's
/// superblock and its cached entries alive, and the swap must show the new directory anyway
/// (§7; V4's review found a swap that said Ok with the old one still bound).
fn swap_check(up: &crate::linux::Up, repo: &str, scratch: &Path) -> Check {
    let t = Instant::now();
    let crate::linux_driver::Driver::Vz(vz) = &up.guest else {
        return check("a share swap", t, Err("not the vz arm".into()));
    };
    let other = scratch.join("vz-smoke-share");
    let _ = std::fs::create_dir_all(&other);
    let _ = std::fs::write(other.join("marker.txt"), "swapped\n");
    let mine = shares(Path::new(repo), &vz.in_dir);
    let theirs = shares(&other, &vz.in_dir);
    let unit = format!("{}vz-smoke-hold", crate::linux_guest::UNIT_PREFIX);
    let _ = up.guest.shell(
        "/",
        &argv(&["systemctl", "--user", "reset-failed", &unit]),
        None,
    );
    let hold = up.guest.shell(
        "/",
        &argv(&[
            "systemd-run",
            "--user",
            "--unit",
            &unit,
            "-p",
            &format!("WorkingDirectory={repo}"),
            "sleep",
            "600",
        ]),
        None,
    );
    let cwd = up.guest.shell(
        "/",
        &argv(&[
            "sh",
            "-c",
            &format!("readlink /proc/$(systemctl --user show -p MainPID --value {unit})/cwd"),
        ]),
        None,
    );
    let holding = hold.ok() && cwd.ok() && cwd.text.trim() == repo;
    let marker = format!("{}/marker.txt", other.display());
    let swapped = vz.swap(&theirs);
    let t_swap = t.elapsed().as_secs_f64();
    let read = up.guest.shell("/", &argv(&["cat", &marker]), None);
    let t1 = Instant::now();
    let back = vz.swap(&mine);
    let t_back = t1.elapsed().as_secs_f64();
    let again = up.guest.shell(
        "/",
        &argv(&["test", "-f", &format!("{repo}/Cargo.toml")]),
        None,
    );
    let _ = up
        .guest
        .shell("/", &argv(&["systemctl", "--user", "stop", &unit]), None);
    check(
        "a share swap and rebind, and back, with a unit inside the checkout",
        t,
        match (
            holding,
            swapped,
            read.ok() && read.text == "swapped\n",
            back,
            again.ok(),
        ) {
            (true, Ok(()), true, Ok(()), true) => Ok(format!(
                "swap and rebind {:.0} ms, back {:.0} ms",
                t_swap * 1000.0,
                t_back * 1000.0
            )),
            (h, s, r, b, a) => Err(format!(
                "holding {h} ({} / {}), swap {s:?}, read {r} ({}), back {b:?}, checkout again {a}",
                hold.tail(2),
                cwd.tail(1),
                read.tail(2)
            )),
        },
    )
}

/// Rosetta's translation cache: an x86_64 program built in the guest (its cross gcc) run
/// three times, each timed, and rosettad's cache read before and after.
fn rosetta_cache_check(up: &crate::linux::Up) -> Check {
    let t = Instant::now();
    let g = &up.guest;
    let has = g.shell(
        "/",
        &argv(&["sh", "-c", "command -v x86_64-linux-gnu-gcc"]),
        None,
    );
    if !has.ok() {
        return check(
            "Rosetta's translation cache",
            t,
            Ok("skipped: no x86_64-linux-gnu-gcc in the guest yet".into()),
        );
    }
    let built = g.shell(
        "/tmp",
        &argv(&[
            "sh",
            "-c",
            "printf 'int main(void){return 0;}\\n' > /tmp/vz-hello.c && x86_64-linux-gnu-gcc -O2 -o /tmp/vz-hello /tmp/vz-hello.c",
        ]),
        None,
    );
    if !built.ok() {
        return check("Rosetta's translation cache", t, Err(built.tail(3)));
    }
    let count = |g: &crate::linux_driver::Driver| {
        let out = g.shell(
            "/",
            &argv(&[
                "sudo",
                "sh",
                "-c",
                "find /var/cache/rosettad -type f | wc -l",
            ]),
            None,
        );
        out.text.trim().parse::<u64>().ok()
    };
    let before = count(g);
    let mut runs = Vec::new();
    for _ in 0..3 {
        let r = g.shell("/tmp", &argv(&["/tmp/vz-hello"]), None);
        if !r.ok() {
            return check(
                "Rosetta's translation cache",
                t,
                Err(format!("{}: {}", r.status(), r.tail(3))),
            );
        }
        runs.push(format!("{:.0} ms", r.secs * 1000.0));
    }
    let after = count(g);
    let socket = g.shell(
        "/",
        &argv(&["test", "-S", "/run/rosettad/rosetta.sock"]),
        None,
    );
    check(
        "Rosetta's translation cache",
        t,
        Ok(format!(
            "runs {}; cache files {before:?} -> {after:?}; the caching socket {}",
            runs.join(", "),
            if socket.ok() {
                "is there"
            } else {
                "is missing"
            }
        )),
    )
}

/// `linux vz-smoke [--keep-up]`: the vz arm end to end, whatever `LAB_LINUX_DRIVER` says:
/// the lane lock, the bring-up (a bake where none is sealed), [`smoke_checks`], then the run's
/// end. Exit 0 every check passed, 1 one failed, 3 NOT RUN.
///
/// # Errors
/// A usage error, or a bring-up that did not run.
pub fn cmd_vz_smoke(rest: &[String]) -> crate::Res {
    let keep_up = match rest {
        [] => false,
        [flag] if flag == "--keep-up" => true,
        _ => {
            return Err(Fail::usage(format!(
                "linux vz-smoke takes only --keep-up ({LAB} linux help)"
            )));
        }
    };
    crate::linux_driver::choose("vz")?;
    let t0 = Instant::now();
    let _lock = crate::linux::LaneLock::take()?;
    let up = crate::linux::bring_up()?;
    for step in &up.timings {
        println!("vz-smoke: bring-up: {} {:.1} s", step.name, step.secs);
    }
    println!(
        "vz-smoke: up in {:.1} s ({}; booted here: {})",
        t0.elapsed().as_secs_f64(),
        up.guest.version(),
        up.booted_here
    );
    let scratch = crate::linux::state_dir()?.join(format!("vz-smoke-{}", std::process::id()));
    let _ = remove_tree(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| Fail::die(format!("{}: {e}", scratch.display())))?;
    let repo = crate::linux::repo();
    let rows = smoke_checks(&up, &repo, &scratch);
    let _ = remove_tree(&scratch);
    let t_end = Instant::now();
    up.end(keep_up);
    let failed = rows.iter().filter(|r| r.failed.is_some()).count();
    println!(
        "vz-smoke: {} of {} checks passed; the run's end took {:.1} s; {:.1} s in all",
        rows.len() - failed,
        rows.len(),
        t_end.elapsed().as_secs_f64(),
        t0.elapsed().as_secs_f64()
    );
    Ok(i32::from(failed > 0))
}

#[cfg(test)]
#[path = "linux_vz_driver_tests.rs"]
mod tests;

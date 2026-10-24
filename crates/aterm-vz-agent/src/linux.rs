// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux guest's arms: the vsock listener (port 1024, CID 2 only) and dialer (the
//! watch connection to CID 2, port 1025), and [`Guest`], the `System` port over systemd
//! and the kernel (the clock, the mounts, the lane user, Rosetta's registration).

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;

use crate::mounts::{self, Op};
use crate::serve::{self, Listener, Stream};
use crate::system::{self, Booted, Facts, Identity, RawJob, System};
use crate::wire::Share;
use crate::{COMMAND_PORT, HOST_CID, WATCH_PORT, boot, key, sys, user, watch};

/// A vsock listener that hands on only the host's connections.
pub struct VsockListener(OwnedFd);

impl VsockListener {
    /// Listen on `port`.
    ///
    /// # Errors
    /// The socket could not be made, bound or listened on.
    pub fn bind(port: u32) -> io::Result<Self> {
        sys::vsock_listen(port).map(Self)
    }
}

impl Listener for VsockListener {
    type Stream = VsockStream;

    /// The next connection from CID 2; one from anywhere else (a guest process over the
    /// local CID) is closed unanswered, so no guest process reaches root's requests.
    fn accept(&self) -> io::Result<VsockStream> {
        loop {
            let (fd, cid) = sys::vsock_accept(self.0.as_raw_fd())?;
            if cid == HOST_CID {
                return Ok(VsockStream(File::from(fd)));
            }
            eprintln!("aterm-vz-agent: refused a command connection from CID {cid}");
        }
    }
}

/// One vsock connection.
pub struct VsockStream(File);

impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for VsockStream {
    fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    fn shutdown(&self) {
        sys::shutdown_both(self.0.as_raw_fd());
    }
}

/// Dial `port` of CID `cid`.
///
/// # Errors
/// The connect failed.
pub fn dial(cid: u32, port: u32) -> io::Result<VsockStream> {
    sys::vsock_connect(cid, port).map(|fd| VsockStream(File::from(fd)))
}

/// systemd's control tool.
const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// `systemctl ARGS`: whether it exited 0, and its stdout.
fn systemctl(args: &[&str]) -> Result<(bool, String), String> {
    let out = serve::command(OsStr::new(SYSTEMCTL))
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{SYSTEMCTL} {}: {e}", args.join(" ")))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|t| t.trim().to_string())
        .unwrap_or_else(|e| format!("unreadable ({e})"))
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} holds a NUL byte", path.display()))
}

/// One step of a mount plan.
fn apply(op: &Op) -> Result<(), String> {
    match op {
        Op::Mkdir(at) => {
            std::fs::create_dir_all(at).map_err(|e| format!("mkdir {}: {e}", at.display()))
        }
        Op::MountShare => {
            let tag = CString::new(mounts::TAG).map_err(|e| e.to_string())?;
            let at = CString::new(mounts::AT).map_err(|e| e.to_string())?;
            sys::mount_virtiofs(&tag, &at)
                .map_err(|e| format!("mount the `{}` share at {}: {e}", mounts::TAG, mounts::AT))
        }
        Op::Unmount(at) => {
            sys::detach(&c_path(at)?).map_err(|e| format!("unmount {}: {e}", at.display()))
        }
        Op::Bind(from, at) => sys::bind_mount(&c_path(from)?, &c_path(at)?)
            .map_err(|e| format!("bind {} at {}: {e}", from.display(), at.display())),
        Op::ReadOnly(at) => sys::remount_read_only(&c_path(at)?)
            .map_err(|e| format!("make {} read-only: {e}", at.display())),
    }
}

/// The guest's own OS.
pub struct Guest;

impl Guest {
    fn lane_uid() -> Result<u32, String> {
        match Guest.identity()? {
            Identity::Switch { uid, .. } => Ok(uid),
            Identity::Keep => Err("no lane user".into()),
        }
    }
}

impl System for Guest {
    fn facts(&self) -> Facts {
        Facts {
            boot_id: read_trimmed("/proc/sys/kernel/random/boot_id"),
            kernel: read_trimmed("/proc/sys/kernel/osrelease"),
        }
    }

    fn uptime_ms(&self) -> u64 {
        std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|t| system::parse_uptime_ms(&t))
            .unwrap_or(0)
    }

    fn jobs(&self) -> Vec<RawJob> {
        match systemctl(&["list-jobs", "--plain", "--no-legend", "--no-pager"]) {
            Ok((_, out)) => system::parse_jobs(&out),
            Err(why) => {
                eprintln!("aterm-vz-agent: {why}");
                Vec::new()
            }
        }
    }

    fn booted(&self) -> Booted {
        let state = match systemctl(&["is-system-running", "--wait"]) {
            Ok((_, out)) => out.trim().to_string(),
            Err(why) => format!("unknown ({why})"),
        };
        let failed_units = match systemctl(&["--failed", "--plain", "--no-legend", "--no-pager"]) {
            Ok((_, out)) => system::parse_failed(&out),
            Err(_) => Vec::new(),
        };
        Booted {
            state,
            failed_units,
        }
    }

    fn user_bus(&self) -> Result<(), String> {
        let bus = format!("/run/user/{}/bus", Self::lane_uid()?);
        std::os::unix::net::UnixStream::connect(&bus)
            .map(drop)
            .map_err(|e| {
                format!("the lane user's manager does not answer at {bus} ({e}): is linger on?")
            })
    }

    fn now_unix_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    fn set_clock(&self, unix_ns: u64) -> Result<(), String> {
        sys::set_realtime(unix_ns).map_err(|e| format!("clock_settime: {e}"))
    }

    fn mount(&self, shares: &[Share]) -> Result<(), String> {
        let info = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
        for op in mounts::plan(&mounts::parse(&info), shares)? {
            apply(&op)?;
        }
        let after = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
        mounts::check(&mounts::parse(&after), shares)
    }

    fn poweroff(&self) -> Result<(), String> {
        match systemctl(&["--no-block", "poweroff"])? {
            (true, _) => Ok(()),
            (false, _) => Err(format!("{SYSTEMCTL} --no-block poweroff failed")),
        }
    }

    fn identity(&self) -> Result<Identity, String> {
        let passwd =
            std::fs::read_to_string("/etc/passwd").map_err(|e| format!("/etc/passwd: {e}"))?;
        let group =
            std::fs::read_to_string("/etc/group").map_err(|e| format!("/etc/group: {e}"))?;
        user::identity(&passwd, &group)
    }
}

/// Register Rosetta for x86_64 ELF executables unless it is: `Ok(true)` when this did.
///
/// # Errors
/// The registration was refused.
pub fn register_rosetta() -> io::Result<bool> {
    if Path::new(boot::ROSETTA_ENTRY).exists() {
        return Ok(false);
    }
    let line = boot::rosetta_registration(&format!("{}/rosetta", boot::ROSETTA_SHARE));
    std::fs::OpenOptions::new()
        .write(true)
        .open(boot::BINFMT_REGISTER)?
        .write_all(line.as_bytes())?;
    Ok(true)
}

/// The agent in the guest: its unit and Rosetta's registration set up, the command
/// listener bound, the boot's key read from the key disk (`/sys/block`'s `vd*`, [`key::find`]),
/// the watch connection dialed, then commands served until the listener fails. A failed
/// dial is a fault (the helper listens before the guest has a userspace), and so is a boot
/// with no key disk (the helper believes no hello without its key): either is written to the
/// console and the agent exits.
#[must_use]
pub fn run() -> ExitCode {
    let say = |what: &str| eprintln!("aterm-vz-agent: {what}");
    match boot::refresh_unit(Path::new(boot::UNIT_PATH), boot::UNIT) {
        Ok(true) => say(&format!("rewrote {} for the next boot", boot::UNIT_PATH)),
        Ok(false) => {}
        Err(e) => say(&format!("could not refresh {}: {e}", boot::UNIT_PATH)),
    }
    match register_rosetta() {
        Ok(true) => say("registered Rosetta for x86_64 executables"),
        Ok(false) => {}
        Err(e) => say(&format!("could not register Rosetta: {e}")),
    }
    let listener = match VsockListener::bind(COMMAND_PORT) {
        Ok(l) => l,
        Err(e) => {
            say(&format!(
                "could not listen on vsock port {COMMAND_PORT}: {e}"
            ));
            return ExitCode::FAILURE;
        }
    };
    let boot_key = match key::find(Path::new("/sys/block"), Path::new("/dev")) {
        Ok(k) => k,
        Err(why) => {
            say(&format!(
                "no boot key: {why}; the host believes no hello without it"
            ));
            return ExitCode::FAILURE;
        }
    };
    let watch_conn = match dial(HOST_CID, WATCH_PORT) {
        Ok(c) => c,
        Err(e) => {
            say(&format!(
                "could not dial the host's watch port (CID {HOST_CID}, port {WATCH_PORT}): {e}"
            ));
            return ExitCode::FAILURE;
        }
    };
    let system: Arc<dyn System> = Arc::new(Guest);
    let hello = match crate::hello(system.as_ref(), Path::new("/proc/self/exe"), boot_key) {
        Ok(h) => h,
        Err(e) => {
            say(&format!("could not hash /proc/self/exe: {e}"));
            return ExitCode::FAILURE;
        }
    };
    crate::serve_with_watch(&listener, watch_conn, &system, &hello, watch::TICK)
}

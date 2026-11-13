// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One owned, unreaped worker group, complete membership checks and bounded output capture.

use std::cell::Cell;
#[cfg(target_os = "linux")]
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::private_file;
use aterm_uds::process::{self, Identity, ProcessPresence};
use aterm_uds::spawnfd::{Child, Command};

use super::{Config, START_WITHIN, TICK, WorkerPermit, fs, live, require, write};
use crate::artifact::tools::{HostTools, Ran, Run, RunError, Tools};
use crate::error::{Error, Result};

const OUTPUT_MAX: u64 = 64 << 20;
const RETIRE_WITHIN: Duration = Duration::from_secs(60);

/// The reaped bit is explicit: no negative-PGID signal is permitted after releasing the
/// leader's number, including when post-reap emptiness remains unknown.
pub(super) struct Worker {
    child: Child,
    reaped: bool,
}
impl Worker {
    pub(super) fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }
}

pub(super) fn scrub(command: &mut Command) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
}

pub(super) fn bind(child: &Worker) -> Result<Identity> {
    let lineage = process::read_lineage(i32::try_from(child.id()).unwrap_or(0))
        .ok_or_else(|| Error::refused("worker birth/group cannot be established"))?;
    if lineage.pgid != child.id() || lineage.parent != std::process::id() {
        return Err(Error::refused(
            "worker does not lead its guardian-owned group",
        ));
    }
    Ok(lineage.identity)
}

/// Whether `group` is shown empty: two whole listings in a row with no member, as a Linux walk of
/// `/proc` is no snapshot (a fork in flight can be missed by one listing whose forker then exits;
/// `procfs::chased_group_of`), which such a relay would have to outrun twice.
pub(super) fn empty_group(group: u32) -> bool {
    (0..2).all(|_| {
        process::group_members(group)
            .is_some_and(|members| members.complete && members.pids.is_empty())
    })
}

pub(super) fn run(
    worker: &mut Worker,
    leader: &Identity,
    dir: &Path,
    config: &Config,
    token: &str,
    stopped: &dyn Fn() -> bool,
    memory_mib: u64,
) -> Result<()> {
    let child = &mut worker.child;
    let start = Instant::now();
    loop {
        if stopped() {
            return Err(Error::refused(
                "local updater stopped before its worker start gate",
            ));
        }
        if child
            .exited()
            .map_err(|e| Error::io("inspect gated worker", &e))?
        {
            return Err(Error::refused("gated worker exited before readiness"));
        }
        if let Some(ready) = fs::read::<String>(dir, "ready.json")? {
            if ready != leader.token() {
                return Err(Error::refused(
                    "worker readiness belongs to another process",
                ));
            }
            break;
        }
        if start.elapsed() >= START_WITHIN {
            return Err(Error::refused("worker readiness timed out"));
        }
        std::thread::sleep(TICK);
    }
    write(dir, "go.json", &token)?;
    let mut exit = aterm_uds::deadline::ExitWait::new(child);
    let mut unknown_since = None;
    loop {
        if stopped() {
            return Err(Error::refused(
                "local updater parent lifeline or stop signal ended its pass",
            ));
        }
        if start.elapsed().as_secs() >= config.pass_seconds {
            return Err(Error::refused(
                "local update pass exceeded its configured duration",
            ));
        }
        if let Some(reason) = fs::read::<String>(dir, "stop.json")? {
            return Err(Error::refused(reason));
        }
        if child
            .exited()
            .map_err(|e| Error::io("inspect local worker", &e))?
        {
            // Reaping is deliberately deferred to retire(). Read the worker's durable result.
            let ack: super::Ack = require(dir, "worker-result.json")?;
            return if ack.done && ack.success {
                Ok(())
            } else {
                Err(Error::refused(ack.reason))
            };
        }
        let usage = match accounting(child.id()) {
            Ok(usage) => {
                unknown_since = None;
                usage
            }
            Err(error) => {
                let since = *unknown_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(1) {
                    return Err(error);
                }
                exit.wait(TICK);
                continue;
            }
        };
        if let Some(reason) = budget_reason(usage, memory_mib, config.vm_mib) {
            return Err(Error::refused(reason));
        }
        let judged =
            private_file::JudgedDir::judge(dir).map_err(|e| Error::refused(e.to_string()))?;
        for name in ["worker.stdout", "worker.stderr"] {
            if judged
                .open(name)
                .map_err(|e| Error::refused(e.to_string()))?
                .1
                .len()
                > OUTPUT_MAX
            {
                return Err(Error::refused(
                    "local worker output exceeded its bounded log",
                ));
            }
        }
        exit.wait(TICK);
    }
}

/// Kill while the unreaped leader pins the PGID. A successful signal is never settlement.
pub(super) fn retire(child: &mut Worker) -> Result<()> {
    let group = child.id();
    let by = Instant::now() + RETIRE_WITHIN;
    loop {
        if child.reaped {
            return Err(Error::refused(
                "post-reap group emptiness remains unproved; no further signals are safe",
            ));
        }
        let _ = process::kill_group_until_empty(i32::try_from(group).unwrap_or(0));
        if finish_if_retired(child)? {
            return Ok(());
        }
        if Instant::now() >= by {
            return Err(Error::refused("worker retirement exceeded one minute"));
        }
        std::thread::sleep(TICK);
    }
}

/// Observation only after the signalling budget expires. Keep the leader unreaped until
/// every other member has positively exited; never signal by its number after that reap.
pub(super) fn finish_if_retired(child: &mut Worker) -> Result<bool> {
    let group = child.id();
    if child.reaped {
        return Ok(empty_group(group));
    }
    let Some(members) = process::group_members(group) else {
        return Ok(false);
    };
    if !members.complete
        || members.pids.iter().any(|pid| {
            *pid != group as i32
                && !matches!(
                    process::process_presence(*pid),
                    ProcessPresence::Exited | ProcessPresence::Absent
                )
        })
    {
        return Ok(false);
    }
    if !child
        .child
        .exited()
        .map_err(|e| Error::io("worker exit before reap", &e))?
    {
        return Ok(false);
    }
    child.reaped = true;
    child
        .child
        .wait()
        .map_err(|e| Error::io("reap retired worker", &e))?;
    Ok(empty_group(group))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Usage {
    memory: u64,
    virtual_max: u64,
}

fn budget_reason(usage: Usage, memory_mib: u64, vm_mib: u64) -> Option<&'static str> {
    if usage.memory > memory_mib << 20 {
        Some("local worker group exceeded its footprint memory budget")
    } else if usage.virtual_max > vm_mib << 20 {
        Some("a local worker process exceeded its virtual-memory ceiling")
    } else {
        None
    }
}

fn accounting(group: u32) -> Result<Usage> {
    let members = process::group_members(group)
        .ok_or_else(|| Error::refused("worker membership is unavailable"))?;
    if !members.complete {
        return Err(Error::refused(
            "worker membership exceeds complete accounting capacity",
        ));
    }
    let mut usage = Usage::default();
    #[cfg(target_os = "macos")]
    let footprints = aterm_sysprobe::group_usage(group)
        .ok_or_else(|| Error::refused("worker footprint accounting is unavailable"))?;
    for pid in members.pids {
        if matches!(
            process::process_presence(pid),
            ProcessPresence::Exited | ProcessPresence::Absent
        ) {
            continue;
        }
        let identity = process::read_identity(pid)
            .ok_or_else(|| Error::refused("worker member identity is unreadable"))?;
        let member = match memory(pid) {
            Some(member) => member,
            None if matches!(
                process::process_presence(pid),
                ProcessPresence::Exited | ProcessPresence::Absent
            ) =>
            {
                continue;
            }
            None => {
                return Err(Error::refused(
                    "worker memory accounting is unsupported or unreadable",
                ));
            }
        };
        #[cfg(target_os = "macos")]
        let member = Usage {
            memory: match footprints.iter().find(|entry| entry.pid == pid as u32) {
                Some(entry) => entry.footprint,
                None if matches!(
                    process::process_presence(pid),
                    ProcessPresence::Exited | ProcessPresence::Absent
                ) =>
                {
                    continue;
                }
                None => {
                    return Err(Error::refused(
                        "a live worker group member lacks a complete footprint reading",
                    ));
                }
            },
            ..member
        };
        if process::read_identity(pid).as_ref() != Some(&identity) {
            if matches!(
                process::process_presence(pid),
                ProcessPresence::Exited | ProcessPresence::Absent
            ) {
                continue;
            }
            return Err(Error::refused(
                "worker member changed while memory was sampled",
            ));
        }
        usage.memory = usage.memory.saturating_add(member.memory);
        usage.virtual_max = usage.virtual_max.max(member.virtual_max);
    }
    Ok(usage)
}

/// No new ABI mirror: the platform's existing libc SDK type supplies the VM fields absent
/// from sysprobe's footprint API. The same PID identity is checked around this read.
#[cfg(target_os = "macos")]
fn memory(pid: i32) -> Option<Usage> {
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: proc_pidinfo flavor4 writes exactly proc_taskinfo, whose SDK layout libc owns;
    // task is a live size-byte output buffer and no pointer is retained.
    let n = unsafe { libc::proc_pidinfo(pid, 4, 0, task.as_mut_ptr().cast(), size) };
    if n != size {
        return None;
    }
    // SAFETY: a whole SDK record was written above.
    let task = unsafe { task.assume_init() };
    Some(Usage {
        memory: task.pti_resident_size,
        virtual_max: task.pti_virtual_size,
    })
}

#[cfg(target_os = "linux")]
fn memory(pid: i32) -> Option<Usage> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(format!("/proc/{pid}/status"))
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::with_capacity(4096);
    file.take(64 << 10).read_to_string(&mut text).ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|word| word.parse::<u64>().ok())
            .and_then(|kib| kib.checked_mul(1024))
    };
    Some(Usage {
        memory: aterm_sysprobe::procs::footprint(u32::try_from(pid).ok()?).ok()?,
        virtual_max: field("VmSize:")?,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn memory(_: i32) -> Option<Usage> {
    None
}

pub(super) fn limits(config: &Config) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let bytes = config.vm_mib << 20;
        let limit = libc::rlimit {
            rlim_cur: bytes,
            rlim_max: bytes,
        };
        // SAFETY: RLIMIT_AS is the Linux address-space limit; limit is a live initialized
        // input record and the worker has not spawned a compiler or another thread.
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
            return Err(Error::io(
                "set inherited worker VM ceiling",
                &std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = config;
    Ok(())
}

pub(super) struct GuardTools<'a> {
    permit: &'a WorkerPermit,
    serial: Cell<u64>,
    host: HostTools,
}

impl<'a> GuardTools<'a> {
    pub(super) fn new(permit: &'a WorkerPermit) -> Self {
        Self {
            permit,
            serial: Cell::new(0),
            host: HostTools,
        }
    }
    pub(super) fn stop(&self, reason: &str) -> ! {
        if write(&self.permit.dir, "stop.json", &reason).is_err() {
            // The owner keeps this leader unreaped until every group member retires.
            // If the durable stop transport failed, our exit is its independent wake.
            process::kill_self_now();
        }
        loop {
            if !live(&self.permit.request.guardian) {
                process::kill_self_now();
            }
            std::thread::sleep(TICK);
        }
    }
}

impl Tools for GuardTools<'_> {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        if !live(&self.permit.request.guardian) {
            self.stop("guardian ended before a local child started");
        }
        let serial = self.serial.get().saturating_add(1);
        self.serial.set(serial);
        let out_name = format!("child-{serial}.stdout");
        let err_name = format!("child-{serial}.stderr");
        let output = private_file::open_private_rw(&self.permit.dir, &out_name)
            .map_err(|e| RunError::Spawn(e.to_string()))?;
        let errors = private_file::open_private_rw(&self.permit.dir, &err_name)
            .map_err(|e| RunError::Spawn(e.to_string()))?;
        let mut command = aterm_uds::spawnfd::command(&run.program);
        command
            .args(&run.args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(output)
            .stderr(errors);
        if run.clear_env {
            command.env_clear();
        }
        for key in &run.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &run.env {
            command.env(key, value);
        }
        if let Some(cwd) = &run.cwd {
            command.current_dir(cwd);
        }
        // Inherit the already owned worker group. The generic deadline runner would
        // create a separate group and reap its leader before the guardian's retirement.
        let mut child = command
            .spawn()
            .map_err(|e| RunError::Spawn(e.to_string()))?;
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let start = Instant::now();
        loop {
            if !live(&self.permit.request.guardian) {
                self.stop("guardian ended while a local child ran");
            }
            if run.deadline.is_some_and(|limit| start.elapsed() >= limit) {
                self.stop("local child exceeded its bounded operation deadline");
            }
            let judged = private_file::JudgedDir::judge(&self.permit.dir)
                .map_err(|e| RunError::AfterStart(e.to_string()))?;
            for name in [&out_name, &err_name] {
                if judged
                    .open(name)
                    .map_err(|e| RunError::AfterStart(e.to_string()))?
                    .1
                    .len()
                    > OUTPUT_MAX
                {
                    self.stop("local child output exceeded its complete capture bound");
                }
            }
            if child
                .exited()
                .map_err(|e| RunError::AfterStart(e.to_string()))?
            {
                break;
            }
            exit.wait(TICK);
        }
        let status = child
            .wait()
            .map_err(|e| RunError::AfterStart(e.to_string()))?;
        let judged = private_file::JudgedDir::judge(&self.permit.dir)
            .map_err(|e| RunError::AfterStart(e.to_string()))?;
        let read = |name: &str| {
            judged
                .read(name, OUTPUT_MAX as usize)
                .map_err(|e| RunError::AfterStart(e.to_string()))
        };
        Ok(Ran {
            code: status.code(),
            stdout: read(&out_name)?,
            stderr: read(&err_name)?,
        })
    }
    fn strip(&self, _: &Path, _: bool) -> std::result::Result<(), String> {
        Err("local harness manager does not strip artifacts".into())
    }
    fn resign(&self, _: &Path) -> std::result::Result<(), String> {
        Err("local harness manager does not sign artifacts".into())
    }
    fn same_unsigned(&self, _: &Path, _: &Path) -> std::result::Result<bool, String> {
        Err("local harness manager does not rewrite signed artifacts".into())
    }
    fn thin(&self, _: &Path, _: &str, _: &Path) -> std::result::Result<(), String> {
        Err("local harness manager requires native artifacts".into())
    }
    fn relocate(
        &self,
        _: &Path,
        _: &[PathBuf],
    ) -> std::result::Result<crate::artifact::relocate::Report, String> {
        Err("local harness manager does not relocate artifacts".into())
    }
    fn compressor(&self) -> &dyn crate::artifact::tarwrite::Compressor {
        self.host.compressor()
    }
}

#[cfg(test)]
#[path = "harness_local_guardian_process_tests.rs"]
mod tests;

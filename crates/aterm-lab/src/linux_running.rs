// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Commands in an existing owned Lima boot, without bring-up or end-of-run VM actions.
//! Arbitrary commands are neither read-only nor output-bounded; callers choose their command.

use super::*;
use crate::vm_admit::Booter;

type Agent = (u32, (u64, u64));

struct Facts<'a> {
    vm: Option<&'a VmState>,
    boot: Option<&'a Booter>,
    agent: Option<Agent>,
    booter_alive: bool,
}

fn in_running(
    facts: Facts<'_>,
    repo: &str,
    mounts: &[String],
    run: impl FnOnce() -> Out,
) -> Result<Out, Fail> {
    let vm = facts
        .vm
        .ok_or_else(|| not_run("exec-running: no existing VM; nothing initialized"))?;
    if vm.status != "Running" {
        return Err(not_run(format!(
            "exec-running: VM is {}; nothing booted",
            vm.status
        )));
    }
    if !mounted_exactly(vm, mounts) {
        return Err(not_run(
            "exec-running: existing VM mounts differ; nothing remounted",
        ));
    }
    let boot = facts
        .boot
        .ok_or_else(|| not_run("exec-running: original boot receipt unavailable"))?;
    if boot.checkout != repo || (!boot.kept && !facts.booter_alive) {
        return Err(not_run(
            "exec-running: boot belongs to another checkout or a dead one-shot run; retained",
        ));
    }
    let recorded = boot.vm_pid.zip(boot.vm_birth).filter(|(pid, _)| *pid > 1);
    if recorded.is_none() || facts.agent != recorded {
        return Err(not_run(
            "exec-running: original VM agent identity is changed or unread; retained",
        ));
    }
    Ok(run())
}

fn agent(dir: &Path) -> Option<Agent> {
    let pid = lima_agent(dir)?;
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    Some((pid, (birth.seconds, birth.microseconds)))
}

fn arguments(args: &[String]) -> Result<&[String], Fail> {
    let args = args.strip_prefix(&["--".into()]).unwrap_or(args);
    if args.is_empty() {
        return Err(Fail::usage(
            "linux exec-running: expected a literal command after --",
        ));
    }
    Ok(args)
}

pub(super) fn cmd(args: &[String]) -> Res {
    let args = arguments(args)?;
    if crate::linux_driver::kind()? != Kind::Lima || crate::linux_gpu::on() {
        return Err(not_run(
            "exec-running requires the existing non-GPU Lima lane; no other driver was contacted",
        ));
    }
    let lima = Lima::find_existing()?;
    let lock = lima.owned.lock_path();
    let mut lane = LaneLock::at(&lock, "linux")?;
    let vm = lima.state().map_err(not_run)?;
    if let Some(vm) = &vm {
        lima.owned.admit_vm(&vm.dir).map_err(not_run)?;
    }
    let boot = Booter::read(&lock.with_file_name(format!(".{}.booter", lima.vm)));
    let repo = repo();
    let mounts = lane_mounts(&repo);
    let current = vm.as_ref().and_then(|vm| agent(&vm.dir));
    let out = in_running(
        Facts {
            vm: vm.as_ref(),
            boot: boot.as_ref(),
            agent: current,
            booter_alive: boot.as_ref().is_some_and(Booter::alive),
        },
        &repo,
        &mounts,
        || {
            // The existing lane lock excludes other supported boot/remount/stop operations.
            // No Up is constructed, so neither its initialization nor its cleanup can run.
            if let Some(vm) = &vm {
                lane.credit(&lima_procs(&vm.dir));
            }
            lima.shell_streamed("/", args, "")
        },
    )?;
    exec_exit(&out, &mut |words| eprintln!("aterm-lab: {words}"))
}

#[cfg(test)]
#[path = "linux_running_tests.rs"]
mod tests;

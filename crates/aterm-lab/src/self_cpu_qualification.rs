// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guest's raw production-reader evidence, checked again by the host's production model.

use aterm_vitals::self_cpu as model;
use serde::{Deserialize, Serialize};

pub const WORKER: &str = "aterm-self-cpu";
pub const WORK_NS: u64 = 5_000_000_000;
pub const MAX_CHILD_OUTPUT: u64 = 1024 * 1024;
pub const TERMINAL_SOURCE: &str = "wait4_saved_exit_rusage";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Counter {
    pub ns: u64,
    pub precision_ns: u64,
}

impl Counter {
    fn model(&self) -> model::OwnCpuCounter {
        model::OwnCpuCounter {
            ns: self.ns,
            precision_ns: self.precision_ns,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: u64,
    pub name: Option<String>,
    pub cpu: Option<Counter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cut {
    pub pid: u32,
    pub start: u64,
    pub version: i32,
    pub begin_ns: u64,
    pub end_ns: u64,
    pub process_before: Counter,
    pub process_after: Counter,
    pub threads: Vec<Thread>,
    pub listed: usize,
    pub inspected: usize,
    pub unread: usize,
    pub capped: bool,
}

impl From<aterm_sysprobe::self_cpu::Cut> for Cut {
    fn from(cut: aterm_sysprobe::self_cpu::Cut) -> Self {
        let counter = |c: aterm_sysprobe::self_cpu::CpuCounter| Counter {
            ns: c.ns,
            precision_ns: c.precision_ns,
        };
        Self {
            pid: cut.process.pid,
            start: cut.process.start,
            version: cut.process.version,
            begin_ns: cut.begin_ns,
            end_ns: cut.end_ns,
            process_before: counter(cut.process_before),
            process_after: counter(cut.process_after),
            threads: cut
                .threads
                .into_iter()
                .map(|t| Thread {
                    id: t.id,
                    name: t.name,
                    cpu: t.cpu.map(counter),
                })
                .collect(),
            listed: cut.coverage.listed,
            inspected: cut.coverage.inspected,
            unread: cut.coverage.unread,
            capped: cut.coverage.capped,
        }
    }
}

impl Cut {
    fn model(&self) -> model::OwnCpuCut {
        model::OwnCpuCut {
            process: model::OwnCpuProcess {
                pid: self.pid,
                start: self.start,
                version: self.version,
            },
            begin_ns: self.begin_ns,
            end_ns: self.end_ns,
            process_before: self.process_before.model(),
            process_after: self.process_after.model(),
            threads: self
                .threads
                .iter()
                .map(|t| model::OwnCpuThread {
                    id: t.id,
                    name: t.name.clone(),
                    cpu: t.cpu.as_ref().map(Counter::model),
                })
                .collect(),
            coverage: model::OwnCpuCoverage {
                listed: self.listed,
                inspected: self.inspected,
                unread: self.unread,
                capped: self.capped,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound {
    pub worker_published_ns: u64,
    pub exit_accounted_ns: u64,
    pub exit_accounted_upper_ns: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildProof {
    pub schema: u32,
    pub kernel_vm: bool,
    pub worker: String,
    pub work_wall_ns: u64,
    pub work_iterations: u64,
    pub before: Cut,
    pub after: Cut,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalOracle {
    pub source: String,
    pub pid: u32,
    pub exit_code: Option<i32>,
    pub user_ns: u64,
    pub system_ns: u64,
    pub timebase_numer: u32,
    pub timebase_denom: u32,
    pub rounding_upper_ns: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proof {
    pub schema: u32,
    pub kernel_vm: bool,
    pub terminal: Option<TerminalOracle>,
    pub child_output_complete: bool,
    pub child_stdout: Vec<u8>,
    pub child_stderr: Vec<u8>,
    pub bound: Option<Bound>,
    pub qualification_error: Option<String>,
}

/// The kernel observation alone authorizes the workload; no argument or environment bypass.
pub fn require_guest(present: Option<bool>) -> Result<(), &'static str> {
    match present {
        Some(true) => Ok(()),
        Some(false) => Err("physical_host_refused"),
        None => Err("kernel_vm_evidence_unavailable"),
    }
}

fn observed(before: &Cut, after: &Cut) -> Result<(), String> {
    let before = before.model();
    let after = after.model();
    let mut state = model::OwnCpuState::default();
    if state.absorb(Some(Ok(&before))) != model::OwnCpuView::Warming {
        return Err("the first production-reader cut did not establish a baseline".into());
    }
    let model::OwnCpuView::Observed(_) = state.absorb(Some(Ok(&after))) else {
        return Err("the production model could not pair both counter observations".into());
    };
    Ok(())
}

/// XNU f6217f89 `kern_resource.c::calcru` converts task totals separately through
/// `absolutetime_to_microtime`; the ARM divisors are exact for an integer-MHz timebase.
/// This bounds representation of the saved exit-accounting snapshot, not final teardown CPU.
pub fn terminal_rounding(numer: u32, denom: u32) -> Option<u64> {
    (numer > 0 && denom > 0 && (1000 * u128::from(denom)) % u128::from(numer) == 0).then_some(2000)
}

fn worker(cut: &Cut) -> Result<&Thread, String> {
    let mut found = cut
        .threads
        .iter()
        .filter(|t| t.name.as_deref() == Some(WORKER));
    let one = found.next().ok_or("missing named worker")?;
    if found.next().is_some() {
        return Err("ambiguous named worker".into());
    }
    Ok(one)
}

pub fn exit_accounting(proof: &Proof) -> Result<Bound, String> {
    require_guest(Some(proof.kernel_vm)).map_err(str::to_owned)?;
    if proof.schema != 2
        || !proof.child_output_complete
        || proof.child_stdout.len() as u64 > MAX_CHILD_OUTPUT
        || proof.child_stderr.len() as u64 > MAX_CHILD_OUTPUT
    {
        return Err("missing complete guest-child output".into());
    }
    let child: ChildProof =
        aterm_json::from_slice(&proof.child_stdout).map_err(|e| e.to_string())?;
    require_guest(Some(child.kernel_vm)).map_err(str::to_owned)?;
    if child.schema != 2
        || child.worker != WORKER
        || child.work_wall_ns < WORK_NS
        || child.work_iterations == 0
    {
        return Err("missing guest, workload or protocol evidence".into());
    }
    if child
        .after
        .begin_ns
        .checked_sub(child.before.end_ns)
        .is_none_or(|inner_ns| inner_ns < child.work_wall_ns)
    {
        return Err("the cuts do not enclose the declared workload duration".into());
    }
    observed(&child.before, &child.after)?;
    let terminal = proof
        .terminal
        .as_ref()
        .ok_or("missing owning terminal accounting")?;
    let rounding = terminal_rounding(terminal.timebase_numer, terminal.timebase_denom)
        .ok_or("terminal-accounting conversion is unqualified")?;
    if terminal.source != TERMINAL_SOURCE
        || terminal.exit_code != Some(0)
        || terminal.pid != child.after.pid
        || terminal.rounding_upper_ns != rounding
        || terminal.user_ns % 1000 != 0
        || terminal.system_ns % 1000 != 0
    {
        return Err("terminal accounting does not identify this successful child".into());
    }
    let before = worker(&child.before)?;
    let after = worker(&child.after)?;
    let first = before
        .cpu
        .as_ref()
        .ok_or("missing initial worker counter")?;
    let last = after.cpu.as_ref().ok_or("missing final worker counter")?;
    if before.id != after.id || last.ns < first.ns {
        return Err("the worker identity or cumulative counter changed".into());
    }
    let reported = terminal
        .user_ns
        .checked_add(terminal.system_ns)
        .ok_or("exit CPU overflow")?;
    let upper = reported
        .checked_add(rounding)
        .ok_or("exit CPU upper overflow")?;
    if reported == 0
        || last.ns == 0
        || last.ns > upper
        || u128::from(last.ns) * 5 < u128::from(upper) * 4
    {
        return Err("the worker did not prove 80% of saved exit accounted CPU".into());
    }
    Ok(Bound {
        worker_published_ns: last.ns,
        exit_accounted_ns: reported,
        exit_accounted_upper_ns: upper,
    })
}

pub fn validate(proof: &Proof) -> Result<Bound, String> {
    if proof.qualification_error.is_some() {
        return Err("the guest qualification reported an error".into());
    }
    let actual = exit_accounting(proof)?;
    if proof.bound.as_ref() != Some(&actual) {
        return Err("declared exit-accounting bound differs from raw evidence".into());
    }
    Ok(actual)
}

#[cfg(test)]
#[path = "self_cpu_qualification_tests.rs"]
pub(crate) mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared instance's functional waits and checked retirement.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use aterm_uds::deadline::{ChildOutput, ProcessProgressProbe, ProgressWindow};
use aterm_uds::process::{Identity, ProcessPresence, process_presence, read_identity};
use aterm_uds::spawnfd;

use super::{Budget, CouldNotRun, MUST_HAPPEN, POLL_GAP, Progress, WORK_TICK, working};

/// Control replies contain metadata/text, never the PNG itself. Overflow refuses the whole
/// response instead of retaining a suffix that could look like a complete authority line.
const OUTPUT_LIMIT: usize = 256 * 1024;

pub(super) fn await_event<T>(
    budget: Option<Budget>,
    pid: u32,
    subject: Option<Identity>,
    what: &str,
    probe: impl FnMut() -> Option<T>,
) -> Result<T, CouldNotRun> {
    let mut tree = ProcessProgressProbe::default();
    working(
        budget,
        what,
        POLL_GAP,
        Progress {
            now: &mut Instant::now,
            runs: &mut || {
                if subject.is_some_and(|original| {
                    read_identity(original.pid).is_some_and(|now| now != original)
                }) {
                    return false;
                }
                !matches!(
                    process_presence(pid as i32),
                    ProcessPresence::Absent | ProcessPresence::Exited
                )
            },
            read: &mut || tree.read(pid),
            pause: &mut std::thread::sleep,
        },
        probe,
    )
}

#[derive(Default)]
struct Captured {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    bytes: u64,
}

impl Captured {
    fn append(&mut self, stream: ChildOutput, bytes: &[u8]) -> std::io::Result<()> {
        let target = match stream {
            ChildOutput::Stdout => &mut self.stdout,
            ChildOutput::Stderr => &mut self.stderr,
        };
        if target
            .len()
            .checked_add(bytes.len())
            .is_none_or(|len| len > OUTPUT_LIMIT)
        {
            return Err(std::io::Error::other(
                "functional control output exceeds its complete capture limit",
            ));
        }
        target.extend_from_slice(bytes);
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }
}

struct Work {
    subject: Identity,
    gui: ProcessProgressProbe,
    client: ProcessProgressProbe,
    seen: Option<(u64, u64)>,
    generation: u64,
}

impl Work {
    fn new(subject: Identity) -> Self {
        Self {
            subject,
            gui: ProcessProgressProbe::default(),
            client: ProcessProgressProbe::default(),
            seen: None,
            generation: 0,
        }
    }

    fn read(&mut self, client: Option<u32>, exited: bool) -> Option<u64> {
        // Once the actual client exited, only drained bytes can explain a still-open pipe.
        // Unrelated future GUI work must not keep an inherited writer alive indefinitely.
        if exited {
            return Some(0);
        }
        let gui = match read_identity(self.subject.pid) {
            Some(now) if now == self.subject => self.gui.read(self.subject.pid as u32),
            Some(_) => Some(0),
            None if matches!(
                process_presence(self.subject.pid),
                ProcessPresence::Absent | ProcessPresence::Exited
            ) =>
            {
                Some(0)
            }
            None => None,
        }?;
        let client = match client {
            Some(pid) => self.client.read(pid),
            None => Some(0),
        }?;
        let pair = (gui, client);
        if self.seen != Some(pair) {
            self.seen = Some(pair);
            self.generation = self.generation.saturating_add(1);
        }
        Some(self.generation)
    }
}

pub(super) fn output(
    command: &mut spawnfd::Command,
    subject: Identity,
) -> Result<std::process::Output, CouldNotRun> {
    if read_identity(subject.pid) != Some(subject) {
        return Err(CouldNotRun::Refused(
            "the functional subject's original identity is no longer proven".into(),
        ));
    }
    let captured = RefCell::new(Captured::default());
    let client = Cell::new(None);
    let exited = Cell::new(false);
    let mut work = Work::new(subject);
    let mut progress = ProgressWindow::new(MUST_HAPPEN, Instant::now());
    let mut checked = None;
    let mut stalled = false;
    let status = aterm_uds::deadline::stream_until_stopped_observing_exit(
        command,
        None,
        |pid| client.set(Some(pid)),
        |stream, bytes| captured.borrow_mut().append(stream, bytes),
        || {
            if checked.is_some_and(|at| Instant::now().saturating_duration_since(at) < WORK_TICK) {
                return false;
            }
            // Native observations may themselves take time. Date the evidence afterwards;
            // only this sample tick observes ProgressWindow, so its reading is always fresh.
            let reading = work.read(client.get(), exited.get());
            let now = Instant::now();
            checked = Some(now);
            stalled = progress
                .observe(now, captured.borrow().bytes, || reading)
                .is_zero();
            stalled
        },
        |_| exited.set(true),
    )
    .map_err(|error| CouldNotRun::Refused(format!("functional control capture: {error}")))?;
    let status = status.ok_or_else(|| CouldNotRun::Refused(if stalled {
        format!("the control client and owned subject made no CPU/I/O or output progress for {MUST_HAPPEN:?}")
    } else { "functional control collection was cancelled".into() }))?;
    let captured = captured.into_inner();
    for bytes in [&captured.stdout, &captured.stderr] {
        std::str::from_utf8(bytes)
            .map_err(|_| CouldNotRun::Refused("functional control output is not UTF-8".into()))?;
    }
    Ok(std::process::Output {
        status,
        stdout: captured.stdout,
        stderr: captured.stderr,
    })
}

#[derive(Default)]
struct Inventory {
    helpers: Vec<Identity>,
    failures: Vec<String>,
}

impl Inventory {
    fn refused(why: impl Into<String>) -> Self {
        Self {
            helpers: Vec::new(),
            failures: vec![why.into()],
        }
    }
}

fn inventory(pid: u32) -> Inventory {
    #[cfg(target_vendor = "apple")]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return Inventory::refused("instance PID does not fit the native reader");
        };
        let Some(children) = aterm_uds::process::child_pids(pid) else {
            return Inventory::refused("the original instance's helper inventory is unreadable");
        };
        let full = children.len() >= aterm_uds::process::CHILD_PIDS_MAX;
        let mut inventory = original_children(pid, children, aterm_uds::process::read_lineage);
        if full {
            inventory
                .failures
                .push("the original instance's helper inventory fills its native bound".into());
        }
        inventory
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        Inventory::refused("checked native helper inventory is unavailable on this platform")
    }
}

#[cfg(any(test, target_vendor = "apple"))]
fn original_children(
    parent: i32,
    children: Vec<i32>,
    mut read: impl FnMut(i32) -> Option<aterm_uds::process::Lineage>,
) -> Inventory {
    let mut inventory = Inventory::default();
    for pid in children {
        // The list alone does not hold a child's PID. Its identity and still-owned parent
        // must come from one native observation, before the original parent is reaped.
        match read(pid).filter(|line| line.identity.pid == pid && line.parent == parent as u32) {
            Some(line) => inventory.helpers.push(line.identity),
            None => inventory.failures.push(format!(
                "the enumerated helper {pid} has no proven identity under the original instance"
            )),
        }
    }
    inventory
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retirement {
    Gone,
    Present,
    Exited,
}

fn retirement(original: Identity) -> Result<Retirement, CouldNotRun> {
    retirement_with(original, read_identity, process_presence)
}

fn retirement_with(
    original: Identity,
    mut identity: impl FnMut(i32) -> Option<Identity>,
    mut presence: impl FnMut(i32) -> ProcessPresence,
) -> Result<Retirement, CouldNotRun> {
    let now = identity(original.pid);
    if now.is_some_and(|now| now.birth != original.birth) {
        return Ok(Retirement::Gone);
    }
    // A helper can exit during even a successful identity read. Fresh native presence
    // decides whether to wait without signalling; an earlier presence read cannot outlive it.
    match presence(original.pid) {
        ProcessPresence::Absent => Ok(Retirement::Gone),
        ProcessPresence::Exited => Ok(Retirement::Exited),
        ProcessPresence::Present | ProcessPresence::Unknown => {
            if now.is_some() {
                Ok(Retirement::Present)
            } else {
                Err(CouldNotRun::Refused(format!(
                    "the retirement of helper {} is unreadable",
                    original.token()
                )))
            }
        }
    }
}

fn begin_retirement(
    original: Identity,
    mut observe: impl FnMut() -> Result<Retirement, CouldNotRun>,
    end: impl FnOnce() -> Result<(), aterm_uds::process::EndRefusal>,
) -> Result<Retirement, CouldNotRun> {
    match observe()? {
        Retirement::Gone => return Ok(Retirement::Gone),
        // An exiting process runs nothing again, but its PID is still held. Wait for
        // positive absence without attempting to signal an identity we cannot read.
        Retirement::Exited => return Ok(Retirement::Exited),
        Retirement::Present => {}
    }
    if let Err(why) = end() {
        match observe()? {
            Retirement::Gone => return Ok(Retirement::Gone),
            Retirement::Exited => return Ok(Retirement::Exited),
            Retirement::Present => {}
        }
        return Err(CouldNotRun::Refused(format!(
            "helper {} could not be retired: {why:?}",
            original.token()
        )));
    }
    Ok(Retirement::Present)
}

fn retire(original: Identity) -> Result<(), CouldNotRun> {
    if begin_retirement(
        original,
        || retirement(original),
        || {
            aterm_uds::process::end_proven(
                original,
                super::HELPER_GRACE,
                &mut aterm_uds::process::EndControlLive,
                // The pre-stop inventory proved this exact identity was a direct child of our
                // still-unreaped instance. That provenance survives its own PTY session and later
                // adoption. It authorizes this individual only, never its numeric process group.
                |id| *id == original,
            )
            .map(|_| ())
        },
    )? == Retirement::Gone
    {
        return Ok(());
    }
    let mut tree = ProcessProgressProbe::default();
    working(
        None,
        "the enumerated helper's retirement",
        POLL_GAP,
        Progress {
            now: &mut Instant::now,
            runs: &mut || true,
            read: &mut || {
                if matches!(process_presence(original.pid), ProcessPresence::Exited) {
                    Some(0)
                } else {
                    tree.read(original.pid as u32)
                }
            },
            pause: &mut std::thread::sleep,
        },
        || match retirement(original) {
            Ok(Retirement::Present | Retirement::Exited) => None,
            Ok(Retirement::Gone) => Some(Ok(())),
            Err(error) => Some(Err(error)),
        },
    )?
}

pub(super) fn finish(child: &mut spawnfd::Child) -> Result<usize, CouldNotRun> {
    finish_with(child, inventory, retire)
}

fn finish_with(
    child: &mut spawnfd::Child,
    list: impl FnOnce(u32) -> Inventory,
    mut retire: impl FnMut(Identity) -> Result<(), CouldNotRun>,
) -> Result<usize, CouldNotRun> {
    let inventory = if child.owns_unreaped() {
        match child.exited() {
            Ok(false) => list(child.id()),
            Ok(true) => {
                Inventory::refused("the instance exited before its helper inventory was taken")
            }
            Err(error) => Inventory::refused(format!(
                "the original instance's state is unreadable: {error}"
            )),
        }
    } else {
        Inventory::refused("the original instance is no longer held unreaped")
    };
    // No work deadline or handed-off handle substitutes for this owning terminal wait.
    aterm_uds::deadline::settle_child(child);
    if !child.is_reaped() {
        return Err(CouldNotRun::Refused(
            "the original instance supplied no owning reap receipt".into(),
        ));
    }
    let Inventory {
        helpers,
        mut failures,
    } = inventory;
    for helper in &helpers {
        if let Err(error) = retire(*helper) {
            // One unproven retirement must not abandon the other proven original helpers.
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        return Err(CouldNotRun::Refused(failures.join("; ")));
    }
    Ok(helpers.len())
}

#[cfg(test)]
#[path = "instance_functional_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Controlled native wire actors; genuine Targo qualification is the explicit
//! `epoch_probe --targo PATH` development example, never an unavailable green test.

use std::{
    fs::{self, File, OpenOptions},
    io::Write as _,
    num::NonZeroU32,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{DirBuilderExt as _, OpenOptionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
use aterm_uds::spawnfd::{self, Child, Command, Stdio};

use crate::{
    build_cpu::{Owner, RETAINED_DIRECTORY_NAME, RetainedLease, RetainedPool, RetainedSlotState},
    epoch_controller::{Config, EpochController},
    epoch_endpoint::{fail_next_sync, fail_next_unlink},
    leadership::Fence,
    ledger::CohortFixture,
};

const EPOCH: &str = "0123456789abcdef0123456789abcdef";

fn owner() -> Owner {
    Owner::current(None)
}

fn mkfifo(path: &Path) {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is one live NUL-terminated owned fixture pathname.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

fn private_dir(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

struct Fixture {
    cohort: CohortFixture,
    pool: RetainedPool,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_units(2)
    }

    fn with_units(units: u32) -> Self {
        let cohort = CohortFixture::new();
        let root = cohort
            .namespace
            .backend()
            .parent()
            .unwrap()
            .join("epoch-fixture.noindex");
        private_dir(&root);
        let pool = RetainedPool::create_private(
            &root.join("cpu.noindex"),
            NonZeroU32::new(units).unwrap(),
        )
        .unwrap();
        Self { cohort, pool, root }
    }

    fn capabilities(&self) -> Vec<RetainedLease> {
        (0..self.pool.slots().get())
            .map(|_| self.pool.try_acquire(owner()).unwrap().unwrap())
            .collect()
    }

    fn controller(&self, fence: &Fence, actor: &Actor) -> EpochController {
        EpochController::new(
            fence,
            self.cohort.claim(),
            self.capabilities(),
            Config {
                helper: actor.program.clone(),
                directory: actor.directory.clone(),
                epoch: EPOCH.to_owned(),
                roots: self.pool.slots().get(),
            },
        )
        .unwrap()
    }
}

// `cat` is a compiled signed stand-in, not a shell wrapper. The Rust fixture
// feeds its private file stream; production still consumes ONLY the actual
// admitted child's stdout. Its stdin remains the real controller request pipe.
struct Actor {
    program: PathBuf,
    directory: PathBuf,
    stream: PathBuf,
    _reader: File,
    writer: Option<File>,
    units: u32,
}

impl Actor {
    fn new(fixture: &Fixture) -> Self {
        let program = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let directory = fixture.root.join("transport.noindex");
        let stream = fixture.root.join("actor.fifo");
        mkfifo(&stream);
        let reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&stream)
            .unwrap();
        let writer = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&stream)
            .unwrap();
        Self {
            program,
            directory,
            stream,
            _reader: reader,
            writer: Some(writer),
            units: fixture.pool.slots().get(),
        }
    }

    fn command(&self) -> Command {
        let mut command = spawnfd::command(&self.program);
        command.arg(&self.stream).stderr(Stdio::null());
        command
    }

    fn frame(&mut self, frame: aterm_json::Value) {
        let writer = self.writer.as_mut().unwrap();
        writeln!(writer, "{}", aterm_json::to_string(&frame).unwrap()).unwrap();
        writer.flush().unwrap();
    }

    fn opened(&mut self) {
        private_dir(&self.directory);
        mkfifo(&self.fifo());
        self.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"opened","fifo":self.fifo(),"backing":self.units,"roots":self.units,"published":0}));
    }

    fn fifo(&self) -> PathBuf {
        self.directory.join("credits.fifo")
    }

    fn retired(&mut self) {
        self.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"closing"}));
        self.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"retired","fifo":self.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens"}));
        self.end();
    }

    fn end(&mut self) {
        self.writer.take();
    }
}

struct Member {
    child: Child,
    input: Option<std::process::ChildStdin>,
}

impl Member {
    fn new(mut child: Child) -> Self {
        let input = Some(aterm_uds::deadline::cat_echoes(&mut child));
        Self { child, input }
    }

    fn end(&mut self) {
        self.input.take();
        assert!(
            until_within(MUST_HAPPEN, "actual member exit", || self
                .child
                .try_wait()
                .unwrap())
            .success()
        );
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.input.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = within(MUST_HAPPEN, Duration::from_millis(10), || {
            self.child.try_wait().ok().flatten()
        });
    }
}

fn opened(controller: &mut EpochController, fence: &Fence) {
    until_within(MUST_HAPPEN, "actual stand-in Opened frame", || {
        let status = controller.poll(fence).unwrap();
        status.opened.then_some(())
    });
}

fn retired(controller: &mut EpochController, fence: &Fence) {
    until_within(
        MUST_HAPPEN,
        "actual retirement, EOF and helper reap",
        || {
            let status = controller.poll(fence).unwrap();
            (status.retired && status.control_eof && status.helper_exited).then_some(())
        },
    );
}

fn close(controller: &mut EpochController, fence: &Fence) {
    controller.close(fence).unwrap();
    // The actor replies only after the real bounded wire has had its turn to
    // flush the controller's request, rather than racing an unsent queue entry.
    controller.poll(fence).unwrap();
}

fn finish(controller: &mut EpochController, fence: &Fence) {
    until_within(
        MUST_HAPPEN,
        "original controller settlement",
        || match controller.try_finish(fence) {
            Ok(()) => Some(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("controller settlement failed: {error}"),
        },
    );
    assert!(controller.is_settled());
}

fn denied(pool: &RetainedPool) {
    assert!(pool.try_acquire(owner()).unwrap().is_none());
    assert!(
        pool.snapshot()
            .unwrap()
            .iter()
            .all(|state| !matches!(state, RetainedSlotState::Available { .. }))
    );
}

fn debt_after_locks_release(pool: &RetainedPool) {
    for slot in 0..pool.slots().get() {
        let path = pool
            .path()
            .join(RETAINED_DIRECTORY_NAME)
            .join(format!("slot-{slot:08x}"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        aterm_uds::file_lock::lock_exclusive_until(
            &file,
            Instant::now() + MUST_HAPPEN,
            "actual CPU descriptor release",
        )
        .unwrap();
        file.unlock().unwrap();
        drop(file);
    }
    let fresh = RetainedPool::open_existing(pool.path()).unwrap().unwrap();
    denied(&fresh);
    assert!(
        fresh
            .snapshot()
            .unwrap()
            .iter()
            .all(|state| matches!(state, RetainedSlotState::Retained { .. }))
    );
}

fn launch(fixture: &Fixture, actor: &mut Actor, fence: &Fence) -> EpochController {
    let mut controller = fixture.controller(fence, actor);
    controller
        .launch_for_test(fence, &mut actor.command())
        .unwrap();
    actor.opened();
    opened(&mut controller, fence);
    controller
}

#[test]
fn retirement_and_helper_exit_keep_all_cpu_until_the_actual_group_member_drains() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let mut command = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/cat")));
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut member = Member::new(
        controller
            .launch_member_for_test(&fence, &mut command)
            .unwrap(),
    );
    let lineage = aterm_uds::process::read_lineage(member.child.id() as i32).unwrap();
    assert_eq!(lineage.pgid, controller.helper_id().unwrap());
    close(&mut controller, &fence);
    actor.retired();
    retired(&mut controller, &fence);
    assert!(!member.child.exited().unwrap());
    assert!(controller.try_finish(&fence).is_err());
    assert!(actor.fifo().exists());
    denied(&fixture.pool);
    member.end();
    finish(&mut controller, &fence);
    assert!(!actor.fifo().exists());
    assert_eq!(controller.status().settled_units, 2);
    let restored = fixture.capabilities();
    denied(&fixture.pool);
    for capability in restored {
        capability.settle().unwrap();
    }
}

#[test]
fn missing_foreign_or_malformed_retirement_never_releases_cpu() {
    for mode in ["missing", "foreign", "wrong-count", "wrong-node"] {
        let fixture = Fixture::new();
        let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
        let mut actor = Actor::new(&fixture);
        let mut controller = launch(&fixture, &mut actor, &fence);
        close(&mut controller, &fence);
        actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"closing"}));
        match mode {
            "missing" => {}
            "foreign" => actor.frame(aterm_json::json!({"v":2,"epoch":"ffffffffffffffffffffffffffffffff","event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens"})),
            "wrong-count" => actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":1,"scope":"owned-roots-and-explicit-tokens"})),
            "wrong-node" => actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"retired","fifo":fixture.root.join("foreign.fifo"),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens"})),
            _ => unreachable!(),
        }
        actor.end();
        until_within(
            MUST_HAPPEN,
            "rejected actual private helper output",
            || match controller.poll(&fence) {
                Err(_) => Some(()),
                Ok(status) if status.control_eof && status.helper_exited => Some(()),
                Ok(_) => None,
            },
        );
        assert!(
            controller.try_finish(&fence).is_err(),
            "accepted {mode} retirement"
        );
        denied(&fixture.pool);
        assert!(actor.fifo().exists());
        drop(controller);
        debt_after_locks_release(&fixture.pool);
    }
}

#[test]
fn replacing_the_original_node_refuses_cleanup_and_cpu_settlement() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    close(&mut controller, &fence);
    actor.retired();
    retired(&mut controller, &fence);
    fs::rename(actor.fifo(), actor.directory.join("original.fifo")).unwrap();
    mkfifo(&actor.fifo());
    assert!(controller.try_finish(&fence).is_err());
    assert_eq!(controller.status().settled_units, 0);
    denied(&fixture.pool);
    assert!(actor.fifo().exists());
    drop(controller);
    debt_after_locks_release(&fixture.pool);
}

#[test]
fn unlink_and_post_unlink_sync_failures_hold_all_cpu_and_retry_original_cleanup() {
    for after_unlink in [false, true] {
        let fixture = Fixture::new();
        let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
        let mut actor = Actor::new(&fixture);
        let mut controller = launch(&fixture, &mut actor, &fence);
        close(&mut controller, &fence);
        actor.retired();
        retired(&mut controller, &fence);
        if after_unlink {
            fail_next_sync();
        } else {
            fail_next_unlink();
        }
        assert!(controller.try_finish(&fence).is_err());
        assert_eq!(actor.fifo().exists(), !after_unlink);
        assert!(!controller.status().endpoint_cleaned);
        assert_eq!(controller.status().settled_units, 0);
        denied(&fixture.pool);
        finish(&mut controller, &fence);
        assert!(!actor.fifo().exists());
        let restored = fixture.capabilities();
        for capability in restored {
            capability.settle().unwrap();
        }
    }
}

#[test]
fn a_consumed_second_capability_error_never_reports_all_cpu_restored() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    close(&mut controller, &fence);
    actor.retired();
    retired(&mut controller, &fence);
    assert!(controller.try_finish(&fence).is_err());
    assert_eq!(controller.status().settled_units, 1);
    let states = fixture.pool.snapshot().unwrap();
    let pending = states
        .iter()
        .find_map(|state| match state {
            RetainedSlotState::Held { slot, .. } | RetainedSlotState::Retained { slot, .. } => {
                Some(*slot)
            }
            RetainedSlotState::Available { .. } => None,
        })
        .unwrap();
    let path = fixture
        .pool
        .path()
        .join(RETAINED_DIRECTORY_NAME)
        .join(format!("slot-{pending:08x}"));
    let mut bytes = fs::read(&path).unwrap();
    let nonce = bytes.windows(6).position(|part| part == b"nonce=").unwrap() + 6;
    bytes[nonce] = if bytes[nonce] == b'0' { b'1' } else { b'0' };
    fs::write(path, bytes).unwrap();
    assert!(controller.try_finish(&fence).is_err());
    assert_eq!(controller.status().settled_units, 1);
    assert_eq!(controller.status().uncertain_units, 1);
    assert!(!controller.is_settled());
    assert!(controller.try_finish(&fence).is_err());
    let available = fixture.pool.try_acquire(owner()).unwrap().unwrap();
    assert!(fixture.pool.try_acquire(owner()).unwrap().is_none());
    available.settle().unwrap();
}

#[test]
fn dropping_the_owner_keeps_debt_after_every_cpu_descriptor_is_released() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let controller = launch(&fixture, &mut actor, &fence);
    actor.end();
    drop(controller);
    debt_after_locks_release(&fixture.pool);
}

#[test]
fn valid_private_output_beyond_one_frame_budget_reaches_retired_before_eof_validation() {
    let fixture = Fixture::with_units(9);
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let manifest = fixture.root.join("Cargo.toml");
    fs::write(&manifest, "[package]\nname='wire_burst'\nversion='0.0.0'\n").unwrap();
    for id in 0..9 {
        controller
            .spawn_root(
                &fence,
                id,
                &manifest,
                &fixture.root.join(format!("target-{id}.noindex")),
            )
            .unwrap();
    }
    close(&mut controller, &fence);
    // These are diagnostic root observations from the actual private actor,
    // not PID-derived child authority. Eighteen valid root frames precede the
    // final Closing/Retired tail and actual writer EOF in one queued burst.
    for id in 0..9 {
        let pid = 1_000_000_u32 + id;
        actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"spawned","id":id,"pid":pid}));
        actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"root-exit","id":id,"pid":pid,"code":0,"success":true}));
    }
    actor.retired();
    let helper = i32::try_from(controller.helper_id().unwrap()).unwrap();
    // Observe terminal exit without reaping the controller's actual Child.
    // Its complete >16-frame tail and EOF must already be queued before the
    // first receive pass, making the read-order regression deterministic.
    until_within(
        MUST_HAPPEN,
        "private actor output fully queued before receive",
        || {
            matches!(
                aterm_uds::process::process_presence(helper),
                aterm_uds::process::ProcessPresence::Exited
            )
            .then_some(())
        },
    );
    retired(&mut controller, &fence);
    assert!(!controller.status().faulted);
    denied(&fixture.pool);
    finish(&mut controller, &fence);
    assert_eq!(controller.status().settled_units, 9);
    let restored = fixture.capabilities();
    denied(&fixture.pool);
    for capability in restored {
        capability.settle().unwrap();
    }
}

#[test]
fn genuine_not_spawned_cancellation_failures_retry_original_receipt_without_stranding_cpu() {
    for after_write in [false, true] {
        let fixture = Fixture::new();
        let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
        let actor = Actor::new(&fixture);
        let mut controller = fixture.controller(&fence, &actor);
        let mut missing = spawnfd::command(fixture.root.join("missing-helper"));
        assert!(controller.launch_for_test(&fence, &mut missing).is_err());
        assert!(controller.status().not_spawned);
        assert!(!controller.poll(&fence).unwrap().faulted);
        assert!(controller.try_finish(&fence).is_err());
        denied(&fixture.pool);
        if after_write {
            fixture.cohort.fail_next_event_after_write();
        } else {
            fixture.cohort.fail_next_event_before_write();
        }
        assert!(controller.cancel_not_spawned(&fence).is_err());
        denied(&fixture.pool);
        assert!(!controller.status().retired);
        assert!(!actor.directory.exists());
        // Accepted receipt custody moved to the original Claim before I/O. It
        // must remain usable even when failure preceded pending-event creation.
        finish(&mut controller, &fence);
        assert!(!controller.status().retired);
        assert!(controller.status().endpoint_cleaned);
        assert_eq!(controller.status().settled_units, 2);
        let restored = fixture.capabilities();
        for capability in restored {
            capability.settle().unwrap();
        }
    }
}

#[test]
fn failed_root_outcome_survives_successful_helper_retirement_and_safe_cpu_settlement() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let manifest = fixture.root.join("Cargo.toml");
    fs::write(
        &manifest,
        "[package]\nname='failed_root'\nversion='0.0.0'\n",
    )
    .unwrap();
    controller
        .spawn_root(&fence, 0, &manifest, &fixture.root.join("target.noindex"))
        .unwrap();
    close(&mut controller, &fence);
    let pid = 1_000_000_u32;
    actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    actor.frame(aterm_json::json!({"v":2,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":17,"success":false}));
    actor.retired();
    retired(&mut controller, &fence);
    assert!(controller.status().helper_exited);
    assert_eq!(controller.status().completed_roots, 1);
    assert_eq!(controller.status().failed_roots, 1);
    let outcome = controller.root_outcome(0).unwrap();
    assert_eq!(outcome.id, 0);
    assert_eq!(outcome.pid, pid);
    assert_eq!(outcome.code, Some(17));
    assert!(!outcome.success);
    assert!(controller.root_outcome(1).is_none());
    denied(&fixture.pool);
    // Resource closure and build success are separate facts. The actual
    // helper/group and endpoint may safely close despite a failed root, while
    // its original failure remains visible after every CPU unit returns.
    finish(&mut controller, &fence);
    assert_eq!(controller.root_outcome(0).unwrap().code, Some(17));
    assert_eq!(controller.status().failed_roots, 1);
    let restored = fixture.capabilities();
    denied(&fixture.pool);
    for capability in restored {
        capability.settle().unwrap();
    }
}

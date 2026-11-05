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
        fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
use aterm_uds::spawnfd::{self, Child, Command, Stdio};

use crate::{
    build_cpu::{Owner, RETAINED_DIRECTORY_NAME, RetainedLease, RetainedPool, RetainedSlotState},
    epoch_controller::{Config, EpochController, RootCancellation},
    epoch_endpoint::{fail_next_sync, fail_next_unlink},
    epoch_wire::VERSION,
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
    closing_sent: bool,
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
            closing_sent: false,
        }
    }

    fn command(&self) -> Command {
        let mut command = spawnfd::command(&self.program);
        command.arg(&self.stream).stderr(Stdio::null());
        command
    }

    fn frame(&mut self, frame: aterm_json::Value) {
        self.closing_sent |=
            frame.get("event").and_then(aterm_json::Value::as_str) == Some("closing");
        let writer = self.writer.as_mut().unwrap();
        writeln!(writer, "{}", aterm_json::to_string(&frame).unwrap()).unwrap();
        writer.flush().unwrap();
    }

    fn opened(&mut self) {
        private_dir(&self.directory);
        mkfifo(&self.fifo());
        self.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"opened","fifo":self.fifo(),"backing":self.units,"roots":self.units,"published":0}));
    }

    fn fifo(&self) -> PathBuf {
        self.directory.join("credits.fifo")
    }

    fn finalize(&mut self, controller: &mut EpochController, fence: &Fence) {
        if !self.closing_sent {
            self.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"closing"}));
        }
        until_within(MUST_HAPPEN, "actual finalization request written", || {
            controller.poll(fence).unwrap();
            controller.finalization_sent_for_test().then_some(())
        });
    }

    fn retired(&mut self, controller: &mut EpochController, fence: &Fence) {
        self.finalize(controller, fence);
        self.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":self.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":controller.finalization_nonce_for_test().unwrap()}));
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
fn dispatch_retains_actual_birth_and_unread_protocol_across_real_bind_lock_failure() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let claim = fixture.cohort.claim();
    let journal = claim.path().to_owned();
    let mut controller = EpochController::new(
        &fence,
        claim,
        fixture.capabilities(),
        Config {
            helper: actor.program.clone(),
            directory: actor.directory.clone(),
            epoch: EPOCH.to_owned(),
            roots: 2,
        },
    )
    .unwrap();
    controller
        .dispatch_for_test(&fence, &mut actor.command())
        .unwrap();
    let helper = controller.helper_id().unwrap();
    let identity = aterm_uds::process::read_lineage(i32::try_from(helper).unwrap())
        .unwrap()
        .identity;
    let armed = fs::read(&journal).unwrap();
    let inode = fs::metadata(&journal).unwrap().ino();
    let cpu_bytes = || {
        (0..2)
            .map(|slot| {
                fs::read(
                    fixture
                        .pool
                        .path()
                        .join(RETAINED_DIRECTORY_NAME)
                        .join(format!("slot-{slot:08x}")),
                )
                .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let original_cpu = cpu_bytes();
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &journal.parent().unwrap().join("mutation.lock"),
        Instant::now(),
        "original BIND obstruction",
    )
    .unwrap();
    actor.opened();
    let status = controller.poll(&fence).unwrap();
    assert!(status.unbound && !status.opened && !status.faulted && !status.not_spawned);
    assert_eq!(
        controller.readiness().interests().count(),
        0,
        "pending BIND exposes no private protocol pipe"
    );
    assert!(
        controller.close(&fence).is_err(),
        "unbound child supplies no root/close authority"
    );
    assert_eq!(
        controller.retry_binding(&fence).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(controller.helper_id(), Some(helper));
    assert_eq!(fs::read(&journal).unwrap(), armed);
    assert_eq!(fs::metadata(&journal).unwrap().ino(), inode);
    assert_eq!(cpu_bytes(), original_cpu);
    assert!(
        controller.launch(&fence).is_err(),
        "dispatch and launch share one birth"
    );
    assert!(
        controller.dispatch(&fence).is_err(),
        "dispatch cannot start a second helper"
    );
    assert_eq!(controller.helper_id(), Some(helper));
    assert!(!controller.status().faulted);
    denied(&fixture.pool);
    drop(lock);
    controller.retry_binding(&fence).unwrap();
    assert_eq!(controller.helper_id(), Some(helper));
    assert_eq!(
        aterm_uds::process::read_lineage(i32::try_from(helper).unwrap())
            .unwrap()
            .identity,
        identity
    );
    opened(&mut controller, &fence);
    assert!(!controller.status().unbound);
    denied(&fixture.pool);
    close(&mut controller, &fence);
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    finish(&mut controller, &fence);
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
    }
}

#[test]
fn dispatch_reaps_original_child_before_bind_without_fabricating_no_child_or_retirement() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut controller = EpochController::new(
        &fence,
        fixture.cohort.claim(),
        fixture.capabilities(),
        Config {
            helper: PathBuf::from("/usr/bin/true"),
            directory: fixture.root.join("transport.noindex"),
            epoch: EPOCH.to_owned(),
            roots: 2,
        },
    )
    .unwrap();
    // This actual fast-exit binary deliberately emits no helper protocol. It
    // tests that original sealed birth survives reap, never that exit is enough.
    controller.dispatch(&fence).unwrap();
    let helper = controller.helper_id().unwrap();
    until_within(MUST_HAPPEN, "unbound actual helper reap", || {
        controller.poll(&fence).unwrap().helper_exited.then_some(())
    });
    let status = controller.status();
    assert!(
        status.unbound
            && status.helper_exited
            && !status.opened
            && !status.not_spawned
            && !status.faulted
    );
    assert!(controller.cancel_not_spawned(&fence).is_err());
    assert!(controller.try_finish(&fence).is_err());
    denied(&fixture.pool);
    controller.retry_binding(&fence).unwrap();
    assert_eq!(
        controller.helper_id(),
        Some(helper),
        "BIND retains the exact reaped Spawned receipt"
    );
    assert!(!controller.status().unbound);
    assert!(
        controller.poll(&fence).is_err(),
        "EOF alone supplies no Retired witness"
    );
    assert!(!controller.status().retired && !controller.status().endpoint_cleaned);
    assert!(controller.try_finish(&fence).is_err());
    denied(&fixture.pool);
    drop(controller);
    debt_after_locks_release(&fixture.pool);
}

#[test]
fn dispatch_genuine_no_child_receipt_can_cancel_without_another_birth() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut controller = EpochController::new(
        &fence,
        fixture.cohort.claim(),
        fixture.capabilities(),
        Config {
            helper: fixture.root.join("missing-genuine-helper"),
            directory: fixture.root.join("transport.noindex"),
            epoch: EPOCH.to_owned(),
            roots: 2,
        },
    )
    .unwrap();
    assert!(controller.dispatch(&fence).is_err());
    assert_eq!(controller.helper_id(), None);
    let status = controller.poll(&fence).unwrap();
    assert!(status.not_spawned && !status.unbound && !status.faulted && !status.helper_exited);
    assert!(controller.dispatch(&fence).is_err());
    denied(&fixture.pool);
    controller.cancel_not_spawned(&fence).unwrap();
    finish(&mut controller, &fence);
    assert!(!controller.status().retired && controller.status().endpoint_cleaned);
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
    }
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
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    assert_eq!(
        controller.readiness().interests().count(),
        0,
        "validated EOF must not stay permanently readable"
    );
    assert!(controller.readiness().retry_finish);
    assert!(
        !controller.readiness().immediate,
        "a live group needs paced checks, not a busy loop"
    );
    assert!(!member.child.exited().unwrap());
    assert!(controller.try_finish(&fence).is_err());
    assert!(actor.fifo().exists());
    denied(&fixture.pool);
    member.end();
    finish(&mut controller, &fence);
    assert!(!controller.readiness().retry_finish);
    assert!(!controller.readiness().immediate);
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
    for mode in [
        "missing",
        "foreign",
        "wrong-count",
        "wrong-node",
        "missing-challenge",
        "wrong-challenge",
    ] {
        let fixture = Fixture::new();
        let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
        let mut actor = Actor::new(&fixture);
        let mut controller = launch(&fixture, &mut actor, &fence);
        close(&mut controller, &fence);
        actor.finalize(&mut controller, &fence);
        let finalization = controller.finalization_nonce_for_test().unwrap();
        let mut wrong_challenge = finalization.to_owned();
        wrong_challenge.replace_range(
            ..1,
            if finalization.starts_with('0') {
                "1"
            } else {
                "0"
            },
        );
        match mode {
            "missing" => {}
            "foreign" => actor.frame(aterm_json::json!({"v":VERSION,"epoch":"ffffffffffffffffffffffffffffffff","event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":finalization})),
            "wrong-count" => actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":1,"scope":"owned-roots-and-explicit-tokens","finalization":finalization})),
            "wrong-node" => actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":fixture.root.join("foreign.fifo"),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":finalization})),
            "missing-challenge" => actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens"})),
            "wrong-challenge" => actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":wrong_challenge})),
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
        assert_eq!(controller.readiness().interests().count(), 0);
        assert!(!controller.readiness().immediate);
        assert!(!controller.readiness().retry_finish);
        drop(controller);
        debt_after_locks_release(&fixture.pool);
    }
}

#[test]
fn closing_and_early_retirement_cannot_manufacture_the_missing_finalization() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    close(&mut controller, &fence);
    // Both complete frames are queued before the receiver can create a fresh
    // finalization challenge. They cannot authorize their own missing request.
    let burst = [
        aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"closing"}),
        aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":EPOCH}),
    ];
    let mut bytes = Vec::new();
    for frame in burst {
        bytes.extend(aterm_json::to_vec(&frame).unwrap());
        bytes.push(b'\n');
    }
    actor.writer.as_mut().unwrap().write_all(&bytes).unwrap();
    until_within(
        MUST_HAPPEN,
        "complete premature retirement in the actual pipe",
        || (controller.output_bytes_for_test().unwrap() == bytes.len()).then_some(()),
    );
    assert!(controller.poll(&fence).is_err());
    assert!(controller.status().faulted && !controller.status().retired);
    assert!(!controller.finalization_sent_for_test());
    assert!(controller.try_finish(&fence).is_err());
    denied(&fixture.pool);
    actor.end();
    drop(controller);
    debt_after_locks_release(&fixture.pool);
}

#[test]
fn already_buffered_retirement_cannot_match_the_later_fresh_finalization() {
    let fixture = Fixture::with_units(8);
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let manifest = fixture.root.join("Cargo.toml");
    for id in 0..8 {
        controller
            .spawn_root(
                &fence,
                id,
                &manifest,
                &fixture.root.join(format!("target-{id}.noindex")),
            )
            .unwrap();
    }
    actor.frame(
        aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":1_000_000}),
    );
    until_within(
        MUST_HAPPEN,
        "first root observation before terminal burst",
        || {
            controller.poll(&fence).unwrap();
            (controller.root_pid(0) == Some(1_000_000)).then_some(())
        },
    );
    close(&mut controller, &fence);
    let mut frames = vec![
        aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"closing"}),
        aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":1_000_000,"code":0,"success":true}),
    ];
    for id in 1..8 {
        let pid = 1_000_000 + id;
        frames.push(
            aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":id,"pid":pid}),
        );
        frames.push(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":id,"pid":pid,"code":0,"success":true}));
    }
    assert_eq!(frames.len(), 16);
    frames.push(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"retired","fifo":actor.fifo(),"withdrawn":0,"scope":"owned-roots-and-explicit-tokens","finalization":EPOCH}));
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend(aterm_json::to_vec(&frame).unwrap());
        bytes.push(b'\n');
    }
    assert!(
        bytes.len() < 4096,
        "one read must carry the complete old response"
    );
    actor.writer.as_mut().unwrap().write_all(&bytes).unwrap();
    until_within(
        MUST_HAPPEN,
        "old response queued before challenge exists",
        || (controller.output_bytes_for_test().unwrap() == bytes.len()).then_some(()),
    );
    controller.poll(&fence).unwrap();
    assert_eq!(controller.status().completed_roots, 8);
    assert!(!controller.finalization_sent_for_test());
    assert!(controller.readiness().immediate);
    // This call writes the queued request BEFORE parsing frame 17. The byte
    // offset alone now matches, but the old response cannot echo its fresh nonce.
    assert!(controller.poll(&fence).is_err());
    assert!(controller.finalization_sent_for_test());
    assert_ne!(controller.finalization_nonce_for_test(), Some(EPOCH));
    assert!(controller.status().faulted && !controller.status().retired);
    denied(&fixture.pool);
    actor.end();
    drop(controller);
    debt_after_locks_release(&fixture.pool);
}

#[test]
fn replacing_the_original_node_refuses_cleanup_and_cpu_settlement() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    close(&mut controller, &fence);
    actor.retired(&mut controller, &fence);
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
        actor.retired(&mut controller, &fence);
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
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    assert!(controller.try_finish(&fence).is_err());
    assert!(
        controller.readiness().immediate,
        "cleaned remaining units need bounded local work"
    );
    assert_eq!(controller.status().settled_units, 1);
    let states = fixture.pool.snapshot().unwrap();
    let pending = states
        .iter()
        .find_map(|state| match state {
            // A busy kernel lock says Held whatever the record says, a free slot's included,
            // so only a reported owner names the slot that still carries the debt.
            RetainedSlotState::Held {
                slot,
                reported_owner: Some(_),
            }
            | RetainedSlotState::Retained { slot, .. } => Some(*slot),
            RetainedSlotState::Held {
                reported_owner: None,
                ..
            }
            | RetainedSlotState::Available { .. } => None,
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
    assert!(
        !controller.readiness().retry_finish,
        "consumed uncertainty cannot improve through timers"
    );
    assert!(!controller.readiness().immediate);
    assert!(controller.try_finish(&fence).is_err());
    let available = fixture.pool.try_acquire(owner()).unwrap().unwrap();
    assert!(fixture.pool.try_acquire(owner()).unwrap().is_none());
    available.settle().unwrap();
}

#[test]
fn pending_commands_watch_writes_and_quiet_live_helper_output_waits_for_reads() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    assert_eq!(controller.readiness().interests().count(), 1);
    assert!(!controller.readiness().immediate);
    controller.close(&fence).unwrap();
    let interests = controller.readiness().interests().collect::<Vec<_>>();
    assert_eq!(interests.len(), 2);
    // The stand-in is live and quiet; only the actual queued-command writer
    // should wake. Polling does not consume Opened's already consumed bytes.
    let ready =
        aterm_uds::ready::any_interests_until(&interests, Instant::now() + MUST_HAPPEN).unwrap();
    assert_eq!(ready, [1]);
    controller.poll(&fence).unwrap();
    assert_eq!(
        controller.readiness().interests().count(),
        1,
        "an empty command queue must not keep a writable watch"
    );
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    finish(&mut controller, &fence);
}

#[test]
fn a_complete_buffered_tail_progresses_while_live_helper_pipe_is_quiet() {
    let fixture = Fixture::with_units(9);
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let manifest = fixture.root.join("Cargo.toml");
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
    let mut burst = Vec::new();
    for id in 0..9 {
        let pid = 1_000_000_u32 + id;
        for frame in [
            aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":id,"pid":pid}),
            aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":id,"pid":pid,"code":0,"success":true}),
        ] {
            burst.extend(aterm_json::to_vec(&frame).unwrap());
            burst.push(b'\n');
        }
    }
    assert!(
        burst.len() < 4096,
        "one actual pipe read can carry this complete burst"
    );
    actor.writer.as_mut().unwrap().write_all(&burst).unwrap();
    // Observe actual kernel byte availability before the first controller read;
    // unlike the EOF regression, the actor remains live with its writer open.
    // A test-only observer borrows no production parser or release capability.
    until_within(
        MUST_HAPPEN,
        "complete live-helper burst queued in actual pipe",
        || (controller.output_bytes_for_test().unwrap() == burst.len()).then_some(()),
    );
    controller.poll(&fence).unwrap();
    assert_eq!(controller.status().completed_roots, 8);
    assert!(
        controller.readiness().immediate,
        "last two frames are local work, not kernel readability"
    );
    assert!(!controller.status().helper_exited);
    controller.poll(&fence).unwrap();
    assert_eq!(controller.status().completed_roots, 9);
    assert!(!controller.readiness().immediate);
    assert_eq!(controller.readiness().interests().count(), 1);
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    finish(&mut controller, &fence);
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
fn roots_beyond_one_frame_budget_complete_before_finalization_and_valid_eof() {
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
    // Closing acknowledgement. Finalization must await all eighteen frames;
    // only afterwards may the actor emit Retired and close its actual writer.
    for id in 0..9 {
        let pid = 1_000_000_u32 + id;
        actor.frame(
            aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":id,"pid":pid}),
        );
        actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":id,"pid":pid,"code":0,"success":true}));
    }
    actor.retired(&mut controller, &fence);
    let helper = i32::try_from(controller.helper_id().unwrap()).unwrap();
    // Observe terminal exit without reaping the controller's actual Child.
    // Retired and EOF are queued only after the actual finalization write;
    // the preceding root burst has already required multiple bounded passes.
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
fn cancellation_while_closing_waits_for_driver_ack_and_keeps_actual_failed_exit() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let pid = 1_000_000_u32;
    controller
        .spawn_root(
            &fence,
            0,
            &fixture.root.join("Cargo.toml"),
            &fixture.root.join("target.noindex"),
        )
        .unwrap();
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    until_within(MUST_HAPPEN, "original root birth", || {
        controller.poll(&fence).unwrap();
        controller.root_pid(0)
    });
    close(&mut controller, &fence);
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"closing"}));
    assert_eq!(
        controller.cancel_root(&fence, 0).unwrap(),
        RootCancellation::Requested
    );
    assert_eq!(
        controller.cancel_root(&fence, 0).unwrap(),
        RootCancellation::Requested
    );
    controller.poll(&fence).unwrap();
    assert!(controller.root_outcome(0).is_none());
    assert!(!controller.root_eligible_for_activation(0));
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"cancelled","id":0,"pid":pid,"state":"stopped"}));
    until_within(MUST_HAPPEN, "actual driver stop acknowledgement", || {
        controller.poll(&fence).unwrap();
        (controller.root_cancellation(0) == Some(RootCancellation::Stopped)).then_some(())
    });
    assert!(!controller.finalization_sent_for_test());
    denied(&fixture.pool);
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":101,"success":false}));
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    let actual = controller.root_outcome(0).unwrap();
    assert_eq!(actual.code, Some(101));
    assert!(!actual.success && !controller.root_eligible_for_activation(0));
    assert_eq!(
        controller.root_cancellation(0),
        Some(RootCancellation::Stopped)
    );
    finish(&mut controller, &fence);
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
    }
}

#[test]
fn successful_exit_racing_cancel_stays_successful_and_withholds_finalization_until_ack() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let pid = 1_000_000_u32;
    controller
        .spawn_root(
            &fence,
            0,
            &fixture.root.join("Cargo.toml"),
            &fixture.root.join("target.noindex"),
        )
        .unwrap();
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    until_within(MUST_HAPPEN, "original root birth", || {
        controller.poll(&fence).unwrap();
        controller.root_pid(0)
    });
    controller.cancel_root(&fence, 0).unwrap();
    close(&mut controller, &fence);
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"closing"}));
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":0,"success":true}));
    until_within(
        MUST_HAPPEN,
        "actual successful exit before cancellation response",
        || {
            controller.poll(&fence).unwrap();
            controller.root_outcome(0)
        },
    );
    assert_eq!(
        controller.root_cancellation(0),
        Some(RootCancellation::Requested)
    );
    assert!(!controller.finalization_sent_for_test());
    assert_eq!(
        controller.readiness().interests().count(),
        1,
        "pending cancellation must not queue Finalize"
    );
    assert!(!controller.root_eligible_for_activation(0));
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"cancelled","id":0,"pid":pid,"state":"already-exited"}));
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    assert!(controller.root_outcome(0).unwrap().success);
    assert_eq!(
        controller.root_cancellation(0),
        Some(RootCancellation::AlreadyExited)
    );
    finish(&mut controller, &fence);
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
    }
}

#[test]
fn cancellation_after_observed_exit_records_intent_without_rewriting_outcome() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let pid = 1_000_000_u32;
    controller
        .spawn_root(
            &fence,
            0,
            &fixture.root.join("Cargo.toml"),
            &fixture.root.join("target.noindex"),
        )
        .unwrap();
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":0,"success":true}));
    until_within(MUST_HAPPEN, "actual completed root", || {
        controller.poll(&fence).unwrap();
        controller.root_outcome(0)
    });
    let actual = controller.root_outcome(0).unwrap();
    assert!(controller.root_eligible_for_activation(0));
    close(&mut controller, &fence);
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    assert_eq!(
        controller.cancel_root(&fence, 0).unwrap(),
        RootCancellation::AlreadyExited
    );
    assert_eq!(
        controller.cancel_root(&fence, 0).unwrap(),
        RootCancellation::AlreadyExited
    );
    assert_eq!(controller.root_outcome(0), Some(actual));
    assert!(!controller.root_eligible_for_activation(0));
    finish(&mut controller, &fence);
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
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
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    until_within(MUST_HAPPEN, "validated live root birth observation", || {
        controller.poll(&fence).unwrap();
        (controller.root_pid(0) == Some(pid)).then_some(())
    });
    assert!(controller.root_outcome(0).is_none());
    assert!(controller.root_pid(1).is_none());
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":17,"success":false}));
    actor.retired(&mut controller, &fence);
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
    assert_eq!(controller.root_pid(0), Some(pid));
    assert_eq!(controller.status().failed_roots, 1);
    let restored = fixture.capabilities();
    denied(&fixture.pool);
    for capability in restored {
        capability.settle().unwrap();
    }
}

#[test]
fn legacy_spawn_pins_parent_alias_to_the_original_physical_target() {
    let fixture = Fixture::new();
    let fence = Fence::acquire(&fixture.cohort.namespace, Instant::now()).unwrap();
    let mut actor = Actor::new(&fixture);
    let mut controller = launch(&fixture, &mut actor, &fence);
    let alias = fixture.root.join("target-parent-alias");
    std::os::unix::fs::symlink(fixture.root.canonicalize().unwrap(), &alias).unwrap();
    let target = alias.join("target.noindex");
    controller
        .spawn_root(&fence, 0, &fixture.root.join("Cargo.toml"), &target)
        .unwrap();
    let actual = fixture.root.canonicalize().unwrap().join("target.noindex");
    assert!(matches!(crate::epoch_target::TargetLease::acquire(&actual),
        Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
    close(&mut controller, &fence);
    let pid = 1_000_000_u32;
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"spawned","id":0,"pid":pid}));
    actor.frame(aterm_json::json!({"v":VERSION,"epoch":EPOCH,"event":"root-exit","id":0,"pid":pid,"code":0,"success":true}));
    actor.retired(&mut controller, &fence);
    retired(&mut controller, &fence);
    finish(&mut controller, &fence);
    // A legacy root retains its loan until original settled controller custody ends.
    drop(controller);
    crate::epoch_target::TargetLease::acquire(&actual)
        .unwrap()
        .check()
        .unwrap();
    for cap in fixture.capabilities() {
        cap.settle().unwrap();
    }
}

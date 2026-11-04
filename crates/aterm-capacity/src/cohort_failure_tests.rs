// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `CapacityCohort` conformance traces: actual host SIGKILL, then worker SIGKILL, with a
//! running closed-guard grandchild; a live holder's actual drain, then its finish and Drop or its
//! Drop alone; an ARM cancelled by its own no-child receipt; and an unknown predecessor that
//! refuses ARM and forced admission. Each projects the ledger, guard and kernel onto the model
//! after its steps. All memory readings are injected; no test allocates the declared heavy-work
//! budget.

use super::*;
use crate::{
    Accounting, Claim, CohortClosureRef, Fixed, Polled, Reading, Waiter, cohort::Lifetime,
    cohort_journal::Journal, cohort_namespace::Namespace,
};
use aterm_spec::{
    derive::Model,
    interp,
    wait::{MUST_HAPPEN, until_within},
};
use aterm_uds::{
    process,
    spawnfd::{self, BirthAttempt, Child, Stdio},
};
use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader},
    net::Shutdown,
    os::{
        fd::{FromRawFd as _, OwnedFd},
        unix::{
            fs::{DirBuilderExt as _, MetadataExt as _},
            net::UnixStream,
            process::ExitStatusExt as _,
        },
    },
    sync::mpsc,
    thread,
};

const ROLE: &str = "ATERM_CAPACITY_DOUBLE_DEATH_ROLE";
const FENCE: &str = "ATERM_CAPACITY_DOUBLE_DEATH_FENCE";
const CHECKOUT: &str = "ATERM_CAPACITY_DOUBLE_DEATH_CHECKOUT";
const GUARD: &str = "ATERM_CAPACITY_DOUBLE_DEATH_GUARD";
const PARENT: &str = "ATERM_CAPACITY_DOUBLE_DEATH_PARENT";
const HOST: &str = "ledger::failure_tests::host_actor";
const WORKER: &str = "ledger::failure_tests::worker_actor";

fn copied_fd(raw: i32) -> File {
    // SAFETY: fcntl accepts an invalid input by returning EBADF and allocates a new live
    // descriptor on success. No borrowed ownership is assumed for the supplied number.
    let copy = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 10) };
    assert!(
        copy >= 0,
        "cannot copy actor descriptor: {}",
        io::Error::last_os_error()
    );
    // SAFETY: the successful F_DUPFD_CLOEXEC returned a fresh owned descriptor.
    File::from(unsafe { OwnedFd::from_raw_fd(copy) })
}

fn command_byte() -> u8 {
    let mut byte = [0];
    copied_fd(2).read_exact(&mut byte).unwrap();
    byte[0]
}

fn emit(tag: &str, value: &Value) {
    println!("{tag} {}", aterm_json::to_string(value).unwrap());
    io::stdout().flush().unwrap();
}

#[test]
fn host_actor() {
    if std::env::var(ROLE).as_deref() != Ok("host") {
        return;
    }
    let ledger = Ledger::at(std::env::var(FENCE).unwrap());
    let machine = Fixed::new(Reading::of_gib(64, 40, 8));
    let need = Need::new("prefix-a")
        .mem_gib(30)
        .checkout_exact(std::env::var(CHECKOUT).unwrap())
        .unwrap();
    let mut claim = match Waiter::enter(&ledger, &machine, need)
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("host claim was not admitted: {other:?}"),
    };
    let reference = claim.cohort_closure_ref().unwrap();
    emit(
        "CAP_ADMITTED",
        &aterm_json::json!({"path": claim.path().to_str().unwrap(), "reference": reference.to_json()}),
    );
    assert_eq!(command_byte(), b'a');
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", WORKER, "--nocapture"])
        .env(ROLE, "worker")
        .env(
            PARENT,
            process::read_identity(std::process::id() as i32)
                .unwrap()
                .token(),
        )
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
        .inherit_guards(vec![claim.descriptor().unwrap()])
        .unwrap();
    let target = command.guard_targets().next().unwrap();
    command.env(GUARD, target.to_string());
    let attempt = claim.arm_cohort().unwrap();
    emit("CAP_ARMED", &aterm_json::json!({}));
    assert_eq!(command_byte(), b's');
    let mut spawned = command.spawn_cohort(attempt).unwrap();
    emit(
        "CAP_SPAWNED",
        &aterm_json::json!({"worker": spawned.child_mut().id()}),
    );
    assert_eq!(command_byte(), b'b');
    let cohort = claim.bind_cohort(&mut spawned).unwrap();
    emit("CAP_BOUND", &cohort.record().encode());
    // The parent is intentionally killed before any destructor or resource settlement.
    loop {
        thread::park();
    }
}

#[test]
fn worker_actor() {
    if std::env::var(ROLE).as_deref() != Ok("worker") {
        return;
    }
    let parent = process::Identity::parse(&std::env::var(PARENT).unwrap()).unwrap();
    assert_eq!(process::read_identity(parent.pid), Some(parent));
    let raw: i32 = std::env::var(GUARD).unwrap().parse().unwrap();
    let mut inherited = copied_fd(raw);
    assert!(
        inherited
            .write_all(b"cannot change declared budget")
            .is_err()
    );
    // No inherit_guards call here: the compiler stand-in holds stdio and closes all guards.
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let child = spawnfd::command(cat)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    emit(
        "CAP_WORKER_READY",
        &aterm_json::json!({"compiler": child.id()}),
    );
    until_within(MUST_HAPPEN, "actual host death", || {
        (process::read_identity(parent.pid) != Some(parent)).then_some(())
    });
    emit("CAP_WORKER_ORPHANED", &aterm_json::json!({}));
    assert_eq!(command_byte(), b'k');
    process::kill_self_now();
}

struct Driver {
    child: Child,
    input: UnixStream,
    control: UnixStream,
    receiver: mpsc::Receiver<String>,
    buffered: Vec<String>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Driver {
    fn start(namespace: &Namespace, checkout: &str) -> Self {
        let (input, child_input) = UnixStream::pair().unwrap();
        let (control, child_control) = UnixStream::pair().unwrap();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", HOST, "--nocapture"])
            .env(ROLE, "host")
            .env(FENCE, namespace.fence())
            .env(CHECKOUT, checkout)
            .stdin(Stdio::from(OwnedFd::from(child_input)))
            .stdout(Stdio::piped())
            .stderr(Stdio::from(OwnedFd::from(child_control)));
        let mut child = command.spawn().unwrap();
        drop(command);
        let output = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            control,
            receiver,
            buffered: Vec::new(),
            reader: Some(reader),
        }
    }

    fn event(&mut self, tag: &str) -> Value {
        loop {
            if let Some(at) = self.buffered.iter().position(|line| line.contains(tag)) {
                let line = self.buffered.remove(at);
                let (_, value) = line.split_once(tag).unwrap();
                return aterm_json::from_str(value.trim()).unwrap();
            }
            self.buffered.push(
                self.receiver
                    .recv_timeout(MUST_HAPPEN)
                    .expect("actor event hang detector"),
            );
        }
    }

    fn go(&mut self, byte: u8) {
        self.control.write_all(&[byte]).unwrap();
    }

    fn finish_reader(&mut self) {
        until_within(MUST_HAPPEN, "actor output EOF", || {
            self.reader.as_ref().unwrap().is_finished().then_some(())
        });
        self.reader.take().unwrap().join().unwrap();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = self.control.write_all(b"k");
        let _ = self.control.shutdown(Shutdown::Both);
        let _ = self.input.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            // Closing both actor input channels makes the compiler exit and the worker die.
            // A failed world's reader is detached instead of an unbounded destructor join.
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

struct Trace {
    model: Model,
    state: BTreeMap<&'static str, i64>,
}
impl Trace {
    fn new() -> Self {
        let model = aterm_models_capacity::capacity_cohort_model();
        let state = model.init_state();
        Self { model, state }
    }
    fn step(&mut self, action: &str) {
        let mut next = self.model.successors(action, &self.state);
        assert_eq!(
            next.len(),
            1,
            "actual operation does not refine {action}: {:?}",
            self.state
        );
        self.state = next.pop().unwrap();
    }
}

/// A trace of the namespace `Namespace::create_private` just made, observed before any claim:
/// its `prepare_private` exchanges the fence into the old ledger's name, checks that the
/// retained predecessor directory is empty, then publishes readiness, so one call takes both
/// steps.
fn prepared(namespace: &Namespace) -> Trace {
    let mut trace = Trace::new();
    trace.step("PublishNamespaceFence");
    trace.step("ProvePredecessorDrain");
    assert_eq!(namespace_project(&trace.state, namespace), trace.state);
    trace
}

fn guards_held(path: &Path) -> bool {
    match RecordLock::try_lock(crate::open_regular_reader(path).unwrap(), RECORD_MAX) {
        Ok(lock) => {
            drop(lock);
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(error) => panic!("guard state is unavailable: {error:?}"),
    }
}

/// The original guard's identity: the claim file's inode, which its renames keep.
fn guard_of(path: &Path) -> ClaimGuardIdentity {
    let held = std::fs::symlink_metadata(path).unwrap();
    ClaimGuardIdentity {
        device: held.dev(),
        inode: held.ino(),
    }
}

fn persist_reference(root: &Path, reference: &CohortClosureRef) -> CohortClosureRef {
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = root.join(format!("{}.reference.json", reference.key()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let encoded = aterm_json::to_vec(&reference.to_json()).unwrap();
    file.write_all(&encoded).unwrap();
    aterm_durable::durable(&file).unwrap();
    aterm_durable::durable(&File::open(root).unwrap()).unwrap();
    let decoded = CohortClosureRef::from_json(
        &aterm_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(&decoded, reference);
    decoded
}

fn observe(
    trace: &Trace,
    namespace: &Namespace,
    ledger: &Ledger,
    path: &Path,
    reference: &CohortClosureRef,
    worker: Option<process::Identity>,
) {
    assert_eq!(
        project(&trace.state, namespace, ledger, path, reference, worker),
        trace.state,
        "actual custody differs from its model projection"
    );
}

/// The namespace's own state as `CapacityCohort`'s variables, the rest kept from `history`: the
/// retained predecessor directory is read before any validating call, since validation refuses
/// beside any entry there and readiness is then unobservable (it stays `history`'s). Otherwise
/// validation needs the fence and readiness, and a refusal fails the trace, so this reads 1.
fn namespace_project(
    history: &BTreeMap<&'static str, i64>,
    namespace: &Namespace,
) -> BTreeMap<&'static str, i64> {
    let mut actual = history.clone();
    let unknown_predecessor = std::fs::read_dir(namespace.predecessors())
        .unwrap()
        .next()
        .transpose()
        .unwrap()
        .is_some();
    actual.insert("unknown_predecessor", i64::from(unknown_predecessor));
    if unknown_predecessor {
        assert!(namespace.validate().is_err());
        assert!(Ledger::at(namespace.fence()).cohort_aware().is_err());
    } else {
        namespace.validate().unwrap();
        actual.insert(
            "namespace_ready",
            i64::from(Ledger::at(namespace.fence()).cohort_aware().unwrap()),
        );
    }
    actual
}

/// The real state of the traced claim at `path` (original epoch/guard `reference`), its namespace and
/// ledger, and the actual group `worker` leads, as `CapacityCohort`'s variables; a journal that
/// does not decode fails the trace. The arm, spawn outcome, cancellation and force history no
/// read here reaches, a retired claim's holder, and everything but the kernel's and the
/// predecessor directory's facts while an unknown predecessor refuses every read, stay
/// `history`'s.
fn project(
    history: &BTreeMap<&'static str, i64>,
    namespace: &Namespace,
    ledger: &Ledger,
    path: &Path,
    reference: &CohortClosureRef,
    worker: Option<process::Identity>,
) -> BTreeMap<&'static str, i64> {
    let mut actual = namespace_project(history, namespace);
    let marker = reference.marker();
    assert_eq!(reference.namespace(), namespace.nonce());
    assert_eq!(path.file_stem().unwrap().to_str().unwrap(), marker);
    actual.insert(
        "group_leader_alive",
        i64::from(worker.is_some_and(|w| process::read_identity(w.pid) == Some(w))),
    );
    actual.insert(
        "group_member_alive",
        i64::from(worker.is_some_and(|w| {
            process::group_presence(w.pid as u32) == process::ProcessPresence::Present
        })),
    );
    if actual["unknown_predecessor"] == 1 {
        // No ledger read, claim transaction or admission proceeds, so the claim's own state is
        // unobservable.
        assert!(ledger.list().is_err());
        return actual;
    }
    let records = ledger.list().unwrap();
    actual.insert(
        "second_admitted",
        i64::from(
            records
                .iter()
                .any(|r| r.held.is_some() && r.cover_marker() != marker),
        ),
    );
    let Some(record) = records.iter().find(|r| r.cover_marker() == marker) else {
        // Retired: only the durable archive matched to the original namespace, epoch,
        // marker and guard proves closure; absence alone is unknown. The active claim
        // no longer names a file on which a reader could acquire the old guard.
        assert_eq!(
            ledger.cohort_claim_state(reference).unwrap(),
            CohortClaimState::Closed
        );
        assert!(!path.try_exists().unwrap(), "retired claim still exists");
        actual.insert("phase", 0);
        actual.insert("lifetime", 2);
        actual.insert("full_budget", 0);
        actual.insert("guards_held", 0);
        return actual;
    };
    assert_eq!(
        guard_of(path),
        reference.guard(),
        "the original guard is unchanged"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let (_, journal) = Journal::decode(&text, true, namespace.nonce()).unwrap();
    let (phase, lifetime, holder) = journal.observation();
    actual.insert("phase", phase);
    actual.insert(
        "lifetime",
        match lifetime {
            Lifetime::Unknown => 0,
            Lifetime::Present => 1,
            Lifetime::Drained => 2,
        },
    );
    actual.insert("original_holder_alive", i64::from(holder));
    actual.insert("guards_held", i64::from(guards_held(path)));
    actual.insert(
        "full_budget",
        i64::from(crate::reserved(std::slice::from_ref(record), None, &[])[0].mem == 30 << 30),
    );
    // Every trace here stays on the host, boot and pid namespace its claim captured.
    actual.insert("scope", 0);
    actual
}

/// A private cohort namespace, its ledger and an injected machine in a scratch world kept when
/// a trace fails, for the traces whose holder is this test process.
struct World {
    scratch: PathBuf,
    namespace: Namespace,
    ledger: Ledger,
    machine: Fixed,
    checkout: String,
}

impl World {
    fn new(name: &str) -> Self {
        aterm_spec::fs_lane::join();
        let scratch = aterm_tempfile::tempdir().unwrap().keep();
        eprintln!("capacity {name} world={}", scratch.display());
        let home = scratch.join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(&scratch, &home).unwrap();
        let checkout = scratch.canonicalize().unwrap().to_str().unwrap().to_owned();
        let ledger = Ledger::at(namespace.fence());
        Self {
            scratch,
            namespace,
            ledger,
            machine: Fixed::new(Reading::of_gib(64, 40, 8)),
            checkout,
        }
    }

    fn admit(&self, what: &str, gib: u64) -> Claim {
        let need = Need::new(what)
            .mem_gib(gib)
            .checkout_exact(self.checkout.as_str())
            .unwrap();
        match Waiter::enter(&self.ledger, &self.machine, need)
            .unwrap()
            .poll(&self.machine)
        {
            Polled::Admitted(claim) => claim,
            other => panic!("{what} was not admitted: {other:?}"),
        }
    }

    fn reference(&self, claim: &Claim) -> CohortClosureRef {
        let reference = claim.cohort_closure_ref().unwrap();
        assert_eq!(reference.guard(), guard_of(claim.path()));
        persist_reference(&self.scratch, &reference)
    }

    fn observe(
        &self,
        trace: &Trace,
        path: &Path,
        reference: &CohortClosureRef,
        worker: Option<process::Identity>,
    ) {
        observe(
            trace,
            &self.namespace,
            &self.ledger,
            path,
            reference,
            worker,
        );
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if !thread::panicking() {
            std::fs::remove_dir_all(&self.scratch).unwrap();
        }
    }
}

#[test]
fn actual_host_then_worker_sigkill_keeps_closed_guard_compiler_reserved() {
    let scratch = aterm_tempfile::tempdir().unwrap().keep();
    eprintln!("capacity double-death world={}", scratch.display());
    let home = scratch.join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace = Namespace::create_private(&scratch, &home).unwrap();
    let mut trace = prepared(&namespace);
    let checkout = scratch.canonicalize().unwrap().to_str().unwrap().to_owned();
    let ledger = Ledger::at(namespace.fence());
    let machine = Fixed::new(Reading::of_gib(64, 40, 8));
    let mut driver = Driver::start(&namespace, &checkout);
    let admitted = driver.event("CAP_ADMITTED");
    let path = PathBuf::from(admitted.get("path").unwrap().as_str().unwrap());
    let reference = CohortClosureRef::from_json(admitted.get("reference").unwrap()).unwrap();
    assert_eq!(reference.guard(), guard_of(&path));
    assert_eq!(reference.namespace(), namespace.nonce());
    assert_eq!(
        reference.marker(),
        path.file_stem().unwrap().to_str().unwrap()
    );
    let reference = persist_reference(&scratch, &reference);
    observe(&trace, &namespace, &ledger, &path, &reference, None);
    driver.go(b'a');
    driver.event("CAP_ARMED");
    trace.step("DurableArm");
    observe(&trace, &namespace, &ledger, &path, &reference, None);
    driver.go(b's');
    let spawned = driver.event("CAP_SPAWNED");
    let worker = spawned.get("worker").unwrap().as_u64().unwrap() as i32;
    let worker = process::read_identity(worker).unwrap();
    trace.step("ActualSpawn");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));
    driver.go(b'b');
    driver.event("CAP_BOUND");
    let ready = driver.event("CAP_WORKER_READY");
    let compiler = ready.get("compiler").unwrap().as_u64().unwrap() as i32;
    assert_eq!(
        process::read_lineage(compiler).unwrap().pgid,
        worker.pid as u32
    );
    trace.step("BindActualChild");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));

    driver.child.kill().unwrap();
    assert_eq!(driver.child.wait().unwrap().signal(), Some(libc::SIGKILL));
    driver.event("CAP_WORKER_ORPHANED");
    trace.step("HolderDies");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));
    driver.go(b'k');
    until_within(MUST_HAPPEN, "actual worker SIGKILL and reaping", || {
        (process::read_identity(worker.pid) != Some(worker)).then_some(())
    });
    assert!(!guards_held(&path), "compiler retained an admission guard");
    trace.step("GroupLeaderDies");
    trace.step("DropAllGuards");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));
    let mut old_rule = trace.state.clone();
    old_rule.insert("full_budget", 0);
    assert!(interp::admits(&trace.model, &trace.state, &old_rule).is_none());
    assert_eq!(
        interp::admits(
            &interp::with_buggy(&trace.model, 1),
            &trace.state,
            &old_rule
        ),
        Some("ReapFreeGuards")
    );

    let nonce = aterm_uds::rand::hex_token::<16>().unwrap();
    driver
        .input
        .write_all(format!("CAP_COMPILER_ALIVE \"{nonce}\"\n").as_bytes())
        .unwrap();
    assert_eq!(
        driver.event("CAP_COMPILER_ALIVE").as_str(),
        Some(nonce.as_str())
    );
    let same_checkout = ledger.for_checkout(&checkout).unwrap();
    assert!(
        same_checkout
            .iter()
            .any(|r| r.accounting == Accounting::Whole && r.need.what == "prefix-a")
    );
    let waiter = Waiter::enter(
        &ledger,
        &machine,
        Need::new("prefix-b")
            .mem_gib(20)
            .checkout_exact(checkout)
            .unwrap(),
    )
    .unwrap();
    let waiter = match waiter.poll(&machine) {
        Polled::Waiting(waiter, _) => waiter,
        other => panic!("closed guards incorrectly released a live compiler: {other:?}"),
    };
    driver.input.shutdown(Shutdown::Write).unwrap();
    until_within(MUST_HAPPEN, "actual compiler group drain", || {
        (process::group_presence(worker.pid as u32) == process::ProcessPresence::Absent)
            .then_some(())
    });
    // No description holds the drained claim, but until a reaping scan's tombstone retires it
    // every reader still reserves its whole budget.
    trace.step("GroupActuallyDrains");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));
    assert_eq!(
        ledger.cohort_claim_state(&reference).unwrap(),
        CohortClaimState::Retained
    );
    // The second waiter's reaping scan finishes and retires it in one tombstone, then admits.
    let second = match waiter.poll(&machine) {
        Polled::Admitted(second) => second,
        other => panic!("an actually drained group kept its reservation: {other:?}"),
    };
    trace.step("FinishDrainedCohort");
    trace.step("RetireDescription");
    trace.step("AdmitSecond");
    observe(&trace, &namespace, &ledger, &path, &reference, Some(worker));
    assert!(!path.exists());
    drop(second);
    driver.finish_reader();
    drop(driver);
    std::fs::remove_dir_all(scratch).unwrap();
}

#[test]
fn a_live_holder_finishes_its_drained_group_and_retires_before_the_next_admission() {
    live_holder_drain("live-drain", true);
}

#[test]
fn a_live_holders_drop_finishes_and_retires_its_drained_group_in_one_tombstone() {
    live_holder_drain("live-drop", false);
}

/// A live holder's bound group drains; the holder then calls `finish_cohort` before its Drop
/// when `finish`, or only drops the claim, whose one tombstone then finishes and retires it.
fn live_holder_drain(name: &str, finish: bool) {
    let world = World::new(name);
    let mut trace = prepared(&world.namespace);
    let mut claim = world.admit("prefix-a", 30);
    let path = claim.path().to_owned();
    let reference = world.reference(&claim);
    world.observe(&trace, &path, &reference, None);
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let attempt = claim.arm_cohort().unwrap();
    trace.step("DurableArm");
    world.observe(&trace, &path, &reference, None);
    let mut spawned = spawnfd::command(cat)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn_cohort(attempt)
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
    let worker = process::read_identity(spawned.child_mut().id() as i32).unwrap();
    trace.step("ActualSpawn");
    world.observe(&trace, &path, &reference, Some(worker));
    let cohort = claim.bind_cohort(&mut spawned).unwrap();
    trace.step("BindActualChild");
    world.observe(&trace, &path, &reference, Some(worker));
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "a live group cannot finish"
    );
    assert!(
        trace
            .model
            .successors("FinishDrainedCohort", &trace.state)
            .is_empty()
    );
    world.observe(&trace, &path, &reference, Some(worker));

    let mut child = spawned.into_child();
    drop(input);
    assert!(child.wait().unwrap().success());
    until_within(MUST_HAPPEN, "actual cohort drain", || {
        (cohort.lifetime() == Lifetime::Drained).then_some(())
    });
    trace.step("GroupActuallyDrains");
    world.observe(&trace, &path, &reference, Some(worker));
    if finish {
        claim.finish_cohort().unwrap();
        trace.step("FinishDrainedCohort");
        world.observe(&trace, &path, &reference, Some(worker));
        drop(claim);
    } else {
        drop(claim);
        trace.step("FinishDrainedCohort");
    }
    trace.step("RetireDescription");
    world.observe(&trace, &path, &reference, Some(worker));
    let second = world.admit("prefix-b", 20);
    trace.step("AdmitSecond");
    world.observe(&trace, &path, &reference, Some(worker));
    drop(second);
}

#[test]
fn only_the_attempts_own_no_child_receipt_cancels_its_arm_before_retirement() {
    let world = World::new("no-child");
    let mut trace = prepared(&world.namespace);
    let mut claim = world.admit("prefix-a", 30);
    let path = claim.path().to_owned();
    let reference = world.reference(&claim);
    world.observe(&trace, &path, &reference, None);
    let attempt = claim.arm_cohort().unwrap();
    trace.step("DurableArm");
    world.observe(&trace, &path, &reference, None);

    // Another attempt's genuine receipt is no proof that this one started nothing.
    let foreign = BirthAttempt::new().unwrap().cancel().unwrap();
    assert!(claim.cancel_cohort(foreign).is_err());
    world.observe(&trace, &path, &reference, None);
    let mut cancelled = trace.state.clone();
    cancelled.insert("phase", 0);
    cancelled.insert("cancelled", 1);
    assert!(interp::admits(&trace.model, &trace.state, &cancelled).is_none());
    assert_eq!(
        interp::admits(
            &interp::with_buggy(&trace.model, 7),
            &trace.state,
            &cancelled
        ),
        Some("CancelWithoutMatchingOutcome")
    );

    let failure = spawnfd::command(world.namespace.backend().join("missing-target"))
        .spawn_cohort(attempt)
        .unwrap_err();
    let proof = failure.into_not_spawned().unwrap();
    trace.step("ActualNoChild");
    world.observe(&trace, &path, &reference, None);
    claim.cancel_cohort(proof).unwrap();
    trace.step("CancelMatchingNoChild");
    world.observe(&trace, &path, &reference, None);

    // The cancelled description still holds its whole budget until its holder retires it.
    let need = Need::new("prefix-b")
        .mem_gib(20)
        .checkout_exact(world.checkout.as_str())
        .unwrap();
    let next = match Waiter::enter(&world.ledger, &world.machine, need)
        .unwrap()
        .poll(&world.machine)
    {
        Polled::Waiting(next, _) => next,
        other => panic!("cancellation released a still-held reservation: {other:?}"),
    };
    world.observe(&trace, &path, &reference, None);
    drop(claim);
    trace.step("RetireDescription");
    world.observe(&trace, &path, &reference, None);
    let second = match next.poll(&world.machine) {
        Polled::Admitted(second) => second,
        other => panic!("a retired no-child claim still blocks admission: {other:?}"),
    };
    trace.step("AdmitSecond");
    world.observe(&trace, &path, &reference, None);
    drop(second);
}

#[test]
fn an_unknown_predecessor_refuses_arm_and_every_admission_forced_or_not() {
    let world = World::new("predecessor");
    let mut trace = prepared(&world.namespace);
    let mut claim = world.admit("prefix-a", 30);
    let path = claim.path().to_owned();
    let reference = world.reference(&claim);
    world.observe(&trace, &path, &reference, None);

    // An old reader that held the retained directory open across the exchange writes into it.
    std::fs::write(world.namespace.predecessors().join("old-reader.claim"), b"").unwrap();
    trace.step("ObserveUnknownPredecessor");
    world.observe(&trace, &path, &reference, None);
    assert!(claim.arm_cohort().is_err());
    assert!(
        trace
            .model
            .successors("DurableArm", &trace.state)
            .is_empty()
    );
    trace.step("RequestForce");
    let forced = crate::admit_with(
        &world.ledger,
        &world.machine,
        Need::new("prefix-b")
            .mem_gib(20)
            .checkout_exact(world.checkout.as_str())
            .unwrap(),
        crate::Admit::Now,
        crate::Pace::default(),
        &mut |_| {},
    );
    assert!(
        forced.is_err(),
        "a forced request was admitted beside an unknown predecessor"
    );
    world.observe(&trace, &path, &reference, None);
}

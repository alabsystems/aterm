// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `VzLifecycle` (`aterm_models_misc::vz_lifecycle_model`;
//! `docs/DESIGN-vz-guest-2026-10-23.md` §5.1, §12; slice V5).
//!
//! - The walk: the helper's real core ([`Core`]) with the Fake machine, its real run files
//!   ([`helper::take`] and the end `helper::run` makes), the real disk layers over a scratch
//!   state directory ([`Layers::prepare`], [`PartialBake::seal`]) and the real command client
//!   ([`aterm_vz::agent::run`]), walked over every state the model reaches without the
//!   world's steps. Each edge is replayed from the start and projected ([`project`]) onto
//!   exactly the model's successor; `Detect` is replayed for each detector that fits the
//!   state ([`VARIANTS`]); and at each state the inputs [`PROBES`] names, which the model
//!   has no step for, change nothing.
//! - The lab's side: [`Lab`] and [`Session`](aterm_vz::client::Session) over the Fake
//!   `Helper` ([`Threaded`]: the real core and serve loop on a thread), and the escalation and
//!   the leftovers over a stand-in, this binary holding the run files as the helper does (as
//!   `tests/stand_in.rs` runs it), each step bound to the model's.
//! - Every anchored step enters its own function, and the anchors and waivers cover every
//!   action ([`StepEvidence`], [`xref::check_closure`]).
//! - Negative controls: each planted defect ([`aterm_vz::fault`]), and each defective caller
//!   the harness plays, is the walk's first disagreement (or, where a case the walk meets
//!   later matters, the step after a path the model admits: [`control_at`]), which no step of
//!   the model admits and which is exactly its mutant's step, breaking that mutant's law. A
//!   helper that waits forever on its hard stop is no mutant's step: the walk sees it at
//!   `HardTimeout`.
//!
//! Every wait for something that must happen is a one-minute hang detector; only the lab's
//! "answers nothing" span is short, since nothing must happen within it.
#![cfg(target_vendor = "apple")]

aterm_durable::plain_durable_flushes_in_tests!();

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_models_misc::vz_lifecycle_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, SpecModule, StepEvidence};
use aterm_uds::spawnfd::{Child, Stdio};
use aterm_vz::client::fake::Threaded;
use aterm_vz::client::{ClientError, Lab, Start, UpError};
use aterm_vz::detect::SPAN;
use aterm_vz::fault::{self, Fault};
use aterm_vz::helper::{self, Config, Core, Flow, Held, Input};
use aterm_vz::layers::{Base, Host, Layers, PARTIAL, PartialBake, Prepared, baked_name};
use aterm_vz::machine::Event;
use aterm_vz::machine::fake::{Call, Fake, FakeEvent, Script, Seen, conn_pair};
use aterm_vz::plan::{COMMAND_LINE, Disk, Plan, mac_for};
use aterm_vz::proto::{Attach, PROTO, Phase, Reply, Request};
use aterm_vz::record::{DiskLock, Kernel, Paths, Pids, Record};
use aterm_vz::space::Free;
use aterm_vz::transport::Channel;
use aterm_vz_agent::wire::{self, Exit, Frame};

type State = BTreeMap<&'static str, i64>;
type Guard = fn(&State) -> bool;

/// The hang detector for what must happen.
const HANG: Duration = Duration::from_secs(60);

/// How long the lab lets a stand-in answer nothing (it never answers).
const SILENT: Duration = Duration::from_millis(500);

/// The core's clock between two steps.
const STEP: Duration = Duration::from_millis(1);

/// The agent the plan names, and another.
const AGENT: [u8; 32] = [0xab; 32];
const OTHER: [u8; 32] = [0xcd; 32];

/// The helper's content address its record and every `Attach` name.
const HELPER_SHA: &str = "4e1f00000000000000000000000000000000000000000000000000000000c0de";

/// The base's pin and the seed's fingerprint every baked disk here is named for.
const BASE_SHA: &str = "ba5e000000000000000000000000000000000000000000000000000000000001";
const SEED: &str = "5eed5eed5eed5eed";

/// The kernel's panic line, as hvc0 carries it.
const PANIC_LINE: &[u8] = b"[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs\n";

/// The anchors' machine.
const MACHINE: &str = "vz_lifecycle";

/// The world's steps and the lab's escalation, which the walk leaves to the stand-in traces.
const WORLD: [&str; 5] = ["Hang", "Die", "PidReused", "ClearDead", "Escalate"];

/// The steps no function of the crate performs: the lab's bring-up lock and the world's.
const ENVIRONMENT: [&str; 6] = [
    "UpLocks",
    "UpRefused",
    "UpUnlocks",
    "Hang",
    "Die",
    "PidReused",
];

/// The model's mutants.
const MUTANTS: [&str; 12] = [
    "SecondBoots",
    "ClonePartial",
    "AttachBaked",
    "PrepareUnderVm",
    "UpWithoutLock",
    "DropExit",
    "KeepRunning",
    "HardUnasked",
    "SignalReused",
    "ReadyUnchecked",
    "ExecBooting",
    "SealHalfMade",
];

/// The steps a function of the crate performs, each anchored on it.
const ANCHORED: [&str; 27] = [
    "Prepare",
    "Seal",
    "Start",
    "SecondRefused",
    "Attach",
    "AttachKeep",
    "AttachRefused",
    "KeepSet",
    "KeepClear",
    "Detach",
    "Quiet",
    "Boot",
    "BootRefused",
    "StartFails",
    "Hello",
    "HelloWrong",
    "Ready",
    "Detect",
    "Stop",
    "GraceOut",
    "HardTimeout",
    "Stopped",
    "StoppedWithError",
    "ExecBegin",
    "ExecExit",
    "ClearDead",
    "Escalate",
];

/// The inputs the model has no step for, each where the code must ignore it: a `Ready`
/// before the hello (a dial of the watch port whose first frame is a `Ready`), a second panic, a second `Boot`, a `Connect` on a VM that is not
/// `Ready`, the plan's agent saying hello again, and a tick within every span (no detector,
/// grace or hard stop due, room to spare).
const PROBES: [(&str, Guard); 6] = [
    ("probe:ReadyEarly", |s| {
        s["helper"] == 1 && s["phase"] == 1 && s["hello"] == 0
    }),
    ("probe:PanicAgain", |s| {
        s["helper"] == 1 && s["phase"] == 3 && s["hard"] == 1
    }),
    ("probe:BootAgain", |s| {
        s["helper"] == 1 && s["sessions"] > 0 && (s["phase"] == 1 || s["phase"] == 2)
    }),
    ("probe:ConnectNotReady", |s| {
        s["helper"] == 1 && s["sessions"] > 0 && s["phase"] != 2 && s["exec"] == 0
    }),
    ("probe:HelloAgain", |s| {
        s["helper"] == 1 && (s["phase"] == 1 || s["phase"] == 2) && s["hello"] == 1
    }),
    ("probe:Tick", |s| {
        s["helper"] == 1 && (1..=3).contains(&s["phase"])
    }),
];

/// A model step's other real inputs, each replayed where it applies and projected onto the
/// same successor: `Detect` is a panic line ([`step`]'s own), and also a tick a span later
/// with nothing new (the boot's detector before the hello, the start jobs' after it, the
/// heartbeat's after `Ready`) or one that reads the volume below the run floor.
const VARIANTS: [(&str, &str, Guard); 2] = [
    ("Detect", "Detect:silence", |s| {
        s["phase"] == 1 || s["phase"] == 2
    }),
    ("Detect", "Detect:space", |s| {
        s["phase"] == 1 || s["phase"] == 2
    }),
];

// ---------------------------------------------------------------------------------------------
// The model's side.

fn in_process(m: &Model) -> Model {
    let mut m = m.clone();
    m.actions.retain(|a| !WORLD.contains(&a.name));
    m
}

fn go(m: &Model, st: &mut State, actions: &[&str]) {
    for action in actions {
        assert!(
            m.fire(action, st),
            "VzLifecycle {action} is disabled at {st:?}"
        );
    }
}

fn key(st: &State) -> Vec<i64> {
    st.values().copied().collect()
}

// ---------------------------------------------------------------------------------------------
// The real side's pieces.

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn attach(keep: bool) -> Attach {
    Attach {
        proto: PROTO,
        helper_sha256: HELPER_SHA.into(),
        agent_sha256: hex(&AGENT),
        shares: Vec::new(),
        keep,
    }
}

fn plan(disk: &Path, console_log: &Path) -> Plan {
    Plan {
        name: "aterm-linux".into(),
        cpus: 2,
        memory_bytes: 2 << 30,
        kernel: "/vz/kernel/Image".into(),
        initrd: "/vz/kernel/initrd".into(),
        command_line: COMMAND_LINE.into(),
        disks: vec![Disk {
            path: disk.to_path_buf(),
            read_only: false,
        }],
        mac: mac_for("aterm-linux"),
        boot_share: "/vz/agent/ab".into(),
        shares: Vec::new(),
        rosetta: false,
        console_log: console_log.to_path_buf(),
        agent_sha256: hex(&AGENT),
    }
}

/// A hello naming `agent`, with this boot's key as guest root reads it off the key disk.
fn hello(agent: [u8; 32], paths: &Paths) -> Frame {
    let key = aterm_vz_agent::key::read_disk(&paths.key_disk())
        .expect("the key disk reads")
        .expect("the boot wrote its key disk");
    Frame::Hello(aterm_vz::machine::fake::hello(agent, key))
}

fn ready() -> Frame {
    Frame::Ready(wire::Ready {
        state: "running".into(),
        failed_units: Vec::new(),
    })
}

fn exec() -> wire::Exec {
    aterm_vz::agent::exec_of(&["true"], "/", &[])
}

fn free() -> Result<Free, String> {
    Ok(Free {
        bytes: 1 << 40,
        volume: "/tmp (the conformance's)".into(),
    })
}

/// A scratch state directory short enough for `sun_path` (`$TMPDIR` is not).
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path")
}

const fn phase_of(p: Phase) -> i64 {
    match p {
        Phase::Absent => 0,
        Phase::Booting => 1,
        Phase::Ready => 2,
        Phase::Stopping => 3,
        Phase::Stopped => 4,
        Phase::Failed => 5,
    }
}

/// Why a stop runs, as the model's `asked` names it, from the words the core keeps for it
/// (a soft stop's keep their start once its grace passes): a `Stop`'s and the release rule's
/// their own, the wrong agent's its NOT RUN, and every detector's (the boot's, the start
/// jobs', the heartbeat's, the free space's, a panic's) end `; VM stopped`. Anything else
/// asked nothing.
fn asked_of(why: Option<&str>) -> i64 {
    match why {
        Some(w) if w.starts_with("the VM was stopped by a Stop") => 2,
        Some(w) if w.starts_with("the last session closed with keep unset") => 3,
        Some(w)
            if w.starts_with("NOT RUN — the guest runs agent") || w.ends_with("; VM stopped") =>
        {
            1
        }
        _ => 0,
    }
}

fn file_id(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn int(b: bool) -> i64 {
    i64::from(b)
}

/// The layers' host: the real APFS clone, each one noted, and room to spare.
#[derive(Default)]
struct Clones(Mutex<Vec<(PathBuf, PathBuf)>>);

impl Host for Clones {
    fn free(&self, _path: &Path) -> Result<Free, String> {
        free()
    }

    fn clone_file(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push((src.to_path_buf(), dst.to_path_buf()));
        aterm_vz::clone::clone_file(src, dst)
    }

    fn make_iso(&self, _files: &Path, _iso: &Path) -> Result<(), String> {
        Err("the conformance makes no seed".into())
    }
}

/// The disk layers under a state directory, with a small base beside them.
struct Disks {
    layers: Layers,
    host: Clones,
    base: Base,
}

/// The small base at `dir/base.img`.
fn base_in(dir: &Path) -> PathBuf {
    let path = dir.join("base.img");
    std::fs::write(&path, vec![0x5a; 4096]).expect("the base");
    path
}

impl Disks {
    fn new(root: &Path) -> Self {
        Self::over(root, base_in(root))
    }

    /// The layers under `root` over the base at `base` (a walk's, which its rigs clone).
    fn over(root: &Path, base: PathBuf) -> Self {
        Self {
            layers: Layers::at(root).with_disk_bytes(1 << 20),
            host: Clones::default(),
            base: Base {
                path: base,
                sha256: BASE_SHA.into(),
            },
        }
    }

    fn prepare(&self) -> Result<Prepared, aterm_vz::Error> {
        self.layers
            .prepare(&self.host, &self.base, SEED, &mut |_| {})
    }

    fn sealed(&self) -> PathBuf {
        self.layers.baked_dir().join(baked_name(BASE_SHA, SEED))
    }

    fn partial(&self) -> PathBuf {
        let name = baked_name(BASE_SHA, SEED);
        let stem = name.strip_suffix(".img").unwrap_or(&name);
        self.layers.baked_dir().join(format!("{stem}{PARTIAL}"))
    }

    /// `bake`: the sealed baked disk, else a bake's partial file, else none.
    fn bake(&self) -> i64 {
        if self.sealed().is_file() {
            2
        } else {
            int(self.partial().is_file())
        }
    }

    /// `live`: the live disk, and whether its clone came from a partial file.
    fn live(&self) -> i64 {
        if !self.layers.live_disk().is_file() {
            return 0;
        }
        let into = self.layers.live_dir().join(format!("aterm-linux{PARTIAL}"));
        let from_partial = self
            .host
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, dst)| *dst == into)
            .is_some_and(|(src, _)| src.to_string_lossy().ends_with(PARTIAL));
        if from_partial { 2 } else { 1 }
    }

    /// `disk`: which layer `disk` is.
    fn class(&self, disk: &Path) -> i64 {
        if disk.to_string_lossy().ends_with(PARTIAL) {
            1
        } else if disk == self.layers.live_disk() {
            2
        } else {
            3
        }
    }
}

/// The evidence window of a step, or none (a replay's prefix, a probe).
struct Win<'a>(Option<&'a mut StepEvidence>);

impl Win<'_> {
    fn on<T>(&mut self, action: &'static str, f: impl FnOnce() -> T) -> T {
        match self.0.as_deref_mut() {
            Some(ev) => ev.step(action, f),
            None => f(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The walk's real system.

/// One helper's life: its core, its Fake, its run files and the labs' ends of its sessions.
struct Life {
    core: Core<Fake>,
    seen: Seen,
    held: Held,
    /// What the watch dials' readers post.
    rx: Receiver<Input>,
    /// The guest's end of the watch connection, once a keyed hello took its dial.
    watch: Option<UnixStream>,
    born: Duration,
    labs: Vec<(u64, Channel)>,
    heard: BTreeMap<u64, Vec<(Reply, Option<OwnedFd>)>>,
    attached: Vec<u64>,
    next: u64,
}

impl Life {
    /// A control connection opened at `now`: its id.
    fn open(&mut self, now: Duration) -> u64 {
        let (ours, theirs) = UnixStream::pair().expect("a session's socket pair");
        theirs
            .set_nonblocking(true)
            .expect("the lab's end reads without waiting");
        let sid = self.next;
        self.next += 1;
        let _ = self.core.handle(Input::Opened(sid, ours), now);
        self.labs.push((sid, Channel::new(theirs)));
        sid
    }

    /// Read every reply the core wrote (the core writes each whole before it returns).
    fn drain(&mut self) {
        for (sid, chan) in &mut self.labs {
            while let Ok(Some(got)) = chan.recv_reply() {
                self.heard.entry(*sid).or_default().push(got);
            }
        }
    }

    fn heard(&self, sid: u64, want: fn(&Reply) -> bool) -> bool {
        self.heard
            .get(&sid)
            .is_some_and(|replies| replies.iter().any(|(r, _)| want(r)))
    }

    fn connected_fd(&mut self, sid: u64) -> Option<OwnedFd> {
        let replies = self.heard.get_mut(&sid)?;
        let at = replies
            .iter()
            .position(|(r, fd)| matches!(r, Reply::Connected { .. }) && fd.is_some())?;
        replies.remove(at).1
    }

    fn hard(&self) -> bool {
        self.seen.calls().contains(&Call::HardStop)
    }

    /// The guest writes `frame` on its watch connection, or, before a hello took one, first
    /// on a new dial of the watch port (the machine's `Event::Watch`); the core takes the
    /// dial, then what the dial's reader posted: the frame, or the dial refused.
    fn say(&mut self, frame: &Frame, now: Duration) -> Flow {
        let (mut guest, mut flow) = match self.watch.take() {
            Some(guest) => (guest, Flow::Go),
            None => {
                let (conn, guest) = conn_pair(aterm_vz_agent::WATCH_PORT, &self.seen.live_conns);
                let flow = self.core.handle(Input::Machine(Event::Watch(conn)), now);
                (guest, flow)
            }
        };
        wire::write_frame(&mut guest, frame).expect("the guest writes into its socket buffer");
        let posted = self
            .rx
            .recv_timeout(HANG)
            .expect("the dial's reader posts within a minute");
        let taken = matches!(posted, Input::Watch(..));
        if flow == Flow::Go {
            flow = self.core.handle(posted, now);
        }
        if taken {
            self.watch = Some(guest);
        }
        flow
    }
}

/// What a helper's life left once it ended.
#[derive(Clone, Copy, Debug, Default)]
struct Last {
    phase: i64,
    keep: i64,
    asked: i64,
    hard: i64,
}

/// The real system one walk path drives: a state directory with its layers and run files,
/// the lab's locks and plan, at most one helper, and the command in flight.
struct Rig {
    _dir: aterm_tempfile::TempDir,
    paths: Paths,
    disks: Disks,
    ups: Vec<DiskLock>,
    bake_plan: Option<PartialBake>,
    disk: Option<PathBuf>,
    disk_id: Option<(u64, u64)>,
    prepared: bool,
    life: Option<Life>,
    last: Last,
    clock: Duration,
    hello: i64,
    ran: i64,
    exec: Option<(OwnedFd, UnixStream)>,
    outcome: i64,
    second: bool,
    pulled: bool,
    boots: VecDeque<bool>,
    low: Arc<AtomicBool>,
}

/// Each helper life's boot answer along `actions`: the framework refuses the configuration
/// in a life whose boot is the model's `BootRefused`.
fn boots<'a>(actions: impl Iterator<Item = &'a str>) -> VecDeque<bool> {
    let mut out = VecDeque::new();
    let mut open = false;
    for a in actions {
        match a {
            "Start" => {
                out.push_back(true);
                open = true;
            }
            "Boot" if open => open = false,
            "BootRefused" if open => {
                if let Some(last) = out.back_mut() {
                    *last = false;
                }
                open = false;
            }
            _ => {}
        }
    }
    out
}

impl Rig {
    fn new(boots: VecDeque<bool>, base: &Path) -> Self {
        let dir = scratch();
        let paths = Paths::at(dir.path());
        std::fs::create_dir_all(paths.run_dir()).expect("run/");
        let disks = Disks::over(dir.path(), base.to_path_buf());
        Self {
            _dir: dir,
            paths,
            disks,
            ups: Vec::new(),
            bake_plan: None,
            disk: None,
            disk_id: None,
            prepared: false,
            life: None,
            last: Last::default(),
            clock: Duration::ZERO,
            hello: 0,
            ran: 0,
            exec: None,
            outcome: 0,
            second: false,
            pulled: false,
            boots,
            low: Arc::new(AtomicBool::new(false)),
        }
    }

    fn up_lock(&self) -> PathBuf {
        self.paths.run_dir().join("bring-up.lock")
    }

    fn phase(&self) -> i64 {
        self.life
            .as_ref()
            .map_or(self.last.phase, |l| phase_of(l.core.phase()))
    }

    fn running_on_live(&self) -> bool {
        matches!(self.phase(), 1..=3)
            && self
                .disk
                .as_deref()
                .is_some_and(|d| self.disks.class(d) == 2)
    }

    /// The lab's plan from what `prepare` handed back.
    fn plan(&mut self, prepared: Prepared) {
        let disk = match prepared {
            Prepared::Live { disk, .. } => disk,
            Prepared::Bake(bake) => {
                let disk = bake.path().to_path_buf();
                self.bake_plan = Some(bake);
                disk
            }
        };
        self.disk_id = file_id(&disk);
        self.disk = Some(disk);
        self.prepared = true;
    }

    /// `helper::take` and the core, as `helper::run` makes them.
    fn start(&mut self, win: &mut Win<'_>) {
        let ident = helper::this_process(HELPER_SHA.into(), self.paths.live_disk());
        let taken = win.on("Start", || helper::take(&self.paths, &ident, &Kernel));
        let Ok(held) = taken else {
            return;
        };
        let ok = self.boots.pop_front().unwrap_or(true);
        let (fake, seen) = Fake::new(Script {
            boot: if ok {
                Ok(())
            } else {
                Err("the virtual machine configuration is invalid (the entitlement)".into())
            },
            on_boot: Vec::new(),
            hard_stop_completes: false,
            guest_obeys: false,
            ..Script::default()
        });
        let (inputs, rx) = std::sync::mpsc::channel();
        let low = Arc::clone(&self.low);
        let cfg = Config {
            ident,
            record: Some((self.paths.record(), held.record.clone())),
            hold_conns: helper::HOLD_CONNS,
            key_disk: self.paths.key_disk(),
            free: Box::new(move || {
                if low.load(Ordering::SeqCst) {
                    Ok(Free {
                        bytes: 3 << 30,
                        volume: "/tmp (the conformance's, read low)".into(),
                    })
                } else {
                    free()
                }
            }),
        };
        self.life = Some(Life {
            core: Core::new(fake, cfg, inputs),
            seen,
            held,
            rx,
            watch: None,
            born: self.clock,
            labs: Vec::new(),
            heard: BTreeMap::new(),
            attached: Vec::new(),
            next: 1,
        });
        self.last = Last::default();
        self.hello = 0;
        self.ran = 0;
        self.exec = None;
        self.outcome = 0;
        self.disk = None;
        self.disk_id = None;
        self.bake_plan = None;
        self.prepared = false;
    }

    /// A second `helper::take` while a helper holds the run files.
    fn second_take(&mut self, win: &mut Win<'_>) {
        let ident = helper::this_process(HELPER_SHA.into(), self.paths.live_disk());
        match win.on("SecondRefused", || {
            helper::take(&self.paths, &ident, &Kernel)
        }) {
            Err(words) => assert!(
                words.contains("is held by helper")
                    && words.contains("a second helper for the same disk does not start"),
                "{words}"
            ),
            Ok(second) => {
                assert!(
                    fault::armed(Fault::NoFlock),
                    "a second helper took the run files the first holds"
                );
                // With the first's VM on the live disk, the second would boot a second VM on it.
                self.second |= self.running_on_live();
                drop(second);
            }
        }
    }

    /// The end `helper::run` makes once its serve loop exits with `words`: its run files
    /// ([`helper::release_run_files`]), then the sessions, then the lock. Its VM ends with it
    /// (fact 44: the VM process exits once its helper is gone), so a VM it left booting,
    /// ready or stopping is stopped, and every connection into it ends: a command in flight
    /// reads its connection's end. A bake's VM that was `Ready` and whose guest then powered
    /// itself off ([`helper::POWERED_OFF`], the words the lab seals after) finished (§4's
    /// step 4).
    fn end(&mut self, words: &str) {
        let Some(life) = self.life.take() else {
            return;
        };
        let asked = asked_of(life.core.stopping_why());
        let phase = life.core.phase();
        self.last = Last {
            phase: match phase {
                Phase::Booting | Phase::Ready | Phase::Stopping => 4,
                other => phase_of(other),
            },
            keep: int(life.core.keep()),
            asked,
            hard: int(life.hard()),
        };
        if self.ran == 1 && words == format!("the VM stopped: {}", helper::POWERED_OFF) {
            self.ran = 2;
        }
        life.seen.guest_ends.lock().unwrap().clear();
        if let Some((fd, guest)) = self.exec.take() {
            drop(guest);
            let ran = aterm_vz::agent::run(fd, &exec(), HANG);
            self.outcome = if ran.is_ok() { 1 } else { 2 };
        }
        let Life { core, held, .. } = life;
        helper::release_run_files(&self.paths, &held.record, &held.socket);
        drop(core);
        drop(held);
    }

    /// `PartialBake::seal` on the bake this up prepared: once it finished it is whole;
    /// otherwise it is a half-made bake under its final name (`ran` 3).
    fn seal(&mut self, win: &mut Win<'_>) {
        let bake = self.bake_plan.take().expect("a bake to seal");
        win.on("Seal", || bake.seal()).expect("the bake is sealed");
        self.disk = None;
        self.disk_id = None;
        self.ran = if self.ran == 2 { 0 } else { 3 };
    }

    /// What the walk reads of the core after each step: a bake's VM that reached `Ready`.
    fn note_ready(&mut self) {
        let ready = self
            .life
            .as_ref()
            .is_some_and(|l| l.core.phase() == Phase::Ready);
        if ready && self.ran == 0 {
            let on_bake = self
                .disk
                .as_deref()
                .is_some_and(|d| self.disks.class(d) == 1);
            self.ran = int(on_bake);
        }
    }

    /// The disk a VM runs on, or the lab prepared for a boot it has not made, was removed
    /// or replaced.
    fn check_pulled(&mut self) {
        if !(self.prepared || matches!(self.phase(), 1..=3)) {
            return;
        }
        if let (Some(disk), Some(id)) = (&self.disk, self.disk_id)
            && file_id(disk) != Some(id)
        {
            self.pulled = true;
        }
    }
}

/// The projection every anchor names: the real system onto `VzLifecycle`'s variables.
fn project(rig: &Rig) -> State {
    let (helper, phase, sessions, keep, asked, hard) = match &rig.life {
        Some(l) => (
            1,
            phase_of(l.core.phase()),
            i64::try_from(l.core.attached()).unwrap_or(i64::MAX),
            int(l.core.keep()),
            asked_of(l.core.stopping_why()),
            int(l.hard()),
        ),
        None => (
            0,
            rig.last.phase,
            0,
            rig.last.keep,
            rig.last.asked,
            rig.last.hard,
        ),
    };
    [
        ("bake", rig.disks.bake()),
        ("live", rig.disks.live()),
        (
            "disk",
            rig.disk.as_deref().map_or(0, |d| rig.disks.class(d)),
        ),
        ("up", i64::try_from(rig.ups.len()).unwrap_or(i64::MAX)),
        ("prepared", int(rig.prepared)),
        ("helper", helper),
        ("phase", phase),
        ("sessions", sessions),
        ("keep", keep),
        ("hello", rig.hello),
        ("asked", asked),
        ("hard", hard),
        ("ran", rig.ran),
        ("exec", int(rig.exec.is_some())),
        ("outcome", rig.outcome),
        ("second", int(rig.second)),
        ("pulled", int(rig.pulled)),
        ("wrong_kill", 0),
    ]
    .into_iter()
    .collect()
}

/// One step of the walk: the model's `action` (or a probe, or a defective caller) on the
/// real system, `win` around the anchored call it is.
#[allow(clippy::too_many_lines)]
fn step(rig: &mut Rig, action: &'static str, win: &mut Win<'_>) {
    rig.clock += STEP;
    let flow = match action {
        "UpLocks" | "UpRefused" => {
            if let Some(lock) = DiskLock::try_take(&rig.up_lock()).expect("the bring-up lock") {
                rig.ups.push(lock);
            }
            None
        }
        "UpUnlocks" => {
            rig.ups.clear();
            rig.prepared = false;
            if rig.phase() == 0 {
                rig.disk = None;
                rig.disk_id = None;
                rig.bake_plan = None;
            }
            None
        }
        "Prepare" => {
            let prepared = win
                .on(action, || rig.disks.prepare())
                .expect("the disks are prepared");
            rig.plan(prepared);
            None
        }
        "defect:PrepareUnderVm" | "defect:UpWithoutLock" => {
            // A lab with no gate: it prepares while a VM runs, or with no lock while another
            // up's prepared boot waits.
            let _ = rig.disks.prepare().expect("the disks are prepared");
            None
        }
        "Seal" => {
            rig.seal(win);
            None
        }
        "defect:SealHalfMade" => {
            // A lab that seals its bake whatever stopped the bake's VM.
            rig.seal(&mut Win(None));
            None
        }
        "Start" => {
            rig.start(win);
            None
        }
        "SecondRefused" => {
            rig.second_take(win);
            None
        }
        "ExecExit" => {
            let (fd, mut guest) = rig.exec.take().expect("a command in flight");
            wire::write_frame(&mut guest, &Frame::Exit(Exit::Exited(0)))
                .expect("the guest's agent writes the command's Exit");
            guest
                .shutdown(std::net::Shutdown::Write)
                .expect("and closes its side");
            let ran = win.on(action, || aterm_vz::agent::run(fd, &exec(), HANG));
            // Its agent wrote its Exit: a `Lost` now is that Exit dropped.
            rig.outcome = if ran.is_ok() { 1 } else { 3 };
            drop(guest);
            None
        }
        _ => {
            let life = rig
                .life
                .as_mut()
                .unwrap_or_else(|| panic!("{action} needs a helper"));
            let mut now = rig.clock - life.born;
            Some(match action {
                "Attach" | "AttachKeep" | "AttachRefused" => {
                    let sid = life.open(now);
                    let a = attach(action == "AttachKeep");
                    let flow = win.on(action, || {
                        life.core
                            .handle(Input::Request(sid, Request::Attach(a)), now)
                    });
                    life.drain();
                    if life.heard(sid, |r| matches!(r, Reply::Attached { .. })) {
                        life.attached.push(sid);
                    }
                    flow
                }
                "KeepSet" | "KeepClear" => {
                    let sid = life.attached[0];
                    let keep = action == "KeepSet";
                    win.on(action, || {
                        life.core
                            .handle(Input::Request(sid, Request::Keep(keep)), now)
                    })
                }
                "Detach" => {
                    let sid = life.attached.pop().expect("an attached session");
                    win.on(action, || {
                        life.core.handle(Input::Request(sid, Request::Detach), now)
                    })
                }
                "Quiet" | "GraceOut" | "HardTimeout" => {
                    rig.clock += SPAN;
                    now += SPAN;
                    win.on(action, || life.core.handle(Input::Tick, now))
                }
                "Detect:silence" => {
                    // A span with nothing new: the detector the boot's progress names.
                    let want = match (life.core.phase(), rig.hello) {
                        (Phase::Ready, _) => "the guest stopped answering (no heartbeat",
                        (_, 1) => "NOT RUN — the boot hung after the agent",
                        _ => "NOT RUN — the boot hung before the agent",
                    };
                    rig.clock += SPAN;
                    now += SPAN;
                    let flow = win.on("Detect", || life.core.handle(Input::Tick, now));
                    let why = life.core.stopping_why().unwrap_or_default();
                    assert!(why.starts_with(want), "{want}: {why}");
                    flow
                }
                "Detect:space" => {
                    rig.low.store(true, Ordering::SeqCst);
                    let flow = win.on("Detect", || life.core.handle(Input::Tick, now));
                    let why = life.core.stopping_why().unwrap_or_default();
                    assert!(why.starts_with("the volume holding"), "{why}");
                    flow
                }
                "probe:Tick" => life.core.handle(Input::Tick, now),
                "Boot" | "BootRefused" | "probe:BootAgain" => {
                    let sid = life.attached[0];
                    let disk = rig.disk.clone().expect("a prepared disk");
                    let p = Box::new(plan(&disk, &rig.paths.console_log()));
                    if action == "probe:BootAgain" {
                        life.core.handle(Input::Request(sid, Request::Boot(p)), now)
                    } else {
                        rig.prepared = false;
                        win.on(action, || {
                            life.core.handle(Input::Request(sid, Request::Boot(p)), now)
                        })
                    }
                }
                "StartFails" => win.on(action, || {
                    let failed = Event::StartFailed("the virtual machine failed to start".into());
                    life.core.handle(Input::Machine(failed), now)
                }),
                "Hello" | "HelloWrong" => {
                    let right = action == "Hello";
                    rig.hello = if right { 1 } else { 2 };
                    let frame = hello(if right { AGENT } else { OTHER }, &rig.paths);
                    win.on(action, || life.say(&frame, now))
                }
                "probe:HelloAgain" => life.say(&hello(AGENT, &rig.paths), now),
                "Ready" => win.on(action, || life.say(&ready(), now)),
                // Before a hello: a dial whose first frame is a `Ready`.
                "probe:ReadyEarly" => life.say(&ready(), now),
                "Detect" => win.on(action, || {
                    let line = Event::Console(PANIC_LINE.to_vec());
                    life.core.handle(Input::Machine(line), now)
                }),
                "probe:PanicAgain" => {
                    let line = Event::Console(PANIC_LINE.to_vec());
                    life.core.handle(Input::Machine(line), now)
                }
                "Stop" => {
                    let sid = life.open(now);
                    win.on(action, || {
                        let stop = Request::Stop { grace_ms: 60_000 };
                        life.core.handle(Input::Request(sid, stop), now)
                    })
                }
                "Stopped" | "StoppedWithError" => {
                    // The helper exits with its VM gone ([`Rig::end`] ends what was in it).
                    let event = if action == "StoppedWithError" {
                        Event::StoppedWithError("Internal Virtualization error".into())
                    } else if life.hard() {
                        Event::HardStopped(Ok(()))
                    } else {
                        Event::GuestStopped
                    };
                    win.on(action, || life.core.handle(Input::Machine(event), now))
                }
                "ExecBegin" | "probe:ConnectNotReady" => {
                    let sid = life.attached[0];
                    let connect = Request::Connect {
                        port: aterm_vz_agent::COMMAND_PORT,
                    };
                    let flow = if action == "ExecBegin" {
                        win.on(action, || {
                            life.core.handle(Input::Request(sid, connect), now)
                        })
                    } else {
                        life.core.handle(Input::Request(sid, connect), now)
                    };
                    life.drain();
                    if let Some(fd) = life.connected_fd(sid) {
                        let guest = life
                            .seen
                            .guest_ends
                            .lock()
                            .unwrap()
                            .pop()
                            .expect("the guest's end of the connection");
                        rig.exec = Some((fd, guest));
                        rig.outcome = 0;
                    }
                    flow
                }
                other => panic!("no real step for {other}"),
            })
        }
    };
    if let Some(Flow::Exit(words)) = &flow {
        rig.end(words);
    }
    if let Some(life) = &mut rig.life {
        life.drain();
    }
    rig.note_ready();
    rig.check_pulled();
}

/// The real system after `path`, then `last` in its evidence window, its layers over `base`.
fn replay(
    path: &[&'static str],
    last: &'static str,
    base: &Path,
    ev: Option<&mut StepEvidence>,
) -> Rig {
    let mut rig = Rig::new(boots(path.iter().copied().chain([last])), base);
    for action in path {
        step(&mut rig, action, &mut Win(None));
    }
    step(&mut rig, last, &mut Win(ev));
    rig
}

/// How many replays run at once across this binary's walks. Each makes and removes its own
/// state directory, and the kernel's file system path stops gaining past four at once
/// (measured 2026-10-28: one process's threads churning such trees ran 1.35 times one
/// thread's rate at two, 1.6 at four and 0.7 at eight; four whole walks at once, 2.3 times
/// one), so a few, not every core.
const REPLAYERS: usize = 4;

/// The replays running now across this binary's walks, and how many have ended.
static REPLAYING: (Mutex<(usize, u64)>, Condvar) = (Mutex::new((0, 0)), Condvar::new());

/// `f` once fewer than [`REPLAYERS`] replays run. What the wait needs is some replay's end,
/// and each wait a replay makes is a one-minute hang detector, so the slot wait's window is
/// two minutes: a replay stuck at one of its own waits fails with its own cause first, and two
/// minutes with every slot taken and no replay of this binary ending is a hang the replays'
/// detectors missed. It fails the waiting check, not the walk, which joins every replay of
/// its depth before it reads one.
fn in_slot<T>(f: impl FnOnce() -> T) -> T {
    struct Free;
    impl Drop for Free {
        fn drop(&mut self) {
            let mut slots = REPLAYING.0.lock().unwrap_or_else(PoisonError::into_inner);
            slots.0 -= 1;
            slots.1 += 1;
            drop(slots);
            REPLAYING.1.notify_one();
        }
    }
    let mut slots = REPLAYING.0.lock().unwrap_or_else(PoisonError::into_inner);
    while slots.0 >= REPLAYERS {
        let ended = slots.1;
        let (held, waited) = REPLAYING
            .1
            .wait_timeout_while(slots, 2 * HANG, |&mut (running, now)| {
                running >= REPLAYERS && now == ended
            })
            .unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() {
            // Not under the lock, which a panic there would poison for every later wait.
            drop(held);
            panic!("a replay of this binary ends within two minutes");
        }
        slots = held;
    }
    slots.0 += 1;
    drop(slots);
    let _free = Free;
    f()
}

/// `f` with `fault` armed on this thread, if there is one.
fn armed<T>(fault: Option<Fault>, run: impl FnOnce() -> T) -> T {
    match fault {
        Some(f) => fault::with(f, run),
        None => run(),
    }
}

/// The walk's first disagreement: the path to it, its step, and both states.
#[derive(Debug)]
struct Off {
    path: Vec<&'static str>,
    step: &'static str,
    prev: State,
    observed: State,
}

#[derive(Debug, Default)]
struct Walked {
    states: usize,
    edges: usize,
    variants: usize,
    probes: usize,
    fired: BTreeSet<&'static str>,
    off: Option<Off>,
}

/// What one check of the walk replays: the model's step (`Edge`), another real input of it
/// (`Variant`), or an input that must change nothing (`Probe`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Edge,
    Variant,
    Probe,
}

/// The checks at `st`, in the order the walk reads them: each enabled step onto the model's
/// successor, each of its [`VARIANTS`] that applies onto the same, then each probe that
/// applies onto `st` itself.
fn checks_at(
    m: &Model,
    probes: &[(&'static str, Guard)],
    st: &State,
) -> Vec<(&'static str, Check, State)> {
    let mut checks: Vec<(&'static str, Check, State)> = Vec::new();
    for action in &m.actions {
        if let Some(next) = m.successors(action.name, st).into_iter().next() {
            for (base, variant, applies) in VARIANTS {
                if base == action.name && applies(st) {
                    checks.push((variant, Check::Variant, next.clone()));
                }
            }
            checks.push((action.name, Check::Edge, next));
        }
    }
    for (probe, applies) in probes {
        if applies(st) {
            checks.push((probe, Check::Probe, st.clone()));
        }
    }
    checks.sort_by_key(|(_, check, _)| match check {
        Check::Edge => 0,
        Check::Variant => 1,
        Check::Probe => 2,
    });
    checks
}

/// One check's replay on a thread of its own, `fault` armed on it: what it projects to and
/// its step's evidence window (none for a probe, or where the walk keeps none), or the panic
/// it raised, which the walk raises again where it reads this check. Nothing once it has its
/// slot and `unread` says the walk will not read it.
fn replayed(
    path: &[&'static str],
    action: &'static str,
    check: Check,
    base: &Path,
    fault: Option<Fault>,
    witness: bool,
    unread: impl FnOnce() -> bool,
) -> Option<std::thread::Result<(State, Option<StepEvidence>)>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        in_slot(|| {
            if unread() {
                return None;
            }
            Some(armed(fault, || {
                let mut window =
                    (witness && check != Check::Probe).then(|| StepEvidence::new(MACHINE));
                let rig = replay(path, action, base, window.as_mut());
                let observed = project(&rig);
                drop(rig);
                (observed, window)
            }))
        })
    }))
    .transpose()
}

/// Walk `m` breadth first, with `fault` armed: every edge from every reachable state replayed
/// on the real system and projected onto exactly the model's successor, each of its
/// [`VARIANTS`] onto the same successor, and every probe at every state where it applies a
/// stutter. Each depth's replays run at once ([`REPLAYERS`]), and are read back in the order
/// one replay at a time would make them; a check past the first that disagrees or panics is
/// not replayed, since the walk never reads it. `strict` panics at a disagreement; otherwise
/// the walk stops at its first.
fn walk(
    m: &Model,
    probes: &[(&'static str, Guard)],
    fault: Option<Fault>,
    mut ev: Option<&mut StepEvidence>,
    strict: bool,
) -> Walked {
    let mut w = Walked::default();
    let shared = scratch();
    let base = base_in(shared.path());
    let init = m.init_state();
    assert_eq!(
        armed(fault, || project(&Rig::new(VecDeque::new(), &base))),
        init,
        "a scratch state directory is the model's start"
    );
    let witness = ev.is_some();
    let mut seen: HashSet<Vec<i64>> = HashSet::from([key(&init)]);
    let mut depth: Vec<(State, Vec<&'static str>)> = vec![(init, Vec::new())];
    while !depth.is_empty() {
        let checks: Vec<Vec<(&'static str, Check, State)>> = depth
            .iter()
            .map(|(st, _)| checks_at(m, probes, st))
            .collect();
        let items: Vec<(usize, &[&'static str], &'static str, Check, &State)> = depth
            .iter()
            .zip(&checks)
            .flat_map(|((_, path), here)| {
                here.iter()
                    .map(move |(action, check, next)| (path.as_slice(), *action, *check, next))
            })
            .enumerate()
            .map(|(at, (path, action, check, next))| (at, path, action, check, next))
            .collect();
        // The depth's first check that disagrees or panics, of those seen so far. It only
        // falls, so every check before the one the walk stops at is replayed, and since the
        // workers take checks in order, only those already started run past it.
        let first_off = AtomicUsize::new(usize::MAX);
        let mut replays = aterm_spec::check::each(&items, |&(at, path, action, check, next)| {
            let replay = replayed(path, action, check, &base, fault, witness, || {
                first_off.load(Ordering::Relaxed) < at
            })?;
            if !replay.as_ref().is_ok_and(|(observed, _)| observed == next) {
                first_off.fetch_min(at, Ordering::Relaxed);
            }
            Some(replay)
        })
        .into_iter();
        let mut deeper = Vec::new();
        for ((st, path), here) in depth.iter().zip(checks) {
            w.states += 1;
            for (action, check, next) in here {
                let (observed, window) = replays
                    .next()
                    .expect("a result for every check")
                    .expect("every check the walk reads is replayed")
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                if let (Some(ev), Some(window)) = (ev.as_deref_mut(), window) {
                    ev.absorb(window);
                }
                match check {
                    Check::Edge => {
                        w.edges += 1;
                        w.fired.insert(action);
                    }
                    Check::Variant => w.variants += 1,
                    Check::Probe => w.probes += 1,
                }
                if observed == next {
                    if check == Check::Edge && seen.insert(key(&next)) {
                        let mut to = path.clone();
                        to.push(action);
                        deeper.push((next, to));
                    }
                    continue;
                }
                let off = Off {
                    path: path.clone(),
                    step: action,
                    prev: st.clone(),
                    observed,
                };
                assert!(
                    !strict,
                    "the real system disagrees with VzLifecycle at {action}: the model's {next:?}, \
                     {off:#?}"
                );
                w.off = Some(off);
                return w;
            }
        }
        depth = deeper;
    }
    w
}

// ---------------------------------------------------------------------------------------------
// The lab's side over the Fake `Helper`.

/// What the lab reads of the helper and the disks, against the model's `st`: the helper's
/// `State` (phase, `keep`, sessions) by `status`, which opens no session, and the layers'
/// files. A step the lab sent may still be on its way to the core, so this waits for the
/// agreement within the hang detector.
fn agrees(st: &State, lab: &mut Lab<Threaded>, disks: &Disks, what: &str) {
    let begun = Instant::now();
    loop {
        let seen: State = match lab.status().expect("status") {
            Some(s) => [
                ("helper", 1),
                ("phase", phase_of(s.phase)),
                ("sessions", i64::from(s.sessions)),
                ("keep", int(s.keep)),
            ]
            .into_iter()
            .collect(),
            None => [("helper", 0)].into_iter().collect(),
        };
        let seen: State = seen
            .into_iter()
            .chain([("bake", disks.bake()), ("live", disks.live())])
            .collect();
        if seen.iter().all(|(k, v)| st[k] == *v) {
            return;
        }
        assert!(
            begun.elapsed() < HANG,
            "{what}: the lab reads {seen:?}, the model is at {st:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The lab's whole life over the Fake `Helper`: `up`'s session, bring-up and command, a
/// second session that joins and keeps it, the detaches, and `down`, which leaves its bake
/// unsealed; a second helper that bakes again and ends as the lab's `end_bake` ends a bake
/// (the guest's own power-off, then the seal); and a third whose boot the framework refuses.
#[allow(clippy::too_many_lines)]
fn client_trace(m: &Model, ev: &mut StepEvidence) {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let disks = Disks::new(dir.path());
    let starts = Arc::new(AtomicUsize::new(0));
    let made = Arc::clone(&starts);
    let mut lab = Lab::new(
        paths.clone(),
        Threaded::with(
            Box::new(move || {
                let n = made.fetch_add(1, Ordering::SeqCst);
                Fake::new(Script {
                    boot: if n <= 1 {
                        Ok(())
                    } else {
                        Err("the virtual machine configuration is invalid".into())
                    },
                    on_boot: vec![
                        FakeEvent::Started,
                        FakeEvent::Agent {
                            agent_sha256: AGENT,
                            ready: true,
                        },
                    ],
                    guest_obeys: true,
                    ..Script::default()
                })
            }),
            HELPER_SHA,
        ),
    );
    let mut st = m.init_state();
    std::fs::create_dir_all(paths.run_dir()).expect("run/");
    let _up = DiskLock::try_take(&paths.run_dir().join("bring-up.lock"))
        .expect("the bring-up lock")
        .expect("no other up");
    go(m, &mut st, &["UpLocks"]);

    let mut a = ev
        .step("Attach", || lab.session(&attach(false)))
        .expect("a session on the helper the lab started");
    go(m, &mut st, &["Start", "Attach"]);
    agrees(&st, &mut lab, &disks, "up's session");

    let mut bake: Option<PartialBake> = None;
    let console = paths.console_log();
    let up = ev.step("Prepare", || {
        a.bring_up(
            || {
                Ok::<Plan, aterm_vz::Error>(match disks.prepare()? {
                    Prepared::Bake(b) => {
                        let p = plan(b.path(), &console);
                        bake = Some(b);
                        p
                    }
                    Prepared::Live { disk, .. } => plan(&disk, &console),
                })
            },
            &mut |_| {},
        )
    });
    assert_eq!(up.expect("the VM is Ready").agent_sha256, AGENT);
    go(m, &mut st, &["Prepare", "Boot", "Hello", "Ready"]);
    agrees(&st, &mut lab, &disks, "the bake's VM is Ready");

    let mut b = ev
        .step("AttachKeep", || lab.join(&attach(true)))
        .expect("a second session joins");
    go(m, &mut st, &["AttachKeep"]);
    let mut prepared = false;
    b.bring_up(
        || {
            prepared = true;
            Err::<Plan, ()>(())
        },
        &mut |_| {},
    )
    .expect("the second session waits for the Ready the first booted");
    assert!(
        !prepared,
        "a session that found a VM prepares nothing under it (bring_up's gate)"
    );
    ev.step("KeepClear", || b.keep(false)).expect("Keep");
    ev.step("KeepSet", || b.keep(true)).expect("Keep");
    go(m, &mut st, &["KeepClear", "KeepSet"]);
    agrees(&st, &mut lab, &disks, "keep");

    let (_, fd) = ev
        .step("ExecBegin", || a.connect(aterm_vz_agent::COMMAND_PORT))
        .expect("a command connection");
    go(m, &mut st, &["ExecBegin"]);
    let mut guest = lab
        .start
        .last()
        .guest_ends
        .lock()
        .unwrap()
        .pop()
        .expect("the guest's end of it");
    wire::write_frame(&mut guest, &Frame::Exit(Exit::Exited(0))).expect("the agent's Exit");
    guest
        .shutdown(std::net::Shutdown::Write)
        .expect("the agent closes its side");
    let out = ev
        .step("ExecExit", || aterm_vz::agent::run(fd, &exec(), HANG))
        .expect("the command's Exit");
    assert_eq!(out.exit, Exit::Exited(0));
    go(m, &mut st, &["ExecExit"]);

    ev.step("Detach", || a.detach());
    go(m, &mut st, &["Detach"]);
    agrees(&st, &mut lab, &disks, "one session detached");
    ev.step("Detach", || b.detach());
    go(m, &mut st, &["Detach"]);
    agrees(&st, &mut lab, &disks, "keep holds the VM with no session");

    lab.down().expect("down stops the VM and the helper ends");
    go(m, &mut st, &["Stop", "Stopped"]);
    let _ = lab.start.join();
    agrees(&st, &mut lab, &disks, "down");
    // `down` stopped the bake's VM, not its guest: the partial file stays unsealed, and the
    // next bring-up bakes again (§4).
    assert!(
        !m.action_enabled("Seal", &st),
        "a bake down stopped is never sealed"
    );
    drop(bake.take().expect("the first bring-up baked"));

    let mut d = ev
        .step("Attach", || lab.session(&attach(false)))
        .expect("a session on a second helper");
    go(m, &mut st, &["Start", "Attach"]);
    let up = ev.step("Prepare", || {
        d.bring_up(
            || {
                Ok::<Plan, aterm_vz::Error>(match disks.prepare()? {
                    Prepared::Bake(b) => {
                        let p = plan(b.path(), &console);
                        bake = Some(b);
                        p
                    }
                    Prepared::Live { .. } => panic!("a bake down stopped is baked again"),
                })
            },
            &mut |_| {},
        )
    });
    assert_eq!(up.expect("the VM is Ready").agent_sha256, AGENT);
    go(m, &mut st, &["Prepare", "Boot", "Hello", "Ready"]);
    agrees(&st, &mut lab, &disks, "the second bake's VM is Ready");
    // As `end_bake`: the session held while the guest powers itself off, its words the
    // helper's `POWERED_OFF`, and only then the seal.
    lab.start.last().send(Event::GuestStopped);
    let why = d
        .wait_stopped()
        .expect("the session hears how the VM stopped");
    assert_eq!(why, helper::POWERED_OFF);
    go(m, &mut st, &["Stopped"]);
    let _ = lab.start.join();
    agrees(&st, &mut lab, &disks, "the guest's own power-off");
    drop(d);

    let bake = bake.expect("the second bring-up baked");
    ev.step("Seal", || bake.seal()).expect("the bake is sealed");
    go(m, &mut st, &["Seal"]);
    agrees(&st, &mut lab, &disks, "the seal");

    let mut c = ev
        .step("Attach", || lab.session(&attach(false)))
        .expect("a session on a third helper");
    go(m, &mut st, &["Start", "Attach"]);
    let refused = ev.step("Prepare", || {
        c.bring_up(
            || {
                Ok::<Plan, aterm_vz::Error>(match disks.prepare()? {
                    Prepared::Live { disk, .. } => plan(&disk, &console),
                    Prepared::Bake(_) => panic!("a sealed bake is cloned, never baked again"),
                })
            },
            &mut |_| {},
        )
    });
    assert!(
        matches!(refused, Err(UpError::Client(ClientError::Failed { .. }))),
        "{refused:?}"
    );
    go(m, &mut st, &["Prepare", "BootRefused"]);
    agrees(&st, &mut lab, &disks, "the framework refused the boot");
    let joined = ev.step("AttachRefused", || lab.join(&attach(false)));
    assert!(
        matches!(&joined, Err(ClientError::Refused(why)) if why.starts_with("the VM is")),
        "{joined:?}"
    );
    go(m, &mut st, &["AttachRefused"]);
    ev.step("Detach", || c.detach());
    go(m, &mut st, &["Detach"]);
    let _ = lab.start.join();
    agrees(
        &st,
        &mut lab,
        &disks,
        "the failed helper's last session closed",
    );
    assert_eq!(starts.load(Ordering::SeqCst), 3);
}

// ---------------------------------------------------------------------------------------------
// The stand-in (§13): this binary in a stand-in mode, holding the run files as the helper
// does and answering nothing (as `tests/stand_in.rs` runs it).

const STAND_IN_STATE: &str = "ATERM_VZ_STAND_IN_STATE";
const STAND_IN_PARENT: &str = "ATERM_VZ_STAND_IN_PARENT";

/// With [`STAND_IN_STATE`] unset, an ordinary run's pass; set, the stand-in.
#[test]
fn stand_in_mode() {
    let (Some(state), Some(parent)) = (
        std::env::var_os(STAND_IN_STATE),
        std::env::var(STAND_IN_PARENT)
            .ok()
            .and_then(|p| p.parse::<u32>().ok()),
    ) else {
        return;
    };
    std::process::exit(stand_in_main(Path::new(&state), parent));
}

fn stand_in_main(state: &Path, parent: u32) -> i32 {
    use std::io::Write as _;
    let paths = Paths::at(state);
    let ident = helper::this_process(HELPER_SHA.into(), paths.live_disk());
    let held = match helper::take(&paths, &ident, &Kernel) {
        Ok(held) => held,
        Err(e) => {
            eprintln!("aterm-vz stand-in: {e}");
            return 1;
        }
    };
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "\nstand-in {} took {}",
            ident.pid,
            held.socket.display()
        );
        let _ = out.flush();
    }
    if let Err(e) = held.listener.set_nonblocking(true) {
        eprintln!("aterm-vz stand-in: {e}");
        return 1;
    }
    let mut open = Vec::new();
    while std::os::unix::process::parent_id() == parent {
        match held.listener.accept() {
            Ok((stream, _)) => open.push(stream),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    drop(open);
    0
}

/// A stand-in on `state`, once it wrote its line (its run files are taken by then).
fn stand_in(state: &Path) -> Child {
    let exe = std::env::current_exe().expect("this test binary's path");
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.env(STAND_IN_STATE, state)
        .env(STAND_IN_PARENT, std::process::id().to_string())
        .args([
            "--exact",
            "stand_in_mode",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("the stand-in starts");
    let line = format!(
        "stand-in {} took {}",
        child.id(),
        Paths::at(state).socket().display()
    );
    aterm_uds::deadline::prints(&mut child, &line, HANG);
    child
}

/// The lab's `Start` over a stand-in this test spawned: it kills and watches as the real one
/// does (by the record's pid, unique id and start time: [`aterm_vz::record::may_run`] and
/// [`aterm_vz::record::kill_helper`]), reaping its own child.
struct Tracked {
    child: Mutex<Child>,
    ended: Mutex<Option<std::process::ExitStatus>>,
}

impl Tracked {
    fn reap(&self) -> Option<std::process::ExitStatus> {
        let mut ended = self.ended.lock().unwrap();
        if ended.is_none() {
            *ended = self.child.lock().unwrap().try_wait().ok().flatten();
        }
        *ended
    }

    /// The stand-in's end, waited for within the hang detector.
    fn ended(&self) -> std::process::ExitStatus {
        let begun = Instant::now();
        loop {
            if let Some(status) = self.reap() {
                return status;
            }
            assert!(
                begun.elapsed() < HANG,
                "the stand-in did not end within a minute"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn signalled(&self) -> bool {
        self.reap().and_then(|s| s.signal()) == Some(libc::SIGKILL)
    }

    /// End the stand-in (SIGKILL) and reap it, within the hang detector.
    fn end(&self) {
        if self.reap().is_none() {
            aterm_uds::deadline::kill_within(&mut self.child.lock().unwrap(), HANG);
        }
    }
}

impl Start for Tracked {
    fn start(&mut self, _paths: &Paths) -> Result<u32, String> {
        Err("this test starts no helper".into())
    }

    fn exited(&mut self) -> Option<String> {
        None
    }

    fn settled(&mut self) {}

    fn alive(&self, record: &Record) -> bool {
        if self.child.lock().unwrap().id() == record.pid && self.reap().is_some() {
            return false;
        }
        aterm_vz::record::may_run(record, &Kernel)
    }

    fn kill(&mut self, record: &Record) -> Result<(), String> {
        aterm_vz::record::kill_helper(record, &Kernel)
    }

    fn pids(&self) -> &dyn Pids {
        &Kernel
    }
}

fn stand_in_lab(paths: &Paths, child: Child) -> Lab<Tracked> {
    let mut lab = Lab::new(
        paths.clone(),
        Tracked {
            child: Mutex::new(child),
            ended: Mutex::new(None),
        },
    );
    lab.span = SILENT;
    lab.end_span = SILENT;
    lab
}

/// What the run files say of the helper, as the model's `helper` and `wrong_kill`: no record;
/// a record whose helper may run (its pid holds its unique id and start time, or, for an
/// earlier build's record with no start time, its unique id: [`aterm_vz::record::may_run`];
/// serving, or hung: `hung`); one whose pid is gone; one whose pid is now another process
/// (another unique id, or another start time: a later boot's). A process the lab signalled
/// while the record named another is the wrong kill.
fn world(paths: &Paths, tracked: &Tracked, hung: bool, reused: bool) -> State {
    let helper = match Record::read(&paths.record()) {
        None => 0,
        Some(r) if tracked.reap().is_some() && tracked.child.lock().unwrap().id() == r.pid => 3,
        Some(r) if aterm_vz::record::may_run(&r, &Kernel) => {
            if hung {
                2
            } else {
                1
            }
        }
        Some(r) if Kernel.uniq(r.pid).is_some() => 4,
        Some(_) => 3,
    };
    [
        ("helper", helper),
        ("wrong_kill", int(reused && tracked.signalled())),
    ]
    .into_iter()
    .collect()
}

fn agrees_world(st: &State, seen: &State, what: &str) {
    assert!(
        seen.iter().all(|(k, v)| st[k] == *v),
        "{what}: the run files say {seen:?}, the model is at {st:?}"
    );
}

/// The escalation and the leftovers (§5.1) over a stand-in: a hung helper SIGKILLed by its
/// pid once its unique id and start time matched, and a dead one's leftovers, its pid gone or
/// now another process's (another unique id, or a later boot's start time), removed with
/// nothing signalled. An earlier build's record, which names no start time, is never signalled:
/// its dead helper's leftovers go once its socket refuses `Stop`, and its hung one is left
/// running, NOT RUN.
fn world_traces(m: &Model, ev: &mut StepEvidence) {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
    let mut st = m.init_state();
    go(m, &mut st, &["Start", "Hang"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, true, false),
        "a hung helper",
    );
    let down = ev.step("Escalate", || lab.down());
    assert!(
        matches!(&down, Err(ClientError::Hung(w))
            if w.ends_with("it was SIGKILLed by its pid, its unique id checked")),
        "{down:?}"
    );
    go(m, &mut st, &["Escalate"]);
    assert_eq!(
        lab.start.ended().signal(),
        Some(libc::SIGKILL),
        "the hung helper was SIGKILLed"
    );
    agrees_world(
        &st,
        &world(&paths, &lab.start, true, false),
        "the escalation",
    );
    assert!(!paths.socket().exists(), "its socket is gone");

    let dir = scratch();
    let paths = Paths::at(dir.path());
    let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
    let mut st = m.init_state();
    go(m, &mut st, &["Start"]);
    agrees_world(&st, &world(&paths, &lab.start, false, false), "a helper");
    lab.start.end();
    go(m, &mut st, &["Die"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, false, false),
        "a dead helper",
    );
    ev.step("ClearDead", || lab.down())
        .expect("down over a dead helper");
    go(m, &mut st, &["ClearDead"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, false, false),
        "its leftovers",
    );
    assert!(!paths.socket().exists(), "its socket is gone");

    for how in REUSES {
        let dir = scratch();
        let paths = Paths::at(dir.path());
        let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
        let mut st = m.init_state();
        reuse(&paths, how);
        go(m, &mut st, &["Start", "Die", "PidReused"]);
        agrees_world(
            &st,
            &world(&paths, &lab.start, false, true),
            &format!("a reused pid ({how:?})"),
        );
        ev.step("ClearDead", || lab.down())
            .expect("down over a dead helper whose pid was reused");
        go(m, &mut st, &["ClearDead"]);
        agrees_world(
            &st,
            &world(&paths, &lab.start, false, true),
            &format!("nothing signalled ({how:?})"),
        );
        assert!(lab.start.reap().is_none(), "the other process runs on");
        lab.start.end();
    }

    // An earlier build's record (no start time, `Holder::Unproven`) whose helper is gone: its
    // socket refuses `down`'s `Stop`, so its leftovers go at once, with nothing signalled.
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
    let mut st = m.init_state();
    strip_birth(&paths);
    go(m, &mut st, &["Start"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, false, false),
        "an earlier build's helper",
    );
    lab.start.end();
    go(m, &mut st, &["Die"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, false, false),
        "an earlier build's dead helper",
    );
    ev.step("ClearDead", || lab.down())
        .expect("down over an earlier build's dead helper");
    go(m, &mut st, &["ClearDead"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, false, false),
        "an earlier build's leftovers",
    );
    assert!(!paths.socket().exists(), "its socket is gone");

    // An earlier build's helper that answers nothing is never escalated: `down` is NOT RUN,
    // naming it, and leaves it running, a step the model's `Escalate` admits but the code
    // never takes for a record it cannot tell from a later process at that pid.
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
    let mut st = m.init_state();
    strip_birth(&paths);
    go(m, &mut st, &["Start", "Hang"]);
    agrees_world(
        &st,
        &world(&paths, &lab.start, true, false),
        "an earlier build's hung helper",
    );
    let down = lab.down();
    assert!(
        matches!(&down, Err(ClientError::Hung(w))
            if w.contains("earlier build") && w.ends_with("so it was not signalled")),
        "{down:?}"
    );
    agrees_world(
        &st,
        &world(&paths, &lab.start, true, false),
        "an earlier build's hung helper, unsignalled",
    );
    assert!(lab.start.reap().is_none(), "it runs on");
    assert!(paths.socket().exists(), "its run files stay");
    lab.start.end();
}

/// How a dead helper's pid now names another process: a later one at that pid (another
/// unique id), or a process of a later boot. Every process a boot makes before its first pid
/// wrap has a unique id equal to its pid, so a record from an earlier boot can match a
/// stranger's pid and unique id, and only the start time tells them apart.
#[derive(Clone, Copy, Debug)]
enum Reuse {
    UniqueId,
    EarlierBoot,
}

const REUSES: [Reuse; 2] = [Reuse::UniqueId, Reuse::EarlierBoot];

/// Four days, in microseconds: how much earlier the earlier boot's helper started.
const EARLIER_BOOT_US: u64 = 4 * 86_400_000_000;

/// The record now names a process its pid no longer is.
fn reuse(paths: &Paths, how: Reuse) {
    let mut reused = Record::read(&paths.record()).expect("the stand-in's record");
    assert_ne!(reused.birth_us, 0, "the stand-in's record names its start");
    match how {
        Reuse::UniqueId => reused.uniq = reused.uniq.wrapping_add(1),
        Reuse::EarlierBoot => {
            reused.birth_us = reused
                .birth_us
                .checked_sub(EARLIER_BOOT_US)
                .expect("a start past the epoch's fourth day");
        }
    }
    reused.write(&paths.record()).expect("the record");
}

/// The record is now one an earlier build wrote: the same, with no start time.
fn strip_birth(paths: &Paths) {
    let text = std::fs::read_to_string(paths.record()).expect("the stand-in's record");
    let mut v = aterm_json::from_str::<aterm_json::Value>(&text).expect("its JSON");
    let had = v
        .as_object_mut()
        .expect("an object")
        .remove("birth_us")
        .and_then(|b| b.as_u64());
    assert!(
        had.is_some_and(|b| b != 0),
        "the stand-in's record names its start"
    );
    std::fs::write(paths.record(), aterm_json::to_string(&v).expect("its JSON"))
        .expect("the record");
    assert_eq!(
        Record::read(&paths.record()).map(|r| r.birth_us),
        Some(0),
        "whole, with no start time"
    );
}

// ---------------------------------------------------------------------------------------------
// The tests.

#[test]
fn tier1_the_lifecycle_moves_exactly_where_the_model_does_and_each_step_enters_its_own_function() {
    let m = vz_lifecycle_model();
    let walked_model = in_process(&m);
    let mut ev = StepEvidence::new(MACHINE);
    let begun = Instant::now();
    let w = walk(&walked_model, &PROBES, None, Some(&mut ev), true);
    eprintln!(
        "VzLifecycle walk: {} states, {} edges, {} variants, {} probes, in {:.1} s",
        w.states,
        w.edges,
        w.variants,
        w.probes,
        begun.elapsed().as_secs_f64()
    );
    assert_eq!(
        w.states,
        interp::reachable_states(&walked_model).len(),
        "every state the model reaches without the world is walked"
    );
    let want: BTreeSet<&str> = ANCHORED
        .into_iter()
        .chain(ENVIRONMENT)
        .filter(|a| !WORLD.contains(a))
        .collect();
    assert_eq!(w.fired, want, "every step but the world's is mirrored");

    client_trace(&m, &mut ev);
    world_traces(&m, &mut ev);

    let audit = ev.audit();
    eprintln!(
        "VzLifecycle evidence: {} actions on {} anchors over {} functions, {} windows, {} \
         entering their own action's function, {} sites of multi-site actions, {} dark",
        audit.actions.len(),
        audit.anchors,
        audit.by_function.len(),
        audit.steps,
        audit.witnessed_steps,
        audit.multi_site_anchors,
        audit.dark_sites.len()
    );
    assert_eq!(
        audit.actions,
        ANCHORED.into_iter().collect::<BTreeSet<_>>(),
        "{audit:?}"
    );
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{:?}", audit.stray);
    assert!(audit.dark_sites.is_empty(), "{:?}", audit.dark_sites);
}

#[test]
fn tier1_the_anchors_name_what_the_code_implements() {
    let m = vz_lifecycle_model();
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == MACHINE)
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == MACHINE)
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = m.actions.iter().map(|a| a.name).collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
    assert!(
        xref::refinements()
            .filter(|r| r.machine == MACHINE)
            .all(|r| r.project == "conformance_lifecycle::project"),
        "every anchor names this file's projection"
    );

    let report = xref::check_closure(&[SpecModule::Embedded(m)]);
    let ours: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.message.contains(MACHINE))
        .collect();
    assert!(
        ours.is_empty(),
        "{:?}",
        ours.iter().map(|v| &v.message).collect::<Vec<_>>()
    );
    let coverage = report
        .coverage
        .iter()
        .find(|c| c.machine == "VzLifecycle")
        .expect("VzLifecycle's coverage");
    assert!(
        coverage.active && coverage.uncovered.is_empty(),
        "{:?}",
        coverage.uncovered
    );
}

/// A planted defect, or a defective caller the harness plays, is the walk's first
/// disagreement: a `step` no action of the healthy model admits, and exactly `mutant`'s
/// step, which breaks `law`.
fn control(
    fault: Option<Fault>,
    defect: Option<(&'static str, Guard)>,
    want_step: &str,
    mutant: &str,
    law: &str,
) {
    let m = vz_lifecycle_model();
    let walked_model = in_process(&m);
    let probes: Vec<(&'static str, Guard)> = PROBES.into_iter().chain(defect).collect();
    let w = walk(&walked_model, &probes, fault, None, false);
    let off = w
        .off
        .unwrap_or_else(|| panic!("{fault:?} {defect:?}: the walk agreed everywhere"));
    assert_eq!(off.step, want_step, "after {:?}: {off:#?}", off.path);
    assert_eq!(
        interp::admits(&m, &off.prev, &off.observed),
        None,
        "the healthy model admits {off:#?}"
    );
    let buggy = interp::with_buggy(&m, 1);
    assert_eq!(
        buggy.successors(mutant, &off.prev),
        vec![off.observed.clone()],
        "the step is {mutant}'s: {off:#?}"
    );
    assert!(
        !buggy.check_invariant(law, &off.observed),
        "{mutant}'s step breaks {law}: {off:#?}"
    );
}

/// The real system after `path`, which agrees with the model step for step, then `last`
/// (with `fault` armed for it, or a defective caller the harness plays): no step of the
/// healthy model admits what it did, and `mutant`'s step does, breaking `law`. For a defect
/// the walk's first disagreement meets elsewhere first.
fn control_at(
    path: &[&'static str],
    last: &'static str,
    fault: Option<Fault>,
    mutant: &str,
    law: &str,
) {
    let m = vz_lifecycle_model();
    let mut prev = m.init_state();
    let shared = scratch();
    let mut rig = Rig::new(boots(path.iter().copied()), &base_in(shared.path()));
    for action in path {
        assert!(m.fire(action, &mut prev), "{action} at {prev:?}");
        step(&mut rig, action, &mut Win(None));
        assert_eq!(project(&rig), prev, "the real system after {action}");
    }
    armed(fault, || step(&mut rig, last, &mut Win(None)));
    let observed = project(&rig);
    assert_eq!(
        interp::admits(&m, &prev, &observed),
        None,
        "the healthy model admits {last} after {path:?}: {prev:?} to {observed:?}"
    );
    let buggy = interp::with_buggy(&m, 1);
    assert_eq!(
        buggy.successors(mutant, &prev),
        vec![observed.clone()],
        "the step is {mutant}'s"
    );
    assert!(
        !buggy.check_invariant(law, &observed),
        "{mutant}'s step breaks {law}: {observed:?}"
    );
}

#[test]
fn tier1_a_command_connection_on_a_booting_vm_is_rejected() {
    control(
        Some(Fault::ConnectBooting),
        None,
        "probe:ConnectNotReady",
        "ExecBooting",
        "EveryExecEndsOnce",
    );
}

#[test]
fn tier1_an_exit_dropped_on_a_stopping_vm_is_rejected() {
    control_at(
        &[
            "UpLocks",
            "Start",
            "Attach",
            "Prepare",
            "Boot",
            "Hello",
            "Ready",
            "ExecBegin",
            "Stop",
        ],
        "ExecExit",
        Some(Fault::DropExit),
        "DropExit",
        "EveryExecEndsOnce",
    );
}

#[test]
fn tier1_a_lab_that_seals_a_bake_whatever_stopped_its_vm_is_rejected() {
    control(
        None,
        Some(("defect:SealHalfMade", |s| {
            s["bake"] == 1 && s["disk"] == 1 && s["phase"] > 3 && s["ran"] <= 1
        })),
        "defect:SealHalfMade",
        "SealHalfMade",
        "SealedOnlyWhenFinished",
    );
    // The review's case: a bake a detector stopped before its hello.
    control_at(
        &[
            "UpLocks", "Start", "Attach", "Prepare", "Boot", "Detect", "Stopped",
        ],
        "defect:SealHalfMade",
        None,
        "SealHalfMade",
        "SealedOnlyWhenFinished",
    );
    // And one a detector stopped after its Ready, while the lab provisioned it.
    control_at(
        &[
            "UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello", "Ready", "Detect", "Stopped",
        ],
        "defect:SealHalfMade",
        None,
        "SealHalfMade",
        "SealedOnlyWhenFinished",
    );
    // And, after its Ready, one `down`'s `Stop` stopped and one the release rule stopped:
    // the helper's words are the stop's, not `POWERED_OFF` (§4: only the guest's own
    // power-off goes on to the seal).
    for end in [&["Stop", "Stopped"][..], &["Detach", "Stopped"]] {
        let path: Vec<&'static str> = [
            "UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello", "Ready",
        ]
        .into_iter()
        .chain(end.iter().copied())
        .collect();
        control_at(
            &path,
            "defect:SealHalfMade",
            None,
            "SealHalfMade",
            "SealedOnlyWhenFinished",
        );
    }
}

/// No mutant's step and no law: a helper that waits forever on a hard stop that never
/// finishes is still the walk's first disagreement, at `HardTimeout`, where it stays.
#[test]
fn tier1_a_helper_waiting_forever_on_its_hard_stop_is_seen() {
    let m = vz_lifecycle_model();
    let walked_model = in_process(&m);
    let w = walk(
        &walked_model,
        &PROBES,
        Some(Fault::HardStopForever),
        None,
        false,
    );
    let off = w.off.expect("the walk agreed everywhere");
    assert_eq!(off.step, "HardTimeout", "after {:?}: {off:#?}", off.path);
    assert_eq!(off.observed, off.prev, "it serves on: {off:#?}");
    assert_ne!(
        m.successors("HardTimeout", &off.prev),
        vec![off.observed.clone()]
    );
}

#[test]
fn tier1_a_second_helper_past_the_flock_boots_a_second_vm_on_the_live_disk() {
    control(
        Some(Fault::NoFlock),
        None,
        "SecondRefused",
        "SecondBoots",
        "LiveAttachedOnce",
    );
}

#[test]
fn tier1_a_live_disk_cloned_from_a_partial_bake_is_rejected() {
    control(
        Some(Fault::ClonePartial),
        None,
        "Prepare",
        "ClonePartial",
        "ClonedOnlyWhenSealed",
    );
}

#[test]
fn tier1_a_sealed_disk_handed_out_writable_is_rejected() {
    control(
        Some(Fault::AttachBaked),
        None,
        "Prepare",
        "AttachBaked",
        "SealedNeverAttached",
    );
}

#[test]
fn tier1_a_command_whose_exit_is_dropped_is_rejected() {
    control(
        Some(Fault::DropExit),
        None,
        "ExecExit",
        "DropExit",
        "EveryExecEndsOnce",
    );
}

#[test]
fn tier1_a_vm_kept_running_after_the_last_session_is_rejected() {
    control(
        Some(Fault::KeepRunning),
        None,
        "Detach",
        "KeepRunning",
        "ReleaseRule",
    );
}

#[test]
fn tier1_a_hard_stop_nobody_asked_for_is_rejected() {
    control(
        Some(Fault::HardOnClose),
        None,
        "Detach",
        "HardUnasked",
        "HardStopOnlyWhenAsked",
    );
}

#[test]
fn tier1_a_ready_before_the_hello_is_rejected() {
    control(
        Some(Fault::ReadyBeforeHello),
        None,
        "probe:ReadyEarly",
        "ReadyUnchecked",
        "ReadyOnlyForThePlansAgent",
    );
}

#[test]
fn tier1_a_lab_that_prepares_under_a_vm_pulls_its_disk() {
    control(
        None,
        Some(("defect:PrepareUnderVm", |s| {
            s["up"] == 1
                && s["helper"] == 1
                && s["sessions"] > 0
                && (s["phase"] == 1 || s["phase"] == 2)
        })),
        "defect:PrepareUnderVm",
        "PrepareUnderVm",
        "NothingPreparedUnderAVm",
    );
}

#[test]
fn tier1_an_up_without_the_lock_pulls_another_ups_prepared_disk() {
    control(
        None,
        Some(("defect:UpWithoutLock", |s| {
            s["prepared"] == 1 && s["phase"] == 0
        })),
        "defect:UpWithoutLock",
        "UpWithoutLock",
        "NothingPreparedUnderAVm",
    );
}

/// For a pid reused within a boot (another unique id) and for a record from an earlier boot
/// (the same pid and unique id, another start time).
#[test]
fn tier1_a_lab_that_signals_by_pid_alone_kills_a_reused_pid() {
    let m = vz_lifecycle_model();
    for how in REUSES {
        let dir = scratch();
        let paths = Paths::at(dir.path());
        let mut lab = stand_in_lab(&paths, stand_in(dir.path()));
        reuse(&paths, how);
        let mut prev = m.init_state();
        go(&m, &mut prev, &["Start", "Die", "PidReused"]);
        agrees_world(
            &prev,
            &world(&paths, &lab.start, false, true),
            &format!("a reused pid ({how:?})"),
        );
        let _ = fault::with(Fault::PidOnly, || lab.down());
        // The lab signalled it: its end must come.
        let _ = lab.start.ended();
        let mut observed = prev.clone();
        observed.extend(world(&paths, &lab.start, false, true));
        assert_eq!(
            (observed["helper"], observed["wrong_kill"]),
            (0, 1),
            "the other process was signalled and its leftovers went ({how:?})"
        );
        assert_eq!(interp::admits(&m, &prev, &observed), None);
        let buggy = interp::with_buggy(&m, 1);
        assert_eq!(
            buggy.successors("SignalReused", &prev),
            vec![observed.clone()]
        );
        assert!(!buggy.check_invariant("SignalOnlyTheRecordedHelper", &observed));
        lab.start.end();
    }
}

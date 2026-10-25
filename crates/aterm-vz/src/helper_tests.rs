// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's core with the Fake machine, on a stepped clock: §10's rows, one test each,
//! and §5.1's session rules at the core. No test sleeps: the clock is the `now` each input
//! is handed, and every wait on another thread is a one-minute hang detector.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use aterm_vz_agent::wire::{self, Frame, Job};

use super::*;
use crate::machine::fake::{BOOT_ID, Call, Fake, FakeEvent, Script, Seen, conn_pair};
use crate::plan::{COMMAND_LINE, Disk, Limits};
use crate::proto::Attach;

const S: Duration = Duration::from_secs(1);
const AGENT: [u8; 32] = [0xab; 32];
const HELPER: &str = "4e1f0000000000000000000000000000000000000000000000000000000000aa";

fn agent_hex() -> String {
    crate::hex(&AGENT)
}

fn plan(console_log: &Path) -> Plan {
    Plan {
        name: "aterm-linux".into(),
        cpus: 8,
        memory_bytes: 16 << 30,
        kernel: "/vz/kernel/Image".into(),
        initrd: "/vz/kernel/initrd".into(),
        command_line: COMMAND_LINE.into(),
        disks: vec![Disk {
            path: "/vz/live/aterm-linux.img".into(),
            read_only: false,
        }],
        mac: crate::plan::mac_for("aterm-linux"),
        boot_share: "/vz/agent/ab".into(),
        shares: shares("/src/aterm"),
        rosetta: true,
        console_log: console_log.to_path_buf(),
        agent_sha256: agent_hex(),
    }
}

fn shares(repo: &str) -> Vec<ShareEntry> {
    vec![ShareEntry {
        name: "repo".into(),
        path: repo.into(),
    }]
}

fn attach(keep: bool) -> Attach {
    Attach {
        proto: PROTO,
        helper_sha256: HELPER.into(),
        agent_sha256: agent_hex(),
        shares: shares("/src/aterm"),
        keep,
    }
}

fn agent_script() -> Script {
    Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Agent {
                agent_sha256: AGENT,
                ready: true,
            },
        ],
        ..Script::default()
    }
}

/// The core, its Fake, and the labs' ends of their sessions.
struct Rig {
    core: Core<Fake>,
    seen: Seen,
    tx: std::sync::mpsc::Sender<Input>,
    rx: Receiver<Input>,
    labs: BTreeMap<u64, Channel>,
    free: Arc<Mutex<u64>>,
    on_boot: usize,
    dir: aterm_tempfile::TempDir,
}

impl Rig {
    fn new(script: Script) -> Self {
        Self::with(script, false, false)
    }

    fn with(script: Script, hold_conns: bool, record: bool) -> Self {
        let on_boot = if script.boot.is_ok() {
            script.on_boot.len()
        } else {
            0
        };
        let dir = aterm_tempfile::tempdir().unwrap();
        let (fake, seen) = Fake::new(script);
        let (tx, rx) = std::sync::mpsc::channel();
        let free = Arc::new(Mutex::new(200u64 << 30));
        let reading = Arc::clone(&free);
        let ident = Ident {
            pid: 4242,
            uniq: 77,
            birth_us: 1_792_000_000_000_000,
            helper_sha256: HELPER.into(),
            live_disk: "/vz/live/aterm-linux.img".into(),
        };
        let record = record.then(|| {
            let r = Record {
                pid: ident.pid,
                uniq: ident.uniq,
                birth_us: ident.birth_us,
                helper_sha256: HELPER.into(),
                proto: PROTO,
                live_disk: ident.live_disk.clone(),
                started: 1_792_000_000,
                boot_id: None,
                agent_sha256: None,
            };
            let path = dir.path().join("aterm-linux.json");
            r.write(&path).unwrap();
            (path, r)
        });
        let cfg = Config {
            ident,
            record,
            hold_conns,
            key_disk: dir.path().join("key.img"),
            free: Box::new(move || {
                Ok(Free {
                    bytes: *reading.lock().unwrap(),
                    volume: "/System/Volumes/Data (container:disk3)".into(),
                })
            }),
        };
        Self {
            core: Core::new(fake, cfg, tx.clone()),
            seen,
            tx,
            rx,
            labs: BTreeMap::new(),
            free,
            on_boot,
            dir,
        }
    }

    fn console_log(&self) -> PathBuf {
        self.dir.path().join("console.log")
    }

    fn open(&mut self, sid: u64) {
        let (ours, lab) = UnixStream::pair().unwrap();
        assert_eq!(
            self.core.handle(Input::Opened(sid, ours), Duration::ZERO),
            Flow::Go
        );
        let lab = Channel::new(lab);
        lab.set_span(Some(SPAN)).unwrap();
        self.labs.insert(sid, lab);
    }

    fn req(&mut self, sid: u64, r: Request, now: Duration) -> Flow {
        self.core.handle(Input::Request(sid, r), now)
    }

    /// The next reply on `sid`, with its descriptor: a one-minute hang detector.
    fn reply_fd(&mut self, sid: u64) -> (Reply, Option<std::os::fd::OwnedFd>) {
        self.labs
            .get_mut(&sid)
            .unwrap()
            .recv_reply()
            .unwrap()
            .expect("a reply, not EOF")
    }

    fn reply(&mut self, sid: u64) -> Reply {
        self.reply_fd(sid).0
    }

    /// Every reply already sent on `sid`, and whether the session ended, without waiting.
    fn sent(&mut self, sid: u64) -> (Vec<Reply>, bool) {
        let lab = self.labs.get_mut(&sid).unwrap();
        lab.stream().set_nonblocking(true).unwrap();
        let mut out = Vec::new();
        let ended = loop {
            match lab.recv_reply() {
                Ok(Some((r, _))) => out.push(r),
                Ok(None) => break true,
                Err(_) => break false,
            }
        };
        lab.stream().set_nonblocking(false).unwrap();
        (out, ended)
    }

    /// Take the next input the machine or the watch reader posted: a one-minute hang
    /// detector.
    fn pump(&mut self, now: Duration) -> Flow {
        let input = self
            .rx
            .recv_timeout(SPAN)
            .expect("an input within a minute");
        self.core.handle(input, now)
    }

    fn attached(&mut self, sid: u64, keep: bool) {
        self.open(sid);
        assert_eq!(
            self.req(sid, Request::Attach(attach(keep)), Duration::ZERO),
            Flow::Go
        );
        assert!(matches!(self.reply(sid), Reply::Attached { .. }));
        // A Ready VM's hello follows at once.
        let _ = self.sent(sid);
    }

    /// Boot at `now` and take the Fake's boot events.
    fn boot(&mut self, sid: u64, now: Duration) -> Flow {
        let plan = plan(&self.console_log());
        let flow = self.req(sid, Request::Boot(Box::new(plan)), now);
        if flow != Flow::Go || !self.seen.calls().iter().any(|c| matches!(c, Call::Boot(_))) {
            return flow;
        }
        for _ in 0..self.on_boot {
            let flow = self.pump(now);
            if flow != Flow::Go {
                return flow;
            }
        }
        Flow::Go
    }

    /// Ticks from `from` to `to`, a second apart, until one exits.
    fn ticks(&mut self, from: Duration, to: Duration) -> Flow {
        let mut t = from;
        while t <= to {
            let flow = self.core.handle(Input::Tick, t);
            if flow != Flow::Go {
                return flow;
            }
            t += S;
        }
        Flow::Go
    }

    fn console(&mut self, line: &str, now: Duration) -> Flow {
        self.core.handle(
            Input::Machine(Event::Console(line.as_bytes().to_vec())),
            now,
        )
    }

    /// The agent's end of its watch connection.
    fn watch_end(&self) -> UnixStream {
        self.seen
            .watch_end
            .lock()
            .unwrap()
            .as_ref()
            .expect("the agent dialed")
            .try_clone()
            .unwrap()
    }

    /// This boot's key, as guest root reads it off the key disk.
    fn key(&self) -> crate::key::Key {
        aterm_vz_agent::key::read_disk(&self.dir.path().join("key.img"))
            .unwrap()
            .expect("a boot wrote its key disk")
    }

    /// Take the posted inputs until `done` holds, a one-minute hang detector each.
    fn pump_until(&mut self, now: Duration, mut done: impl FnMut(&Self) -> bool) -> Flow {
        for _ in 0..64 {
            if done(self) {
                return Flow::Go;
            }
            let flow = self.pump(now);
            if flow != Flow::Go {
                return flow;
            }
        }
        panic!("the core did not get there within 64 inputs");
    }

    /// The agent sends `frame`; the core takes it at `now`.
    fn say(&mut self, frame: &Frame, now: Duration) -> Flow {
        wire::write_frame(&mut self.watch_end(), frame).unwrap();
        self.pump(now)
    }
}

/// Booted at 0 with an agent that said hello (9 s) and Ready (10 s), one session (1).
fn ready_rig(hold_conns: bool, keep: bool) -> Rig {
    let mut rig = Rig::with(agent_script(), hold_conns, true);
    rig.attached(1, keep);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.pump(S * 9), Flow::Go, "Hello");
    assert_eq!(rig.pump(S * 10), Flow::Go, "Ready");
    assert_eq!(rig.core.phase(), Phase::Ready);
    rig
}

fn lost_why(replies: &[Reply]) -> Vec<String> {
    replies
        .iter()
        .filter_map(|r| match r {
            Reply::Lost { why, .. } => Some(why.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_boot_reaches_ready_and_the_record_takes_the_agents_hello() {
    let mut rig = ready_rig(false, false);
    let (sent, ended) = rig.sent(1);
    assert!(!ended);
    assert_eq!(sent[0], Reply::Started);
    assert!(
        matches!(&sent[1], Reply::Booting(note) if note.contains(BOOT_ID)),
        "{sent:?}"
    );
    assert!(matches!(&sent[2], Reply::Ready(h) if h.agent_sha256 == AGENT && h.boot_id == BOOT_ID));
    let (path, _) = rig.core.cfg.record.clone().unwrap();
    let record = Record::read(&path).unwrap();
    assert_eq!(record.boot_id.as_deref(), Some(BOOT_ID));
    assert_eq!(record.agent_sha256.as_deref(), Some(agent_hex().as_str()));
    // The host's clock went to the agent right after its hello.
    let mut end = rig.watch_end();
    end.set_read_timeout(Some(SPAN)).unwrap();
    assert!(
        matches!(wire::read_frame(&mut end).unwrap(), Some(Frame::Clock { unix_ns }) if unix_ns > 0)
    );
    // The plan reached the machine whole, under the framework's limits.
    assert!(
        matches!(&rig.seen.calls()[..2], [Call::Limits, Call::Boot(p)] if p.command_line == COMMAND_LINE)
    );
}

#[test]
fn a_refused_configuration_starts_nothing_and_says_vzs_own_words() {
    let words = "Error Domain=VZErrorDomain Code=2 \"Invalid virtual machine configuration. The \
                 process doesn’t have the “com.apple.security.virtualization” entitlement.\"";
    let mut rig = Rig::new(Script {
        boot: Err(words.into()),
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Failed);
    assert!(matches!(rig.reply(1), Reply::Failed { error, .. } if error == words));
    assert!(
        !rig.seen.calls().contains(&Call::HardStop),
        "nothing was started"
    );
    // Its session closes: no VM runs, so the helper exits.
    assert_eq!(
        rig.req(1, Request::Detach, S),
        Flow::Exit("no session is left, and no VM runs".into())
    );
}

#[test]
fn the_storage_attachment_refusal_after_a_free_flock_is_failed_with_its_text() {
    let words = "Error Domain=VZErrorDomain Code=2 \"The storage device attachment is invalid.\"";
    let mut rig = Rig::new(Script {
        on_boot: vec![FakeEvent::StartFailed(words.into())],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Failed);
    assert!(matches!(rig.reply(1), Reply::Failed { error, .. } if error == words));
}

#[test]
fn a_memory_ask_past_the_host_is_refused_before_any_boot() {
    let mut rig = Rig::new(Script {
        limits: Ok(Limits {
            max_cpus: 64,
            max_memory: 128 << 30,
            host_cpus: 8,
            host_memory: 16 << 30,
        }),
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert!(
        matches!(rig.reply(1), Reply::Failed { error, .. } if error.starts_with("NOT RUN — this host has 16.0 GiB"))
    );
    assert_eq!(rig.seen.calls(), [Call::Limits], "no boot was asked");
}

#[test]
fn a_boot_that_hangs_before_the_agent_is_stopped_hard_with_the_console_tail() {
    let mut rig = Rig::new(Script::default());
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(
        rig.console("[    0.0] Booting Linux on physical CPU 0x0\n", S),
        Flow::Go
    );
    assert_eq!(
        rig.console("[    1.9] Run /init as init process\n", S * 2),
        Flow::Go
    );
    // The same line again is no progress.
    assert_eq!(
        rig.console("[    2.0] Run /init as init process\n", S * 30),
        Flow::Go
    );
    assert_eq!(rig.ticks(S * 3, S * 61), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 62, S * 62), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    let lost = sent.iter().find_map(|r| match r {
        Reply::Lost { why, tail } => Some((why.clone(), tail.clone())),
        _ => None,
    });
    let (why, tail) = lost.expect("Lost");
    assert!(
        why.starts_with("NOT RUN — the boot hung before the agent"),
        "{why}"
    );
    assert!(tail.contains("Run /init as init process"), "{tail}");
    // The hard stop's completion ends the helper, with no second Lost.
    assert!(
        matches!(rig.pump(S * 62), Flow::Exit(why) if why.contains("the boot hung before the agent"))
    );
    let (sent, _) = rig.sent(1);
    assert!(lost_why(&sent).is_empty(), "{sent:?}");
}

#[test]
fn a_boot_that_hangs_after_the_agent_names_its_stuck_jobs() {
    let mut rig = Rig::new(Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Agent {
                agent_sha256: AGENT,
                ready: false,
            },
        ],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.pump(S * 4), Flow::Go, "Hello");
    let job = |elapsed_s: u64| Job {
        id: 117,
        unit: "systemd-networkd-wait-online.service".into(),
        kind: "start".into(),
        state: "running".into(),
        elapsed_ms: elapsed_s * 1000,
    };
    for t in (5..=60u64).step_by(5) {
        assert_eq!(
            rig.say(
                &Frame::Booting(vec![job(t - 5)]),
                S * u32::try_from(t).unwrap()
            ),
            Flow::Go
        );
        // Console lines keep arriving: once the agent speaks they are no progress.
        assert_eq!(
            rig.console(&format!("line {t}\n"), S * u32::try_from(t).unwrap()),
            Flow::Go
        );
        assert_eq!(
            rig.core.handle(Input::Tick, S * u32::try_from(t).unwrap()),
            Flow::Go
        );
    }
    // The job set last changed at 5 s.
    assert_eq!(rig.ticks(S * 61, S * 64), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 65, S * 65), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    let why = lost_why(&sent);
    assert_eq!(why.len(), 1, "{sent:?}");
    assert!(
        why[0].starts_with("NOT RUN — the boot hung after the agent")
            && why[0].contains("systemd-networkd-wait-online.service start running (55 s)"),
        "{}",
        why[0]
    );
    // The sessions heard what the boot waited on every 5 s.
    assert!(sent.iter().any(|r| matches!(r, Reply::Booting(n) if n.starts_with("start jobs: systemd-networkd-wait-online.service"))));
}

#[test]
fn the_negative_control_a_job_set_that_changes_within_each_span_never_stops_the_boot() {
    let mut rig = Rig::new(Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Agent {
                agent_sha256: AGENT,
                ready: false,
            },
        ],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.pump(Duration::ZERO), Flow::Go, "Hello");
    for t in (5..=300u32).step_by(5) {
        let id = t / 50;
        let job = Job {
            id,
            unit: format!("unit-{id}.service"),
            kind: "start".into(),
            state: "running".into(),
            elapsed_ms: 0,
        };
        assert_eq!(rig.say(&Frame::Booting(vec![job]), S * t), Flow::Go);
        assert_eq!(rig.ticks(S * (t - 4), S * t), Flow::Go);
    }
    assert!(!rig.seen.calls().contains(&Call::HardStop));
}

#[test]
fn a_kernel_panic_stops_the_vm_at_once_with_its_line() {
    let mut rig = Rig::new(Script::default());
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(
        rig.console(
            "[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)\n",
            S * 2
        ),
        Flow::Go
    );
    assert_eq!(
        rig.seen.calls().last(),
        Some(&Call::HardStop),
        "at once, no tick"
    );
    let (sent, _) = rig.sent(1);
    let why = lost_why(&sent);
    assert!(
        why[0].starts_with(
            "NOT RUN — the guest's kernel panicked: [    1.9] Kernel panic - not syncing: VFS"
        ),
        "{why:?}"
    );
    // The console reached console.log as the guest wrote it.
    let log = std::fs::read_to_string(rig.console_log()).unwrap();
    assert!(log.contains("Unable to mount root fs"), "{log}");
}

#[test]
fn a_guest_running_another_agent_is_stopped_naming_both_hashes_never_through_it() {
    let mut rig = Rig::new(Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Agent {
                agent_sha256: [0xcd; 32],
                ready: true,
            },
        ],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.pump(S * 3), Flow::Go, "Hello");
    // Its Ready is not taken: the VM is stopping.
    assert_eq!(rig.pump(S * 4), Flow::Go, "Ready");
    assert_eq!(rig.core.phase(), Phase::Stopping);
    let calls = rig.seen.calls();
    assert_eq!(calls.last(), Some(&Call::RequestStop));
    assert!(
        !calls.contains(&Call::Connect(aterm_vz_agent::COMMAND_PORT)),
        "{calls:?}"
    );
    let (sent, _) = rig.sent(1);
    assert!(!sent.iter().any(|r| matches!(r, Reply::Ready(_))));
    let why = lost_why(&sent);
    assert_eq!(
        why,
        [format!(
            "NOT RUN — the guest runs agent {} (protocol {p}); this checkout built {} (protocol {p})",
            "cd".repeat(32),
            agent_hex(),
            p = wire::PROTO
        )]
    );
    // A guest that does not power off is stopped hard a span after the request (3 s).
    assert_eq!(rig.ticks(S * 5, S * 62), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::RequestStop));
    assert_eq!(rig.ticks(S * 63, S * 63), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
}

#[test]
fn a_guest_that_stops_beating_is_lost_with_the_heartbeat_words() {
    let mut rig = ready_rig(false, false);
    for t in (15..=100u32).step_by(5) {
        assert_eq!(
            rig.say(
                &Frame::Beat {
                    uptime_ms: u64::from(t) * 1000
                },
                S * t
            ),
            Flow::Go
        );
        assert_eq!(rig.ticks(S * (t - 4), S * t), Flow::Go);
    }
    assert_eq!(rig.ticks(S * 101, S * 159), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 160, S * 160), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    assert_eq!(
        lost_why(&sent),
        ["the guest stopped answering (no heartbeat for 60 s); VM stopped"]
    );
}

#[test]
fn the_guest_stopping_by_itself_or_with_an_error_ends_every_session() {
    let mut rig = ready_rig(false, false);
    rig.attached(2, false);
    let flow = rig.core.handle(Input::Machine(Event::GuestStopped), S * 20);
    assert_eq!(
        flow,
        Flow::Exit("the VM stopped: the guest stopped: it powered itself off".into())
    );
    for sid in [1, 2] {
        let (sent, _) = rig.sent(sid);
        assert_eq!(
            lost_why(&sent),
            ["the guest stopped: it powered itself off"]
        );
    }

    let mut rig = ready_rig(false, false);
    let flow = rig.core.handle(
        Input::Machine(Event::StoppedWithError(
            "VZErrorDomain 1: internal error".into(),
        )),
        S * 20,
    );
    assert!(matches!(flow, Flow::Exit(_)));
    assert_eq!(rig.core.phase(), Phase::Failed);
    let (sent, _) = rig.sent(1);
    assert!(sent.iter().any(|r| matches!(r, Reply::Failed { error, .. } if error == "the guest stopped with an error: VZErrorDomain 1: internal error")));
}

#[test]
fn the_volume_falling_below_the_run_floor_stops_the_vm_naming_it() {
    let mut rig = ready_rig(false, false);
    for t in (15..=40u32).step_by(5) {
        assert_eq!(rig.say(&Frame::Beat { uptime_ms: 1 }, S * t), Flow::Go);
        assert_eq!(rig.ticks(S * (t - 4), S * t), Flow::Go);
    }
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    *rig.free.lock().unwrap() = 3 << 30;
    assert_eq!(rig.ticks(S * 41, S * 45), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    assert_eq!(
        lost_why(&sent),
        ["the volume holding aterm-linux.img has 3.0 GiB free; VM stopped"]
    );
}

#[test]
fn the_last_session_closing_with_keep_unset_powers_off_through_the_agent_then_stops_hard() {
    let mut rig = ready_rig(false, false);
    assert_eq!(rig.core.handle(Input::Closed(1), S * 20), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Stopping);
    assert_eq!(
        rig.seen.calls().last(),
        Some(&Call::Connect(aterm_vz_agent::COMMAND_PORT))
    );
    let mut guest = rig.seen.guest_ends.lock().unwrap().remove(0);
    guest.set_read_timeout(Some(SPAN)).unwrap();
    assert_eq!(wire::read_frame(&mut guest).unwrap(), Some(Frame::Poweroff));
    // The guest's shutdown prints, so the detector waits; then it goes quiet.
    assert_eq!(
        rig.console("[  OK  ] Stopped target graphical.target.\n", S * 50),
        Flow::Go
    );
    assert_eq!(rig.ticks(S * 21, S * 109), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 110, S * 110), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    assert!(
        matches!(rig.pump(S * 110), Flow::Exit(why) if why.contains("the last session closed with keep unset"))
    );
}

#[test]
fn the_negative_control_with_keep_set_the_last_session_closing_leaves_the_vm_up() {
    let mut rig = ready_rig(false, true);
    assert_eq!(rig.core.handle(Input::Closed(1), S * 20), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Ready);
    assert!(rig.core.keep());
    assert!(
        !rig.seen
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Connect(_) | Call::RequestStop | Call::HardStop))
    );
    // The watches run on while it idles: beats keep it up.
    for t in (25..=200u32).step_by(5) {
        assert_eq!(rig.say(&Frame::Beat { uptime_ms: 1 }, S * t), Flow::Go);
        assert_eq!(rig.ticks(S * (t - 4), S * t), Flow::Go);
    }
    assert_eq!(rig.core.phase(), Phase::Ready);
}

#[test]
fn a_session_that_clears_keep_lets_its_close_stop_the_vm() {
    let mut rig = ready_rig(false, true);
    let _ = rig.sent(1);
    assert_eq!(rig.req(1, Request::Keep(false), S * 20), Flow::Go);
    assert_eq!(rig.reply(1), Reply::Done);
    assert_eq!(rig.req(1, Request::Detach, S * 21), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Stopping);
}

#[test]
fn stop_powers_off_and_the_helper_exits_answering_any_connection() {
    let mut rig = ready_rig(false, true);
    rig.attached(2, false);
    // `down` opens no session: Stop is a frozen kind.
    rig.open(3);
    assert_eq!(
        rig.req(3, Request::Stop { grace_ms: 60_000 }, S * 20),
        Flow::Go
    );
    assert_eq!(rig.reply(3), Reply::Stopping);
    assert!(!rig.core.keep(), "keep goes with it");
    assert_eq!(
        rig.seen.calls().last(),
        Some(&Call::Connect(aterm_vz_agent::COMMAND_PORT))
    );
    // Every 5 s the Stop's connection hears the helper is still at it.
    assert_eq!(rig.ticks(S * 21, S * 25), Flow::Go);
    assert_eq!(rig.reply(3), Reply::Stopping);
    let flow = rig.core.handle(Input::Machine(Event::GuestStopped), S * 30);
    assert_eq!(
        flow,
        Flow::Exit("the VM stopped: the VM was stopped by a Stop (down)".into())
    );
    let (sent, _) = rig.sent(2);
    assert_eq!(lost_why(&sent), ["the VM was stopped by a Stop (down)"]);
}

#[test]
fn stop_with_no_vm_exits_at_once() {
    let mut rig = Rig::new(Script::default());
    rig.open(1);
    assert_eq!(
        rig.req(1, Request::Stop { grace_ms: 1 }, S),
        Flow::Exit("Stop with the VM absent: nothing to power off".into())
    );
    assert_eq!(rig.reply(1), Reply::Stopping);
}

#[test]
fn a_helper_no_session_attached_to_exits_after_a_minute() {
    let mut rig = Rig::new(Script::default());
    rig.open(1);
    assert_eq!(rig.req(1, Request::State, S), Flow::Go);
    assert!(matches!(rig.reply(1), Reply::State(_)));
    assert_eq!(rig.ticks(S, S * 59), Flow::Go);
    assert_eq!(
        rig.ticks(S * 60, S * 60),
        Flow::Exit("no session attached within 60 s of the helper's start".into())
    );
}

#[test]
fn attaches_from_another_protocol_helper_or_agent_are_refused_by_name() {
    let mut rig = ready_rig(false, true);
    let cases = [
        (
            Attach {
                proto: PROTO + 1,
                ..attach(false)
            },
            "this helper speaks protocol 1; the session's checkout speaks 2",
        ),
        (
            Attach {
                helper_sha256: "0".repeat(64),
                ..attach(false)
            },
            "the session's checkout signed 0000",
        ),
        (
            Attach {
                agent_sha256: "ef".repeat(32),
                ..attach(false)
            },
            "the guest runs agent abab",
        ),
    ];
    for (sid, (a, words)) in (10u64..).zip(cases) {
        rig.open(sid);
        assert_eq!(rig.req(sid, Request::Attach(a), S * 20), Flow::Go);
        assert!(
            matches!(rig.reply(sid), Reply::Refused(why) if why.contains(words)),
            "{words}"
        );
        // A refused connection is no session: it cannot boot or connect.
        assert_eq!(
            rig.req(sid, Request::Connect { port: 1024 }, S * 20),
            Flow::Go
        );
        assert!(matches!(rig.reply(sid), Reply::Error(why) if why.contains("Attach first")));
    }
    assert_eq!(rig.core.attached(), 1);
}

#[test]
fn the_same_agent_with_other_shares_swaps_the_share_live() {
    let mut rig = ready_rig(false, true);
    rig.open(2);
    let other = Attach {
        shares: shares("/src/aterm-wt/other"),
        ..attach(false)
    };
    assert_eq!(rig.req(2, Request::Attach(other), S * 20), Flow::Go);
    assert_eq!(
        rig.reply(2),
        Reply::Attached {
            phase: Phase::Ready,
            keep: true,
            swapped: true
        }
    );
    // A session attaching to a Ready VM hears its hello.
    assert!(matches!(rig.reply(2), Reply::Ready(h) if h.boot_id == BOOT_ID));
    assert_eq!(
        rig.seen.calls().last(),
        Some(&Call::Share(shares("/src/aterm-wt/other")))
    );
    // The same shares again swap nothing.
    rig.open(3);
    let same = Attach {
        shares: shares("/src/aterm-wt/other"),
        ..attach(false)
    };
    assert_eq!(rig.req(3, Request::Attach(same), S * 21), Flow::Go);
    assert!(matches!(
        rig.reply(3),
        Reply::Attached { swapped: false, .. }
    ));
}

#[test]
fn with_objects_held_none_outlives_its_release_or_its_session() {
    let mut rig = ready_rig(true, true);
    let _ = rig.sent(1);
    let watch = rig.seen.live();
    rig.attached(2, false);
    let mut ids = Vec::new();
    for sid in [1, 1, 2] {
        assert_eq!(
            rig.req(sid, Request::Connect { port: 1024 }, S * 20),
            Flow::Go
        );
        let (reply, fd) = rig.reply_fd(sid);
        let Reply::Connected { id, port } = reply else {
            panic!("{reply:?}")
        };
        assert_eq!(port, 1024);
        assert!(fd.is_some());
        ids.push(id);
    }
    assert_eq!(rig.core.held(), 3);
    assert_eq!(rig.seen.live(), watch + 3);
    // Another session's id is not this one's to release.
    assert_eq!(
        rig.req(2, Request::Release { id: ids[0] }, S * 21),
        Flow::Go
    );
    assert_eq!(rig.reply(2), Reply::Done);
    assert_eq!(rig.core.held(), 3);
    assert_eq!(
        rig.req(1, Request::Release { id: ids[0] }, S * 21),
        Flow::Go
    );
    assert_eq!(rig.reply(1), Reply::Done);
    assert_eq!(rig.seen.live(), watch + 2);
    // A session's close lets its objects go.
    assert_eq!(rig.core.handle(Input::Closed(1), S * 22), Flow::Go);
    assert_eq!(rig.seen.live(), watch + 1);
    assert_eq!(rig.req(2, Request::Detach, S * 23), Flow::Go);
    assert_eq!(rig.core.held(), 0);
    assert_eq!(rig.seen.live(), watch);
}

#[test]
fn with_objects_not_held_each_goes_once_its_descriptor_is_passed() {
    let mut rig = ready_rig(false, true);
    let _ = rig.sent(1);
    let watch = rig.seen.live();
    assert_eq!(
        rig.req(1, Request::Connect { port: 1024 }, S * 20),
        Flow::Go
    );
    let (reply, fd) = rig.reply_fd(1);
    assert!(matches!(reply, Reply::Connected { .. }));
    assert_eq!(rig.seen.live(), watch, "dropped once passed");
    // The lab's copy is its own: it still reaches the guest's end.
    let mut ours = UnixStream::from(fd.unwrap());
    wire::write_frame(&mut ours, &Frame::Poweroff).unwrap();
    let mut guest = rig.seen.guest_ends.lock().unwrap().remove(0);
    guest.set_read_timeout(Some(SPAN)).unwrap();
    assert_eq!(wire::read_frame(&mut guest).unwrap(), Some(Frame::Poweroff));
}

#[test]
fn requests_before_attach_and_out_of_phase_are_answered_by_name() {
    let mut rig = Rig::new(Script::default());
    rig.open(1);
    for r in [
        Request::Boot(Box::new(plan(Path::new("/dev/null")))),
        Request::Keep(true),
        Request::Share(Vec::new()),
    ] {
        assert_eq!(rig.req(1, r, S), Flow::Go);
        assert!(matches!(rig.reply(1), Reply::Error(why) if why.contains("Attach first")));
    }
    assert_eq!(rig.req(1, Request::Attach(attach(false)), S), Flow::Go);
    assert_eq!(
        rig.reply(1),
        Reply::Attached {
            phase: Phase::Absent,
            keep: false,
            swapped: false
        }
    );
    assert_eq!(rig.req(1, Request::Connect { port: 1024 }, S), Flow::Go);
    assert_eq!(
        rig.reply(1),
        Reply::Error("the VM is absent: no connection".into())
    );
}

#[test]
fn state_reports_the_record_the_beat_the_free_space_and_the_tail() {
    let mut rig = ready_rig(false, true);
    assert_eq!(
        rig.console("[  OK  ] Reached target multi-user.target.\n", S * 11),
        Flow::Go
    );
    assert_eq!(rig.say(&Frame::Beat { uptime_ms: 1 }, S * 15), Flow::Go);
    assert_eq!(rig.ticks(S * 15, S * 15), Flow::Go);
    rig.open(5);
    assert_eq!(rig.req(5, Request::State, S * 17), Flow::Go);
    let Reply::State(state) = rig.reply(5) else {
        panic!("no state")
    };
    assert_eq!(
        (
            state.pid,
            state.uniq,
            state.helper_sha256.as_str(),
            state.proto
        ),
        (4242, 77, HELPER, PROTO)
    );
    assert_eq!(state.phase, Phase::Ready);
    assert!(state.keep);
    assert_eq!(state.sessions, 1, "a State-only connection is no session");
    assert_eq!(state.boot_id, BOOT_ID);
    assert_eq!(state.agent_sha256, agent_hex());
    assert_eq!(state.beat_ms_ago, Some(2_000));
    assert_eq!(state.free_bytes, Some(200 << 30));
    assert!(state.console_tail.contains("multi-user.target"));
}

#[test]
fn a_keyed_dial_is_kept_and_its_frames_reach_the_core_with_the_key_withheld() {
    let mut rig = Rig::new(Script::default());
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    // The boot's plan gained the key disk, read-only, last; the file is owner-only.
    let Some(Call::Boot(p)) = rig
        .seen
        .calls()
        .into_iter()
        .find(|c| matches!(c, Call::Boot(_)))
    else {
        panic!("booted");
    };
    let key_disk = rig.dir.path().join("key.img");
    assert_eq!(
        p.disks.last(),
        Some(&Disk {
            path: key_disk.clone(),
            read_only: true
        })
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key_disk).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let key = rig.key();
    let (conn, mut guest) = conn_pair(aterm_vz_agent::WATCH_PORT, &rig.seen.live_conns);
    assert_eq!(
        rig.core.handle(Input::Machine(Event::Watch(conn)), S),
        Flow::Go
    );
    assert_eq!(rig.seen.live(), 1, "the dial's object is held");
    wire::write_frame(
        &mut guest,
        &Frame::Hello(crate::machine::fake::hello(AGENT, key)),
    )
    .unwrap();
    wire::write_frame(&mut guest, &Frame::Beat { uptime_ms: 5 }).unwrap();
    let Input::Watch(dial, Frame::Hello(h)) = rig.rx.recv_timeout(SPAN).unwrap() else {
        panic!("the hello first");
    };
    assert_eq!(
        h.key,
        [0; crate::key::LEN],
        "the key is zeroed before the core"
    );
    assert!(matches!(
        rig.rx.recv_timeout(SPAN).unwrap(),
        Input::Watch(d, Frame::Beat { uptime_ms: 5 }) if d == dial
    ));
    drop(guest);
    assert!(matches!(
        rig.rx.recv_timeout(SPAN).unwrap(),
        Input::WatchClosed(d, _) if d == dial
    ));
}

#[test]
fn a_forger_that_dials_first_is_refused_and_the_agent_that_dials_after_is_taken() {
    // A guest process that is not root dials port 1025 before the agent, with a hello that
    // names the plan's agent, a Ready and a Beat, but a key it guessed.
    let mut rig = Rig::with(
        Script {
            on_boot: vec![
                FakeEvent::Started,
                FakeEvent::Forger {
                    agent_sha256: AGENT,
                    speaks: true,
                },
                FakeEvent::Agent {
                    agent_sha256: AGENT,
                    ready: true,
                },
            ],
            ..Script::default()
        },
        false,
        true,
    );
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(
        rig.pump_until(S * 9, |r| r.core.phase() == Phase::Ready),
        Flow::Go
    );
    let (sent, ended) = rig.sent(1);
    assert!(!ended);
    // Every hello the session heard is the agent's: no frame of the forger's was believed.
    let readies: Vec<_> = sent
        .iter()
        .filter_map(|r| match r {
            Reply::Ready(h) => Some(h.boot_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(readies, [BOOT_ID]);
    assert!(
        !sent
            .iter()
            .any(|r| matches!(r, Reply::Booting(n) if n.contains("forged"))),
        "{sent:?}"
    );
    let (path, _) = rig.core.cfg.record.clone().unwrap();
    assert_eq!(
        Record::read(&path).unwrap().boot_id.as_deref(),
        Some(BOOT_ID)
    );
    // The forger's dial is closed (its end reads EOF, and no Clock); the agent's got the
    // host's clock.
    let mut forger = rig.seen.forger_ends.lock().unwrap().remove(0);
    // macOS refuses the option on a socket its peer closed (EINVAL): already closed.
    let _ = forger.set_read_timeout(Some(SPAN));
    assert!(
        !matches!(wire::read_frame(&mut forger), Ok(Some(_))),
        "the forger's dial is closed, and hears nothing"
    );
    let mut end = rig.watch_end();
    end.set_read_timeout(Some(SPAN)).unwrap();
    assert!(matches!(
        wire::read_frame(&mut end).unwrap(),
        Some(Frame::Clock { .. })
    ));
    // Only the agent's dial is held.
    assert_eq!(rig.seen.live(), 1);
}

#[test]
fn the_agents_keyed_dial_closes_every_dial_still_waiting_at_once() {
    // Two guest processes dial before the agent and say nothing, so no tick has closed them
    // when the agent's keyed hello comes.
    let mut rig = Rig::with(
        Script {
            on_boot: vec![
                FakeEvent::Started,
                FakeEvent::Forger {
                    agent_sha256: AGENT,
                    speaks: false,
                },
                FakeEvent::Forger {
                    agent_sha256: AGENT,
                    speaks: false,
                },
                FakeEvent::Agent {
                    agent_sha256: AGENT,
                    ready: true,
                },
            ],
            ..Script::default()
        },
        false,
        true,
    );
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(
        rig.pump_until(S * 9, |r| r.core.phase() == Phase::Ready),
        Flow::Go
    );
    // Long before a silent dial's minute: only the agent's dial is held.
    assert_eq!(rig.seen.live(), 1);
    assert!(rig.core.dials.is_empty());
    let ends = std::mem::take(&mut *rig.seen.forger_ends.lock().unwrap());
    assert_eq!(ends.len(), 2);
    for end in &ends {
        // Closed, not waiting: EOF at once, never WouldBlock (the negative control is the
        // silent dials' own test, where a waiting dial reads WouldBlock until its tick).
        end.set_nonblocking(true).unwrap();
        let mut probe = [0u8; 1];
        let got = (&*end).read(&mut probe);
        assert!(matches!(got, Ok(0)), "a waiting dial was closed: {got:?}");
    }
}

#[test]
fn a_helpers_exit_removes_the_key_disk_and_only_its_own_record_and_socket() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path().join("vz.noindex"));
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    let ours = Record {
        pid: 4242,
        uniq: 77,
        birth_us: 1_792_000_000_000_000,
        helper_sha256: HELPER.into(),
        proto: PROTO,
        live_disk: paths.live_disk(),
        started: 1_792_000_000,
        boot_id: None,
        agent_sha256: None,
    };
    let socket = paths.socket();
    let lay = |record: &Record| {
        record.write(&paths.record()).unwrap();
        std::fs::write(&socket, b"").unwrap();
        std::fs::write(paths.key_disk(), b"aterm-vz-key v1").unwrap();
    };
    lay(&ours);
    release_run_files(&paths, &ours, &socket);
    assert!(!paths.record().exists() && !socket.exists());
    assert!(!paths.key_disk().exists(), "no key outlives its helper");
    // The negative control: another helper's record and socket stay; the key disk this
    // helper wrote under the lock still goes.
    let theirs = Record {
        pid: 5151,
        uniq: 88,
        ..ours.clone()
    };
    lay(&theirs);
    release_run_files(&paths, &ours, &socket);
    assert_eq!(Record::read(&paths.record()).map(|r| r.pid), Some(5151));
    assert!(socket.exists());
    assert!(!paths.key_disk().exists());
}

#[test]
fn a_boot_keeps_the_last_boots_console_beside_its_own() {
    let mut rig = ready_rig(false, true);
    assert_eq!(rig.console("[    1.0] the first boot\n", S * 11), Flow::Go);
    // The guest stops by itself; a second boot follows on another helper's core.
    let log = rig.console_log();
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("the first boot")
    );
    let plan = plan(&log);
    let mut second = Rig::with(agent_script(), false, false);
    second.attached(1, false);
    assert_eq!(
        second.req(1, Request::Boot(Box::new(plan)), Duration::ZERO),
        Flow::Go
    );
    let kept = std::fs::read_to_string(previous_console(&log)).unwrap();
    assert!(kept.contains("the first boot"), "{kept}");
    let fresh = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!fresh.contains("the first boot"), "{fresh}");
}

#[test]
fn the_negative_control_a_forger_alone_never_makes_the_vm_ready_and_the_boot_detector_names_it() {
    let mut rig = Rig::new(Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Forger {
                agent_sha256: AGENT,
                speaks: true,
            },
        ],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.pump(S), Flow::Go, "its refusal");
    assert_eq!(rig.core.phase(), Phase::Booting);
    assert_eq!(rig.ticks(S * 2, S * 59), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Booting);
    assert_eq!(rig.ticks(S * 60, S * 60), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    assert!(!sent.iter().any(|r| matches!(r, Reply::Ready(_))));
    let why = lost_why(&sent);
    assert_eq!(why.len(), 1, "{why:?}");
    assert!(
        why[0].starts_with("NOT RUN — the boot hung before the agent")
            && why[0].ends_with(&format!(
                "1 dial(s) of the watch port refused, the last: its hello (agent {}, boot \
                 forged) carried another key",
                agent_hex()
            )),
        "{why:?}"
    );
}

#[test]
fn silent_dials_close_after_a_minute_and_dials_past_the_cap_close_unread() {
    let mut on_boot = vec![FakeEvent::Started];
    on_boot.extend((0..=MAX_DIALS).map(|_| FakeEvent::Forger {
        agent_sha256: AGENT,
        speaks: false,
    }));
    let mut rig = Rig::new(Script {
        on_boot,
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    let mut ends = std::mem::take(&mut *rig.seen.forger_ends.lock().unwrap());
    assert_eq!(ends.len(), MAX_DIALS + 1);
    // The one past the cap was closed unread at once; the others wait.
    let mut last = ends.pop().unwrap();
    let _ = last.set_read_timeout(Some(SPAN));
    assert!(!matches!(wire::read_frame(&mut last), Ok(Some(_))));
    for end in &ends {
        end.set_nonblocking(true).unwrap();
        let mut probe = [0u8; 1];
        let got = (&*end).read(&mut probe);
        assert!(
            matches!(&got, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a waiting dial is still open: {got:?}"
        );
        end.set_nonblocking(false).unwrap();
    }
    // The console keeps the boot alive; a minute after they dialed, a tick closes them.
    for t in 1..60 {
        assert_eq!(
            rig.console(&format!("[  {t}.0] line {t}\n"), S * t),
            Flow::Go
        );
    }
    assert_eq!(rig.ticks(S * 59, S * 59), Flow::Go);
    assert_eq!(rig.seen.live(), MAX_DIALS);
    assert_eq!(rig.ticks(S * 60, S * 60), Flow::Go);
    assert_eq!(rig.seen.live(), 0);
    for mut end in ends {
        let _ = end.set_read_timeout(Some(SPAN));
        assert!(!matches!(wire::read_frame(&mut end), Ok(Some(_))));
    }
    // The agent's keyed dial after them is taken.
    let key = rig.key();
    let (conn, mut guest) = conn_pair(aterm_vz_agent::WATCH_PORT, &rig.seen.live_conns);
    wire::write_frame(
        &mut guest,
        &Frame::Hello(crate::machine::fake::hello(AGENT, key)),
    )
    .unwrap();
    assert_eq!(
        rig.core.handle(Input::Machine(Event::Watch(conn)), S * 61),
        Flow::Go
    );
    assert_eq!(
        rig.pump_until(S * 61, |r| r.core.hello.is_some()),
        Flow::Go,
        "Hello"
    );
    let (sent, _) = rig.sent(1);
    assert!(
        sent.iter()
            .any(|r| matches!(r, Reply::Booting(n) if n.contains(BOOT_ID))),
        "{sent:?}"
    );
}

#[test]
fn the_agents_own_watch_code_is_taken_after_a_forger_and_beats() {
    let mut rig = Rig::new(Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Forger {
                agent_sha256: AGENT,
                speaks: true,
            },
            FakeEvent::RealAgent {
                agent_sha256: AGENT,
                tick_ms: 20,
            },
        ],
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(
        rig.pump_until(S * 9, |r| r.core.phase() == Phase::Ready),
        Flow::Go
    );
    // Its beats reach the heartbeat detector: past a minute after Ready, the VM is up.
    assert_eq!(
        rig.pump_until(S * 70, |r| {
            r.core.watch.as_ref().and_then(Watch::last_beat) == Some(S * 70)
        }),
        Flow::Go,
        "a Beat"
    );
    assert_eq!(rig.ticks(S * 70, S * 70), Flow::Go);
    assert_eq!(rig.core.phase(), Phase::Ready);
    let (sent, _) = rig.sent(1);
    assert!(matches!(
        sent.iter().find(|r| matches!(r, Reply::Ready(_))),
        Some(Reply::Ready(h)) if h.boot_id == BOOT_ID && h.key == [0; crate::key::LEN]
    ));
}

#[test]
fn a_second_dial_of_the_watch_port_is_refused_and_none_of_its_frames_reach_the_core() {
    let mut rig = ready_rig(false, false);
    let live = rig.seen.live();
    // Something else in the guest dials port 1025 after the agent did, even with this
    // boot's key (guest root), and has already written a beat and a hello.
    let key = rig.key();
    let (conn, mut other) = conn_pair(aterm_vz_agent::WATCH_PORT, &rig.seen.live_conns);
    other.set_read_timeout(Some(SPAN)).unwrap();
    let mut forged = crate::machine::fake::hello([0xcd; 32], key);
    forged.boot_id = "forged".into();
    wire::write_frame(&mut other, &Frame::Hello(forged)).unwrap();
    wire::write_frame(&mut other, &Frame::Beat { uptime_ms: 1 }).unwrap();
    assert_eq!(
        rig.core.handle(Input::Machine(Event::Watch(conn)), S * 11),
        Flow::Go
    );
    assert_eq!(rig.seen.live(), live, "its object was dropped at once");
    assert!(
        !matches!(wire::read_frame(&mut other), Ok(Some(_))),
        "its connection is closed, unread"
    );
    // Nothing of it reaches the core (a short wait: nothing must happen within it).
    assert!(
        rig.rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "no reader posts its frames"
    );
    assert_eq!(rig.core.phase(), Phase::Ready);
    // The negative control: the agent's own connection is still the watch connection.
    assert_eq!(
        rig.say(&Frame::Beat { uptime_ms: 15_000 }, S * 15),
        Flow::Go
    );
    // And once the agent goes silent, the heartbeat detector fires a minute later: no beat
    // of another dial holds the VM up.
    assert_eq!(rig.ticks(S * 16, S * 74), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 75, S * 75), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    let (sent, _) = rig.sent(1);
    assert_eq!(
        lost_why(&sent),
        ["the guest stopped answering (no heartbeat for 60 s); VM stopped"]
    );
}

#[test]
fn a_hello_is_believed_only_with_this_boots_key() {
    let key = [0x42; crate::key::LEN];
    let mut other = key;
    other[31] ^= 1;
    let h = crate::machine::fake::hello(AGENT, key);
    assert_eq!(
        keyed_hello(Frame::Hello(h.clone()), &key).map(|h| h.key),
        Ok([0; crate::key::LEN])
    );
    assert!(
        keyed_hello(Frame::Hello(h), &other)
            .unwrap_err()
            .contains("carried another key")
    );
    assert!(
        keyed_hello(Frame::Beat { uptime_ms: 1 }, &key)
            .unwrap_err()
            .contains("not the agent's hello")
    );
}

#[test]
fn a_hard_stop_that_never_finishes_ends_the_helper_a_minute_later() {
    let panic = "[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs\n";
    let mut rig = Rig::new(Script {
        hard_stop_completes: false,
        ..Script::default()
    });
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.console(panic, S * 2), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    assert_eq!(
        rig.ticks(S * 3, S * 61),
        Flow::Go,
        "the hard stop has its minute"
    );
    assert_eq!(
        rig.ticks(S * 62, S * 62),
        Flow::Exit(
            "the hard stop did not finish within 60 s; the helper exits, which ends its VM".into()
        )
    );
    // The negative control: a hard stop that finishes ends the helper by its completion.
    let mut rig = Rig::new(Script::default());
    rig.attached(1, false);
    assert_eq!(rig.boot(1, Duration::ZERO), Flow::Go);
    assert_eq!(rig.console(panic, S * 2), Flow::Go);
    assert!(
        matches!(rig.pump(S * 2), Flow::Exit(why) if why.starts_with("the VM stopped: NOT RUN — the guest's kernel panicked"))
    );
}

#[test]
fn an_attach_while_the_vm_stops_is_refused_and_one_before_the_stop_is_not() {
    let mut rig = ready_rig(false, true);
    // The negative control: before the stop a second session joins the running VM.
    rig.attached(2, false);
    assert_eq!(rig.core.attached(), 2);
    rig.open(3);
    assert_eq!(
        rig.req(3, Request::Stop { grace_ms: 60_000 }, S * 20),
        Flow::Go
    );
    assert_eq!(rig.reply(3), Reply::Stopping);
    assert_eq!(rig.core.phase(), Phase::Stopping);
    rig.open(4);
    assert_eq!(rig.req(4, Request::Attach(attach(true)), S * 21), Flow::Go);
    assert_eq!(
        rig.reply(4),
        Reply::Refused("the VM is stopping: this helper exits once its sessions close".into())
    );
    assert_eq!(rig.core.attached(), 2, "the refused one is no session");
    assert!(!rig.core.keep(), "nor did it set keep");
}

#[test]
fn stop_waits_its_own_grace_after_the_last_console_line_before_the_hard_stop() {
    // The grace is the Stop's (10 s), not the release rule's minute.
    let mut rig = ready_rig(false, true);
    rig.open(3);
    assert_eq!(
        rig.req(3, Request::Stop { grace_ms: 10_000 }, S * 20),
        Flow::Go
    );
    assert_eq!(rig.ticks(S * 21, S * 29), Flow::Go);
    assert!(
        !rig.seen.calls().contains(&Call::HardStop),
        "the grace runs"
    );
    assert_eq!(rig.ticks(S * 30, S * 30), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    // A console line within it is progress: the grace runs again from that line.
    let mut rig = ready_rig(false, true);
    rig.open(3);
    assert_eq!(
        rig.req(3, Request::Stop { grace_ms: 10_000 }, S * 20),
        Flow::Go
    );
    assert_eq!(rig.ticks(S * 21, S * 26), Flow::Go);
    assert_eq!(
        rig.console("[  OK  ] Stopped target graphical.target.\n", S * 27),
        Flow::Go
    );
    assert_eq!(rig.ticks(S * 27, S * 36), Flow::Go);
    assert!(!rig.seen.calls().contains(&Call::HardStop));
    assert_eq!(rig.ticks(S * 37, S * 37), Flow::Go);
    assert_eq!(rig.seen.calls().last(), Some(&Call::HardStop));
    assert!(
        matches!(rig.pump(S * 37), Flow::Exit(why) if why.contains("no new console line for 10 s")),
    );
}

#[test]
fn a_connection_opened_behind_the_input_the_core_exits_on_ends_with_its_sessions() {
    // The core takes its inputs in order, so a `down` that connected while the last session
    // was closing is opened behind the close the core exits on. Its reader thread holds a
    // duplicate of the socket (`held` here): only a shutdown gives the lab EOF.
    let rig = Rig::new(agent_script());
    let dir = aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let socket = dir.path().join("s.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let (ours, lab) = UnixStream::pair().unwrap();
    let held = ours.try_clone().unwrap();
    rig.tx
        .send(Input::Request(9, Request::Stop { grace_ms: 1 }))
        .unwrap();
    rig.tx.send(Input::Opened(1, ours)).unwrap();
    let (why, core) = serve(listener, &socket, rig.core, &rig.tx, &rig.rx);
    assert!(why.contains("nothing to power off"), "{why}");
    drop(core);
    lab.set_read_timeout(Some(SPAN)).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(
        std::io::Read::read(&mut &lab, &mut byte).expect("EOF within a minute, not silence"),
        0
    );
    drop(held);
}

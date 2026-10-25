// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session rules with the Fake `Helper` (§13): §5.1's table, verb by verb, through the
//! real client, protocol, serve loop and core, with the Fake machine on a thread over a
//! scratch state directory. Every wait is a one-minute hang detector.

use super::fake::Threaded;
use super::*;
use crate::machine::fake::{BOOT_ID, Call, FakeEvent, Script};
use crate::plan::{COMMAND_LINE, Disk};
use crate::record::DiskLock;

const AGENT: [u8; 32] = [0xab; 32];

fn helper_a() -> String {
    "a1".repeat(32)
}

fn obeying(agent: [u8; 32]) -> Script {
    Script {
        on_boot: vec![
            FakeEvent::Started,
            FakeEvent::Agent {
                agent_sha256: agent,
                ready: true,
            },
        ],
        guest_obeys: true,
        ..Script::default()
    }
}

fn shares(repo: &str) -> Vec<ShareEntry> {
    vec![ShareEntry {
        name: "repo".into(),
        path: repo.into(),
    }]
}

fn attach(helper: &str, agent: [u8; 32], keep: bool) -> Attach {
    Attach {
        proto: crate::proto::PROTO,
        helper_sha256: helper.into(),
        agent_sha256: crate::hex(&agent),
        shares: shares("/src/aterm"),
        keep,
    }
}

fn plan(paths: &Paths, agent: [u8; 32]) -> Plan {
    Plan {
        name: "aterm-linux".into(),
        cpus: 8,
        memory_bytes: 16 << 30,
        kernel: "/vz/kernel/Image".into(),
        initrd: "/vz/kernel/initrd".into(),
        command_line: COMMAND_LINE.into(),
        disks: vec![Disk {
            path: paths.live_disk(),
            read_only: false,
        }],
        mac: crate::plan::mac_for("aterm-linux"),
        boot_share: "/vz/agent/ab".into(),
        shares: shares("/src/aterm"),
        rosetta: true,
        console_log: paths.console_log(),
        agent_sha256: crate::hex(&agent),
    }
}

/// A scratch state directory short enough for the socket's `sun_path` (`$TMPDIR` on macOS
/// is not), as aterm-uds' own socket tests take.
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path")
}

struct Rig {
    lab: Lab<Threaded>,
    _dir: aterm_tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let dir = scratch();
        let paths = Paths::at(dir.path().join("vz.noindex"));
        Self {
            lab: Lab::new(paths, Threaded::new(obeying(AGENT), &helper_a())),
            _dir: dir,
        }
    }

    fn paths(&self) -> Paths {
        self.lab.paths.clone()
    }

    /// A session with `keep`, booted to Ready.
    fn booted(&mut self, keep: bool) -> Session {
        let mut s = self.lab.session(&attach(&helper_a(), AGENT, keep)).unwrap();
        let mut notes = Vec::new();
        let hello = s
            .boot(&plan(&self.paths(), AGENT), &mut |n| {
                notes.push(n.to_string())
            })
            .unwrap();
        assert_eq!(hello.boot_id, BOOT_ID);
        assert!(notes.iter().any(|n| n == "the VM started"), "{notes:?}");
        s
    }

    fn boots(&self) -> usize {
        self.lab
            .start
            .last()
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Boot(_)))
            .count()
    }

    /// The running helper's state, waiting (a one-minute hang detector) until `done` holds.
    fn state_until(&mut self, done: impl Fn(&StateReport) -> bool) -> StateReport {
        let begun = Instant::now();
        loop {
            let s = self.lab.status().unwrap().expect("a live helper");
            if done(&s) {
                return s;
            }
            assert!(
                begun.elapsed() < SPAN,
                "no such state within a minute: {s:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// The helper's end: its thread joined (a one-minute hang detector), its run files
    /// gone and the disk's lock free.
    fn ended(&mut self) -> String {
        let ended = self.lab.start.join().expect("a helper ran").unwrap();
        let paths = self.paths();
        assert!(!paths.record().exists() && !paths.socket().exists());
        assert!(DiskLock::try_take(&paths.disk_lock()).unwrap().is_some());
        ended
    }
}

#[test]
fn up_keeps_the_vm_and_an_exec_under_up_attaches_and_leaves_it_running() {
    let mut rig = Rig::new();
    // `up`: boots, sets keep, detaches.
    let up = rig.booted(true);
    up.detach();
    let state = rig.state_until(|s| s.sessions == 0);
    assert_eq!((state.phase, state.keep), (Phase::Ready, true));
    // `exec` under `up`: attaches to the running VM (booted_here false), detaches.
    let mut exec = rig.lab.session(&attach(&helper_a(), AGENT, false)).unwrap();
    assert_eq!(exec.phase, Phase::Ready);
    assert!(exec.keep, "keep stays set");
    let hello = exec.boot(&plan(&rig.paths(), AGENT), &mut |_| {}).unwrap();
    assert_eq!(hello.boot_id, BOOT_ID, "the VM that up left running");
    exec.detach();
    let state = rig.state_until(|s| s.sessions == 0);
    assert_eq!(state.phase, Phase::Ready);
    assert_eq!((rig.lab.start.starts(), rig.boots()), (1, 1));
    // `down`: Stop; the helper powers the guest off through its agent and exits.
    rig.lab.down().unwrap();
    assert_eq!(
        rig.ended(),
        "the VM stopped: the VM was stopped by a Stop (down)"
    );
    assert!(
        rig.lab
            .start
            .last()
            .calls()
            .contains(&Call::Connect(aterm_vz_agent::COMMAND_PORT))
    );
    assert_eq!(rig.lab.status().unwrap(), None);
}

#[test]
fn an_exec_with_no_vm_boots_and_its_detach_stops_the_vm() {
    let mut rig = Rig::new();
    let exec = rig.booted(false);
    exec.detach();
    assert!(
        rig.ended()
            .starts_with("the VM stopped: the last session closed with keep unset"),
        "{:?}",
        rig.lab.start.ended
    );
}

#[test]
fn a_lab_that_dies_mid_run_stops_the_vm_it_booted() {
    let mut rig = Rig::new();
    let exec = rig.booted(false);
    // EOF, no Detach: what the lab's death leaves.
    drop(exec);
    assert!(
        rig.ended()
            .starts_with("the VM stopped: the last session closed with keep unset")
    );
}

#[test]
fn keep_up_on_any_verb_keeps_the_vm_up() {
    let mut rig = Rig::new();
    let mut proof = rig.booted(false);
    proof.keep(true).unwrap();
    proof.detach();
    let state = rig.state_until(|s| s.sessions == 0);
    assert_eq!((state.phase, state.keep), (Phase::Ready, true));
    rig.lab.down().unwrap();
    rig.ended();
}

#[test]
fn status_reads_the_state_and_opens_nothing_without_a_record() {
    let mut rig = Rig::new();
    assert_eq!(rig.lab.status().unwrap(), None);
    assert_eq!(rig.lab.start.starts(), 0, "status never starts a helper");
    assert!(!rig.paths().socket().exists());
    // `down` with no helper does nothing either.
    rig.lab.down().unwrap();
    assert_eq!(rig.lab.start.starts(), 0);
}

#[test]
fn destroy_stops_the_helper_before_anything_is_deleted() {
    let mut rig = Rig::new();
    let up = rig.booted(true);
    up.detach();
    let live = rig.paths().live_disk();
    std::fs::write(&live, b"disk").unwrap();
    // While the helper runs, the disk is held.
    assert!(
        DiskLock::try_take(&rig.paths().disk_lock())
            .unwrap()
            .is_none()
    );
    rig.lab.down().unwrap();
    // `down` returned only once the helper let the disk go: destroy may delete it now.
    rig.ended();
    std::fs::remove_file(&live).unwrap();
}

#[test]
fn a_session_from_another_checkouts_helper_replaces_it() {
    let mut rig = Rig::new();
    let up = rig.booted(true);
    up.detach();
    let other = "b2".repeat(32);
    rig.lab.start.helper_sha256 = other.clone();
    let s = rig.lab.session(&attach(&other, AGENT, false)).unwrap();
    assert_eq!(s.phase, Phase::Absent, "a new helper, no VM yet");
    assert_eq!(rig.lab.start.starts(), 2);
    assert_eq!(
        rig.lab.start.ended,
        [Ok(
            "the VM stopped: the VM was stopped by a Stop (down)".to_string()
        )]
    );
    let record = Record::read(&rig.paths().record()).unwrap();
    assert_eq!(record.helper_sha256, other);
    let replaced = rig
        .lab
        .replaced
        .clone()
        .expect("the lab names the helper it replaced");
    assert_eq!(replaced.record.helper_sha256, helper_a());
    // A start that then met the framework's storage refusal names that helper.
    let refusal = "Error Domain=VZErrorDomain Code=2 \"The storage device attachment is invalid.\"";
    let words = failed_words(
        refusal,
        Path::new("/vz/host/x/aterm-vz-host"),
        Some(&replaced),
    );
    assert!(
        words.starts_with(&format!(
            "NOT RUN — aterm-linux.img is still attached to the VM of helper {} (unique id {}), which ended by",
            replaced.record.pid, replaced.record.uniq
        )) && words.contains("(fact 44)"),
        "{words}"
    );
    s.detach();
    rig.ended();
}

#[test]
fn the_entitlements_refusal_names_the_unsigned_helper_and_other_errors_keep_their_words() {
    let exe = Path::new("/src/target/debug/aterm-vz-host");
    let entitlement = "Error Domain=VZErrorDomain Code=2 \"Invalid virtual machine configuration. The \
                       process doesn’t have the “com.apple.security.virtualization” entitlement.\"";
    let words = failed_words(entitlement, exe, None);
    assert!(
        words.starts_with(
            "NOT RUN — /src/target/debug/aterm-vz-host lacks the com.apple.security.virtualization entitlement"
        ) && words.contains("aterm-vz-host.entitlements"),
        "{words}"
    );
    // Negative control: the storage refusal with no replaced helper, and any other error,
    // are the framework's words.
    let storage = "Error Domain=VZErrorDomain Code=2 \"The storage device attachment is invalid.\"";
    assert_eq!(
        failed_words(storage, exe, None),
        format!("NOT RUN — {storage}")
    );
    assert_eq!(
        failed_words("Code=1 boom", exe, None),
        "NOT RUN — Code=1 boom"
    );
}

#[test]
fn a_session_with_another_agent_replaces_the_vm() {
    let mut rig = Rig::new();
    let up = rig.booted(true);
    up.detach();
    let s = rig
        .lab
        .session(&attach(&helper_a(), [0xcd; 32], true))
        .unwrap();
    assert_eq!(s.phase, Phase::Absent);
    assert_eq!(rig.lab.start.starts(), 2);
    s.detach();
    rig.lab.down().unwrap();
    rig.ended();
}

/// A failed send or read is the helper's end when its side of the socket is gone: a closed
/// pipe, a reset, or a socket no longer connected (`down` racing a helper that had just found
/// no session left read ENOTCONN). Any other failure keeps its words.
#[test]
fn a_socket_its_helper_left_is_a_closed_session_and_other_failures_keep_their_words() {
    for errno in [libc::EPIPE, libc::ECONNRESET, libc::ENOTCONN] {
        let e = ProtoError::Io(std::io::Error::from_raw_os_error(errno));
        assert_eq!(
            read_error(e, "Stop", SPAN),
            ClientError::Closed,
            "errno {errno}"
        );
    }
    let e = ProtoError::Io(std::io::Error::from_raw_os_error(libc::EBADF));
    assert!(matches!(read_error(e, "Stop", SPAN), ClientError::Io(_)));
}

/// A connection the helper closed before the lab set its span (the helper's exit, with the
/// lab's `connect` already through) is a closed session, which every verb reads as the
/// helper's end, never the "Invalid argument" XNU answers the span with on such a socket.
#[test]
fn a_connection_the_helper_closed_before_its_span_is_a_closed_session() {
    let (lab, helper) = UnixStream::pair().unwrap();
    drop(helper);
    let e = Session::attach(lab, &attach(&helper_a(), AGENT, false), SPAN).unwrap_err();
    assert_eq!(e, ClientError::Closed);
}

#[test]
fn the_same_agent_with_other_shares_swaps_the_share_with_no_reboot() {
    let mut rig = Rig::new();
    let up = rig.booted(true);
    up.detach();
    let other = Attach {
        shares: shares("/src/aterm-wt/other"),
        ..attach(&helper_a(), AGENT, false)
    };
    let s = rig.lab.session(&other).unwrap();
    assert!(s.swapped);
    assert_eq!(s.phase, Phase::Ready);
    assert_eq!((rig.lab.start.starts(), rig.boots()), (1, 1));
    assert!(
        rig.lab
            .start
            .last()
            .calls()
            .contains(&Call::Share(shares("/src/aterm-wt/other")))
    );
    s.detach();
    rig.lab.down().unwrap();
    rig.ended();
}

#[test]
fn no_connection_object_outlives_its_release_or_its_session() {
    let mut rig = Rig::new();
    rig.lab.start.hold_conns = true;
    let mut s = rig.booted(true);
    let seen = rig.lab.start.last();
    let watch = seen.live();
    let (first, _fd1) = s.connect(aterm_vz_agent::COMMAND_PORT).unwrap();
    let (_second, _fd2) = s.connect(aterm_vz_agent::COMMAND_PORT).unwrap();
    assert_eq!(seen.live(), watch + 2);
    s.release(first).unwrap();
    assert_eq!(
        seen.live(),
        watch + 1,
        "Release answers once the object is gone"
    );
    s.detach();
    let begun = Instant::now();
    while seen.live() != watch {
        assert!(begun.elapsed() < SPAN, "the session's objects outlived it");
        std::thread::sleep(POLL);
    }
    rig.lab.down().unwrap();
    rig.ended();
}

#[test]
fn a_refused_connect_and_a_lost_guest_reach_the_session_in_their_words() {
    let dir = scratch();
    let paths = Paths::at(dir.path().join("vz.noindex"));
    let script = Script {
        connect_refusal: Some(
            "Error Domain=NSPOSIXErrorDomain Code=54 \"Connection reset by peer\"".into(),
        ),
        ..obeying(AGENT)
    };
    let mut lab = Lab::new(paths.clone(), Threaded::new(script, &helper_a()));
    let mut s = lab.session(&attach(&helper_a(), AGENT, false)).unwrap();
    s.boot(&plan(&paths, AGENT), &mut |_| {}).unwrap();
    let e = s.connect(aterm_vz_agent::COMMAND_PORT).unwrap_err();
    assert!(
        matches!(&e, ClientError::Error(why) if why.contains("Connection reset by peer")),
        "{e}"
    );
    // The guest stops by itself: the session hears it before the helper ends it.
    lab.start.last().send(crate::machine::Event::GuestStopped);
    let lost = s
        .until("the guest", &mut |_| {}, |_, _| {
            None::<Result<(), ClientError>>
        })
        .unwrap_err();
    assert_eq!(
        lost,
        ClientError::Lost {
            why: "the guest stopped: it powered itself off".into(),
            tail: String::new()
        }
    );
    drop(s);
    let ended = lab.start.join().unwrap().unwrap();
    assert_eq!(
        ended,
        "the VM stopped: the guest stopped: it powered itself off"
    );
}

#[test]
fn a_session_waits_for_its_guests_own_power_off_and_hears_how_the_vm_stopped() {
    // The bake's end: `Poweroff` on a command connection of the session's, and the session
    // held until the helper says the VM stopped.
    let mut rig = Rig::new();
    let mut s = rig.booted(false);
    let (_id, fd) = s.connect(aterm_vz_agent::COMMAND_PORT).unwrap();
    crate::agent::poweroff(fd).unwrap();
    assert_eq!(
        s.wait_stopped().unwrap(),
        crate::helper::POWERED_OFF,
        "a guest that powered itself off"
    );
    s.detach();
    assert_eq!(
        rig.ended(),
        format!("the VM stopped: {}", crate::helper::POWERED_OFF)
    );
    // The negative control: a VM the helper stopped (a `Stop` from another verb) is heard
    // in the stop's words, which a bake never seals after.
    let mut rig = Rig::new();
    let mut s = rig.booted(false);
    let paths = rig.paths();
    let stopper = std::thread::spawn(move || stop(&paths, 60_000, SPAN));
    let why = s.wait_stopped().unwrap();
    assert_ne!(why, crate::helper::POWERED_OFF);
    assert!(why.contains("stopped by a Stop"), "{why}");
    stopper.join().unwrap().unwrap();
    s.detach();
    let _ = rig.ended();
}

#[test]
fn a_command_lost_while_the_vm_stops_hears_why_it_stopped_never_only_stopping() {
    // A guest that ignores the power-off: a `Stop` holds the VM in `Stopping` for its grace,
    // then stops it hard, as the heartbeat detector does to a guest that stopped answering.
    let mut rig = Rig::new();
    rig.lab.start = Threaded::new(
        Script {
            guest_obeys: false,
            ..obeying(AGENT)
        },
        &helper_a(),
    );
    let mut s = rig.booted(false);
    let paths = rig.paths();
    let stopper = std::thread::spawn(move || stop(&paths, 3_000, SPAN));
    rig.state_until(|st| st.phase == Phase::Stopping);
    // A command connection ended now: its words wait for the stop's own.
    let why = s.why_lost();
    assert!(why.contains("stopped by a Stop"), "{why}");
    assert!(!why.contains("the VM is stopping"), "{why}");
    stopper.join().unwrap().unwrap();
    s.detach();
    let _ = rig.ended();
    // A `Lost` that another wait read first (on the VM, a command's `Release` once its
    // connection ended) is still the words, though the helper has ended the session since.
    let mut rig = Rig::new();
    let mut s = rig.booted(false);
    stop(&rig.paths(), 0, SPAN).unwrap();
    let heard = s.wait_stopped().unwrap();
    assert!(heard.contains("stopped by a Stop"), "{heard}");
    assert_eq!(s.why_lost(), heard);
    drop(s);
    let _ = rig.ended();
    // The negative control: on a ready VM nothing is waited for, and the words say so.
    let mut rig = Rig::new();
    let mut s = rig.booted(false);
    assert_eq!(
        s.why_lost(),
        "the agent closed the command connection before its Exit (the VM is still ready)"
    );
    s.detach();
    let _ = rig.ended();
}

#[test]
fn up_prepares_the_disks_only_for_a_session_that_found_no_vm() {
    let mut rig = Rig::new();
    let mut prepared = 0;
    // A fresh helper has no VM: this session prepares, then boots.
    let mut up = rig.lab.session(&attach(&helper_a(), AGENT, true)).unwrap();
    assert_eq!(up.phase, Phase::Absent);
    let paths = rig.paths();
    let hello = up
        .bring_up(
            || {
                prepared += 1;
                Ok::<_, String>(plan(&paths, AGENT))
            },
            &mut |_| {},
        )
        .unwrap();
    assert_eq!((hello.boot_id.as_str(), prepared), (BOOT_ID, 1));
    up.detach();
    rig.state_until(|s| s.sessions == 0);
    // A second `up` meets the VM the first kept: it prepares nothing (the preparing removes
    // every `*.partial.img`, the disk a bake's VM runs on) and boots nothing.
    let mut again = rig.lab.session(&attach(&helper_a(), AGENT, true)).unwrap();
    assert_eq!(again.phase, Phase::Ready);
    let hello = again
        .bring_up(
            || {
                prepared += 1;
                Err("a running VM's disks were prepared".to_string())
            },
            &mut |_| {},
        )
        .unwrap();
    assert_eq!((hello.boot_id.as_str(), prepared), (BOOT_ID, 1));
    assert_eq!((rig.lab.start.starts(), rig.boots()), (1, 1));
    again.detach();
    rig.lab.down().unwrap();
    rig.ended();
}

#[test]
fn a_plan_that_could_not_be_made_boots_nothing_and_the_helper_ends() {
    let mut rig = Rig::new();
    let mut s = rig.lab.session(&attach(&helper_a(), AGENT, false)).unwrap();
    let err = s
        .bring_up(|| Err::<Plan, _>("below the floor"), &mut |_| {})
        .unwrap_err();
    assert_eq!(err, UpError::Prepare("below the floor"));
    drop(s);
    assert_eq!(rig.ended(), "no session is left, and no VM runs");
    assert_eq!(rig.boots(), 0);
}

#[test]
fn join_never_stops_or_replaces_the_vm_it_is_refused_by() {
    // A guest still booting (no hello yet), as `up` left it for an `exec` in another shell.
    let mut rig = Rig::new();
    rig.lab.start = Threaded::new(
        Script {
            on_boot: vec![FakeEvent::Started],
            guest_obeys: true,
            ..Script::default()
        },
        &helper_a(),
    );
    let up = rig.lab.session(&attach(&helper_a(), AGENT, true)).unwrap();
    up.send(&Request::Boot(Box::new(plan(&rig.paths(), AGENT))))
        .unwrap();
    rig.state_until(|s| s.phase == Phase::Booting);
    // A session naming another agent is refused, and join leaves the VM as it was.
    let other = attach(&helper_a(), [0xcd; 32], false);
    let Err(ClientError::Refused(why)) = rig.lab.join(&other) else {
        panic!("join reports the refusal");
    };
    assert!(why.contains("the guest runs agent abab"), "{why}");
    let state = rig.state_until(|_| true);
    assert_eq!(
        (state.phase, rig.lab.start.starts()),
        (Phase::Booting, 1),
        "the booting VM runs on"
    );
    // The negative control: `session` replaces the VM on the same refusal.
    let s = rig.lab.session(&other).unwrap();
    assert_eq!((s.phase, rig.lab.start.starts()), (Phase::Absent, 2));
    drop(up);
    s.detach();
    rig.ended();
    // With no helper at all, join starts none.
    assert!(matches!(
        rig.lab.join(&attach(&helper_a(), AGENT, false)),
        Err(ClientError::NoHelper(_))
    ));
    assert_eq!(rig.lab.start.starts(), 2);
}

#[test]
fn not_run_is_said_once_whatever_the_helpers_words_begin_with() {
    let exe = Path::new("/vz/host/x/aterm-vz-host");
    let dlopen = "NOT RUN — /System/Library/Frameworks/Virtualization.framework/Virtualization: \
                  dlopen failed";
    assert_eq!(failed_words(dlopen, exe, None), dlopen);
    assert_eq!(
        ClientError::Failed {
            error: dlopen.into(),
            tail: String::new()
        }
        .to_string(),
        dlopen
    );
    assert_eq!(not_run_words(dlopen), dlopen);
    // The negative control: words without it gain it, once.
    assert_eq!(not_run_words("Code=1 boom"), "NOT RUN — Code=1 boom");
    assert_eq!(
        ClientError::Failed {
            error: "Code=1 boom".into(),
            tail: String::new()
        }
        .to_string(),
        "NOT RUN — Code=1 boom"
    );
    // A refusal the lab rewords keeps the framework's words inside, unprefixed.
    let entitlement = "NOT RUN — Error Domain=VZErrorDomain Code=2 \"… the \
                       “com.apple.security.virtualization” entitlement.\"";
    let words = failed_words(entitlement, exe, None);
    assert_eq!(words.matches("NOT RUN").count(), 1, "{words}");
    assert!(words.contains("(Error Domain=VZErrorDomain"), "{words}");
}

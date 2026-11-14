// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two windows on one Mac, as when a dev `aterm-gui` runs beside the installed app on the same
//! state: a fleet drive reaches the window that hosts its session, one naming none the installed
//! app's, and a survey reads both. Each window is a relay acceptor this test serves; the node's
//! drive and survey are handed lines as a peer's connection hands them.

use std::collections::BTreeSet;
use std::sync::Mutex;

use aterm_net::fleet::{DriveGrant, NodeKey};
use aterm_types::control_verbs::Requester;
use aterm_types::fleet_access::FleetAccess;
use aterm_uds::CtlStream;

use super::super::{
    NodeOpenTable, WindowRelays, activity_of_windows, merge_window_surveys, relay_names,
    relay_to_window, spawn_node_drive, spawn_node_survey,
};
use super::*;

/// A live stand-in process whose pid a second test window's relay is named for: a window's relay
/// is found by a live pid, and the order of the two pids is which window is the newer.
struct StandIn(aterm_uds::spawnfd::Child);

impl StandIn {
    fn start() -> Self {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        Self(
            aterm_uds::spawnfd::command(&sleep)
                .arg("600")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the stand-in starts"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The sessions a test window holds: those it hosts, the retired workers whose rows it keeps,
/// and which of its sessions have task news.
#[derive(Clone, Default)]
struct Holds {
    live: BTreeSet<String>,
    retired: BTreeSet<String>,
    news: BTreeSet<String>,
}

impl Holds {
    fn live(sids: &[&str]) -> Self {
        Self {
            live: sids.iter().map(|sid| (*sid).to_owned()).collect(),
            ..Self::default()
        }
    }

    fn with_retired(mut self, sids: &[&str]) -> Self {
        self.retired = sids.iter().map(|sid| (*sid).to_owned()).collect();
        self
    }

    fn with_news(mut self, sids: &[&str]) -> Self {
        self.news = sids.iter().map(|sid| (*sid).to_owned()).collect();
        self
    }

    fn holds(&self, sid: &str) -> bool {
        self.live.contains(sid) || self.retired.contains(sid)
    }
}

/// A window this test serves at `wf-<pid>.sock`: it takes a drive by the relay's own rule over
/// its `holds` (or, as `every_drive`, takes each drive whatever it names, as every window did
/// before drives were routed by the sessions they name), records each line a drive hands it, and
/// answers each as a window's drive gate would.
struct TestWindow {
    served: Arc<Mutex<Vec<String>>>,
    /// Each drive's requester, as the window read it in the drive's relay header.
    requesters: Arc<Mutex<Vec<Requester>>>,
    stop: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl TestWindow {
    fn open(relay_dir: &Path, pid: u32, holds: Holds, every_drive: bool) -> Self {
        let served = Arc::new(Mutex::new(Vec::new()));
        let requesters = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let dir = relay_dir.to_path_buf();
            let stop = Arc::clone(&stop);
            let served = Arc::clone(&served);
            let requesters = Arc::clone(&requesters);
            let hosting = holds.clone();
            std::thread::spawn(move || {
                let hosts = move |sid: &SessionId| every_drive || hosting.holds(sid.as_str());
                let _ =
                    serve_window_relay_at(&dir, pid, Some(&stop), hosts, move |header, stream| {
                        let served = Arc::clone(&served);
                        let requesters = Arc::clone(&requesters);
                        let holds = holds.clone();
                        std::thread::spawn(move || match header {
                            RelayHeader::Drive { requester, .. } => {
                                requesters
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .push(requester);
                                serve_drive(pid, stream, &holds, &served);
                            }
                            RelayHeader::Survey { .. } => {
                                serve_survey(pid, stream, &holds, &served)
                            }
                            RelayHeader::Ask { .. } | RelayHeader::Applies | RelayHeader::Needs => {
                            }
                        });
                        true
                    });
            })
        };
        let path = relay_socket_path(relay_dir, pid);
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the window's relay", || {
            super::super::relay_answers(&path).then_some(())
        });
        Self {
            served,
            requesters,
            stop,
            serving: Some(serving),
        }
    }

    fn served(&self) -> Vec<String> {
        self.served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn requesters(&self) -> Vec<Requester> {
        self.requesters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Serve one drive's lines as a window's drive gate and `await task` answer them, from `holds`.
fn serve_drive(pid: u32, mut stream: CtlStream, holds: &Holds, served: &Mutex<Vec<String>>) {
    while let Ok(Some(line)) = read_line_raw(&mut stream, 4096) {
        let line = line.trim_end().to_owned();
        served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.clone());
        let Some(reply) = gate_answer(pid, &line, holds) else {
            // A wait with no news parks until its driver hangs up.
            let mut rest = [0u8; 64];
            while matches!(stream.read(&mut rest), Ok(n) if n > 0) {}
            return;
        };
        if stream.write_all(reply.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
    }
}

/// The survey a window answers of its own sessions: its `instance` line (it counts two
/// instances here), a row per hosted session (`s-busy…` ones mid-turn), and this Mac's one
/// account line.
fn window_survey(pid: u32, holds: &Holds) -> String {
    let rows: Vec<String> = holds
        .live
        .iter()
        .map(|sid| {
            let agent = if sid.starts_with("s-busy") {
                "busy"
            } else {
                "idle"
            };
            format!(
                "session local=1 sid={sid} state=running program=claude program_kind=agent \
                 agent={agent} attention=- needs=- window={pid}"
            )
        })
        .collect();
    let mut out = format!(
        "OK {}\ninstance pid={pid} host=m111 version=0.150.0 sessions={} level=content cut=0 \
         at_ms=1 node={} instances=2 accounts=1\n",
        rows.len() + 2,
        rows.len(),
        "11".repeat(16),
    );
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out.push_str(&format!("account pool=claude id=a{pid}\n"));
    out
}

/// Serve one survey connection's lines from `holds`.
fn serve_survey(pid: u32, mut stream: CtlStream, holds: &Holds, served: &Mutex<Vec<String>>) {
    while let Ok(Some(line)) = read_line_raw(&mut stream, 4096) {
        let line = line.trim_end().to_owned();
        served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.clone());
        let reply = if line.starts_with("survey") {
            window_survey(pid, holds)
        } else if line == "version" {
            format!("OK version window={pid}\n")
        } else {
            "ERR denied: the fleet serves only version, survey\n".to_owned()
        };
        if stream.write_all(reply.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
    }
}

/// What a window's drive gate answers `line` from `holds` (`None`: a wait it parks on).
fn gate_answer(pid: u32, line: &str, holds: &Holds) -> Option<String> {
    const SID: &str = "ERR denied: a drive names a session the driven Mac hosts by its sid";
    let mut words = line.split_whitespace();
    let first = words.next().unwrap_or_default();
    if let Some(sid) = first.strip_prefix('@') {
        let verb = words.next().unwrap_or_default();
        return Some(if holds.live.contains(sid) {
            format!("OK 1\nwindow={pid} {verb} {sid}\n")
        } else if holds.retired.contains(sid) && verb == "result" {
            format!("OK 1\nwindow={pid} retired result {sid}\n")
        } else if holds.retired.contains(sid) {
            "ERR denied: a worker whose tab the driven Mac closed once its task concluded answers \
             only @<sid> result and await task\n"
                .to_owned()
        } else {
            format!("{SID}; not `@{sid}`\n")
        });
    }
    if first == "spawn" {
        return Some(format!("OK 1\nwindow={pid} spawned\n"));
    }
    if first != "await" {
        return Some(format!("ERR unexpected line {line}\n"));
    }
    let list = line
        .split_whitespace()
        .find(|word| word.starts_with('@'))
        .unwrap_or_default();
    let targets: Vec<&str> = list
        .split(',')
        .map(|element| {
            let body = element.trim_start_matches('@');
            body.split_once(':').map_or(body, |(sid, _)| sid)
        })
        .collect();
    if let Some(gone) = targets.iter().find(|sid| !holds.holds(sid)) {
        return Some(format!("ERR no such session @{gone}\n"));
    }
    let all = line.split_whitespace().any(|word| word == "all");
    let news = |sid: &str| holds.news.contains(sid) || holds.retired.contains(sid);
    let with_news = targets.iter().filter(|sid| news(sid)).count();
    let answered = if all {
        with_news == targets.len()
    } else {
        with_news > 0
    };
    let now = line.split_whitespace().any(|word| word == "timeout=0");
    if !answered && !now {
        return None;
    }
    let mut out = format!(
        "OK {}{}\n",
        targets.len(),
        if answered { "" } else { " status=timeout" }
    );
    for sid in targets {
        out.push_str(&format!(
            "@{sid} rev=2 state=working window={pid} task_news={}\n",
            u8::from(news(sid))
        ));
    }
    Some(out)
}

/// A launcher whose own app runs the window at `app` (`None`: no window here runs it).
struct Apps {
    app: Option<u32>,
}

impl WindowLauncher for Apps {
    fn is_logged_in(&self) -> bool {
        true
    }

    fn launch_window(&self, _state_dir: &Path, _relay_dir: &Path) -> Result<(), String> {
        Err("this test opens no window".to_owned())
    }

    fn runs_this_app(&self, pid: u32) -> bool {
        self.app == Some(pid)
    }
}

/// Two live pids, older (lower) first: this test process's and a stand-in's.
fn two_pids(stand_in: &StandIn) -> (u32, u32) {
    let (mine, other) = (std::process::id(), stand_in.pid());
    (mine.min(other), mine.max(other))
}

/// Hand `line` to this Mac's node drive as a driver's connection does, behind a requester line
/// that names nobody, over `relay_dir`'s windows with `app` the installed app's, and read the
/// whole answer.
fn drive(relay_dir: &Path, app: Option<u32>, line: &str) -> String {
    // These drives are about which window serves a line: this scratch Mac admits every act.
    drive_sending(
        relay_dir,
        app,
        FleetAccess::Full,
        &format!("{}{line}\n", Requester::default().line()),
    )
}

/// [`drive`] sending `sent` whole, on a Mac at fleet access `access`.
fn drive_sending(relay_dir: &Path, app: Option<u32>, access: FleetAccess, sent: &str) -> String {
    let state = aterm_tempfile::tempdir().expect("state dir");
    aterm_agent::fleet_access::set(state.path(), access, aterm_agent::fleet_access::By::Cli)
        .expect("store the access");
    let windows = WindowRelays::new(relay_dir.to_path_buf(), Arc::new(Apps { app }));
    let node = "ab".repeat(16);
    let table = Arc::new(NodeOpenTable::default());
    let (near, mut far) = CtlStream::pair().expect("a pair");
    far.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a bound on the answer");
    let grant = DriveGrant::from_word(&format!("{node}:{}", "cd".repeat(32))).expect("grant");
    let open = table
        .admit(true, Some(grant), &near, |_, _| true)
        .expect("admit")
        .expect("admitted");
    spawn_node_drive(
        near,
        open,
        None,
        state.path().to_path_buf(),
        windows,
        (
            crate::fabric::Driver {
                node,
                host: "m22.local".to_owned(),
            },
            NodeKey::from_bytes([0xcd; 32]),
        ),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the drive starts");
    // A driver keeps its side open while it waits, as a wait's window reads a close as a hang-up.
    far.write_all(sent.as_bytes()).expect("the lines");
    let mut answer = read_line_raw(&mut far, 4096)
        .expect("an answer")
        .expect("a status line");
    let rows = answer
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    for _ in 0..rows {
        answer.push_str(
            &read_line_raw(&mut far, 4096)
                .expect("a row")
                .expect("a row"),
        );
    }
    answer
}

/// A DRIVE NAMING A SESSION REACHES THE WINDOW THAT HOSTS IT: with the installed app's window
/// older and a second window newer, `@<sid> status` for a worker in the older one reaches it and
/// the newer never serves it. Each window answers its own sessions, whichever is newer.
/// NEGATIVE CONTROL: windows that take each drive whatever it names, as every window did before
/// drives were routed by their sessions, hand `@s-0d` to the newest window, which hosts it not.
#[test]
fn a_drive_naming_a_session_reaches_the_window_that_hosts_it() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-sid");
    {
        let old = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), false);
        let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-ae"]), false);
        let answer = drive(relay.path(), None, "@s-0d status");
        assert_eq!(answer, format!("OK 1\nwindow={older} status s-0d\n"));
        assert_eq!(old.served(), ["@s-0d status"]);
        assert!(new.served().is_empty(), "{:?}", new.served());
        let answer = drive(relay.path(), Some(older), "@s-ae status");
        assert_eq!(answer, format!("OK 1\nwindow={newer} status s-ae\n"));
        assert_eq!(new.served(), ["@s-ae status"]);
        assert_eq!(old.served(), ["@s-0d status"]);
        let answer = drive(relay.path(), None, "hold s-0d on");
        assert_eq!(old.served(), ["@s-0d status", "hold s-0d on"], "{answer}");
    }
    // NEGATIVE CONTROL: the newest-first rule reaches the wrong window.
    let old = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), true);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-ae"]), true);
    let answer = drive(relay.path(), None, "@s-0d status");
    assert!(
        answer.starts_with("ERR denied: a drive names a session"),
        "{answer}"
    );
    assert_eq!(new.served(), ["@s-0d status"]);
    assert!(old.served().is_empty());
}

/// A NODE DRIVE HANDS ITS WINDOW THE REQUESTER ITS DRIVER SENT, in the relay header, never as a
/// line ahead of the request (which a window of the build before requesters would serve as one,
/// and answer every request after it one reply late): the session, program, model and account
/// reach it whole, and the first line the window serves is the request.
/// NEGATIVE CONTROLS: a drive whose first line is a request, and one whose requester names a
/// session aterm never minted, are refused `ERR drive:` before the fleet access lock judges the
/// spawn behind them, and reach no window; a well-formed requester's spawn reaches the lock,
/// which refuses it on this Read Only Mac.
#[test]
fn a_node_drive_hands_its_window_the_requester_and_refuses_a_drive_without_one() {
    let stand_in = StandIn::start();
    let (older, _) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-req");
    let window = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), false);
    let asked = Requester::of(
        Some("s-0123456789abcdef0123"),
        Some("claude"),
        Some("Opus 5.5"),
        Some("team acct"),
    );
    let answer = drive_sending(
        relay.path(),
        Some(older),
        FleetAccess::Full,
        &format!("{}@s-0d status\n", asked.line()),
    );
    assert_eq!(answer, format!("OK 1\nwindow={older} status s-0d\n"));
    assert_eq!(window.requesters(), [asked]);
    assert_eq!(window.served(), ["@s-0d status"]);
    for sent in [
        "spawn cwd=/tmp\n".to_owned(),
        "requester sid=worker program=- model=- account=-\nspawn cwd=/tmp\n".to_owned(),
    ] {
        let answer = drive_sending(relay.path(), Some(older), FleetAccess::ReadOnly, &sent);
        assert!(answer.starts_with("ERR drive: "), "{sent:?}: {answer}");
    }
    let answer = drive_sending(
        relay.path(),
        Some(older),
        FleetAccess::ReadOnly,
        &format!("{}spawn cwd=/tmp\n", Requester::default().line()),
    );
    assert!(
        answer.starts_with(aterm_types::control_verbs::DRIVE_LOCKED),
        "CONTROL: {answer}"
    );
    assert_eq!(window.served(), ["@s-0d status"]);
    assert_eq!(window.requesters().len(), 1);
}

/// A DRIVE NAMING NO SESSION REACHES THE INSTALLED APP'S WINDOW: `spawn` goes to the window that
/// runs this node service's own app even while a newer window of another build runs, and to the
/// newest only where no window runs the app.
/// NEGATIVE CONTROL: with no window running the app, the same spawn reaches the newer window.
#[test]
fn a_spawn_reaches_the_installed_apps_window_not_the_newest() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-app");
    let old = TestWindow::open(relay.path(), older, Holds::default(), false);
    let new = TestWindow::open(relay.path(), newer, Holds::default(), false);
    let answer = drive(relay.path(), Some(older), "spawn cwd=/tmp");
    assert_eq!(answer, format!("OK 1\nwindow={older} spawned\n"));
    assert_eq!(old.served(), ["spawn cwd=/tmp"]);
    assert!(new.served().is_empty(), "{:?}", new.served());
    // NEGATIVE CONTROL: no window runs the app, so newest first.
    let answer = drive(relay.path(), None, "spawn cwd=/tmp");
    assert_eq!(answer, format!("OK 1\nwindow={newer} spawned\n"));
    assert_eq!(new.served(), ["spawn cwd=/tmp"]);
}

/// A RETIRED WORKER'S RESULT STILL ANSWERS: the window that keeps a reaped worker's row takes
/// `@<sid> result` and `await task @<sid>`, though another window is newer and the installed
/// app's; any other verb gets that window's own refusal.
#[test]
fn a_retired_workers_result_answers_from_the_window_that_keeps_its_row() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-retired");
    let old = TestWindow::open(
        relay.path(),
        older,
        Holds::default().with_retired(&["s-dead"]),
        false,
    );
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-be"]), false);
    let answer = drive(relay.path(), Some(newer), "@s-dead result");
    assert_eq!(
        answer,
        format!("OK 1\nwindow={older} retired result s-dead\n")
    );
    let answer = drive(
        relay.path(),
        Some(newer),
        "await task @s-dead:1 any timeout=60000",
    );
    assert_eq!(
        answer,
        format!("OK 1\n@s-dead rev=2 state=working window={older} task_news=1\n")
    );
    let answer = drive(relay.path(), Some(newer), "@s-dead status");
    assert!(answer.contains("answers only @<sid> result"), "{answer}");
    assert_eq!(
        old.served(),
        [
            "@s-dead result",
            "await task @s-dead:1 any timeout=60000",
            "@s-dead status"
        ]
    );
    assert!(new.served().is_empty(), "{:?}", new.served());
}

/// A DRIVE NAMING A SESSION NO WINDOW HOSTS IS ANSWERED BY THE GATE, NOT AS BUSY: every window
/// declines it, and the installed app's window, handed it naming no session, refuses it in its own
/// words; a wait naming one session no window hosts beside one a single window hosts reaches that
/// window, which says which is gone.
#[test]
fn a_drive_naming_a_session_no_window_hosts_hears_the_gate() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-nowhere");
    let old = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), false);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-ae"]), false);
    let answer = drive(relay.path(), Some(older), "@s-bad status");
    assert!(
        answer.starts_with("ERR denied: a drive names a session"),
        "{answer}"
    );
    assert_eq!(old.served(), ["@s-bad status"]);
    let answer = drive(
        relay.path(),
        Some(older),
        "await task @s-bad,@s-ae any timeout=9",
    );
    assert_eq!(answer, "ERR no such session @s-bad\n");
    assert_eq!(new.served(), ["await task @s-bad,@s-ae any timeout=9"]);
}

/// AN AWAIT ACROSS TWO WINDOWS IS SPLIT AND ANSWERED AS ONE: each window waits on its own
/// workers; under `any` the share with news ends the wait, the other share's wait is hung up and
/// read again at once, and the driver gets one row per target in its line's order; under `all`
/// every share answers. A target no window hosts, beside targets in both, is answered gone here.
/// NEGATIVE CONTROL: unrouted, the whole wait reached the newest window, which hosts only one of
/// the targets and says the other is gone, which the driver reads as that worker's end.
#[test]
fn an_await_across_two_windows_is_split_and_answered_as_one() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-split");
    {
        let old = TestWindow::open(
            relay.path(),
            older,
            Holds::live(&["s-a"]).with_news(&["s-a"]),
            false,
        );
        let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-b"]), false);
        let answer = drive(
            relay.path(),
            Some(older),
            "await task @s-b:2,@s-a:1 any timeout=60000",
        );
        assert_eq!(
            answer,
            format!(
                "OK 2\n@s-b rev=2 state=working window={newer} task_news=0\n@s-a rev=2 \
                 state=working window={older} task_news=1\n"
            )
        );
        assert_eq!(old.served(), ["await task @s-a:1 any timeout=60000"]);
        // The share's first ask and its look again come on two connections, recorded by two
        // threads: the first may be recorded after the answer went out.
        let mut newer_served = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the newer window recording both asks of its share",
            || {
                let served = new.served();
                (served.len() == 2).then_some(served)
            },
        );
        newer_served.sort();
        assert_eq!(
            newer_served,
            [
                "await task @s-b:2 any timeout=0",
                "await task @s-b:2 any timeout=60000",
            ]
        );
        let answer = drive(
            relay.path(),
            Some(older),
            "await task @s-a,@s-dead,@s-b any",
        );
        assert_eq!(answer, "ERR no such session @s-dead\n");
    }
    {
        let _old = TestWindow::open(
            relay.path(),
            older,
            Holds::live(&["s-a"]).with_news(&["s-a"]),
            false,
        );
        let _new = TestWindow::open(
            relay.path(),
            newer,
            Holds::live(&["s-b"]).with_news(&["s-b"]),
            false,
        );
        let answer = drive(relay.path(), None, "await task @s-a,@s-b all timeout=60000");
        assert_eq!(
            answer,
            format!(
                "OK 2\n@s-a rev=2 state=working window={older} task_news=1\n@s-b rev=2 \
                 state=working window={newer} task_news=1\n"
            )
        );
    }
    // NEGATIVE CONTROL: the newest window took the whole wait.
    let _old = TestWindow::open(
        relay.path(),
        older,
        Holds::live(&["s-a"]).with_news(&["s-a"]),
        true,
    );
    let _new = TestWindow::open(relay.path(), newer, Holds::live(&["s-b"]), true);
    let answer = drive(
        relay.path(),
        None,
        "await task @s-b:2,@s-a:1 any timeout=60000",
    );
    assert_eq!(answer, "ERR no such session @s-a\n");
}

/// The sessions a drive's line names, which node serve routes by, are its selector's, a hold's,
/// and each wait target's, once each and in order; a spawn's `from=` and a line that names
/// none carry none, and an id a relay line cannot carry is left to the gate.
#[test]
fn a_drive_lines_sessions_are_its_selectors_holds_and_wait_targets() {
    let named = |line: &str| -> Vec<String> {
        crate::control::drive_line_sessions(line)
            .into_iter()
            .map(|sid| sid.as_str().to_owned())
            .collect()
    };
    assert_eq!(named("@s-a status\n"), ["s-a"]);
    // A selector names a session only in the id's own spelling, as the window's gate reads it.
    assert!(named("@s-old status").is_empty());
    assert_eq!(named("@s-a post key=k kind=note -- hi"), ["s-a"]);
    assert_eq!(named("hold s-b on reason=x"), ["s-b"]);
    assert_eq!(
        named("await task @s-a:3,@s-b,@s-a:4:1 any facts=1 timeout=5"),
        ["s-a", "s-b"]
    );
    assert!(named("spawn agent=claude from=s-c len=4").is_empty());
    assert!(named("survey").is_empty());
    assert!(named("board").is_empty());
    assert!(named("@1 status").is_empty());
    assert!(relay_names("s-4c56f539e92639ba37ae"));
    assert!(!relay_names("s-a,b"));
    assert!(!relay_names("s-"));
    assert!(!relay_names("x-a"));
}

/// THE LIVE LAUNCHER KNOWS ITS OWN APP'S WINDOW: a window's process whose image is this service's
/// own executable runs its app; a process of another build's executable does not.
#[test]
fn the_live_launcher_knows_its_own_apps_window() {
    let stand_in = StandIn::start();
    let launcher = super::super::LiveWindowLauncher::default();
    assert!(launcher.runs_this_app(std::process::id()));
    assert!(!launcher.runs_this_app(stand_in.pid()));
}

/// Ask this Mac's node a survey connection's `lines` over `relay_dir`'s windows, with `app` the
/// installed app's, and read each counted answer (or one-line refusal).
fn survey(relay_dir: &Path, app: Option<u32>, lines: &[&str]) -> Vec<String> {
    survey_read_by(relay_dir, app, lines, (relay_dir.to_path_buf(), None))
}

/// [`survey`] over a connection whose handshake proved `reader`, this Mac's state folder
/// `state`: where the node job's fleet activity record is.
fn survey_read_by(
    relay_dir: &Path,
    app: Option<u32>,
    lines: &[&str],
    (state, reader): (PathBuf, Option<crate::fleet_activity::Reader>),
) -> Vec<String> {
    let windows = WindowRelays::new(relay_dir.to_path_buf(), Arc::new(Apps { app }));
    let table = Arc::new(NodeOpenTable::default());
    let (near, mut far) = CtlStream::pair().expect("a pair");
    far.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a bound on the answers");
    let open = table
        .admit(true, None, &near, |_, _| true)
        .expect("admit")
        .expect("admitted");
    let header = RelayHeader::Survey {
        content: true,
        drive: true,
        peer: None,
    };
    spawn_node_survey(near, windows, header, open, (state, reader)).expect("the survey starts");
    lines
        .iter()
        .map(|line| {
            far.write_all(format!("{line}\n").as_bytes())
                .expect("the line");
            let mut answer = read_line_raw(&mut far, 4096)
                .expect("an answer")
                .expect("a status line");
            let rows = answer
                .strip_prefix("OK ")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            for _ in 0..rows {
                answer.push_str(
                    &read_line_raw(&mut far, 4096)
                        .expect("a row")
                        .expect("a row"),
                );
            }
            answer
        })
        .collect()
}

/// A SURVEY READS EVERY WINDOW: `survey --host` of a Mac running two windows lists both windows'
/// sessions under one `instance` line, the installed app's window's (its pid, its account line,
/// once), counting both windows' sessions and saying both instances were read; `version` goes to
/// the installed app's window.
/// NEGATIVE CONTROL: a hand-over to the first window that takes it, as a survey was relayed before
/// DR1, reads one window's sessions and says one of two instances.
#[test]
fn a_survey_lists_the_sessions_of_every_window() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-survey");
    let old = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), false);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-ae"]), false);
    let answers = survey(relay.path(), Some(older), &["survey", "version"]);
    let merged = &answers[0];
    let lines: Vec<&str> = merged.lines().collect();
    assert_eq!(lines[0], "OK 4", "{merged}");
    assert!(
        lines[1].starts_with(&format!("instance pid={older} ")),
        "{merged}"
    );
    assert!(lines[1].contains(" sessions=2 "), "{merged}");
    assert!(lines[1].ends_with(" instances_read=2"), "{merged}");
    assert!(lines[2].contains(" sid=s-0d "), "{merged}");
    assert!(lines[3].contains(" sid=s-ae "), "{merged}");
    assert_eq!(lines[4], format!("account pool=claude id=a{older}"));
    assert_eq!(answers[1], format!("OK version window={older}\n"));
    assert_eq!(old.served(), ["survey", "version"]);
    assert_eq!(new.served(), ["survey"]);
    // NEGATIVE CONTROL: one window's hand-over reads one window.
    let windows = WindowRelays::new(relay.path().to_path_buf(), Arc::new(Apps { app: None }));
    let header = RelayHeader::Survey {
        content: true,
        drive: true,
        peer: None,
    };
    let super::super::RelayOpen::Opened(mut one) = relay_to_window(&windows, &header) else {
        panic!("a window takes the hand-over");
    };
    one.write_all(b"survey\n").expect("survey");
    let head = read_line_raw(&mut one, 4096).expect("read").expect("head");
    let instance = read_line_raw(&mut one, 4096)
        .expect("read")
        .expect("instance");
    assert_eq!(head, "OK 3\n");
    assert!(instance.contains(" sessions=1 ") && !instance.contains("instances_read"));
}

/// A SURVEY A SESSION ASKED FOR IS ONE READ IN THE NODE JOB'S RECORD, AND EVERY WINDOW HEARS IT
/// PLAIN: the word that names the session stops at the node job, since a window may run an aterm
/// from before the word and would answer it with its usage. The merged answer is the plain
/// survey's, and a survey that names no session (aterm's own poll) leaves no row.
#[test]
fn a_sessions_survey_reaches_each_window_plain_and_is_one_read_in_the_record() {
    use aterm_types::fleet_activity::{Outcome, Plain, Way};
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-asked");
    let state = aterm_tempfile::tempdir().expect("a state folder");
    let old = TestWindow::open(relay.path(), older, Holds::live(&["s-0d"]), false);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["s-ae"]), false);
    let asker = "s-0123456789abcdef0123";
    let reader = crate::fleet_activity::Reader {
        node: "05".repeat(16),
    };
    // The plain survey at the end is answered once the rows of the two before it were handed
    // over: the node job takes a connection's lines one at a time.
    let answers = survey_read_by(
        relay.path(),
        Some(older),
        &[
            "survey",
            &format!("survey by={asker}"),
            &format!("survey facts by={asker}"),
            "survey",
        ],
        (state.path().to_path_buf(), Some(reader.clone())),
    );
    assert_eq!(answers[1], answers[0], "the same survey");
    let heard = ["survey", "survey", "survey facts", "survey"];
    assert_eq!(old.served(), heard);
    assert_eq!(new.served(), heard);
    aterm_ctl::fleet_activity::settle();
    let rows = aterm_ctl::fleet_activity::Record::in_state(state.path())
        .read(aterm_ctl::fleet_activity::now_ms())
        .rows;
    let reads: u32 = rows
        .iter()
        .map(|row| {
            assert_eq!(
                (row.way, row.node.as_str(), row.what, row.outcome),
                (
                    Way::In,
                    reader.node.as_str(),
                    Plain::Read,
                    Outcome::Admitted
                )
            );
            assert_eq!(row.by.as_deref(), Some(asker));
            row.n
        })
        .sum();
    assert_eq!(reads, 2, "the two surveys the session asked for: {rows:?}");
}

/// A MERGED SURVEY KEEPS ITS BOUND AS A WINDOW DOES: past the reply bound, the rows a window
/// itself would cut first go (the least kept, the last window's latest first), the busy agent's
/// row stays, the Mac's own lines are the first window's once, and `sessions=` still counts every
/// window's sessions, so a reader says how many were not sent. One window's answer passes as it
/// came.
#[test]
fn a_merged_survey_cuts_the_least_kept_rows_to_its_bound() {
    let first = window_survey(7, &Holds::live(&["s-busy1", "s-idle1"]));
    let second = window_survey(9, &Holds::live(&["s-idle2", "s-idle3"]));
    assert_eq!(
        merge_window_surveys(std::slice::from_ref(&first), 64),
        first
    );
    let whole = merge_window_surveys(&[first.clone(), second.clone()], usize::MAX);
    assert_eq!(whole.lines().count(), 7, "{whole}");
    assert!(whole.starts_with("OK 6\ninstance pid=7 "), "{whole}");
    assert!(whole.ends_with("account pool=claude id=a7\n"), "{whole}");
    assert!(!whole.contains("id=a9"), "{whole}");
    let row = |sid: &str| {
        whole
            .lines()
            .find(|line| line.contains(&format!(" sid={sid} ")))
            .map_or(0, |line| line.len() + 1)
    };
    // Room for all but two rows: the latest idle ones go, the busy one stays.
    let bound = whole.len() - row("s-idle3") - row("s-idle2");
    let cut = merge_window_surveys(&[first, second], bound);
    assert!(cut.len() <= bound, "{cut}");
    assert!(cut.starts_with("OK 4\n"), "{cut}");
    assert!(cut.contains(" sessions=4 "), "{cut}");
    assert!(
        cut.contains(" sid=s-busy1 ") && cut.contains(" sid=s-idle1 "),
        "{cut}"
    );
    assert!(
        !cut.contains(" sid=s-idle2 ") && !cut.contains(" sid=s-idle3 "),
        "{cut}"
    );
}

/// A MERGED SURVEY COUNTS THE CUT ROWS IT SENDS: `cut=` is the rows sent cut to facts (a content
/// window's rows without `said=`), recounted once the merge dropped rows to fit, so a row the
/// merge dropped is not counted cut too.
#[test]
fn a_merged_survey_counts_the_cut_rows_it_sends() {
    let first = "OK 4\ninstance pid=7 host=m111 sessions=2 level=content cut=1 at_ms=1 node=n \
                 instances=2\nsession sid=s-a program_kind=agent agent=busy said=done\nsession \
                 sid=s-b program_kind=shell agent=- attention=- needs=-\naccount pool=claude \
                 id=a7\n"
        .to_owned();
    let second = "OK 2\ninstance pid=9 host=m111 sessions=1 level=content cut=0 at_ms=1 node=n \
                  instances=2\nsession sid=s-c program_kind=agent agent=idle said=ok\n"
        .to_owned();
    let whole = merge_window_surveys(&[first.clone(), second.clone()], usize::MAX);
    assert!(
        whole.starts_with("OK 5\ninstance pid=7 host=m111 sessions=3 level=content cut=1 "),
        "{whole}"
    );
    let row = whole
        .lines()
        .find(|line| line.contains(" sid=s-b "))
        .map_or(0, |line| line.len() + 1);
    let fitted = merge_window_surveys(&[first, second], whole.len() - row);
    assert!(!fitted.contains(" sid=s-b "), "{fitted}");
    assert!(
        fitted.starts_with("OK 4\ninstance pid=7 host=m111 sessions=3 level=content cut=0 "),
        "{fitted}"
    );
    assert!(fitted.contains(" instances_read=2\n"), "{fitted}");
}

/// A MERGED SURVEY KEEPS ONE UPDATER, ON THE FIRST WINDOW'S LINE: the first window's `instance`
/// line is the Mac's, so where two windows answer, each having read the updater at its own moment
/// (here a hold, then its lapse), the reader is told the one on that line, beside its `version=`
/// and its clock, never the other window's and never both.
/// NEGATIVE CONTROL: a merge whose instance line is the last window's fails "the first window's
/// updater".
#[test]
fn a_merged_survey_keeps_the_first_windows_updater_beside_its_version() {
    let first = "OK 2\ninstance pid=7 host=m221 version=0.178.0 sessions=1 level=facts cut=0 \
                 at_ms=1793130000000 node=n instances=2 update=held update_at=1793129400 \
                 update_to=0.199.0 update_until=1793130060 update_why=crash-loop \
                 update_tries=2 update_checks=on access=read\nsession sid=s-a program_kind=shell\n"
        .to_owned();
    let second = "OK 2\ninstance pid=9 host=m221 version=0.198.0 sessions=1 level=facts cut=0 \
                  at_ms=1793130120000 node=n instances=2 update=retrying update_at=1793129400 \
                  update_to=0.199.0 update_why=crash-loop update_checks=on access=read\n\
                  session sid=s-b program_kind=shell\n"
        .to_owned();
    let merged = merge_window_surveys(&[first, second], usize::MAX);
    let lines: Vec<String> = merged.lines().skip(1).map(str::to_owned).collect();
    assert_eq!(
        aterm_net::fleet_member::Answerer::of(&lines),
        aterm_net::fleet_member::Answerer::Window("0.178.0".to_owned()),
        "{merged}"
    );
    let update = aterm_net::fleet_member::update_of(&lines).expect("an updater");
    assert_eq!(
        (update.standing.word(), update.to.as_deref(), update.said_at),
        ("held", Some("0.199.0"), Some(1_793_130_000)),
        "the first window's updater: {merged}"
    );
    assert_eq!(merged.matches(" update=").count(), 1, "{merged}");
}

/// THE NODE COUNTS EVERY WINDOW'S AGENTS (Fleet Field §11, §14): a survey of each window here,
/// merged, counts the agents of both windows by their words, listing the one whose sid is a
/// minted one; with no window here the count is a reading of no agents.
/// NEGATIVE CONTROL: a window that takes no survey hand-over is no reading: nothing is counted,
/// so nothing is published and the last count ages out.
#[test]
fn the_node_counts_the_agents_of_every_window() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("activity-two");
    let minted = "s-00000000000000000001";
    let _old = TestWindow::open(
        relay.path(),
        older,
        Holds::live(&["s-busy1", minted]),
        false,
    );
    let _new = TestWindow::open(relay.path(), newer, Holds::live(&["s-busy2"]), false);
    let windows = WindowRelays::new(relay.path().to_path_buf(), Arc::new(Apps { app: None }));
    let row = activity_of_windows(&windows, 1_793_160_000).expect("both windows answered");
    assert_eq!(
        (
            row.at,
            row.busy,
            row.idle,
            row.waiting,
            row.walled,
            row.unknown
        ),
        (1_793_160_000, 2, 1, 0, 0, 0)
    );
    assert_eq!(
        row.sessions
            .iter()
            .map(|e| (e.sid.to_string(), e.word.to_string()))
            .collect::<Vec<_>>(),
        [(minted.to_owned(), "idle".to_owned())]
    );

    let empty = crate::test_scratch::short_dir("activity-none");
    let none = WindowRelays::new(empty.path().to_path_buf(), Arc::new(Apps { app: None }));
    assert_eq!(
        activity_of_windows(&none, 7).map(|row| (row.at, row.agents())),
        Some((7, 0)),
        "no window runs no agent"
    );

    let refusing = crate::test_scratch::short_dir("activity-refused");
    let stop = Arc::new(AtomicBool::new(false));
    let relay_thread = {
        let (dir, stop) = (refusing.path().to_path_buf(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let _ = serve_window_relay_at(
                &dir,
                std::process::id(),
                Some(&stop),
                hosts_every,
                |_, _| false,
            );
        })
    };
    let socket = relay_socket_path(refusing.path(), std::process::id());
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the window's relay", || {
        super::super::relay_answers(&socket).then_some(())
    });
    let refused = WindowRelays::new(refusing.path().to_path_buf(), Arc::new(Apps { app: None }));
    assert_eq!(
        activity_of_windows(&refused, 7),
        None,
        "a window that took no survey"
    );
    stop.store(true, Ordering::Release);
    let _ = aterm_uds::connect::until(&socket, Instant::now() + Duration::from_millis(100));
    let _ = relay_thread.join();
}

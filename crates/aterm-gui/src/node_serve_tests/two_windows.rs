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

/// The sessions a test window holds: those it hosts, the ended sessions whose rows its session
/// table keeps, and which of its sessions have task news; and whether its survey says its own look at the
/// machine, as a window of an aterm that has the look does.
#[derive(Clone, Default)]
struct Holds {
    live: BTreeSet<String>,
    rows: BTreeSet<String>,
    news: BTreeSet<String>,
    looks: bool,
}

impl Holds {
    fn live(sids: &[&str]) -> Self {
        Self {
            live: sids.iter().map(|sid| (*sid).to_owned()).collect(),
            ..Self::default()
        }
    }

    fn with_rows(mut self, sids: &[&str]) -> Self {
        self.rows = sids.iter().map(|sid| (*sid).to_owned()).collect();
        self
    }

    fn with_news(mut self, sids: &[&str]) -> Self {
        self.news = sids.iter().map(|sid| (*sid).to_owned()).collect();
        self
    }

    fn with_look(mut self) -> Self {
        self.looks = true;
        self
    }

    fn holds(&self, sid: &str) -> bool {
        self.live.contains(sid) || self.rows.contains(sid)
    }
}

/// The look a test window that has one says of the machine ([`Holds::with_look`]).
const WINDOW_LOOK: &str = " m_at_ms=77 cores=2";

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
/// account line. Its header says no look at the machine and no access, as an older window's,
/// unless it [`Holds::with_look`].
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
         at_ms=1 node={} instances=2 accounts=1{}\n",
        rows.len() + 2,
        rows.len(),
        "11".repeat(16),
        if holds.looks { WINDOW_LOOK } else { "" },
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
        } else if holds.rows.contains(sid) && verb == "result" {
            format!("OK 1\nwindow={pid} row result {sid}\n")
        } else if holds.rows.contains(sid) {
            "ERR denied: a session that ended on the driven Mac answers \
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
    let news = |sid: &str| holds.news.contains(sid) || holds.rows.contains(sid);
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

/// A legacy tab's ID, as a shell opened before the `t` form still holds it.
const LEGACY: &str = "s-4c56f539e92639ba37ae";

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
/// the newer never serves it. Each window answers its own sessions, whichever is newer, a
/// legacy tab's by the `s-` ID its shell still holds.
/// NEGATIVE CONTROL: windows that take each drive whatever it names, as every window did before
/// drives were routed by their sessions, hand `@t000000000d` to the newest window, which hosts
/// it not.
#[test]
fn a_drive_naming_a_session_reaches_the_window_that_hosts_it() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-sid");
    {
        let old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
        let new = TestWindow::open(
            relay.path(),
            newer,
            Holds::live(&["t00000000ae", LEGACY]),
            false,
        );
        let answer = drive(relay.path(), None, "@t000000000d status");
        assert_eq!(answer, format!("OK 1\nwindow={older} status t000000000d\n"));
        assert_eq!(old.served(), ["@t000000000d status"]);
        assert!(new.served().is_empty(), "{:?}", new.served());
        let answer = drive(relay.path(), Some(older), "@t00000000ae status");
        assert_eq!(answer, format!("OK 1\nwindow={newer} status t00000000ae\n"));
        assert_eq!(new.served(), ["@t00000000ae status"]);
        assert_eq!(old.served(), ["@t000000000d status"]);
        let answer = drive(relay.path(), None, "hold t000000000d on");
        assert_eq!(
            old.served(),
            ["@t000000000d status", "hold t000000000d on"],
            "{answer}"
        );
        // A legacy tab is named by its own ID, and reaches the window that hosts it.
        let legacy = format!("@{LEGACY} status");
        let answer = drive(relay.path(), Some(older), &legacy);
        assert_eq!(answer, format!("OK 1\nwindow={newer} status {LEGACY}\n"));
        assert_eq!(new.served(), ["@t00000000ae status", legacy.as_str()]);
        assert_eq!(old.served().len(), 2, "{:?}", old.served());
    }
    // NEGATIVE CONTROL: the newest-first rule reaches the wrong window.
    let old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), true);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["t00000000ae"]), true);
    let answer = drive(relay.path(), None, "@t000000000d status");
    assert!(
        answer.starts_with("ERR denied: a drive names a session"),
        "{answer}"
    );
    assert_eq!(new.served(), ["@t000000000d status"]);
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
    let window = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
    let asked = Requester::of(
        Some("tqn5r6e0148"),
        Some("claude"),
        Some("Opus 5.5"),
        Some("team acct"),
    );
    assert_eq!(asked.sid(), Some("tqn5r6e0148"));
    let answer = drive_sending(
        relay.path(),
        Some(older),
        FleetAccess::Full,
        &format!("{}@t000000000d status\n", asked.line()),
    );
    assert_eq!(answer, format!("OK 1\nwindow={older} status t000000000d\n"));
    assert_eq!(window.requesters(), [asked]);
    assert_eq!(window.served(), ["@t000000000d status"]);
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
    assert_eq!(window.served(), ["@t000000000d status"]);
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

/// AN ENDED SESSION'S RESULT STILL ANSWERS: the window whose session table keeps an ended
/// worker's row takes `@<sid> result` and `await task @<sid>`, though another window is newer and
/// the installed app's; any other verb gets that window's own refusal.
#[test]
fn an_ended_workers_result_answers_from_the_window_that_keeps_its_row() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-row");
    let old = TestWindow::open(
        relay.path(),
        older,
        Holds::default().with_rows(&["t000000dead"]),
        false,
    );
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["t00000000be"]), false);
    let answer = drive(relay.path(), Some(newer), "@t000000dead result");
    assert_eq!(
        answer,
        format!("OK 1\nwindow={older} row result t000000dead\n")
    );
    let answer = drive(
        relay.path(),
        Some(newer),
        "await task @t000000dead:1 any timeout=60000",
    );
    assert_eq!(
        answer,
        format!("OK 1\n@t000000dead rev=2 state=working window={older} task_news=1\n")
    );
    let answer = drive(relay.path(), Some(newer), "@t000000dead status");
    assert!(answer.contains("answers only @<sid> result"), "{answer}");
    assert_eq!(
        old.served(),
        [
            "@t000000dead result",
            "await task @t000000dead:1 any timeout=60000",
            "@t000000dead status"
        ]
    );
    assert!(new.served().is_empty(), "{:?}", new.served());
}

/// A DRIVE NAMING A SESSION NO WINDOW HOSTS IS ANSWERED BY THE GATE, NOT AS BUSY: every window
/// declines it, and the installed app's window, handed it naming no session, refuses it in its own
/// words; a wait naming one session no window hosts beside one a single window hosts reaches that
/// window, which says which is gone.
/// NEGATIVE CONTROL: a loose `s-` word, as an earlier build took for a session's ID, names no
/// session: the line goes, naming none, to the installed app's window, though another window
/// holds a session of that name.
#[test]
fn a_drive_naming_a_session_no_window_hosts_hears_the_gate() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("dr1-nowhere");
    let old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
    let new = TestWindow::open(
        relay.path(),
        newer,
        Holds::live(&["t00000000ae", "s-5e55"]),
        false,
    );
    let answer = drive(relay.path(), Some(older), "@t0000000bad status");
    assert!(
        answer.starts_with("ERR denied: a drive names a session"),
        "{answer}"
    );
    assert_eq!(old.served(), ["@t0000000bad status"]);
    let answer = drive(
        relay.path(),
        Some(older),
        "await task @t0000000bad,@t00000000ae any timeout=9",
    );
    assert_eq!(answer, "ERR no such session @t0000000bad\n");
    assert_eq!(
        new.served(),
        ["await task @t0000000bad,@t00000000ae any timeout=9"]
    );
    // NEGATIVE CONTROL: the loose form is routed as a line that names no session.
    let answer = drive(relay.path(), Some(older), "@s-5e55 status");
    assert!(
        answer.starts_with("ERR denied: a drive names a session"),
        "{answer}"
    );
    assert_eq!(old.served(), ["@t0000000bad status", "@s-5e55 status"]);
    assert_eq!(new.served().len(), 1, "{:?}", new.served());
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
            Holds::live(&["t000000000a"]).with_news(&["t000000000a"]),
            false,
        );
        let new = TestWindow::open(relay.path(), newer, Holds::live(&["t000000000b"]), false);
        let answer = drive(
            relay.path(),
            Some(older),
            "await task @t000000000b:2,@t000000000a:1 any timeout=60000",
        );
        assert_eq!(
            answer,
            format!(
                "OK 2\n@t000000000b rev=2 state=working window={newer} task_news=0\n\
                 @t000000000a rev=2 state=working window={older} task_news=1\n"
            )
        );
        assert_eq!(
            old.served(),
            ["await task @t000000000a:1 any timeout=60000"]
        );
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
                "await task @t000000000b:2 any timeout=0",
                "await task @t000000000b:2 any timeout=60000",
            ]
        );
        let answer = drive(
            relay.path(),
            Some(older),
            "await task @t000000000a,@t000000dead,@t000000000b any",
        );
        assert_eq!(answer, "ERR no such session @t000000dead\n");
    }
    {
        let _old = TestWindow::open(
            relay.path(),
            older,
            Holds::live(&["t000000000a"]).with_news(&["t000000000a"]),
            false,
        );
        let _new = TestWindow::open(
            relay.path(),
            newer,
            Holds::live(&["t000000000b"]).with_news(&["t000000000b"]),
            false,
        );
        let answer = drive(
            relay.path(),
            None,
            "await task @t000000000a,@t000000000b all timeout=60000",
        );
        assert_eq!(
            answer,
            format!(
                "OK 2\n@t000000000a rev=2 state=working window={older} task_news=1\n\
                 @t000000000b rev=2 state=working window={newer} task_news=1\n"
            )
        );
    }
    // NEGATIVE CONTROL: the newest window took the whole wait.
    let _old = TestWindow::open(
        relay.path(),
        older,
        Holds::live(&["t000000000a"]).with_news(&["t000000000a"]),
        true,
    );
    let _new = TestWindow::open(relay.path(), newer, Holds::live(&["t000000000b"]), true);
    let answer = drive(
        relay.path(),
        None,
        "await task @t000000000b:2,@t000000000a:1 any timeout=60000",
    );
    assert_eq!(answer, "ERR no such session @t000000000a\n");
}

/// The sessions a drive's line names, which node serve routes by, are its selector's, a hold's,
/// and each wait target's, once each and in order; a spawn's `from=` and a line that names
/// none carry none, and an id a relay line cannot carry is left to the gate. A legacy tab is
/// named by its exact `s-` ID.
/// NEGATIVE CONTROL: the loose `s-` forms an earlier build took (any hex, a name, upper case,
/// another length) name no session and ride no relay line.
#[test]
fn a_drive_lines_sessions_are_its_selectors_holds_and_wait_targets() {
    const A: &str = "t000000000a";
    const B: &str = "t000000000b";
    let named = |line: &str| -> Vec<String> {
        crate::control::drive_line_sessions(line)
            .into_iter()
            .map(|sid| sid.as_str().to_owned())
            .collect()
    };
    assert_eq!(named(&format!("@{A} status\n")), [A]);
    // A selector names a session only in the id's own spelling, as the window's gate reads it.
    assert!(named("@s-old status").is_empty());
    assert_eq!(named(&format!("@{A} post key=k kind=note -- hi")), [A]);
    assert_eq!(named(&format!("hold {B} on reason=x")), [B]);
    assert_eq!(
        named(&format!(
            "await task @{A}:3,@{B},@{A}:4:1 any facts=1 timeout=5"
        )),
        [A, B]
    );
    assert!(named("spawn agent=claude from=t000000000c len=4").is_empty());
    assert!(named("survey").is_empty());
    assert!(named("board").is_empty());
    assert!(named("@1 status").is_empty());
    // A legacy tab's ID names it, as its shell still does.
    assert_eq!(named(&format!("@{LEGACY} status")), [LEGACY]);
    assert_eq!(named(&format!("hold {LEGACY} on")), [LEGACY]);
    assert_eq!(
        named(&format!("await task @{LEGACY}:3,@{A} any timeout=5")),
        [LEGACY, A]
    );
    assert!(relay_names(LEGACY));
    // NEGATIVE CONTROL: no loose `s-` form names a session.
    for loose in [
        "s-a",
        "s-b1",
        "s-5e55",
        "s-4C56F539E92639BA37AE",
        "s-4c56f539e92639ba37a",
        "s-4c56f539e92639ba37ae0",
    ] {
        assert!(named(&format!("@{loose} status")).is_empty(), "{loose}");
        assert!(named(&format!("hold {loose} on")).is_empty(), "{loose}");
        assert_eq!(
            named(&format!("await task @{loose},@{A} any timeout=5")),
            [A],
            "{loose}"
        );
        assert!(!relay_names(loose), "{loose}");
    }
    assert!(!relay_names("t000000000a,b"));
    assert!(!relay_names("s-"));
    assert!(!relay_names("x-a"));
}

/// A drive names a tab by its ID as it names a legacy tab by its `s-` one, by its selector, a
/// hold or a wait target, and a relay line carries either; a malformed `t` ID names nothing and
/// rides no relay line.
#[test]
fn a_drive_line_names_a_t_tab_and_a_relay_carries_either_form() {
    let named = |line: &str| -> Vec<String> {
        crate::control::drive_line_sessions(line)
            .into_iter()
            .map(|sid| sid.as_str().to_owned())
            .collect()
    };
    assert_eq!(named("@tqn5r6e0148 status"), ["tqn5r6e0148"]);
    assert_eq!(named("hold t0000000001 on reason=x"), ["t0000000001"]);
    assert_eq!(
        named("await task @tqn5r6e0148:3,@s-1a2b3c4d5e6f70819203,@tqn5r6e0148:4 any timeout=5"),
        ["tqn5r6e0148", "s-1a2b3c4d5e6f70819203"]
    );
    for malformed in [
        "T0000000000",
        "t000000000",
        "tqn5r6e014u",
        "tQN5R6E0148",
        "s-a",
        "s-0000000000000000000A",
    ] {
        assert!(
            named(&format!("@{malformed} status")).is_empty(),
            "{malformed}"
        );
        assert!(
            named(&format!("hold {malformed} on")).is_empty(),
            "{malformed}"
        );
        assert!(!relay_names(malformed), "{malformed}");
    }
    assert!(relay_names("tqn5r6e0148") && relay_names("s-1a2b3c4d5e6f70819203"));
    assert!(!relay_names("tqn5r6e0148,t0000000001"));
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
    spawn_node_survey(near, windows, header, open, None, (state, reader))
        .expect("the survey starts");
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
    let old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["t00000000ae"]), false);
    let answers = survey(relay.path(), Some(older), &["survey", "version"]);
    let merged = &answers[0];
    let lines: Vec<&str> = merged.lines().collect();
    assert_eq!(lines[0], "OK 4", "{merged}");
    assert!(
        lines[1].starts_with(&format!("instance pid={older} ")),
        "{merged}"
    );
    assert!(lines[1].contains(" sessions=2 "), "{merged}");
    // The merge ends the line with it; the node job's own look follows, these windows
    // sending none (`the_node_job_adds_its_look_under_a_window_that_sends_none_…`).
    assert!(lines[1].contains(" instances_read=2 os="), "{merged}");
    assert!(lines[2].contains(" sid=t000000000d "), "{merged}");
    assert!(lines[3].contains(" sid=t00000000ae "), "{merged}");
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

/// THE NODE JOB ADDS ITS LOOK UNDER A WINDOW THAT SENDS NONE, AND NOT OVER ONE THAT DOES: a
/// window of an aterm older than the look answers a header with no `m_at_ms=`; the node job
/// that relays it adds its own look's tokens, once, before the header's `access=` (so the
/// header ends as the window wrote it), to one window's answer as to several merged, and the
/// strict parser then reads the machine's resources. The header's free space is the look's
/// (the volume's size and `disk_lim=` were judged against it), and its `at_ms=` is the look's
/// time where the look is later, so a reader's age of the look is 0 and never unread. The rows,
/// the other lines and the count are the window's. A window that sends its own look is relayed
/// as it came, and the node job reads no look for it. An answer at the reply bound loses a row
/// to the look, the least kept first; only where no row is left to cut does it go without.
/// NEGATIVE CONTROL (2026-11-12): with the `m_at_ms=` test dropped, a header carries two looks;
/// with the tokens appended to the line, it no longer ends in `access=`; with the look added
/// to merged answers alone, one window's answer goes out with none.
#[test]
fn the_node_job_adds_its_look_under_a_window_that_sends_none_and_not_over_one_that_does() {
    use aterm_agent::survey_facts::{MachineFacts, SwarmSurveyFacts};
    use aterm_capacity::machine_look::MachineLook;

    const LOOKED: u64 = 1_793_160_000_123;
    let own = || MachineFacts {
        look: MachineLook {
            at_unix_ms: Some(LOOKED),
            cores: Some(8),
            mem_total_mb: Some(16_384),
            disk_free_mb: Some(850),
            ..MachineLook::UNREAD
        },
        up_since_ms: Some(1_793_100_000_000),
        os: Some("macos"),
        target: Some("aarch64-apple-darwin".to_owned()),
    };
    let never = || -> MachineFacts { panic!("its window sends a look: the node job reads none") };
    let tokens = own().header_tokens();
    assert!(
        tokens.starts_with(" os=macos ") && tokens.contains(" m_at_ms=1793160000123 cores=8 "),
        "{tokens}"
    );
    let looks = |answer: &str| {
        answer
            .lines()
            .nth(1)
            .map_or(0, |head| head.matches(" m_at_ms=").count())
    };
    // A window's answer as an aterm before the look writes it, ending `tail`.
    let old = |pid: u32, tail: &str| {
        format!(
            "OK 3\ninstance pid={pid} host=m111 version=0.198.0 sessions=1 level=facts cut=0 \
             at_ms=1 node=n instances=2 label=m7 window=1 window_last=5 ssh_host_key=- \
             home_free_mb=900 accounts=1 accounts_unread=- moves_7d=- move_tries_7d=- \
             last_move=- roster_unread=0{tail}\nsession sid=s-{pid} program_kind=agent \
             agent=idle attention=- needs=-\naccount pool=claude id=a{pid} condition=usable \
             enrolled=1 pct5=- pct7=- week_reset=- walled_until=- last_used=-\n"
        )
    };
    // Each line after the header; under a header the look raised from the window's `at_ms=1`,
    // each `session` row ends with that time, when its window read it.
    let rows_read_at_1 = |answer: &str| {
        answer
            .lines()
            .skip(2)
            .map(|line| {
                if line.starts_with("session ") {
                    format!("{line} at_ms=1")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
    };
    let after_head = |answer: &str| {
        answer
            .lines()
            .skip(2)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // The header the window wrote, with the node job's edits: the look's free space and time.
    let edited = |head: &str| {
        head.replacen(" at_ms=1 ", &format!(" at_ms={LOOKED} "), 1)
            .replacen(" home_free_mb=900 ", " home_free_mb=850 ", 1)
    };

    // One window's answer, returned by the merge as it came where no look is asked.
    let single = old(7, " access=full drive=1");
    assert_eq!(
        merge_window_surveys(std::slice::from_ref(&single), usize::MAX, None),
        single
    );
    let with = merge_window_surveys(std::slice::from_ref(&single), usize::MAX, Some(&own));
    let head = with.lines().nth(1).expect("the instance line");
    assert!(with.starts_with("OK 3\ninstance pid=7 "), "{with}");
    assert_eq!(
        head,
        format!(
            "{}{tokens} access=full drive=1",
            edited(single.lines().nth(1).expect("its line"))
                .strip_suffix(" access=full drive=1")
                .expect("its access")
        ),
    );
    assert_eq!(looks(&with), 1, "{head}");
    assert_eq!(after_head(&with), rows_read_at_1(&single));
    let parsed = SwarmSurveyFacts::from_survey_reply(&with).expect("the strict parser reads it");
    assert_eq!(
        parsed.machine,
        Some(own()),
        "the node job's look, with the free space it read"
    );
    assert_eq!(parsed.home_free_mb, Some(850));
    assert_eq!(
        SwarmSurveyFacts::from_survey_reply(&single)
            .expect("an older window's")
            .machine,
        None,
        "CONTROL: as it came, the machine is unread"
    );
    assert_eq!(
        merge_window_surveys(std::slice::from_ref(&with), usize::MAX, Some(&never)),
        with,
        "an answer that says a look takes no other"
    );
    // Through the fleet read's own reader: the look's age is read, and it is 0.
    let json = format!(
        "[{{\"label\":\"pid 7\",\"instance\":{{{}}},\"sessions\":[]}}]",
        head.split(' ')
            .skip(1)
            .filter_map(|token| token.split_once('='))
            .map(|(key, value)| format!("\"{key}\":\"{value}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let read = crate::fleet_glance::sightings(&json, LOOKED + 5).expect("a fleet read");
    let aterm_db::fleet::read::Answer::Answered(answered) = &read.machines[0].answer else {
        panic!("this Mac answered: {read:?}");
    };
    assert_eq!(answered.look.age_ms, Some(0), "{:?}", answered.look);
    assert_eq!(answered.home_free_mb, Some(850));

    // A window that stamped its answer after the look keeps its time.
    let later = old(7, "").replacen(" at_ms=1 ", " at_ms=1793160000999 ", 1);
    let with = merge_window_surveys(std::slice::from_ref(&later), usize::MAX, Some(&own));
    assert!(with.contains(" at_ms=1793160000999 "), "{with}");
    assert_eq!(
        after_head(&with),
        after_head(&later),
        "its header still times its rows"
    );
    // A look that read no volume leaves the window's free space.
    let volumeless = || MachineFacts {
        look: MachineLook {
            disk_free_mb: None,
            ..own().look
        },
        ..own()
    };
    let with = merge_window_surveys(std::slice::from_ref(&later), usize::MAX, Some(&volumeless));
    assert!(with.contains(" home_free_mb=900 "), "{with}");

    // A window before `access=` too: the tokens end its line.
    let before_access = old(7, "");
    let with = merge_window_surveys(std::slice::from_ref(&before_access), usize::MAX, Some(&own));
    assert!(
        with.lines()
            .nth(1)
            .is_some_and(|head| head.ends_with(&format!(" roster_unread=0{tokens}"))),
        "{with}"
    );
    assert_eq!(after_head(&with), rows_read_at_1(&before_access));

    // Two windows' answers merged.
    let pair = [old(7, " access=read"), old(9, " access=read")];
    let merged = merge_window_surveys(&pair, usize::MAX, None);
    assert_eq!(looks(&merged), 0, "{merged}");
    let with = merge_window_surveys(&pair, usize::MAX, Some(&own));
    let head = with.lines().nth(1).expect("the instance line");
    assert!(with.starts_with("OK 4\ninstance pid=7 "), "{with}");
    assert!(
        head.ends_with(&format!(
            " roster_unread=0{tokens} access=read instances_read=2"
        )),
        "{head}"
    );
    assert!(head.contains(&format!(" at_ms={LOOKED} ")), "{head}");
    assert_eq!(looks(&with), 1, "{head}");
    assert_eq!(after_head(&with), rows_read_at_1(&merged));

    // A window that sends its own look, alone and first of two.
    let new = old(7, " m_at_ms=42 cores=2 access=read");
    assert_eq!(
        merge_window_surveys(std::slice::from_ref(&new), usize::MAX, Some(&never)),
        new
    );
    let merged = merge_window_surveys(&[new, old(9, " access=read")], usize::MAX, Some(&never));
    assert_eq!(looks(&merged), 1, "{merged}");

    // At the reply bound: rows go before the look does, the least kept first (the idle
    // shells, the latest first), and `sessions=` still counts them.
    let mut full = old(7, " access=full").replacen("OK 3\n", "OK 13\n", 1);
    let at = full.find("\naccount ").expect("the account line") + 1;
    let shells: String = (0..10)
        .map(|n| format!("session sid=s-shell{n} program_kind=shell agent=- attention=- needs=-\n"))
        .collect();
    full.insert_str(at, &shells);
    let full = full.replacen(" sessions=1 ", " sessions=11 ", 1);
    let bound = full.len();
    let fitted = merge_window_surveys(std::slice::from_ref(&full), bound, Some(&own));
    assert!(fitted.len() <= bound, "{fitted}");
    assert_eq!(looks(&fitted), 1, "{fitted}");
    assert!(
        fitted.contains(" sid=s-7 "),
        "the agent's row stays: {fitted}"
    );
    assert!(
        fitted.contains(" sid=s-shell0 "),
        "the earliest shell stays: {fitted}"
    );
    assert!(
        !fitted.contains(" sid=s-shell9 "),
        "the latest shell went: {fitted}"
    );
    assert!(fitted.contains(" sessions=11 "), "{fitted}");
    assert!(
        fitted.ends_with("last_used=-\n"),
        "the Mac's own lines stay: {fitted}"
    );
    assert!(
        SwarmSurveyFacts::from_survey_reply(&fitted).is_ok(),
        "a counted answer: {fitted}"
    );
    // With no row left to cut, the answer goes as it came, without the look.
    let rowless = "OK 1\ninstance pid=7 at_ms=1 home_free_mb=900 access=full\n".to_owned();
    assert_eq!(
        merge_window_surveys(std::slice::from_ref(&rowless), rowless.len(), Some(&own)),
        rowless
    );

    // Through the node job's survey: one older window, then two, then one that sends a look.
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("look-old");
    let _old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
    let one = &survey(relay.path(), Some(older), &["survey"])[0];
    assert!(
        one.starts_with(&format!("OK 3\ninstance pid={older} ")),
        "{one}"
    );
    assert_eq!(
        looks(one),
        1,
        "one window's answer carries the node job's look: {one}"
    );
    let head = fields_of(one.lines().nth(1).expect("the instance line"));
    assert_eq!(head("os"), Some(std::env::consts::OS), "{one}");
    assert!(
        head("m_at_ms").is_some_and(|at| at.parse::<u64>().is_ok_and(|at| at > 77)),
        "this machine's own look, taken now: {one}"
    );
    assert!(
        head("cores").is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 0)),
        "{one}"
    );
    let _new = TestWindow::open(relay.path(), newer, Holds::live(&["t00000000ae"]), false);
    let two = &survey(relay.path(), Some(older), &["survey"])[0];
    assert!(
        two.starts_with(&format!("OK 4\ninstance pid={older} ")),
        "{two}"
    );
    assert_eq!(looks(two), 1, "{two}");
    // These test windows say no `access=` either: the look ends the merged line.
    assert!(
        two.lines()
            .nth(1)
            .is_some_and(|head| head.contains(" instances_read=2 os=")),
        "{two}"
    );

    let relay = crate::test_scratch::short_dir("look-new");
    let holds = Holds::live(&["t000000000d"]).with_look();
    let _looking = TestWindow::open(relay.path(), older, holds.clone(), false);
    assert_eq!(
        survey(relay.path(), Some(older), &["survey"])[0],
        window_survey(older, &holds),
        "a window that sends its look is relayed as it came"
    );
}

/// THE NODE JOB SAYS THE VOLUME'S SIZE ONCE, UNDER AN OLDER WINDOW AND A NEWER ONE: a window
/// older than `home_total_mb=` sends a header with no size and no look, and the node job's look
/// adds its volume's size once, right after the free space it also makes the look's; a newer
/// window that sends no look but its own size has that one token made the look's, never a
/// second; a look that read no volume leaves the window's size as it came; a line with no free
/// space either says the look's size before the look's tokens. The strict parser reads the size
/// into the facts and the look alike.
/// NEGATIVE CONTROL: without the insertion after `home_free_mb=`, the older window's header says
/// no size; without the `home_total_mb` arm, the newer one says the window's 950000; with the
/// size left among the look's tokens too, a header says it twice.
#[test]
fn the_node_job_says_the_volumes_size_once_under_an_older_window_and_a_newer_one() {
    use aterm_agent::survey_facts::{MachineFacts, SwarmSurveyFacts};
    use aterm_capacity::machine_look::MachineLook;

    let own = || MachineFacts {
        look: MachineLook {
            at_unix_ms: Some(1_793_160_000_123),
            cores: Some(8),
            disk_total_mb: Some(953_000),
            disk_free_mb: Some(850),
            ..MachineLook::UNREAD
        },
        up_since_ms: None,
        os: Some("macos"),
        target: None,
    };
    let volumeless = || MachineFacts {
        look: MachineLook {
            disk_total_mb: None,
            disk_free_mb: None,
            ..own().look
        },
        ..own()
    };
    // A window's answer with no look, its header saying `tail` after its free space.
    let answer = |tail: &str| {
        format!(
            "OK 2\ninstance pid=7 host=m111 version=0.198.0 sessions=0 level=facts cut=0 \
             at_ms=1 node=n instances=1 label=m7 window=1 window_last=5 ssh_host_key=- \
             home_free_mb=900{tail} accounts=1 accounts_unread=- moves_7d=- move_tries_7d=- \
             last_move=- roster_unread=0 access=full\naccount pool=claude id=a7 \
             condition=usable enrolled=1 pct5=- pct7=- week_reset=- walled_until=- \
             last_used=-\n"
        )
    };
    let head_of = |answer: &str| answer.lines().nth(1).expect("the instance line").to_owned();
    let sizes = |head: &str| head.matches(" home_total_mb=").count();

    // A window older than the key: the look's size, once, right after the free space.
    let older = answer("");
    let with = merge_window_surveys(std::slice::from_ref(&older), usize::MAX, Some(&own));
    let head = head_of(&with);
    assert_eq!(sizes(&head), 1, "{head}");
    assert!(
        head.contains(" home_free_mb=850 home_total_mb=953000 accounts=1 "),
        "{head}"
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&with).expect("the strict parser reads it");
    assert_eq!(parsed.home_total_mb, Some(953_000));
    assert_eq!(
        parsed.machine.map(|m| m.look.disk_total_mb),
        Some(Some(953_000))
    );

    // A newer window that sends its own size and no look: its one token made the look's.
    let newer =
        answer(" home_total_mb=950000 home_build_mb=- home_build_at_ms=- home_build_partial=-");
    let with = merge_window_surveys(std::slice::from_ref(&newer), usize::MAX, Some(&own));
    let head = head_of(&with);
    assert_eq!(sizes(&head), 1, "{head}");
    assert!(
        head.contains(" home_free_mb=850 home_total_mb=953000 home_build_mb=- "),
        "{head}"
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&with).expect("the strict parser reads it");
    assert_eq!(parsed.home_total_mb, Some(953_000));

    // A look that read no volume leaves the window's size as it came.
    let with = merge_window_surveys(std::slice::from_ref(&newer), usize::MAX, Some(&volumeless));
    let head = head_of(&with);
    assert_eq!(sizes(&head), 1, "{head}");
    assert!(
        head.contains(" home_free_mb=900 home_total_mb=950000 "),
        "{head}"
    );

    // A line with no free space either: the size goes before the look's own tokens.
    let bare = "OK 1\ninstance pid=7 at_ms=1 access=full\n".to_owned();
    let with = merge_window_surveys(std::slice::from_ref(&bare), usize::MAX, Some(&own));
    let head = head_of(&with);
    assert_eq!(sizes(&head), 1, "{head}");
    assert!(head.contains(" home_total_mb=953000 os=macos "), "{head}");
}

/// The value an `instance` line says under a key.
fn fields_of<'a>(line: &'a str) -> impl Fn(&str) -> Option<&'a str> {
    move |key: &str| {
        line.split(' ')
            .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
    }
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
    let old = TestWindow::open(relay.path(), older, Holds::live(&["t000000000d"]), false);
    let new = TestWindow::open(relay.path(), newer, Holds::live(&["t00000000ae"]), false);
    let asker = "tqn5r6e0148";
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
    assert_eq!(
        without_live_readings(&answers[1]),
        without_live_readings(&answers[0]),
        "the same survey"
    );
    let kept = without_live_readings(&answers[0]);
    assert!(
        kept.contains(" sid=t000000000d ")
            && kept.contains(" sid=t00000000ae ")
            && kept.contains(" sessions="),
        "the comparison keeps the survey: {kept}"
    );
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

/// `answer` without what the machine reads afresh for each answer: the node job adds its own
/// look under these windows, which send none, so its clock and its figures (`at_ms=` and the
/// machine keys, [`aterm_agent::survey_facts::MachineFacts::header_tokens`]) differ between two
/// answers of one survey. Every other byte is the survey's.
fn without_live_readings(answer: &str) -> String {
    let unread = aterm_agent::survey_facts::MachineFacts {
        look: aterm_capacity::machine_look::MachineLook::UNREAD,
        up_since_ms: None,
        os: None,
        target: None,
    }
    .header_tokens();
    let live: BTreeSet<&str> = unread
        .split_whitespace()
        .filter_map(|token| token.split_once('=').map(|(key, _)| key))
        .chain(["at_ms"])
        .collect();
    answer
        .lines()
        .map(|line| {
            line.split(' ')
                .filter(|token| {
                    token
                        .split_once('=')
                        .is_none_or(|(key, _)| !live.contains(key))
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
        merge_window_surveys(std::slice::from_ref(&first), 64, None),
        first
    );
    let whole = merge_window_surveys(&[first.clone(), second.clone()], usize::MAX, None);
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
    let cut = merge_window_surveys(&[first, second], bound, None);
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
                 instances=2\nsession sid=t000000000a program_kind=agent agent=busy said=done\nsession \
                 sid=t000000000b program_kind=shell agent=- attention=- needs=-\naccount pool=claude \
                 id=a7\n"
        .to_owned();
    let second = "OK 2\ninstance pid=9 host=m111 sessions=1 level=content cut=0 at_ms=1 node=n \
                  instances=2\nsession sid=t000000000c program_kind=agent agent=idle said=ok\n"
        .to_owned();
    let whole = merge_window_surveys(&[first.clone(), second.clone()], usize::MAX, None);
    assert!(
        whole.starts_with("OK 5\ninstance pid=7 host=m111 sessions=3 level=content cut=1 "),
        "{whole}"
    );
    let row = whole
        .lines()
        .find(|line| line.contains(" sid=t000000000b "))
        .map_or(0, |line| line.len() + 1);
    let fitted = merge_window_surveys(&[first, second], whole.len() - row, None);
    assert!(!fitted.contains(" sid=t000000000b "), "{fitted}");
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
                 update_tries=2 update_checks=on access=read\nsession sid=t000000000a program_kind=shell\n"
        .to_owned();
    let second = "OK 2\ninstance pid=9 host=m221 version=0.198.0 sessions=1 level=facts cut=0 \
                  at_ms=1793130120000 node=n instances=2 update=retrying update_at=1793129400 \
                  update_to=0.199.0 update_why=crash-loop update_checks=on access=read\n\
                  session sid=t000000000b program_kind=shell\n"
        .to_owned();
    let merged = merge_window_surveys(&[first, second], usize::MAX, None);
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
/// canonical metric session ID (a legacy tab's `s-` ID is counted and not listed); with no window
/// here the count is a reading of no agents.
/// NEGATIVE CONTROL: a window that takes no survey hand-over is no reading: nothing is counted,
/// so nothing is published and the last count ages out.
#[test]
fn the_node_counts_the_agents_of_every_window() {
    let stand_in = StandIn::start();
    let (older, newer) = two_pids(&stand_in);
    let relay = crate::test_scratch::short_dir("activity-two");
    let minted = "t0000000001";
    let _old = TestWindow::open(
        relay.path(),
        older,
        Holds::live(&["s-busy1", minted, LEGACY]),
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
        (1_793_160_000, 2, 2, 0, 0, 0)
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

/// `answer`, one Mac's merged survey answer, as `aterm ctl survey --json` hands a fleet peer's to
/// the fleet read: its `instance` line and each `session` row an object of their `key=value`
/// words (no value here needs decoding).
fn peer_json(label: &str, answer: &str) -> String {
    let object = |line: &str| {
        let members: Vec<String> = line
            .split(' ')
            .skip(1)
            .filter_map(|token| token.split_once('='))
            .map(|(key, value)| format!("\"{key}\":\"{value}\""))
            .collect();
        format!("{{{}}}", members.join(","))
    };
    let instance = answer
        .lines()
        .find(|line| line.starts_with("instance "))
        .map_or_else(|| "null".to_owned(), object);
    let sessions: Vec<String> = answer
        .lines()
        .filter(|line| line.starts_with("session "))
        .map(object)
        .collect();
    format!(
        "[{{\"label\":\"{label}\",\"fleet\":true,\"instance\":{instance},\"sessions\":[{}]}}]",
        sessions.join(",")
    )
}

/// A PEER'S AGENTS ARE TIMED BY THEIR OWN WINDOW'S ANSWER: a Mac's node service merges its
/// windows' answers into one, under the first window's `instance` line, its `at_ms=` raised to
/// the node job's look where that is later. Each window read its agents' counters at its own
/// time, so a row the merge sends under a header of another time carries its window's own
/// `at_ms=` (`-` where that window's clock was unread), and the fleet read times each agent by
/// it: two windows, the look later than both, one window under a raised header, and a window
/// with no clock. CONTROLS: the merge sending each row as it came (every agent timed by the
/// header), the one-window path alone doing so, and the fleet read ignoring a row's own time
/// are each caught.
#[test]
fn a_peers_agents_are_timed_by_their_own_windows_answer_through_the_merge() {
    use aterm_agent::survey_facts::MachineFacts;
    use aterm_capacity::machine_look::MachineLook;

    const FIRST: u64 = 1_793_160_000_000;
    const SECOND: u64 = FIRST + 1_800;
    const LOOKED: u64 = FIRST + 2_500;
    let own = || MachineFacts {
        look: MachineLook {
            at_unix_ms: Some(LOOKED),
            ..MachineLook::UNREAD
        },
        up_since_ms: None,
        os: None,
        target: None,
    };
    let window = |pid: u32, at_ms: u64, sid: &str| {
        format!(
            "OK 2\ninstance pid={pid} host=m111 version=0.200.0 sessions=1 level=facts cut=0 \
             at_ms={at_ms} node=n instances=2 access=read\nsession sid={sid} state=running \
             program=claude program_kind=agent agent=busy attention=- needs=- turn=1\n"
        )
    };
    let header_at = |answer: &str| {
        answer
            .lines()
            .nth(1)
            .and_then(|head| head.split(' ').find_map(|t| t.strip_prefix("at_ms=")))
            .map(str::to_owned)
    };
    let times = |answer: &str| -> Vec<(String, Option<u64>)> {
        let read = crate::fleet_glance::sightings(&peer_json("m111", answer), LOOKED + 9)
            .expect("a fleet read");
        let aterm_db::fleet::read::Answer::Answered(answered) = &read.machines[0].answer else {
            panic!("the peer answered: {read:?}");
        };
        answered
            .agents
            .iter()
            .map(|agent| (agent.sid.clone(), agent.answer_at_ms))
            .collect()
    };
    let both = |second: Option<u64>| {
        vec![
            ("s-p1".to_owned(), Some(FIRST)),
            ("s-p2".to_owned(), second),
        ]
    };

    let pair = [window(7, FIRST, "s-p1"), window(9, SECOND, "s-p2")];
    let merged = merge_window_surveys(&pair, usize::MAX, None);
    assert_eq!(header_at(&merged), Some(FIRST.to_string()), "{merged}");
    assert_eq!(times(&merged), both(Some(SECOND)), "two windows: {merged}");

    let looked = merge_window_surveys(&pair, usize::MAX, Some(&own));
    assert_eq!(header_at(&looked), Some(LOOKED.to_string()), "{looked}");
    assert_eq!(
        times(&looked),
        both(Some(SECOND)),
        "under the look: {looked}"
    );

    let one = merge_window_surveys(std::slice::from_ref(&pair[0]), usize::MAX, Some(&own));
    assert_eq!(header_at(&one), Some(LOOKED.to_string()), "{one}");
    assert_eq!(
        times(&one),
        [("s-p1".to_owned(), Some(FIRST))],
        "one window under a raised header: {one}"
    );

    let clockless = [window(7, FIRST, "s-p1"), window(9, 0, "s-p2")];
    let merged = merge_window_surveys(&clockless, usize::MAX, None);
    assert_eq!(
        times(&merged),
        both(None),
        "a window with no clock: {merged}"
    );
}

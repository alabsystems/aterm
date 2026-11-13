// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::person::NEEDS_REMOTE_FDA;
use crate::model::{Effect, HostFacts, ItemReport, Mode};
use crate::port::Fake;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

fn admin() -> User {
    User {
        name: "admin".into(),
        uid: 501,
        gid: 20,
        home: "/Users//admin".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    }
}

fn row(id: &str, outcome: Outcome, why: Option<&str>) -> ItemReport {
    ItemReport {
        id: id.into(),
        family: id.split('.').next().unwrap().into(),
        title: id.into(),
        outcome,
        from: None,
        to: None,
        why: why.map(str::to_owned),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

fn report(items: Vec<ItemReport>) -> HostReport {
    HostReport {
        schema: 1,
        mode: Mode::Plan,
        serial: 1,
        key_id: String::new(),
        run_id: String::new(),
        started: 0,
        finished: 0,
        facts: HostFacts::default(),
        items,
        refused: None,
    }
}

/// A scripted host: replies keyed by the exec's argv, consumed in order, the last one
/// repeating; every request recorded.
#[derive(Default)]
struct Host<'a> {
    replies: Mutex<BTreeMap<String, Vec<Ran>>>,
    seen: Mutex<Vec<String>>,
    /// `launchctl print gui/501`, reading by reading, the last repeating.
    sessions: Mutex<Vec<Result<Ran, String>>>,
    /// Each pane open's answer in turn, the last repeating.
    panes: Mutex<Vec<Result<Ran, String>>>,
    /// The lock request's answer (`locked` when `None`).
    lock: Option<Result<Ran, String>>,
    /// Each arm's answer in turn, the last repeating (`armed 1800000000-77` when none).
    arm: Mutex<Vec<Result<Ran, String>>>,
    /// Each disarm's answer in turn, the last repeating (`disarmed 1800000000-77` when
    /// none).
    disarm: Mutex<Vec<Result<Ran, String>>>,
    /// The port whose relay the lock request is checked against.
    sys: Option<&'a Fake>,
    /// The viewer the arm is checked against.
    viewer: Option<&'a Recorder>,
    /// Whether that relay had closed when the lock request came.
    closed_at_lock: Mutex<Option<bool>>,
    /// Whether that relay had opened when the first session reading came.
    relayed_at_first_reading: Mutex<Option<bool>>,
    /// Whether the relay had opened, and the viewer, when the arm came.
    opened_at_arm: Mutex<Option<(bool, bool)>>,
}

const WATCH: &str = "1800000000-77";

fn next(script: &Mutex<Vec<Result<Ran, String>>>) -> Option<Result<Ran, String>> {
    let mut script = script.lock().unwrap();
    match script.len() {
        0 => None,
        1 => Some(script[0].clone()),
        _ => Some(script.remove(0)),
    }
}

impl Host<'_> {
    fn logs_in_after(&self, absent: usize) {
        let mut script: Vec<Result<Ran, String>> = (0..absent)
            .map(|_| {
                Ok(Ran::exit(
                    125,
                    "Could not print domain: 125: Domain does not support specified action\n",
                ))
            })
            .collect();
        script.push(Ok(Ran::ok("")));
        *self.sessions.lock().unwrap() = script;
    }

    fn on(&self, cmd: &Cmd, reply: Ran) {
        let key = exec_of(cmd).unwrap().argv.join(" ");
        self.replies
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(reply);
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Remote for Host<'_> {
    fn exec(&self, exec: &Exec) -> Result<Ran, String> {
        let key = exec.argv.join(" ");
        self.seen.lock().unwrap().push(key.clone());
        if exec.argv.last().map(String::as_str) == Some("lock-screen") {
            if let Some(sys) = self.sys {
                *self.closed_at_lock.lock().unwrap() =
                    Some(sys.calls().last().map(String::as_str) == Some("relay closed"));
            }
            return self.lock.clone().unwrap_or_else(|| Ok(Ran::ok("locked\n")));
        }
        match exec.argv.get(3..5) {
            Some([verb, arm]) if verb == "screen-watch" && arm == "arm" => {
                *self.opened_at_arm.lock().unwrap() = Some((
                    self.sys
                        .is_some_and(|sys| sys.calls().iter().any(|c| c.starts_with("relay "))),
                    self.viewer
                        .is_some_and(|viewer| !viewer.0.lock().unwrap().is_empty()),
                ));
                return next(&self.arm).unwrap_or_else(|| Ok(Ran::ok(&format!("armed {WATCH}\n"))));
            }
            Some([verb, disarm]) if verb == "screen-watch" && disarm == "disarm" => {
                return next(&self.disarm)
                    .unwrap_or_else(|| Ok(Ran::ok(&format!("disarmed {WATCH}\n"))));
            }
            _ => {}
        }
        if exec.argv.get(1).map(String::as_str) == Some("asuser") {
            return next(&self.panes).unwrap_or_else(|| Ok(Ran::ok("")));
        }
        if exec.argv.get(1).map(String::as_str) == Some("print") {
            if let Some(sys) = self.sys {
                self.relayed_at_first_reading
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| sys.calls().iter().any(|c| c.starts_with("relay ")));
            }
            let mut script = self.sessions.lock().unwrap();
            return match script.len() {
                0 => Ok(Ran::ok("gui/501 = {\n\ttype = login\n\tsession = Aqua\n")),
                1 => script[0].clone(),
                _ => script.remove(0),
            };
        }
        let mut replies = self.replies.lock().unwrap();
        match replies.get_mut(&key) {
            Some(list) if list.len() > 1 => Ok(list.remove(0)),
            Some(list) if !list.is_empty() => Ok(list[0].clone()),
            _ => Err(format!("no reply scripted for {key}")),
        }
    }
}

/// A clock that moves only when the flow pauses.
#[derive(Default)]
struct TestClock(Mutex<Duration>);

impl Clock for TestClock {
    fn elapsed(&self) -> Duration {
        *self.0.lock().unwrap()
    }
    fn pause(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}

/// The inert opener: records each URL, opens nothing.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Opener for Recorder {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

const ATERM_ROWS_OFF: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|0\n";
const ATERM_ROWS_FDA: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n";
const ATERM_ROWS_BOTH: &str = "com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\ncom.aterm.aterm|kTCCServiceDeveloperTool|2\n";

fn tcc_aterm() -> Cmd {
    person::tcc_cmd(&["com.aterm.aterm"])
}

fn paths() -> Paths {
    Paths {
        root: PathBuf::from("/Users//owner/.config/aterm/machines"),
    }
}

fn target() -> Target {
    Target {
        user: "admin".into(),
        address: "lab-07.local".into(),
        alias: Some("4C4C4544-0000-1000-8000-000000000007".into()),
    }
}

fn steps(ids: &[&str]) -> Vec<Step> {
    ids.iter()
        .map(|id| Step {
            step: PersonStep::parse(id).unwrap(),
            measured: true,
            why: None,
        })
        .collect()
}

const TTY: Terminals = Terminals {
    stdin: true,
    stderr: true,
};

#[test]
fn outstanding_steps_come_from_the_report_with_fda_before_developer_tools() {
    let r = report(vec![
        row("person.developer_tools:aterm", Outcome::Person, Some("…")),
        row("power.ac.sleep", Outcome::Person, None),
        row("person.remote_login", Outcome::Ok, None),
        row(
            "person.full_disk_access:aterm",
            Outcome::Unknown,
            Some(NEEDS_REMOTE_FDA),
        ),
        row("person.remote_full_disk_access", Outcome::Person, Some("…")),
        row(
            "person.screen_sharing",
            Outcome::Unknown,
            Some("launchctl: timed out"),
        ),
    ]);
    let got: Vec<(String, bool)> = prepare_steps(&r)
        .into_iter()
        .map(|s| (s.id(), s.measured))
        .collect();
    assert_eq!(
        got,
        [
            ("remote_full_disk_access".to_owned(), true),
            ("screen_sharing".to_owned(), false),
            ("full_disk_access:aterm".to_owned(), false),
            ("developer_tools:aterm".to_owned(), true),
        ]
    );
    let unread = prepare_steps(&r)
        .into_iter()
        .find(|s| s.id() == "screen_sharing")
        .unwrap();
    assert_eq!(unread.why.as_deref(), Some("launchctl: timed out"));
}

#[test]
fn one_named_step_is_walked_even_when_the_report_does_not_list_it() {
    let got = only(steps(&["full_disk_access:aterm"]), "developer_tools:aterm").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id(), "developer_tools:aterm");
    assert!(!got[0].measured);
    assert!(only(Vec::new(), "nope").is_err());
}

#[test]
fn the_admins_uid_comes_from_the_hosts_own_report() {
    let mut r = report(Vec::new());
    assert!(
        account(&r, "admin")
            .unwrap_err()
            .contains("no account admin")
    );
    r.facts.users.push(admin());
    assert_eq!(account(&r, "admin").unwrap().uid, 501);
}

#[test]
fn the_pane_opens_in_the_admins_session_through_launchctl_asuser_with_no_shell() {
    let exec = open_pane_cmd(&admin(), person::PANE_FULL_DISK_ACCESS);
    assert_eq!(exec.user, None, "launchctl asuser needs root");
    assert_eq!(
        exec.argv,
        [
            "/bin/launchctl",
            "asuser",
            "501",
            "/usr/bin/sudo",
            "-n",
            "-u",
            "admin",
            "-H",
            "--",
            "/usr/bin/env",
            "-i",
            "HOME=/Users//admin",
            "USER=admin",
            "LOGNAME=admin",
            "SHELL=/bin/zsh",
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "/usr/bin/open",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
        ]
    );
}

fn screen_sharing_on(host: &Host) {
    host.on(&person::print_disabled_cmd(), Ran::ok(
        "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n",
    ));
    host.on(
        &person::tcc_cmd(&[person::SCREEN_SHARING_AGENT]),
        Ran::ok("com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
    );
}

fn relaying() -> Fake {
    let mut sys = Fake::default();
    sys.on(
        &relay_cmd(&paths(), &target()).unwrap().display(),
        Ran::ok("RFB 003.889\n"),
    );
    sys.relay_port = 55_901;
    sys
}

fn relay_calls() -> [String; 5] {
    let cmd = relay_cmd(&paths(), &target()).unwrap().display();
    [
        cmd.clone(),
        "catch stops".into(),
        format!("relay {cmd}"),
        "relay closed".into(),
        "stops restored".into(),
    ]
}

#[test]
fn each_relayed_connection_is_the_fleets_screen_request_and_nothing_else() {
    let cmd = relay_cmd(&paths(), &target()).unwrap();
    assert_eq!(cmd.program, PathBuf::from("/usr/bin/ssh"));
    let args: Vec<String> = cmd
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut want = vec!["-T".to_owned()];
    want.extend(
        ssh::base_options(&paths(), &target())
            .iter()
            .map(|a| a.to_string_lossy().into_owned()),
    );
    want.extend(["--", "admin@lab-07.local", "screen"].map(str::to_owned));
    assert_eq!(args, want);
    assert!(
        !args
            .iter()
            .any(|a| matches!(a.as_str(), "-L" | "-R" | "-D" | "-W" | "-N"))
    );
    assert!(args.iter().any(|a| a == "ClearAllForwardings=yes"));
    assert_eq!(vnc_url("admin", 55_901), "vnc://admin@127.0.0.1:55901");
    let mut odd = target();
    odd.address = "-oProxyCommand=evil".into();
    assert!(relay_cmd(&paths(), &odd).is_err());
}

struct Sessions(Mutex<Vec<Ran>>);

impl Remote for Sessions {
    fn exec(&self, _: &Exec) -> Result<Ran, String> {
        let mut left = self.0.lock().unwrap();
        Ok(if left.len() > 1 {
            left.remove(0)
        } else {
            left[0].clone()
        })
    }
}

#[test]
fn a_session_is_its_exit_status_even_when_the_kept_output_never_names_it() {
    let host = Sessions(Mutex::new(vec![
        Ran::exit(
            125,
            "Could not print domain: 125: Domain does not support specified action\n",
        ),
        Ran::exit(112, "Could not find domain for user gui: 501\n"),
        Ran::ok("\t\t       0    M   D   com.apple.metadata.mdflagwriter\n\t}\n"),
    ]));
    let clock = TestClock::default();
    let said = Mutex::new(Vec::new());
    let say = |line: &str| said.lock().unwrap().push(line.to_owned());
    let flow = Flow {
        sys: &Fake::default(),
        remote: &host,
        clock: &clock,
        say: &say,
        hang: Duration::from_secs(60),
    };
    assert_eq!(
        await_session(&flow, &admin(), false).unwrap(),
        Found::LoggedIn
    );
    assert_eq!(clock.elapsed(), POLL * 2);
    assert_eq!(said.lock().unwrap().len(), 1);
}

#[test]
fn without_terminals_both_flows_refuse_before_anything_runs() {
    let sys = Fake::default();
    let host = Host::default();
    let opener = Recorder::default();
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let piped = Terminals {
        stdin: false,
        stderr: true,
    };
    let s = steps(&["full_disk_access:aterm"]);
    let err = screen(&flow, &paths(), &target(), &admin(), &s, &opener, piped).unwrap_err();
    assert!(err.contains("not a terminal"), "{err}");
    let err = here(
        &flow,
        &admin(),
        &s,
        Terminals {
            stdin: true,
            stderr: false,
        },
    )
    .unwrap_err();
    assert!(err.starts_with("aterm machines person --here"), "{err}");
    assert!(sys.calls().is_empty());
    assert!(host.seen().is_empty());
    assert!(opener.0.lock().unwrap().is_empty());
}

#[test]
fn screen_relays_opens_the_viewer_then_walks_fda_before_developer_tools() {
    let sys = relaying();
    let host = Host::default();
    screen_sharing_on(&host);
    for rows in [
        ATERM_ROWS_OFF,
        ATERM_ROWS_OFF,
        ATERM_ROWS_FDA,
        ATERM_ROWS_FDA,
        ATERM_ROWS_BOTH,
    ] {
        host.on(&tcc_aterm(), Ran::ok(rows));
    }
    let opener = Recorder::default();
    let clock = TestClock::default();
    let said = Mutex::new(Vec::new());
    let say = |line: &str| said.lock().unwrap().push(line.to_owned());
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &say,
        hang: Duration::from_secs(60),
    };
    let screened = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm", "developer_tools:aterm"]),
        &opener,
        TTY,
    )
    .unwrap();
    assert_eq!(
        screened.walked,
        [
            Walked {
                step: "full_disk_access:aterm".into(),
                done: Done::Granted
            },
            Walked {
                step: "developer_tools:aterm".into(),
                done: Done::Granted
            },
        ]
    );
    assert_eq!(*opener.0.lock().unwrap(), ["vnc://admin@127.0.0.1:55901"]);
    let panes: Vec<String> = host
        .seen()
        .into_iter()
        .filter(|s| s.contains("/usr/bin/open"))
        .collect();
    assert_eq!(panes.len(), 2);
    assert!(panes[0].ends_with("Privacy_AllFiles"), "{panes:?}");
    assert!(panes[1].ends_with("Privacy_DevTools"), "{panes:?}");
    let walked_requests: Vec<String> = host
        .seen()
        .into_iter()
        .filter(|s| !s.contains("print-disabled") && !s.contains("screensharing.agent"))
        .collect();
    assert!(
        walked_requests[0].ends_with("print gui/501"),
        "the session comes first: {walked_requests:?}"
    );
    assert_eq!(sys.calls()[..4], relay_calls()[..4]);
    drop(screened.stops);
    assert_eq!(sys.calls(), relay_calls());
    assert!(
        said.lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("log in as admin"))
    );
}

#[test]
fn a_step_nobody_grants_ends_at_the_hang_detector_and_the_rest_is_not_attempted() {
    let sys = relaying();
    let host = Host::default();
    screen_sharing_on(&host);
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let walked = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm", "developer_tools:aterm"]),
        &Recorder::default(),
        TTY,
    )
    .unwrap()
    .walked;
    assert!(
        matches!(&walked[0].done, Done::NotGranted(why) if why.contains("not granted after 60s")),
        "{walked:?}"
    );
    assert!(
        matches!(&walked[1].done, Done::NotGranted(why) if why.starts_with("not attempted")),
        "{walked:?}"
    );
    assert!(clock.elapsed() >= Duration::from_secs(60));
    assert_eq!(
        sys.calls(),
        relay_calls(),
        "the relay closed after the hang"
    );
}

fn pane_opens(host: &Host<'_>) -> usize {
    host.seen()
        .iter()
        .filter(|s| s.contains("/usr/bin/open"))
        .count()
}

fn not_answered() -> Result<Ran, String> {
    Err(
        "the agent answered no report: exit 255: Connection timed out during banner exchange"
            .to_owned(),
    )
}

#[test]
fn a_pane_open_that_did_not_run_is_sent_again_until_it_runs() {
    let sys = relaying();
    let host = Host::default();
    *host.panes.lock().unwrap() = vec![not_answered(), not_answered(), Ok(Ran::ok(""))];
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(screened.walked[0].done, Done::Granted);
    assert_eq!(pane_opens(&host), 3);
}

#[test]
fn a_pane_open_that_ran_and_failed_ends_the_step_at_once() {
    let sys = relaying();
    let host = Host::default();
    *host.panes.lock().unwrap() = vec![Ok(Ran::exit(1, "LSOpenURLsWithRole() failed\n"))];
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(
        screened.walked[0].done,
        Done::NotGranted("the pane did not open: exit 1: LSOpenURLsWithRole() failed".into())
    );
    assert_eq!(pane_opens(&host), 1);
}

#[test]
fn a_pane_open_that_never_runs_ends_the_step_at_the_hang_detector() {
    let sys = relaying();
    let host = Host::default();
    *host.panes.lock().unwrap() = vec![not_answered()];
    let (screened, _) = screen_fda(&host, &sys);
    assert!(
        matches!(&screened.walked[0].done, Done::NotGranted(why)
            if why.starts_with("the pane did not open: the request did not run: the agent answered no report")
                && why.ends_with("(sent for 60s)")),
        "{:?}",
        screened.walked
    );
    assert_eq!(
        pane_opens(&host),
        31,
        "one open, then one every 2 s for 60 s"
    );
}

#[test]
fn screen_refuses_while_screen_sharing_itself_is_outstanding() {
    let sys = relaying();
    let host = Host::default();
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let err = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["screen_sharing", "full_disk_access:aterm"]),
        &Recorder::default(),
        TTY,
    )
    .unwrap_err();
    assert!(err.contains("--here"), "{err}");
    assert!(sys.calls().is_empty(), "no relay was opened");
}

#[test]
fn no_viewer_opens_unless_the_host_greets_as_screen_sharing_and_the_relay_opens() {
    let check = relay_cmd(&paths(), &target()).unwrap().display();
    let old_agent = "refused: \"screen\" is not a request this key may make (hello, put \
                     <sha256>, apply, attach <run>)";
    let cases = [
        (
            Ran {
                status: Some(2),
                stdout: format!("{old_agent}\n").into_bytes(),
                stderr: Vec::new(),
            },
            old_agent,
        ),
        (
            Ran::exit(255, "admin@lab-07.local: Permission denied (publickey).\n"),
            "Permission denied (publickey).",
        ),
        (Ran::ok("SSH-2.0-x\n"), "SSH-2.0-x"),
        (Ran::ok("RFB 003.889\n"), "fake: no relay port scripted"),
    ];
    for (answer, why) in cases {
        let sys = Fake::default();
        sys.on(&check, answer.clone());
        let host = Host::default();
        screen_sharing_on(&host);
        let opener = Recorder::default();
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &opener,
            TTY,
        )
        .unwrap_err();
        assert!(err.contains("lab-07.local") && err.contains(why), "{err}");
        assert!(opener.0.lock().unwrap().is_empty(), "{why}: no viewer");
        assert!(
            host.seen().iter().all(|s| s.contains("print-disabled")
                || s.contains("screensharing.agent")
                || s.ends_with("print gui/501")),
            "{why}: nothing asked of the host but Screen Sharing's and the session's \
             readings: {:?}",
            host.seen()
        );
        let calls = relay_calls();
        let want: Vec<&String> = if answer.stdout.starts_with(b"RFB ") {
            vec![&calls[0], &calls[1], &calls[2], &calls[4]]
        } else {
            vec![&calls[0]]
        };
        assert_eq!(sys.calls().iter().collect::<Vec<_>>(), want, "{why}");
    }
}

#[test]
fn no_relay_opens_unless_screen_sharing_reads_on_now_whatever_the_report_said() {
    let off = "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => disabled\n\t}\n";
    for (reply, want) in [
        (Ran::ok(off), "is not on with control"),
        (Ran::exit(1, "launchctl: timed out\n"), "could not be read"),
    ] {
        let sys = relaying();
        let host = Host::default();
        host.on(&person::print_disabled_cmd(), reply);
        let opener = Recorder::default();
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        let err = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &opener,
            TTY,
        )
        .unwrap_err();
        assert!(err.contains(want), "{err}");
        assert!(
            sys.calls().is_empty(),
            "no greeting asked for, no relay opened"
        );
        assert!(opener.0.lock().unwrap().is_empty());
    }
}

#[test]
fn here_walks_without_a_relay_or_viewer_and_skips_what_is_already_granted() {
    let sys = relaying();
    let host = Host::default();
    host.on(&person::print_disabled_cmd(), Ran::ok(
        "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n",
    ));
    host.on(
        &person::tcc_cmd(&[person::SCREEN_SHARING_AGENT]),
        Ran::ok("com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
    );
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let walked = here(
        &flow,
        &admin(),
        &steps(&["screen_sharing", "full_disk_access:aterm"]),
        TTY,
    )
    .unwrap();
    assert_eq!(walked[0].done, Done::AlreadyGranted);
    assert_eq!(walked[1].done, Done::Granted);
    assert!(sys.calls().is_empty(), "no relay at the Mac");
    let panes = host
        .seen()
        .into_iter()
        .filter(|s| s.contains("/usr/bin/open"))
        .count();
    assert_eq!(panes, 1, "the granted step opened no pane");
}

struct Unreachable;

impl Remote for Unreachable {
    fn exec(&self, _: &Exec) -> Result<Ran, String> {
        Err("ssh: connect to host lab-07.local port 22: Operation timed out".into())
    }
}

#[test]
fn a_session_wait_that_ends_at_the_hang_detector_says_what_it_last_read() {
    let clock = TestClock::default();
    let sys = Fake::default();
    let flow = Flow {
        sys: &sys,
        remote: &Unreachable,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let err = await_session(&flow, &admin(), false).unwrap_err();
    assert!(err.contains("the request did not run"), "{err}");
    assert!(err.contains("Operation timed out"), "{err}");
}

#[test]
fn over_screen_sharing_the_keyboard_only_steps_are_left_for_the_mac() {
    let host = Host::default();
    let clock = TestClock::default();
    let sys = Fake::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let walked = walk(&flow, &admin(), &steps(&["remote_login"]), false);
    assert!(matches!(&walked[0].done, Done::AtConsole(how) if how.contains("Remote Login")));
    assert!(host.seen().is_empty());
}

#[test]
fn a_failed_login_wait_keeps_the_session_cleanup_obligation() {
    for newly_logged_in in [false, true] {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        if newly_logged_in {
            host.logs_in_after(1);
        }
        let (screened, _) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(
            screened.session,
            if newly_logged_in {
                Session::Locked
            } else {
                Session::Kept
            }
        );
        assert_eq!(lock_requests(&host), usize::from(newly_logged_in));
        assert_eq!(sys.calls(), relay_calls());
    }
    let missing = || Ok(Ran::exit(112, "no login session"));
    let present = || Ok(Ran::ok(""));
    let unread = || Err("synthetic session read refused".to_owned());
    let run = |script: Vec<Result<Ran, String>>| {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            ..Host::default()
        };
        *host.sessions.lock().unwrap() = script;
        screen_sharing_on(&host);
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::ZERO,
        };
        let result = screen(
            &flow,
            &paths(),
            &target(),
            &admin(),
            &steps(&["full_disk_access:aterm"]),
            &Recorder::default(),
            TTY,
        )
        .map(|mut screened| {
            screened.stops = Box::new(());
            screened
        });
        let locks = lock_requests(&host);
        let closed = *host.closed_at_lock.lock().unwrap();
        let requests = host.seen();
        (result, locks, closed, requests, sys.calls())
    };
    for script in [
        vec![missing(), missing(), missing()],
        vec![present(), unread(), present()],
    ] {
        let (result, locks, _, _, calls) = run(script);
        assert!(result.is_err());
        assert_eq!(locks, 0);
        assert_eq!(calls, relay_calls());
    }
    let mut wrong = Vec::new();
    for (case, script) in [
        ("initially absent", vec![missing(), missing(), present()]),
        ("became absent", vec![present(), missing(), present()]),
        ("unknown after close", vec![missing(), missing(), unread()]),
    ] {
        let (result, locks, closed, requests, calls) = run(script);
        let kept_failure = match &result {
            Ok(screened) => {
                screened.session == Session::Locked
                    && matches!(&screened.walked[..], [Walked { done: Done::NotGranted(why), .. }]
                    if why.contains("has not logged in") && why.contains("no login session"))
            }
            Err(_) => false,
        };
        if !kept_failure
            || locks != 1
            || closed != Some(true)
            || calls != relay_calls()
            || requests.iter().any(|r| r.contains("/usr/bin/open"))
        {
            wrong.push(format!("{case}: result={result:?}, locks={locks}, closed={closed:?}, calls={calls:?}, requests={requests:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "RELAY_FAILURE_LOST_SESSION_CUSTODY: {wrong:?}"
    );
}

#[test]
fn the_lock_runs_the_agents_own_verb_in_the_admins_session_like_the_pane() {
    let lock = lock_cmd(&admin());
    let pane = open_pane_cmd(&admin(), person::PANE_DEVELOPER_TOOLS);
    assert_eq!(lock.user, None, "launchctl asuser needs root");
    assert_eq!(lock.argv[..16], pane.argv[..16]);
    assert_eq!(
        lock.argv[16..],
        [
            "/Library/Management/aterm/bin/aterm",
            "machines",
            "agent",
            "lock-screen"
        ]
    );
}

fn screen_fda(host: &Host<'_>, sys: &Fake) -> (Screened, Vec<String>) {
    screen_sharing_on(host);
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
    let clock = TestClock::default();
    let said = Mutex::new(Vec::new());
    let say = |line: &str| said.lock().unwrap().push(line.to_owned());
    let flow = Flow {
        sys,
        remote: host,
        clock: &clock,
        say: &say,
        hang: Duration::from_secs(60),
    };
    let mut screened = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm"]),
        &Recorder::default(),
        TTY,
    )
    .unwrap();
    screened.stops = Box::new(());
    (screened, said.into_inner().unwrap())
}

fn lock_requests(host: &Host<'_>) -> usize {
    let lock = lock_cmd(&admin()).argv.join(" ");
    host.seen().iter().filter(|s| **s == lock).count()
}

fn lock_and_watch(host: &Host<'_>) -> Vec<String> {
    let lock = lock_cmd(&admin()).argv.join(" ");
    let arm = arm_cmd(&admin()).argv.join(" ");
    host.seen()
        .into_iter()
        .filter_map(|s| {
            if s == lock {
                Some("lock".to_owned())
            } else if s == arm {
                Some("arm".to_owned())
            } else {
                s.strip_prefix(&disarm_cmd("").argv.join(" "))
                    .map(|id| format!("disarm {id}"))
            }
        })
        .collect()
}

#[test]
fn a_walk_that_waited_for_the_login_locks_that_session_once_the_relay_closed() {
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(screened.walked[0].done, Done::Granted);
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(lock_requests(&host), 1);
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
    let seen = host.seen();
    assert_eq!(seen[seen.len() - 2], lock_cmd(&admin()).argv.join(" "));
    assert_eq!(
        *host.closed_at_lock.lock().unwrap(),
        Some(true),
        "the lock comes after the relay closed"
    );
    assert_eq!(sys.calls(), relay_calls());
    assert!(
        said.iter().any(|l| l.starts_with("locked admin's session")),
        "{said:?}"
    );
    assert_eq!(screened.session.unlocked_line("lab-07", "admin"), None);
}

#[test]
fn a_session_there_at_the_first_reading_is_left_to_macos() {
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(screened.walked[0].done, Done::Granted);
    assert_eq!(screened.session, Session::Kept);
    assert_eq!(
        lock_and_watch(&host),
        Vec::<String>::new(),
        "{:?}",
        host.seen()
    );
    assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    *host.sessions.lock().unwrap() = vec![
        Ok(Ran::ok("")),
        Ok(Ran::exit(112, "Could not find domain for user gui: 501\n")),
        Ok(Ran::ok("")),
    ];
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(lock_and_watch(&host), ["lock"]);
    assert!(
        said.iter()
            .any(|l| l.ends_with("the Mac stays logged in and unlocked at its display")),
        "{said:?}"
    );
}

#[test]
fn the_session_is_read_before_anything_is_relayed_and_a_reading_that_did_not_run_relays_nothing() {
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        *host.relayed_at_first_reading.lock().unwrap(),
        Some(false),
        "the first session reading came before the relay opened"
    );
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    *host.sessions.lock().unwrap() = vec![
        Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
        Ok(Ran::ok("")),
    ];
    screen_sharing_on(&host);
    let opener = Recorder::default();
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let err = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm"]),
        &opener,
        TTY,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "whether admin is logged in on lab-07.local could not be read (the request did not \
         run: ssh: connect to host lab-07.local port 22: Operation timed out); nothing was \
         relayed"
    );
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    assert!(opener.0.lock().unwrap().is_empty());
    assert_eq!(lock_requests(&host), 0);
}

#[test]
fn a_stop_while_waiting_for_the_login_closes_the_relay_then_locks_a_session_let_in_meanwhile() {
    let mut sys = relaying();
    sys.stop_after = Some(0);
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(2);
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(
        screened.walked,
        [Walked {
            step: "full_disk_access:aterm".into(),
            done: Done::NotGranted("not attempted: stopped by SIGINT".into()),
        }]
    );
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
    assert_eq!(*host.closed_at_lock.lock().unwrap(), Some(true));
    assert_eq!(
        sys.calls(),
        relay_calls(),
        "caught until the command let them go"
    );
    assert!(!host.seen().iter().any(|s| s.contains("/usr/bin/open")));
    assert!(said.iter().any(|l| l.starts_with("locked admin's session")));
    let mut sys = relaying();
    sys.stop_after = Some(0);
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(9);
    screen_sharing_on(&host);
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let err = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm"]),
        &Recorder::default(),
        TTY,
    )
    .unwrap_err();
    assert!(
        err.starts_with("stopped by SIGINT while waiting for admin to log in"),
        "{err}"
    );
    assert_eq!(lock_and_watch(&host), ["arm", &format!("disarm {WATCH}")]);
    assert_eq!(sys.calls(), relay_calls());
}

#[test]
fn a_stop_during_a_step_still_locks_a_session_logged_in_over_the_relay_and_none_other() {
    let mut sys = relaying();
    sys.stop_after = Some(1);
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(
        screened.walked[0].done,
        Done::NotGranted("stopped by SIGINT".into())
    );
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(*host.closed_at_lock.lock().unwrap(), Some(true));
    assert_eq!(sys.calls(), relay_calls());
    assert!(
        said.iter().any(|l| l
            == "admin logged in over Screen Sharing: the session is locked again when the \
                walk ends or is stopped; if aterm is killed or loses the Mac first, the host \
                locks it itself once no request has reached it for 60s"),
        "{said:?}"
    );
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
    let mut sys = relaying();
    sys.stop_after = Some(0);
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(
        screened.walked[0].done,
        Done::NotGranted("not attempted: stopped by SIGINT".into())
    );
    assert_eq!(screened.session, Session::Kept);
    assert_eq!(lock_requests(&host), 0, "{:?}", host.seen());
    assert!(
        !said
            .iter()
            .any(|l| l.contains("logged in over Screen Sharing"))
    );
    assert_eq!(sys.calls(), relay_calls());
}

#[test]
fn a_lock_that_does_not_read_back_is_reported_as_a_mac_left_unlocked() {
    let refused_verb = "refused: `aterm machines agent lock-screen` is not an agent verb (ssh, \
                        hello, put, apply, run-job, attach, put-file, install, version)";
    for (answer, why) in [
        (
            Ran {
                status: Some(1),
                stdout: b"not locked: SACLockScreenImmediate answered 22\n".to_vec(),
                stderr: Vec::new(),
            },
            "SACLockScreenImmediate answered 22".to_owned(),
        ),
        (
            Ran {
                status: Some(2),
                stdout: format!("{refused_verb}\n").into_bytes(),
                stderr: Vec::new(),
            },
            format!("exit 2: {refused_verb}"),
        ),
        (
            Ran::exit(1, "sudo: a password is required\n"),
            "exit 1: sudo: a password is required".to_owned(),
        ),
    ] {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            lock: Some(Ok(answer)),
            ..Host::default()
        };
        host.logs_in_after(2);
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(
            screened.session,
            Session::Unlocked {
                why: why.clone(),
                unread: None,
                watched: true,
            }
        );
        assert_eq!(lock_and_watch(&host), ["arm", "lock"]);
        assert!(!said.iter().any(|l| l.starts_with("locked")), "{said:?}");
        let line = screened.session.unlocked_line("lab-07", "admin").unwrap();
        assert_eq!(
            line,
            format!(
                "lab-07: the Mac is still logged in as admin and unlocked at its display: \
                 the lock after the walk failed ({why}); the host's screen watch locks it \
                 once no request has reached the host for 60s"
            )
        );
    }
    struct NoLock<'a>(Host<'a>);
    impl Remote for NoLock<'_> {
        fn exec(&self, exec: &Exec) -> Result<Ran, String> {
            if exec.argv.last().map(String::as_str) == Some("lock-screen") {
                return Err("ssh: connect to host lab-07.local port 22: Connection refused".into());
            }
            self.0.exec(exec)
        }
    }
    let sys = relaying();
    let host = NoLock(Host::default());
    host.0.logs_in_after(1);
    screen_sharing_on(&host.0);
    host.0.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let screened = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm"]),
        &Recorder::default(),
        TTY,
    )
    .unwrap();
    assert!(
        matches!(&screened.session, Session::Unlocked { why, unread: None, watched: true } if why.starts_with("the request did not run") && why.contains("Connection refused")),
        "{:?}",
        screened.session
    );
}

fn screen_with(
    host: &Host<'_>,
    sys: &Fake,
    opener: &Recorder,
) -> (Result<Screened, String>, Vec<String>) {
    screen_sharing_on(host);
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
    let clock = TestClock::default();
    let said = Mutex::new(Vec::new());
    let say = |line: &str| said.lock().unwrap().push(line.to_owned());
    let flow = Flow {
        sys,
        remote: host,
        clock: &clock,
        say: &say,
        hang: Duration::from_secs(60),
    };
    let screened = screen(
        &flow,
        &paths(),
        &target(),
        &admin(),
        &steps(&["full_disk_access:aterm"]),
        opener,
        TTY,
    );
    (screened, said.into_inner().unwrap())
}

#[test]
fn with_nobody_logged_in_the_watch_is_armed_before_the_relay_and_the_viewer() {
    let sys = relaying();
    let opener = Recorder::default();
    let host = Host {
        sys: Some(&sys),
        viewer: Some(&opener),
        ..Host::default()
    };
    host.logs_in_after(1);
    let (screened, _) = screen_with(&host, &sys, &opener);
    let screened = screened.unwrap();
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        *host.opened_at_arm.lock().unwrap(),
        Some((false, false)),
        "neither the relay nor the viewer had opened when the arm came"
    );
    let seen = host.seen();
    let arm = arm_cmd(&admin()).argv.join(" ");
    let at = seen.iter().position(|s| *s == arm).unwrap();
    assert!(seen[at - 1].ends_with("print gui/501"), "{seen:?}");
    assert_eq!(
        arm_cmd(&admin()).argv,
        [
            "/Library/Management/aterm/bin/aterm",
            "machines",
            "agent",
            "screen-watch",
            "arm",
            "--uid",
            "501",
            "--user",
            "admin"
        ]
    );
    assert_eq!(arm_cmd(&admin()).user, None, "root's");
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
    assert_eq!(sys.calls().last().map(String::as_str), Some("relay closed"));
    drop(screened.stops);
    assert_eq!(sys.calls(), relay_calls());
}

#[test]
fn a_watch_that_is_not_armed_relays_nothing_and_opens_no_viewer() {
    let old_agent = "refused: `aterm machines agent screen-watch arm --uid 501 --user admin` \
                     is not an agent verb (ssh, hello, put, apply, run-job, attach, put-file, \
                     install, lock-screen, version)";
    for (answer, why) in [
        (
            Ok(Ran {
                status: Some(1),
                stdout: b"not armed: account admin is uid 501, not 502\n".to_vec(),
                stderr: Vec::new(),
            }),
            "account admin is uid 501, not 502".to_owned(),
        ),
        (
            Ok(Ran {
                status: Some(2),
                stdout: format!("{old_agent}\n").into_bytes(),
                stderr: Vec::new(),
            }),
            format!("exit 2: {old_agent}"),
        ),
        (
            Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
            "the request did not run: ssh: connect to host lab-07.local port 22: Operation \
             timed out"
                .to_owned(),
        ),
    ] {
        let sys = relaying();
        let opener = Recorder::default();
        let host = Host {
            sys: Some(&sys),
            arm: Mutex::new(vec![answer]),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, _) = screen_with(&host, &sys, &opener);
        assert_eq!(
            screened.unwrap_err(),
            format!(
                "the screen watch on lab-07.local was not armed ({why}), so no lock could \
                 be promised if aterm is killed or loses the Mac; nothing was relayed"
            )
        );
        assert_eq!(
            sys.calls(),
            [relay_cmd(&paths(), &target()).unwrap().display()]
        );
        assert!(opener.0.lock().unwrap().is_empty());
        assert_eq!(lock_and_watch(&host), ["arm"]);
    }
}

#[test]
fn a_disarm_that_fails_is_one_line_and_changes_nothing_else() {
    for (answer, line) in [
        (
            Err("ssh: connect to host lab-07.local port 22: Operation timed out".to_owned()),
            "the host's screen watch was not disarmed (the request did not run: ssh: connect \
             to host lab-07.local port 22: Operation timed out): once no request has reached \
             the host for 60s it locks admin's session if there is one, which is harmless",
        ),
        (
            Ok(Ran {
                status: Some(1),
                stdout: b"not disarmed: Operation not permitted (os error 1)\n".to_vec(),
                stderr: Vec::new(),
            }),
            "the host's screen watch was not disarmed (Operation not permitted (os error \
             1)): once no request has reached the host for 60s it locks admin's session if \
             there is one, which is harmless",
        ),
        (
            Ok(Ran::ok(&format!(
                "disarmed {WATCH} (it had ended: locked admin's session: no signed \
                 request reached the host for 61s)\n"
            ))),
            "the host's screen watch had already ended: locked admin's session: no signed \
             request reached the host for 61s",
        ),
    ] {
        let sys = relaying();
        let host = Host {
            sys: Some(&sys),
            disarm: Mutex::new(vec![answer]),
            ..Host::default()
        };
        host.logs_in_after(1);
        let (screened, said) = screen_fda(&host, &sys);
        assert_eq!(screened.walked[0].done, Done::Granted);
        assert_eq!(screened.session, Session::Locked);
        assert_eq!(screened.session.unlocked_line("lab-07", "admin"), None);
        let watch_lines: Vec<&String> =
            said.iter().filter(|l| l.contains("screen watch")).collect();
        assert_eq!(watch_lines, [line]);
        assert!(said.last().unwrap().contains("screen watch"), "{said:?}");
    }
}

fn opens_after(host: &Host<'_>, n: usize) {
    let mut script: Vec<Result<Ran, String>> = (0..n).map(|_| not_answered()).collect();
    script.push(Ok(Ran::ok("")));
    *host.panes.lock().unwrap() = script;
}

#[test]
fn a_gap_that_may_have_let_the_watch_lock_and_end_arms_it_again() {
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    opens_after(&host, 16);
    *host.disarm.lock().unwrap() = vec![
        Ok(Ran::ok(&format!(
            "disarmed {WATCH} (it had ended: locked admin's session: no signed request \
             reached the host for 60s)\n"
        ))),
        Ok(Ran::ok("disarmed 1800000040-78\n")),
    ];
    *host.arm.lock().unwrap() = vec![
        Ok(Ran::ok(&format!("armed {WATCH}\n"))),
        Ok(Ran::ok("armed 1800000040-78\n")),
    ];
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(screened.walked[0].done, Done::Granted);
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        lock_and_watch(&host),
        [
            "arm",
            "arm",
            &format!("disarm {WATCH}"),
            "lock",
            "disarm 1800000040-78"
        ]
    );
    let seen = host.seen();
    let open = seen
        .iter()
        .rposition(|s| s.contains("/usr/bin/open"))
        .unwrap();
    assert!(seen[open + 1].contains("screen-watch arm"), "{seen:?}");
    let watch_lines: Vec<&String> = said.iter().filter(|l| l.contains("screen watch")).collect();
    assert_eq!(
        watch_lines,
        [
            "the host's screen watch had already ended: locked admin's session: no signed \
          request reached the host for 60s"
        ]
    );
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    opens_after(&host, 14);
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(screened.walked[0].done, Done::Granted);
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
}

#[test]
fn an_arm_again_that_fails_is_said_tried_at_the_next_request_and_not_promised_at_the_end() {
    let refused = || {
        Ok(Ran {
            status: Some(1),
            stdout: b"not armed: /A/state/screen-watch: No space left on device\n".to_vec(),
            stderr: Vec::new(),
        })
    };
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(1);
    opens_after(&host, 16);
    *host.arm.lock().unwrap() = vec![
        Ok(Ran::ok(&format!("armed {WATCH}\n"))),
        refused(),
        Ok(Ran::ok("armed 1800000040-78\n")),
    ];
    let (screened, said) = screen_fda(&host, &sys);
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        lock_and_watch(&host),
        [
            "arm",
            "arm",
            "arm",
            &format!("disarm {WATCH}"),
            "lock",
            "disarm 1800000040-78"
        ]
    );
    let watch_lines: Vec<&String> = said.iter().filter(|l| l.contains("screen watch")).collect();
    assert_eq!(
        watch_lines,
        [
            "the host's screen watch was not armed again (/A/state/screen-watch: No space \
             left on device): if aterm is killed or loses the Mac before the walk's own \
             lock, admin's session stays as it is left; tried again at the next request",
            "the host's screen watch is armed again",
        ]
    );
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        lock: Some(Ok(Ran::exit(1, "sudo: a password is required\n"))),
        ..Host::default()
    };
    host.logs_in_after(1);
    opens_after(&host, 16);
    *host.arm.lock().unwrap() = vec![Ok(Ran::ok(&format!("armed {WATCH}\n"))), refused()];
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(
        screened.session,
        Session::Unlocked {
            why: "exit 1: sudo: a password is required".into(),
            unread: None,
            watched: false,
        }
    );
    let watched = lock_and_watch(&host);
    let disarms: Vec<&String> = watched.iter().filter(|r| r.starts_with("disarm")).collect();
    assert!(disarms.is_empty(), "{watched:?}");
    assert_eq!(
        watched.last().map(String::as_str),
        Some("arm"),
        "{watched:?}"
    );
    assert!(
        !screened
            .session
            .unlocked_line("lab-07", "admin")
            .unwrap()
            .contains("screen watch")
    );
}

#[test]
fn a_reading_after_a_stop_that_did_not_run_does_not_claim_a_login() {
    let mut sys = relaying();
    sys.stop_after = Some(0);
    let host = Host {
        sys: Some(&sys),
        lock: Some(Err(
            "ssh: connect to host lab-07.local port 22: Operation timed out".into(),
        )),
        ..Host::default()
    };
    *host.sessions.lock().unwrap() = vec![
        Ok(Ran::exit(
            125,
            "Could not print domain: 125: Domain does not support specified action\n",
        )),
        Ok(Ran::exit(
            125,
            "Could not print domain: 125: Domain does not support specified action\n",
        )),
        Err("ssh: connect to host lab-07.local port 22: Operation timed out".into()),
    ];
    let (screened, _) = screen_fda(&host, &sys);
    assert_eq!(
        screened.walked[0].done,
        Done::NotGranted("not attempted: stopped by SIGINT".into())
    );
    let line = screened.session.unlocked_line("lab-07", "admin").unwrap();
    assert_eq!(
        line,
        "lab-07: the Mac may be logged in as admin and unlocked at its display: whether \
         admin logged in over Screen Sharing is not known (the last reading: the request \
         did not run: ssh: connect to host lab-07.local port 22: Operation timed out), and \
         the lock after the walk failed (the request did not run: ssh: connect to host \
         lab-07.local port 22: Operation timed out); the host's screen watch locks it once \
         no request has reached the host for 60s"
    );
    assert_eq!(lock_and_watch(&host), ["arm", "lock"]);
}

#[test]
fn a_wait_for_the_login_that_outlasts_the_hang_detector_still_locks_a_session_there_now() {
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(32);
    let (screened, _) = screen_fda(&host, &sys);
    assert!(
        matches!(&screened.walked[0].done, Done::NotGranted(why) if why.starts_with("not attempted: admin has not logged in on the host after 60s")),
        "{:?}",
        screened.walked
    );
    assert_eq!(screened.session, Session::Locked);
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "lock", &format!("disarm {WATCH}")]
    );
    let sys = relaying();
    let host = Host {
        sys: Some(&sys),
        ..Host::default()
    };
    host.logs_in_after(99);
    let (screened, _) = screen_with(&host, &sys, &Recorder::default());
    assert!(
        screened
            .unwrap_err()
            .starts_with("admin has not logged in on the host after 60s")
    );
    assert_eq!(lock_and_watch(&host), ["arm", &format!("disarm {WATCH}")]);
}

#[test]
fn here_never_locks_even_after_waiting_for_the_login() {
    let sys = Fake::default();
    let host = Host::default();
    host.logs_in_after(1);
    screen_sharing_on(&host);
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_OFF));
    host.on(&tcc_aterm(), Ran::ok(ATERM_ROWS_FDA));
    let clock = TestClock::default();
    let flow = Flow {
        sys: &sys,
        remote: &host,
        clock: &clock,
        say: &|_| {},
        hang: Duration::from_secs(60),
    };
    let walked = here(&flow, &admin(), &steps(&["full_disk_access:aterm"]), TTY).unwrap();
    assert_eq!(walked[0].done, Done::Granted);
    assert!(
        host.seen().iter().any(|s| s.ends_with("print gui/501")),
        "the walk read the session"
    );
    assert_eq!(
        lock_and_watch(&host),
        Vec::<String>::new(),
        "{:?}",
        host.seen()
    );
    assert!(sys.calls().is_empty());
}

#[test]
fn a_read_that_needs_a_session_or_a_variable_cannot_travel_as_exec() {
    let user = admin();
    let plain = exec_of(&person::tcc_open_cmd()).unwrap();
    assert_eq!(
        plain.argv,
        [crate::protocol::AGENT_EXE, "machines", "agent", "tcc"]
    );
    assert_eq!(plain.user, None);
    let as_user = exec_of(&Cmd::new("/bin/cat").arg("/x").as_user(&user)).unwrap();
    assert_eq!(as_user.user.as_deref(), Some("admin"));
    assert!(exec_of(&Cmd::new("/usr/bin/open").as_user(&user).in_session()).is_err());
    assert!(exec_of(&Cmd::new("/usr/bin/true").env("A", "b")).is_err());
}

#[test]
fn a_replacement_watch_is_acknowledged_before_the_previous_arm_is_retired() {
    let ordinary = Host::default();
    assert_eq!(arm_watch(&ordinary, &admin()).as_deref(), Ok(WATCH));
    let host = Host {
        arm: Mutex::new(vec![
            Ok(Ran::exit(1, "not armed: replacement refused\n")),
            Ok(Ran::ok("armed 1800000040-78\n")),
        ]),
        ..Host::default()
    };
    let clock = TestClock::default();
    let said = Mutex::new(Vec::new());
    let say = |line: &str| said.lock().unwrap().push(line.to_owned());
    let admin = admin();
    let watched = Watched {
        remote: &host,
        clock: &clock,
        say: &say,
        admin: &admin,
        id: RefCell::new(Some(WATCH.to_owned())),
        ran: Cell::new(Some(clock.elapsed())),
    };
    assert!(watched.exec(&session_cmd(&admin)).is_ok());
    assert_eq!(watched.id().as_deref(), Some(WATCH));
    assert!(lock_and_watch(&host).is_empty());
    clock.pause(REARM);
    assert!(watched.exec(&session_cmd(&admin)).is_ok());
    assert_eq!(watched.id().as_deref(), Some(WATCH));
    assert_eq!(lock_and_watch(&host), ["arm"]);
    assert!(watched.exec(&session_cmd(&admin)).is_ok());
    assert_eq!(watched.id().as_deref(), Some("1800000040-78"));
    assert_eq!(
        lock_and_watch(&host),
        ["arm", "arm", &format!("disarm {WATCH}")]
    );
    let before = host.seen().len();
    assert!(watched.exec(&session_cmd(&admin)).is_ok());
    assert_eq!(host.seen().len(), before + 1);
    assert_eq!(watched.id().as_deref(), Some("1800000040-78"));
}

#[test]
fn a_session_query_that_does_not_exit_normally_is_never_absence() {
    let read = |answer: Result<Ran, String>| {
        let sys = Fake::default();
        let host = Host::default();
        *host.sessions.lock().unwrap() = vec![answer];
        let clock = TestClock::default();
        let flow = Flow {
            sys: &sys,
            remote: &host,
            clock: &clock,
            say: &|_| {},
            hang: Duration::from_secs(60),
        };
        read_session(&flow, &admin())
    };
    assert_eq!(read(Ok(Ran::ok(""))), Ok(Reading::There));
    assert!(read(Err("the transport did not run".into())).is_err());
    for stdout in [
        Vec::new(),
        b"Could not find domain for user gui: 501\n".to_vec(),
    ] {
        assert!(
            read(Ok(Ran {
                status: None,
                stdout,
                stderr: Vec::new()
            }))
            .is_err()
        );
    }
}

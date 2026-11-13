// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// What a reached peer says (2026-11-06's guests: sshd's refusal, and a close by `::1`
/// when the guest's own name resolved to loopback). NEGATIVE CONTROLS: a broker that is not
/// installed, a name that did not resolve, a refused connect, and ssh's close of a proxy
/// that passed nothing (`UNKNOWN port 65535`) are no reach.
#[test]
fn a_dial_reached_only_a_peer() {
    assert!(dial_reached("", "0"));
    for out in [
        "admin@h.local: Permission denied (publickey,password,keyboard-interactive).",
        "Warning: Permanently added 'h.local' (ED25519) to the list of known hosts.",
        "Connection closed by ::1 port 22",
        "Connection reset by 192.168.64.5 port 22",
    ] {
        assert!(dial_reached(out, "255"), "{out}");
    }
    for out in [
        "aterm lan fdpass: the LAN broker is not installed: setup's Mac password installs it",
        "ssh: Could not resolve hostname h.local: nodename nor servname provided, or not known",
        "ssh: connect to host h.local port 22: Connection refused",
        "ssh: connect to host 192.168.64.5 port 22: Operation timed out",
        "Connection closed by UNKNOWN port 65535",
    ] {
        assert!(!dial_reached(out, "255"), "{out}");
    }
}

fn ndjson(ts: &str, message: &str) -> String {
    format!(
        "{{\"timestamp\":\"{ts}\",\"subsystem\":\"com.apple.networkextension\",\"eventMessage\":\"{message}\"}}\n"
    )
}

fn words(at_ms: u64, pid: i32, words: &str) -> Words {
    Words {
        at_ms,
        pid,
        owner: "UserNotificationCenter".into(),
        words: words.into(),
    }
}

const ATERM_ASKS: &str = "AXStaticText \"Allow “aterm” to find devices on local networks?\"";
const AGENT_ASKS: &str =
    "AXStaticText \"Allow “tart-guest-agent” to find devices on local networks?\"";

/// An aterm alert counts; the guest agent's (a lane's own `tart exec` traffic) never does,
/// and each alert counts once however both sources read it.
#[test]
fn only_aterms_own_local_network_alerts_count_and_each_once() {
    let log = format!(
        "{}{}{}{}",
        ndjson(
            "2026-11-05 10:00:01.000000-0800",
            "Showing local network alert for com.aterm.aterm"
        ),
        ndjson(
            "2026-11-05 10:00:01.500000-0800",
            "Showing local network alert for com.aterm.aterm"
        ),
        ndjson(
            "2026-11-05 10:00:02.000000-0800",
            "Showing local network alert for com.cirruslabs.tart-guest-agent"
        ),
        "garbage\n"
    );
    let shown = nehelper_alerts(&log);
    assert_eq!(shown.len(), 3);
    assert_eq!(shown[0].1, "com.aterm.aterm");
    let open = shown[0].0.unwrap() - 1000;
    let read = [words(open + 1200, 300, ATERM_ASKS)];
    let a = count_alerts(&shown, &read, open, Some("aterm"));
    assert_eq!(a.aterm, 1, "one alert, read by both, shown twice: {a:?}");
    assert_eq!(a.aterm_seen.len(), 3);
    assert_eq!(
        a.others,
        ["nehelper com.cirruslabs.tart-guest-agent 2000 ms"]
    );
    // NEGATIVE CONTROL: the guest agent's alert alone, by both sources, counts nothing.
    let agent = nehelper_alerts(&ndjson(
        "2026-11-05 10:00:02.000000-0800",
        "Showing local network alert for com.cirruslabs.tart-guest-agent",
    ));
    let a = count_alerts(&agent, &[words(open + 2100, 301, AGENT_ASKS)], open, None);
    assert_eq!(a.aterm, 0, "{a:?}");
    assert_eq!(a.others.len(), 2);
    // Before the open is not the launch's.
    let a = count_alerts(&shown, &[], shown[2].0.unwrap() + 1, None);
    assert_eq!(a.aterm, 0);
    // A showing at an unreadable time is never counted, and is named apart.
    let untimed = [(None, "com.aterm.aterm".to_owned())];
    let a = count_alerts(&untimed, &[], open, None);
    assert_eq!((a.aterm, a.untimed.len()), (0, 1), "{a:?}");
    assert_eq!(
        nehelper_alerts(&ndjson(
            "not a time",
            "Showing local network alert for com.aterm.aterm"
        )),
        untimed
    );
    assert!(is_aterm_identity("com.aterm.aterm-dev"));
    assert!(!is_aterm_identity("com.atermx"));
}

/// The reader's alerts: words that change around the question are the same alert, the very
/// words again (the window went and came back) a new one, another process a new one.
#[test]
fn the_readers_alerts_are_each_time_the_question_came_up() {
    let read = [
        words(100, 300, "AXStaticText \"Background Items Added\""),
        words(200, 300, ATERM_ASKS),
        words(300, 300, &format!("{ATERM_ASKS}; AXButton \"Allow\"")),
        words(400, 300, ATERM_ASKS),
        words(500, 300, ATERM_ASKS),
        words(600, 301, ATERM_ASKS),
    ];
    let alerts = reader_alerts(&read);
    assert_eq!(
        alerts
            .iter()
            .map(|(at, pid, _)| (*at, *pid))
            .collect::<Vec<_>>(),
        [(200, 300), (500, 300), (600, 301)]
    );
    assert_eq!(count_alerts(&[], &read, 0, None).aterm, 3);
    // An --app bundle with its own name: its alert is aterm's by that name.
    let dev = [words(
        100,
        400,
        "AXStaticText \"Allow “aterm (dev)” to find devices on local networks?\"",
    )];
    assert_eq!(count_alerts(&[], &dev, 0, None).aterm, 0);
    assert_eq!(count_alerts(&[], &dev, 0, Some("aterm (dev)")).aterm, 1);
    assert_eq!(
        app_name("", "aterm (dev)\n").as_deref(),
        Some("aterm (dev)")
    );
    assert_eq!(app_name("Shown", "aterm").as_deref(), Some("Shown"));
    assert_eq!(app_name(" ", ""), None);
    assert!(
        reader_alerts(&read[..1]).is_empty(),
        "no question, no alert"
    );
}

/// A hold of a process under the guest agent is the lane's, even an aterm CLI the lane ran;
/// the app and its children are aterm's.
#[test]
fn a_holds_process_is_placed_by_its_ancestors() {
    let tree = ps_tree(
        "    1     0 /sbin/launchd\n\
         \x20 210     1 /usr/local/bin/tart-guest-agent --run-daemon\n\
         \x20 400   210 /bin/sh -c nc 192.168.64.117 22\n\
         \x20 401   400 /usr/bin/nc 192.168.64.117 22\n\
         \x20 500     1 /Applications/aterm.app/Contents/MacOS/aterm\n\
         \x20 501   500 /usr/bin/dns-sd -B _alab-vault._tcp local.\n\
         \x20 600   210 /Applications/aterm.app/Contents/MacOS/aterm ctl status\n\
         \x20 700     1 /usr/libexec/rapportd\n",
    );
    assert_eq!(tree.get(&501).map(|(p, _)| *p), Some(500));
    assert_eq!(whose(&tree, 500), Whose::Aterm);
    assert_eq!(whose(&tree, 501), Whose::Aterm);
    assert_eq!(whose(&tree, 401), Whose::Lane);
    assert_eq!(
        whose(&tree, 600),
        Whose::Lane,
        "the lane's tart exec of aterm"
    );
    assert_eq!(whose(&tree, 700), Whose::Other);
    assert_eq!(whose(&tree, 999), Whose::Gone);
}

#[test]
fn launchd_and_lan_status_read_the_broker() {
    let print = "system/systems.alab.aterm-lan = {\n\
                 \tactive count = 1\n\
                 \tpath = /Library/LaunchDaemons/systems.alab.aterm-lan.plist\n\
                 \tstate = running\n\
                 \n\
                 \tprogram = /Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm\n\
                 \targuments = {\n\
                 \t\t/Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm\n\
                 \t\tlan\n\
                 \t\tserve\n\
                 \t}\n\
                 \tendpoints = {\n\
                 \t\tstate = active\n\
                 \t}\n\
                 \truns = 2\n\
                 \tpid = 812\n\
                 \tlast exit code = 0\n\
                 }\n";
    let d = read_launchctl(print).unwrap();
    assert_eq!(
        d.state, "running",
        "a nested block's state is not the job's"
    );
    assert_eq!(d.pid, Some(812));
    assert_eq!(d.runs, Some(2));
    assert!(d.program.ends_with("lan/aterm.app/Contents/MacOS/aterm"));
    assert_eq!(
        broker_exits(Some(&d)),
        1,
        "one adoption restart ended a broker"
    );
    assert_eq!(
        read_launchctl("Could not find service \"systems.alab.aterm-lan\" in domain for system\n"),
        None
    );
    assert_eq!(broker_exits(None), 0);
    let stopped = Daemon {
        state: "not running".into(),
        runs: Some(1),
        ..Daemon::default()
    };
    assert_eq!(broker_exits(Some(&stopped)), 1);

    assert_eq!(
        read_lan_status(
            "broker protocol=1 build=0.155.0+gabc1234 socket=/var/run/systems.alab.aterm-lan/broker.sock\n"
        ),
        LanStatus::Serving {
            protocol: 1,
            build: "0.155.0+gabc1234".into(),
            socket: "/var/run/systems.alab.aterm-lan/broker.sock".into(),
        }
    );
    // What a build with no `lan` verb prints under `tart exec` (no terminal on stdin).
    assert_eq!(
        read_lan_status("aterm: unknown command 'lan' (try --help)\n"),
        LanStatus::NoVerb
    );
    assert_eq!(
        read_lan_status("aterm: unknown command lan (try --help)\n"),
        LanStatus::NoVerb
    );
    assert!(matches!(
        read_lan_status("no broker: setup's elevation has not installed it\n"),
        LanStatus::Not(why) if why.contains("not installed")
    ));
    // The direct path's line (crates/aterm/src/lan_verb.rs), and the plain one beside it.
    assert_eq!(
        read_lan_status(
            "the LAN broker is not installed: until setup's Mac password installs it, aterm \
             makes its own LAN connections as macOS allowed com.aterm.aterm (team A66A9P66Z7) \
             on the local network (socket=/var/run/systems.alab.aterm-lan/broker.sock)\n"
        ),
        LanStatus::Direct(
            "macOS allowed com.aterm.aterm (team A66A9P66Z7) on the local network".into()
        )
    );
    assert!(matches!(
        read_lan_status(
            "the LAN broker is not installed: setup's Mac password installs it \
             (`sudo aterm machines here apply --plain`) (socket=/var/run/systems.alab.aterm-lan/broker.sock)\n"
        ),
        LanStatus::Not(_)
    ));
}

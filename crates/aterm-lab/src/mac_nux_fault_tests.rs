// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn look(at_ms: u64, text: &str) -> Look {
    Look {
        at_ms,
        text: (text != "absent").then(|| text.to_owned()),
    }
}

#[test]
fn a_launch_after_a_kill_names_its_first_stage_and_any_done_stage_it_re_ran() {
    let launch = 10_000;
    let looks = [
        look(10_100, "absent"),
        look(11_000, r#"{"state":"pending","done":1,"running":2}"#),
        look(12_000, r#"{"state":"pending","done":3,"running":4}"#),
        look(13_000, r#"{"state":"completed","done":63}"#),
    ];
    let r = resume(&looks, 1, launch);
    assert_eq!(r.first, Some((1000, 2)));
    assert_eq!((r.reran, r.ran), (0, 6));
    assert_eq!(
        resume_text(1, &r),
        "after Workstation's kill: ran Spotlight first, 1000 ms after its launch; re-ran no done stage"
    );
    // NEGATIVE CONTROL: a launch that ran Workstation again after it was done names it.
    let again = [
        look(10_500, r#"{"state":"pending","done":1,"running":1}"#),
        look(11_500, r#"{"state":"pending","done":1,"running":2}"#),
    ];
    let r = resume(&again, 1, launch);
    assert_eq!(r.reran, 1);
    assert!(
        resume_text(2, &r).ends_with("RE-RAN done Workstation"),
        "{}",
        resume_text(2, &r)
    );
    let none = resume(&[look(10_100, "absent")], 0, launch);
    assert_eq!((none.first, none.ran), (None, 0));
    assert!(resume_text(4, &none).contains("ran no stage"));
    assert_eq!(bits_text(0), "none");
    assert_eq!(bits_text(1 | 8 | 32), "Workstation+Secrets+Accounts");
    assert_eq!(
        last_record(&looks).map(|r| r.state),
        Some("completed".to_owned())
    );
    assert_eq!(last_record(&[look(1, "absent")]), None);
}

#[test]
fn the_first_change_after_the_network_returns_is_timed_from_its_return() {
    let looks = [
        look(1_000, r#"{"state":"pending","done":0,"running":1}"#),
        look(40_000, r#"{"state":"pending","done":0,"running":1}"#),
        look(41_250, r#"{"state":"pending","done":1,"running":2}"#),
    ];
    // The look at 40 000 says what the one before the return said: no progress yet.
    assert_eq!(first_change_after(&looks, 31_000), Some(10_250));
    assert_eq!(first_change_after(&looks, 50_000), None);
    // With no look before the return, the first look after it is the change.
    assert_eq!(first_change_after(&looks, 0), Some(1_000));
}

#[test]
fn the_shaping_pipe_route_processes_presence_and_dock_read_from_their_tools() {
    let show = "00001:   1.000 Mbit/s  150 ms   50 sl.plr 0.010000 2 queues (64 buckets) droptail\n    \
                mask: 0x00 0x00000000/0x0000 -> 0x00000000/0x0000\n\
                BKT Prot ___Source IP/port____ ____Dest. IP/port____ Tot_pkt/bytes Pkt/Byte Drp\n  \
                0 tcp     192.168.64.5/49234   17.253.144.10/443    1093  1412245  0    0   12\n  \
                1 udp     192.168.64.5/5353    224.0.0.251/5353       7     420  0    0   0\n";
    assert_eq!(pipe_counts(show), Some((1100, 12)));
    // NEGATIVE CONTROL: a pipe that carried nothing shows no queue, never 0 packets.
    assert_eq!(pipe_counts("00001:   1.000 Mbit/s  150 ms\n"), None);

    let route =
        "   route to: default\ndestination: default\n    gateway: 192.168.64.1\n  interface: en0\n";
    assert_eq!(default_interface(route), Some("en0"));
    assert_eq!(
        default_interface("route: writing to routing socket: not in table\n"),
        None
    );

    let ps = "  412 /Applications/aterm.app/Contents/MacOS/aterm\n  \
              413 /Applications/aterm.app/Contents/MacOS/aterm-helper --x\n  \
              500 /Volumes/aterm/aterm.app/Contents/MacOS/aterm --restore\n  \
              501 /private/var/folders/x/T/AppTranslocation/AB12/d/aterm.app/Contents/MacOS/aterm\n  \
              9 /usr/bin/grep aterm\n";
    let runs = app_runs(ps);
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(
        runs.iter().map(|b| posture(b)).collect::<Vec<_>>(),
        ["installed", "mounted image", "translocated"]
    );
    assert_eq!(posture("/tmp/aterm-nux/aterm.app"), "elsewhere");

    let away = "Workstation  deferred  needs a person at this Mac's console (a coding agent started this): run aterm init there\n";
    assert_eq!(
        presence_read(away).as_deref(),
        Some("a coding agent started this")
    );
    assert_eq!(presence_read("Workstation ok\n"), None);
    assert_eq!(
        presence_cell(away),
        Value::Text("away: a coding agent started this".into())
    );

    assert_eq!(defaults_bool("1\n"), Some(true));
    assert_eq!(defaults_bool("0"), Some(false));
    assert_eq!(defaults_bool(""), None);
}

#[test]
fn the_shaped_and_mdns_filters_keep_the_guard_and_the_agent_runs_as_claude() {
    let rules = lossy_rules();
    assert!(rules.ends_with(PF_GUARD), "{rules}");
    assert!(
        rules
            .lines()
            .take(2)
            .all(|l| l.contains("pipe 1") && l.contains("127.0.0.0/8"))
    );
    // The agent's perl names itself `claude` before it runs the command as its child.
    assert!(AS_AGENT.starts_with(r#"exec { "/usr/bin/perl" } "claude""#));
    assert!(AS_AGENT.contains("system { $ARGV[0] } @ARGV"));
    assert!(KILL_PUBLISH.contains("token"));
    assert_eq!(STAGES.count_ones(), 6);
}

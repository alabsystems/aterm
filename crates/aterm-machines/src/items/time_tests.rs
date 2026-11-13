// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::{Fake, Stat};

fn ctx() -> Ctx {
    Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

const NOISE: &str = "2026-10-16 23:07:26.027 systemsetup[2456:28578] ### Error:-99 File:/AppleInternal/Library/BuildRoots/4~CV23ugBcSuPoqTCv2d4ySbycsdbOG37Qm_PyGCo/Library/Caches/com.apple.xbs/TemporaryDirectory.ALqQnw/Sources/Admin/InternetServices.m Line:395";

fn set_ok(stdout: &str) -> Ran {
    Ran {
        status: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: NOISE.as_bytes().to_vec(),
    }
}

fn zone_file() -> Stat {
    Stat {
        uid: 0,
        gid: 0,
        mode: 0o644,
        dir: false,
        symlink: false,
        len: 2_852,
    }
}

#[test]
fn the_zone_is_what_follows_the_zoneinfo_directory_in_the_link() {
    assert_eq!(
        zone_of_link("/var/db/timezone/zoneinfo/GMT\n").as_deref(),
        Some("GMT")
    );
    assert_eq!(
        zone_of_link("/var/db/timezone/zoneinfo/America/Los_Angeles").as_deref(),
        Some("America/Los_Angeles")
    );
    assert_eq!(
        zone_of_link("/usr/share/zoneinfo.default/Europe/London").as_deref(),
        Some("Europe/London")
    );
    assert_eq!(zone_of_link("/var/db/timezone/zoneinfo/"), None);
    assert_eq!(zone_of_link("/etc/elsewhere"), None);
}

#[test]
fn setting_the_zone_runs_systemsetup_and_reads_back_the_link() {
    let sys = Fake::root();
    sys.on(
        "/usr/bin/readlink /etc/localtime",
        Ran::ok("/var/db/timezone/zoneinfo/GMT\n"),
    )
    .on(
        "/usr/bin/readlink /etc/localtime",
        Ran::ok("/var/db/timezone/zoneinfo/America/Los_Angeles\n"),
    )
    .on(
        "/usr/sbin/systemsetup -settimezone America/Los_Angeles",
        set_ok("Set TimeZone: America/Los_Angeles\n"),
    )
    .stat_of("/var/db/timezone/zoneinfo/America/Los_Angeles", zone_file());
    let t = Table {
        zone: Some("America/Los_Angeles".into()),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
    assert_eq!(rows[0].from.as_deref(), Some("GMT"));
    assert_eq!(
        rows[0].revert.as_deref(),
        Some("/usr/sbin/systemsetup -settimezone GMT")
    );
}

#[test]
fn a_zone_this_mac_lacks_is_caught_by_the_plan_not_by_the_apply() {
    let sys = Fake::root();
    sys.on(
        "/usr/bin/readlink /etc/localtime",
        Ran::ok("/var/db/timezone/zoneinfo/GMT\n"),
    );
    let t = Table {
        zone: Some("Mars/Base".into()),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Unknown);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("no time zone Mars/Base")
    );
    assert!(sys.calls().iter().all(|c| !c.contains("-settimezone")));
}

#[test]
fn a_zone_or_server_that_is_not_a_name_is_refused_when_the_baseline_expands() {
    for zone in ["", "/etc/passwd", "../x", "America//X", "Amer ica/X", "-x"] {
        let t = Table {
            zone: Some(zone.into()),
            ..Table::default()
        };
        assert!(expand(&t, &ctx()).is_err(), "{zone:?}");
    }
    for server in ["", "-x", "time apple com", "a;b"] {
        let t = Table {
            server: Some(server.into()),
            ..Table::default()
        };
        assert!(expand(&t, &ctx()).is_err(), "{server:?}");
    }
    let t = Table {
        zone: Some("Etc/GMT+8".into()),
        server: Some("time.apple.com".into()),
        network_time: Some(true),
    };
    assert_eq!(expand(&t, &ctx()).unwrap().len(), 3);
}

#[test]
fn network_time_and_its_server_read_systemsetups_own_lines() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/systemsetup -getusingnetworktime",
        Ran::ok("Network Time: On\n"),
    )
    .on(
        "/usr/sbin/systemsetup -getnetworktimeserver",
        Ran::ok("Network Time Server: time.apple.com\n"),
    );
    let t = Table {
        network_time: Some(true),
        server: Some("Time.Apple.com".into()),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:#?}");
    assert_eq!(rows[0].to.as_deref(), Some("on"));
}

#[test]
fn turning_network_time_off_reverts_by_turning_it_back_on() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/systemsetup -getusingnetworktime",
        Ran::ok("Network Time: On\n"),
    )
    .on(
        "/usr/sbin/systemsetup -getusingnetworktime",
        Ran::ok("Network Time: Off\n"),
    )
    .on(
        "/usr/sbin/systemsetup -setusingnetworktime off",
        set_ok("setUsingNetworkTime: Off\n"),
    );
    let t = Table {
        network_time: Some(false),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed);
    assert_eq!(
        rows[0].revert.as_deref(),
        Some("/usr/sbin/systemsetup -setusingnetworktime on")
    );
}

#[test]
fn systemsetup_refusing_a_non_root_caller_with_exit_0_is_not_read_as_an_answer() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/systemsetup -getusingnetworktime",
        Ran::ok("You need administrator access to run this tool... exiting!\n"),
    );
    let t = Table {
        network_time: Some(true),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Unknown);
    assert!(rows[0].why.as_deref().unwrap().contains("needs root"));
}

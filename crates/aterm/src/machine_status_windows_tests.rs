// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm status` on Windows, over readings a test writes: each area's line, that an area
//! Windows lacks is one `off` line with no step, and the whole report as it prints.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aterm_agent::harness::disk::{Config, Thresholds};
use aterm_sysprobe::VolumeReading;

use super::super::{
    Agents, Area, Clt, Level, Who, agents_area_at, render_json, render_text, tools_area,
};
use super::{
    DiskRead, Instances, OtherElevation, about_area, disk_area, instances_area, publishes,
    repos_area, unported_areas,
};

const GIB: u64 = 1 << 30;

/// Lines a test writes: warn under 20 GiB, free build caches under 10, an emergency under 1.
const LINES: Thresholds = Thresholds {
    warn: 20 * GIB,
    floor: 10 * GIB,
    target: 15 * GIB,
    emergency: GIB,
    files_emergency: 0,
};

fn drive(free: u64, total: u64) -> DiskRead {
    drive_held_to(free, total, LINES)
}

fn drive_held_to(free: u64, total: u64, lines: Thresholds) -> DiskRead {
    DiskRead {
        volume: VolumeReading {
            total,
            free,
            reserved: 0,
            mount: PathBuf::from(r"C:\"),
            fs: "NTFS".into(),
        },
        lines,
    }
}

fn found(running: &[u32], stale: usize) -> Result<Instances, String> {
    Ok(Instances {
        running: running.to_vec(),
        stale,
        other: None,
    })
}

#[test]
fn each_unix_only_area_is_one_off_line_with_no_step() {
    let areas: Vec<Area> = unported_areas().collect();
    let names: Vec<&str> = areas.iter().map(|area| area.name).collect();
    assert_eq!(names, ["setup", "membership", "vault", "fleet", "name"]);
    for area in &areas {
        assert_eq!(area.level, Level::Off, "{}", area.name);
        assert_eq!(area.next, None, "{}", area.name);
        assert!(
            area.says.starts_with("not available on Windows yet: "),
            "{}: {}",
            area.name,
            area.says
        );
    }
}

#[test]
fn the_build_line_names_its_version_profile_and_path() {
    let area = about_area(
        "0.199.0+g1a37de31a3",
        "release",
        Some(Path::new(r"C:\Programs\aterm\aterm.exe")),
    );
    assert_eq!(area.level, Level::Ok);
    assert_eq!(
        area.says,
        r"0.199.0+g1a37de31a3 (release build) at C:\Programs\aterm\aterm.exe"
    );
    let pathless = about_area("0.199.0+g1a37de31a3", "debug", None);
    assert_eq!(pathless.level, Level::Ok);
    assert_eq!(
        pathless.says,
        "0.199.0+g1a37de31a3 (debug build); its own path did not read"
    );
}

#[test]
fn repos_says_the_clone_is_absent_and_where_git_is() {
    let found = repos_area(Some(Path::new(r"C:\Program Files\Git\cmd\git.exe")));
    assert_eq!(found.level, Level::Off);
    assert_eq!(found.next, None);
    assert!(found.says.contains("not available on Windows yet"));
    assert!(
        found
            .says
            .ends_with(r"git is C:\Program Files\Git\cmd\git.exe")
    );
    let missing = repos_area(None);
    assert_eq!(missing.level, Level::Off);
    assert!(missing.says.ends_with("no git on PATH"));
}

#[test]
fn disk_rounds_down_to_a_tenth_of_a_gigabyte() {
    let area = disk_area(
        Path::new(r"C:\Users\someone"),
        Some(&drive(123 * GIB + GIB / 2 + GIB / 20, 931 * GIB)),
    );
    assert_eq!(area.level, Level::Ok);
    assert_eq!(area.says, r"C:\ (NTFS): 123.5 GB free of 931.0 GB");
    let small = disk_area(Path::new(r"C:\"), Some(&drive(GIB / 20, GIB)));
    assert!(
        small.says.starts_with(r"C:\ (NTFS): 0.0 GB free of 1.0 GB"),
        "{}",
        small.says
    );
}

/// `ok` is a volume with room. Under the warning line, the floor or the emergency line the area
/// is `due` with an agent's step and says which line; the same readings held to no line at all
/// (the negative control) are `ok`.
#[test]
fn a_disk_under_one_of_its_lines_is_due_and_says_which() {
    let at = Path::new(r"C:\Users\someone");
    let total = 500 * GIB;
    let roomy = disk_area(at, Some(&drive(20 * GIB, total)));
    assert_eq!((roomy.level, roomy.next), (Level::Ok, None), "at the line");
    for (free, says) in [
        (
            20 * GIB - 1,
            r"C:\ (NTFS): 19.9 GB free of 500.0 GB; low: under the 20.0 GB warning line",
        ),
        (
            10 * GIB - 1,
            r"C:\ (NTFS): 9.9 GB free of 500.0 GB; short: under the 10.0 GB floor aterm frees build caches at",
        ),
        (
            50 << 20,
            r"C:\ (NTFS): 0.0 GB free of 500.0 GB; emergency: under 1.0 GB, writes are about to fail",
        ),
    ] {
        let area = disk_area(at, Some(&drive(free, total)));
        assert_eq!(area.level, Level::Due, "{says}");
        assert_eq!(area.says, says);
        let step = area.next.expect("a step");
        assert_eq!(step.who, Who::Agent);
        assert!(step.act.contains("await sys 'free>=room'"), "{}", step.act);
        let unheld = Thresholds {
            warn: 0,
            floor: 0,
            target: 0,
            emergency: 0,
            files_emergency: 0,
        };
        let control = disk_area(at, Some(&drive_held_to(free, total, unheld)));
        assert_eq!((control.level, control.next), (Level::Ok, None), "{says}");
    }
}

/// The lines the shipped `[disk]` knobs put on a volume: a full one is never `ok`, an empty
/// one is.
#[test]
fn the_shipped_knobs_call_a_full_disk_due_and_an_empty_one_ok() {
    let at = Path::new(r"C:\Users\someone");
    let total = 931 * GIB;
    let lines = Config::default().thresholds(Some(total));
    let full = disk_area(at, Some(&drive_held_to(50 << 20, total, lines)));
    assert_eq!(full.level, Level::Due, "{}", full.says);
    assert!(full.next.is_some());
    let empty = disk_area(at, Some(&drive_held_to(total, total, lines)));
    assert_eq!(
        (empty.level, empty.next),
        (Level::Ok, None),
        "{}",
        empty.says
    );
}

#[test]
fn an_unread_disk_is_unknown_with_an_agents_step() {
    let area = disk_area(Path::new(r"Z:\share"), None);
    assert_eq!(area.level, Level::Unknown);
    assert!(area.says.contains(r"Z:\share"));
    assert_eq!(area.next.map(|step| step.who), Some(Who::Agent));
}

#[test]
fn instances_lists_each_running_pid_or_says_none() {
    let none = instances_area(&found(&[], 0));
    assert_eq!(none.level, Level::Ok);
    assert_eq!(none.says, "none has published a control socket here");
    let two = instances_area(&found(&[4100, 5200], 0));
    assert_eq!(two.level, Level::Ok);
    assert_eq!(
        two.says,
        "2 running with a published control socket (pid 4100, 5200); `aterm ctl ls` lists the \
         sessions of each that answers"
    );
    assert_eq!(two.next, None);
    let unread = instances_area(&Err("control-socket directory cannot be read".into()));
    assert_eq!(unread.level, Level::Unknown);
    let step = unread.next.unwrap();
    assert_eq!(step.who, Who::Agent);
    assert!(step.act.contains("aterm ctl ls"));
}

/// A socket file left by a killed instance is counted, never listed as an instance by its pid.
#[test]
fn a_socket_file_of_a_process_that_is_gone_is_counted_as_stale() {
    let only_stale = instances_area(&found(&[], 2));
    assert_eq!(only_stale.level, Level::Ok);
    assert_eq!(
        only_stale.says,
        "none running; 2 socket files of processes that are gone"
    );
    let mixed = instances_area(&found(&[4100], 1));
    assert_eq!(
        mixed.says,
        "1 running with a published control socket (pid 4100), and 1 socket file of a process \
         that is gone; `aterm ctl ls` lists the sessions of each that answers"
    );
}

/// A pid is its socket file's publisher only while its process runs and did not start after
/// the file was written (a later holder of the pid did).
#[test]
fn a_pid_publishes_its_socket_only_while_it_runs_and_predates_the_file() {
    let written = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let before = written - Duration::from_secs(30);
    let after = written + Duration::from_secs(30);
    assert!(publishes(true, Some(before), Some(written)));
    assert!(!publishes(false, Some(before), Some(written)), "it is gone");
    assert!(
        !publishes(true, Some(after), Some(written)),
        "a process that took the pid later"
    );
    assert!(
        publishes(true, Some(written + Duration::from_secs(1)), Some(written)),
        "within the clocks' grain"
    );
    assert!(publishes(true, None, Some(written)), "an unread start");
    assert!(publishes(true, Some(after), None), "an unread file time");
    assert!(!publishes(false, None, None));
}

/// This test's own process runs and is no publisher of a socket file written before it began;
/// a pid nothing holds publishes nothing.
#[test]
fn this_process_is_read_as_running_and_born_before_now() {
    let me = std::process::id();
    let now = SystemTime::now();
    assert!(aterm_uds::process::pid_alive(me));
    let born = super::born(me).expect("this process's start reads");
    assert!(born <= now, "{born:?}");
    assert!(publishes(true, Some(born), Some(now)));
    assert!(
        !publishes(true, Some(born), born.checked_sub(Duration::from_secs(60))),
        "a file written a minute before this process began is another's"
    );
}

/// Each elevation lists its own control directory; the line says the other was not read.
#[test]
fn the_other_elevations_directory_is_named_as_not_read() {
    let dir = PathBuf::from(r"C:\Users\someone\AppData\Local\Temp\aterm");
    let elevated = instances_area(&Ok(Instances {
        other: Some(OtherElevation {
            elevated: true,
            dir: dir.clone(),
        }),
        ..Instances::default()
    }));
    assert_eq!(
        elevated.says,
        r"none has published a control socket here; this shell is elevated, so only elevated instances were read: unelevated ones publish in C:\Users\someone\AppData\Local\Temp\aterm"
    );
    let plain = instances_area(&Ok(Instances {
        running: vec![4100],
        other: Some(OtherElevation {
            elevated: false,
            dir: dir.with_file_name("aterm-elevated"),
        }),
        ..Instances::default()
    }));
    assert!(
        plain.says.ends_with(
            r"; elevated instances publish in C:\Users\someone\AppData\Local\Temp\aterm-elevated, which only an elevated shell can list"
        ),
        "{}",
        plain.says
    );
    let alone = instances_area(&found(&[4100], 0));
    assert!(!alone.says.contains("elevated"), "{}", alone.says);
}

#[test]
fn a_sign_in_only_a_person_makes_names_this_pc() {
    let agents = Agents {
        asked: vec![("claude", "signed-out")],
        by_person: vec!["claude"],
        person_steps: vec!["`claude` then /login".into()],
        ..Agents::default()
    };
    let area = agents_area_at(&agents, "this PC");
    assert_eq!(area.level, Level::Due);
    let step = area.next.unwrap();
    assert_eq!(step.who, Who::Person);
    assert_eq!(
        step.act,
        "sign claude in once at this PC (`claude` then /login)"
    );
}

/// The whole report over written readings, as `aterm status` prints it and as `--json` gives it.
#[test]
fn the_report_reads_top_to_bottom() {
    let mut areas = vec![about_area(
        "0.199.0+g1a37de31a3",
        "release",
        Some(Path::new(r"C:\Programs\aterm\aterm.exe")),
    )];
    areas.extend(unported_areas());
    areas.push(tools_area(Clt::NotHere, &Ok(None)));
    areas.push(repos_area(None));
    areas.push(agents_area_at(&Agents::default(), "this PC"));
    areas.push(disk_area(
        Path::new(r"C:\Users\someone"),
        Some(&drive(100 * GIB, 500 * GIB)),
    ));
    areas.push(instances_area(&found(&[4100], 0)));

    let names: Vec<&str> = areas.iter().map(|area| area.name).collect();
    assert_eq!(
        names,
        [
            "aterm",
            "setup",
            "membership",
            "vault",
            "fleet",
            "name",
            "tools",
            "repos",
            "agents",
            "disk",
            "instances"
        ]
    );
    for area in &areas {
        assert_eq!(
            area.next.is_some(),
            matches!(area.level, Level::Working | Level::Due | Level::Unknown),
            "{}",
            area.name
        );
    }

    let text = render_text(&areas);
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("aterm status: this machine, read now (nothing was changed)")
    );
    assert_eq!(
        lines.next(),
        Some(
            r"aterm       ok       0.199.0+g1a37de31a3 (release build) at C:\Programs\aterm\aterm.exe"
        )
    );
    assert_eq!(
        lines.next(),
        Some(
            "setup       off      not available on Windows yet: the first-launch setup is macOS's"
        )
    );
    assert!(text.contains("\ntools       working  no toolset store yet\n"));
    assert!(text.contains(r"disk        ok       C:\ (NTFS): 100.0 GB free of 500.0 GB"));
    assert!(
        text.contains(
            "\ninstances   ok       1 running with a published control socket (pid 4100)"
        )
    );

    let json = render_json(&areas);
    assert!(json.contains(r#""schema":1"#), "{json}");
    for name in names {
        assert!(json.contains(&format!(r#""area":"{name}""#)), "{name}");
    }
}

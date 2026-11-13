// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

const DIR: &str = r"C:\Users\someone\AppData\Local\aterm\dist\0123456789ab";

fn pointer_text(build: u64, smoke: &str) -> String {
    format!(
        r#"{{"schema":1,"version":"0.200.0","commit":"0123456789ab","commit_full":"0123456789abcdef0123456789abcdef01234567","build_number":{build},"dir":"C:\\Users\\someone\\AppData\\Local\\aterm\\dist\\0123456789ab","staged_at":"2026-11-11T10:00:00Z","smoke":"{smoke}"}}"#
    )
}

fn staged(build: u64, smoke: Smoke) -> StagedBuild {
    StagedBuild {
        version: "0.200.0".to_string(),
        commit: "0123456789ab".to_string(),
        build_number: build,
        dir: PathBuf::from(DIR),
        staged_at: "2026-11-11T10:00:00Z".to_string(),
        smoke,
        dirty: false,
    }
}

/// A running copy that is not the staged commit.
const RUNNING: Running<'static> = Running {
    version: "0.199.0",
    commit: "1a37de31a34a",
    build_number: 100,
};

/// A running copy of the staged commit (`0123456789ab`) at `build`.
const fn running_staged_commit(build: u64) -> Running<'static> {
    Running {
        version: "0.200.0",
        commit: "0123456789ab",
        build_number: build,
    }
}

/// The record `build.ps1` writes for the staged commit at `build`.
fn installed(build: u64, dirty: bool) -> InstalledBuild {
    InstalledBuild {
        version: "0.200.0".to_string(),
        commit: "0123456789ab".to_string(),
        build_number: build,
        dirty,
    }
}

fn build_json(build: u64, dirty: bool) -> String {
    format!(
        r#"{{"schema":1,"version":"0.200.0","commit":"0123456789ab","commit_full":"0123456789abcdef0123456789abcdef01234567","build_number":{build},"arch":"x64","toolchain":"stock 1.97.1","dirty":{dirty},"built_at":"2026-11-11T23:23:04Z"}}"#
    )
}

fn here(standing: Standing) -> Here {
    Here {
        standing,
        pointer: PathBuf::from(r"C:\scratch\aterm\dist\latest.json"),
        installed: None,
        blocked: None,
    }
}

/// The control wire's reader of a quoted field (the unescaping `aterm-lab` and the macOS
/// status readers apply): `None` for a field that does not close or escapes what it cannot.
fn quoted_field(line: &str, key: &str) -> Option<String> {
    let open = format!(" {key}=\"");
    let start = line.find(&open)? + open.len();
    let mut out = String::new();
    let mut chars = line.get(start..)?.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

#[test]
fn a_schema_1_pointer_reads_whole() {
    assert_eq!(
        parse_pointer(&pointer_text(101, "pass")),
        Ok(staged(101, Smoke::Pass))
    );
    assert_eq!(
        parse_pointer(&pointer_text(101, "not-run")).map(|s| s.smoke),
        Ok(Smoke::NotRun)
    );
    // Windows PowerShell 5.1 writes a byte-order mark; a field this build does not know is
    // not an error; a missing smoke verdict is "not-run".
    let text = "\u{feff}{\"schema\":1,\"version\":\"0.200.0\",\"commit\":\"0123456789AB\",\
                \"build_number\":7,\"dir\":\"D:/dist/x\",\"later_field\":[1,2]}";
    let read = parse_pointer(text).expect("a pointer with a BOM and an unknown field");
    assert_eq!(
        read.commit, "0123456789ab",
        "the commit is named in lower case"
    );
    assert_eq!(read.smoke, Smoke::NotRun);
    assert_eq!(read.staged_at, "");
    assert_eq!(read.build_number, 7);
    assert!(!read.dirty, "a pointer that says nothing is a clean build");
    let dirty =
        pointer_text(101, "pass").replacen("\"schema\":1", "\"schema\":1,\"dirty\":true", 1);
    assert_eq!(parse_pointer(&dirty).map(|s| s.dirty), Ok(true));
}

#[test]
fn a_pointer_this_build_cannot_use_says_which_field() {
    let good = pointer_text(101, "pass");
    assert!(parse_pointer(&good).is_ok(), "the control: {good}");
    for (from, to, want) in [
        ("\"schema\":1", "\"schema\":2", "schema 2"),
        ("\"schema\":1,", "", "no schema"),
        ("\"version\":\"0.200.0\"", "\"version\":\"\"", "version"),
        (
            "\"version\":\"0.200.0\"",
            "\"version\":\"0.2 0\"",
            "version",
        ),
        (
            "\"commit\":\"0123456789ab\"",
            "\"commit\":\"0123456\"",
            "commit",
        ),
        (
            "\"commit\":\"0123456789ab\"",
            "\"commit\":\"0123456789zz\"",
            "commit",
        ),
        ("\"build_number\":101", "\"build_number\":0", "build_number"),
        ("\"build_number\":101", "\"build_number\":\"101\"", "JSON"),
        ("\"build_number\":101,", "", "build_number"),
        ("C:\\\\Users", "Users", "absolute"),
        ("\"smoke\":\"pass\"", "\"smoke\":\"maybe\"", "smoke"),
    ] {
        let bad = good.replacen(from, to, 1);
        assert_ne!(bad, good, "{from} is in the fixture");
        let why = parse_pointer(&bad).expect_err(&bad);
        assert!(why.contains(want), "{bad}: {why}");
        assert!(!why.contains('\n'), "one line: {why}");
    }
    assert!(parse_pointer("").unwrap_err().contains("JSON"));
    assert!(parse_pointer("{").unwrap_err().contains("JSON"));
}

#[test]
fn a_build_record_names_the_build_a_folder_holds() {
    assert_eq!(
        parse_installed(&build_json(101, false)),
        Some(installed(101, false))
    );
    assert_eq!(
        parse_installed(&format!("\u{feff}{}", build_json(101, true))),
        Some(installed(101, true)),
        "Windows PowerShell 5.1's byte-order mark, and a build from a changed tree"
    );
    // The long commit names the build where the short one is missing.
    let long_only = build_json(101, false).replacen("\"commit\":\"0123456789ab\",", "", 1);
    assert_eq!(parse_installed(&long_only), Some(installed(101, false)));
    // A record that names no build is no evidence of what is installed.
    for (from, to) in [
        ("\"build_number\":101,", ""),
        ("\"build_number\":101", "\"build_number\":0"),
        (
            "\"commit\":\"0123456789ab\",\"commit_full\":\"0123456789abcdef0123456789abcdef01234567\",",
            "",
        ),
    ] {
        let bad = build_json(101, false).replacen(from, to, 1);
        assert_ne!(bad, build_json(101, false), "{from} is in the fixture");
        assert_eq!(parse_installed(&bad), None, "{bad}");
    }
    assert_eq!(parse_installed("not a record"), None);

    let scratch = crate::scratch_dir("win-record");
    assert_eq!(read_installed(&scratch), None, "no BUILD.json");
    std::fs::write(scratch.join("BUILD.json"), build_json(7, false)).unwrap();
    assert_eq!(read_installed(&scratch), Some(installed(7, false)));
    assert_eq!(
        installed(7, true).name(),
        "aterm 0.200.0 (commit 0123456789ab-dirty, build 7)"
    );
}

#[test]
fn the_build_number_orders_the_staged_build_against_the_running_copy() {
    let read = |build| Ok(Some(staged(build, Smoke::Pass)));
    assert_eq!(
        classify(RUNNING, None, read(101)),
        Standing::Newer(staged(101, Smoke::Pass))
    );
    assert_eq!(
        classify(running_staged_commit(100), None, read(100)),
        Standing::Same(staged(100, Smoke::Pass))
    );
    assert_eq!(
        classify(RUNNING, None, read(99)),
        Standing::Older(staged(99, Smoke::Pass))
    );
    assert_eq!(classify(RUNNING, None, Ok(None)), Standing::Absent);
    assert_eq!(
        classify(RUNNING, None, Err("why".to_string())),
        Standing::Unreadable("why".to_string())
    );
    // An unstamped copy (build 0) is below every build number, and is still not "older".
    let unstamped = Running {
        build_number: 0,
        commit: "unknown",
        ..RUNNING
    };
    assert_eq!(
        classify(unstamped, None, read(101)),
        Standing::Unstamped(staged(101, Smoke::Pass))
    );
    // The version is a name, not an order: a higher version with a lower build is older.
    let mut renamed = staged(99, Smoke::Pass);
    renamed.version = "9.0.0".to_string();
    assert_eq!(
        classify(RUNNING, None, Ok(Some(renamed.clone()))),
        Standing::Older(renamed)
    );
}

/// A staged build is offered only while it is not what is installed on disk. The window that
/// keeps running build 100 after build 101 was laid under it is told to restart; the same
/// pointer with no record, or with an older one, is the offer (the controls).
#[test]
fn a_staged_build_already_installed_on_disk_is_not_offered_again() {
    let read = || Ok(Some(staged(101, Smoke::Pass)));
    let laid = installed(101, false);
    let standing = classify(RUNNING, Some(&laid), read());
    assert_eq!(standing, Standing::Installed(staged(101, Smoke::Pass)));
    assert_eq!(standing.offered(), None);
    assert_eq!(standing.word(), "installed");
    let why = apply_refusal(&standing).unwrap_err();
    assert!(why.contains("already installed"), "{why}");
    assert!(why.contains("restart aterm"), "{why}");
    assert_eq!(
        here(standing).notice(),
        Notice::Installed(staged(101, Smoke::Pass))
    );

    // The controls: nothing recorded, and an older record, leave the build offered.
    for on_disk in [None, Some(installed(100, false))] {
        let standing = classify(RUNNING, on_disk.as_ref(), read());
        assert_eq!(
            standing,
            Standing::Newer(staged(101, Smoke::Pass)),
            "{on_disk:?}"
        );
        assert_eq!(
            here(standing).notice(),
            Notice::Offered(staged(101, Smoke::Pass)),
            "{on_disk:?}"
        );
    }
    // The window that was started from the installed build is simply current.
    assert_eq!(
        classify(running_staged_commit(101), Some(&laid), read()),
        Standing::Same(staged(101, Smoke::Pass))
    );
    // What is installed is the base, not the image: a record newer than the staged build
    // means there is nothing to install, whatever this window runs.
    assert_eq!(
        classify(RUNNING, Some(&installed(102, false)), read()),
        Standing::Older(staged(101, Smoke::Pass))
    );
}

/// The build number is HEAD's commit time, so a build from a changed tree shares it with its
/// clean commit, and so do two commits of one second. Neither is the staged build.
#[test]
fn a_copy_with_the_staged_build_number_that_is_another_build_is_offered_the_staged_one() {
    let read = || Ok(Some(staged(100, Smoke::Pass)));
    let differs = Standing::Differs(staged(100, Smoke::Pass));
    let dirty_image = Running {
        commit: "0123456789ab-dirty",
        ..running_staged_commit(100)
    };
    for (running, on_disk, what) in [
        (dirty_image, None, "a running image from a changed tree"),
        (RUNNING, None, "another commit of the same second"),
        (
            running_staged_commit(100),
            Some(installed(100, true)),
            "an installed record from a changed tree",
        ),
    ] {
        let standing = classify(running, on_disk.as_ref(), read());
        assert_eq!(standing, differs, "{what}");
        assert_eq!(
            standing.offered(),
            Some(&staged(100, Smoke::Pass)),
            "{what}"
        );
    }
    assert_eq!(differs.word(), "differs");
    // The control: the same number, commit and clean tree is the staged build.
    assert_eq!(
        classify(running_staged_commit(100), None, read()),
        Standing::Same(staged(100, Smoke::Pass))
    );
    // The commit compares without case.
    let upper = Running {
        commit: "0123456789AB",
        ..running_staged_commit(100)
    };
    assert_eq!(
        classify(upper, None, read()),
        Standing::Same(staged(100, Smoke::Pass))
    );
    // A stamp that carries more of the commit is the same commit; one that carries less
    // cannot be told to be (the control).
    for (commit, same) in [
        ("0123456789abcdef0123456789abcdef01234567", true),
        ("0123456789", false),
    ] {
        let stamped = Running {
            commit,
            ..running_staged_commit(100)
        };
        let want = if same {
            Standing::Same(staged(100, Smoke::Pass))
        } else {
            Standing::Differs(staged(100, Smoke::Pass))
        };
        assert_eq!(classify(stamped, None, read()), want, "{commit}");
    }
    // A clean install under a window that runs the changed-tree image: restart, not install.
    assert_eq!(
        classify(dirty_image, Some(&installed(100, false)), read()),
        Standing::Installed(staged(100, Smoke::Pass))
    );
    // A staged build that is itself from a changed tree is what its install recorded: it is
    // not offered again once laid.
    let mut dirty_staged = staged(100, Smoke::Pass);
    dirty_staged.dirty = true;
    assert_eq!(
        classify(
            dirty_image,
            Some(&installed(100, true)),
            Ok(Some(dirty_staged.clone()))
        ),
        Standing::Same(dirty_staged.clone())
    );
    assert_eq!(
        classify(
            running_staged_commit(100),
            Some(&installed(100, false)),
            Ok(Some(dirty_staged.clone()))
        ),
        Standing::Differs(dirty_staged)
    );
}

#[test]
fn only_a_build_that_is_not_installed_and_did_not_fail_its_smoke_test_is_offered() {
    let offered = |standing: Standing| standing.offered().cloned();
    assert_eq!(
        offered(Standing::Newer(staged(101, Smoke::Pass))),
        Some(staged(101, Smoke::Pass))
    );
    assert_eq!(
        offered(Standing::Newer(staged(101, Smoke::NotRun))),
        Some(staged(101, Smoke::NotRun))
    );
    assert_eq!(
        offered(Standing::Differs(staged(100, Smoke::Pass))),
        Some(staged(100, Smoke::Pass))
    );
    // The negative controls: every other standing shows nothing.
    for quiet in [
        Standing::Newer(staged(101, Smoke::Fail)),
        Standing::Differs(staged(100, Smoke::Fail)),
        Standing::Installed(staged(101, Smoke::Pass)),
        Standing::Same(staged(100, Smoke::Pass)),
        Standing::Older(staged(99, Smoke::Pass)),
        Standing::Unstamped(staged(101, Smoke::Pass)),
        Standing::Absent,
        Standing::Unreadable("why".to_string()),
    ] {
        assert_eq!(offered(quiet.clone()), None, "{quiet:?}");
        assert!(
            !matches!(here(quiet.clone()).notice(), Notice::Offered(_)),
            "{quiet:?}"
        );
    }
    // A build this copy cannot install is not announced.
    let mut blocked = here(Standing::Newer(staged(101, Smoke::Pass)));
    assert_eq!(blocked.notice(), Notice::Offered(staged(101, Smoke::Pass)));
    blocked.blocked = Some("this copy is an MSIX package".to_string());
    assert_eq!(blocked.notice(), Notice::Nothing);
}

#[test]
fn the_pointer_and_the_build_record_are_read_from_disk() {
    let scratch = crate::scratch_dir("win-pointer");
    let path = pointer_path_under(&scratch);
    assert!(path.ends_with(Path::new("aterm").join("dist").join("latest.json")));
    let install = scratch.join("Programs").join("aterm");
    std::fs::create_dir_all(&install).unwrap();
    let exe = install.join("aterm.exe");
    let look_from = |running| look(running, Some(&exe), &path);

    assert_eq!(look_from(RUNNING).standing, Standing::Absent);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, pointer_text(101, "pass")).unwrap();
    let newer = look_from(RUNNING);
    assert_eq!(newer.standing, Standing::Newer(staged(101, Smoke::Pass)));
    assert_eq!(newer.installed, None);
    assert_eq!(newer.pointer, path);
    // The pointer's dist is on no machine that runs this test: an install folder would take
    // the build, but there is no installer to run, so nothing is announced. (The control,
    // with the installer there, is the Windows-only test below.)
    assert!(
        newer
            .blocked
            .as_deref()
            .is_some_and(|why| why.contains("has no installer")),
        "{newer:?}"
    );
    assert_eq!(newer.notice(), Notice::Nothing);
    assert_eq!(
        newer.installable(),
        Err(newer.blocked.clone().unwrap_or_default())
    );
    assert_eq!(
        look_from(running_staged_commit(101)).standing,
        Standing::Same(staged(101, Smoke::Pass))
    );

    // The installer lays the staged build's record under the window that runs build 100: the
    // next look says installed, and the offer is gone.
    std::fs::write(install.join("BUILD.json"), build_json(101, false)).unwrap();
    let laid = look_from(RUNNING);
    assert_eq!(laid.standing, Standing::Installed(staged(101, Smoke::Pass)));
    assert_eq!(laid.installed, Some(installed(101, false)));
    assert_eq!(laid.notice(), Notice::Installed(staged(101, Smoke::Pass)));
    // Without the executable's place there is no record to read: the image speaks.
    assert_eq!(
        look(RUNNING, None, &path).standing,
        Standing::Newer(staged(101, Smoke::Pass))
    );

    // A build tree is told why apply does not install there, and announces nothing.
    let target = scratch.join("target").join("debug");
    std::fs::create_dir_all(target.join("deps")).unwrap();
    let tree = look(RUNNING, Some(&target.join("aterm.exe")), &path);
    assert_eq!(tree.standing, Standing::Newer(staged(101, Smoke::Pass)));
    assert!(
        tree.blocked
            .as_deref()
            .is_some_and(|why| why.contains("build tree")),
        "{tree:?}"
    );
    assert_eq!(tree.notice(), Notice::Nothing);

    std::fs::write(&path, "not a pointer").unwrap();
    assert!(matches!(
        look_from(RUNNING).standing,
        Standing::Unreadable(_)
    ));
}

/// A pointer that says nothing of `dirty` takes it from the staged dist's own record. The
/// pointer's `dir` is a Windows path, so this reads a real folder on Windows alone.
#[cfg(windows)]
#[test]
fn a_staged_dist_from_a_changed_tree_is_known_by_its_own_record() {
    let scratch = crate::scratch_dir("win-dirty");
    let dist = scratch.join("dist").join("0123456789ab");
    std::fs::create_dir_all(&dist).unwrap();
    let path = scratch.join("dist").join("latest.json");
    let dir = dist.display().to_string().replace('\\', "\\\\");
    let text = pointer_text(101, "pass").replacen(
        "C:\\\\Users\\\\someone\\\\AppData\\\\Local\\\\aterm\\\\dist\\\\0123456789ab",
        &dir,
        1,
    );
    std::fs::write(&path, &text).unwrap();
    let read = || read_pointer(&path).unwrap().unwrap();
    assert_eq!(read().dir, dist);
    assert!(!read().dirty, "no record: a clean build");
    std::fs::write(dist.join("BUILD.json"), build_json(101, false)).unwrap();
    assert!(!read().dirty, "a clean record");
    std::fs::write(dist.join("BUILD.json"), build_json(101, true)).unwrap();
    assert!(read().dirty, "the dist's record says a changed tree");
    std::fs::write(dist.join("BUILD.json"), build_json(99, true)).unwrap();
    assert!(
        !read().dirty,
        "another build's record says nothing of this one"
    );

    // From an install folder, the build is announced only while its installer is there.
    let install = scratch.join("Programs").join("aterm");
    std::fs::create_dir_all(&install).unwrap();
    let exe = install.join("aterm.exe");
    let mut want = staged(101, Smoke::Pass);
    want.dir = dist.clone();
    let gone = look(RUNNING, Some(&exe), &path);
    assert_eq!(gone.standing, Standing::Newer(want.clone()));
    assert_eq!(gone.notice(), Notice::Nothing, "{gone:?}");
    std::fs::write(dist.join("install.ps1"), "exit 0\n").unwrap();
    let ready = look(RUNNING, Some(&exe), &path);
    assert_eq!(ready.blocked, None, "an install folder takes the build");
    assert_eq!(ready.notice(), Notice::Offered(want.clone()));
    assert_eq!(ready.installable(), Ok(&want));
    // The same dist seen from a test binary's own folder (a cargo profile's `deps`).
    let deps = scratch.join("target").join("debug").join("deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::create_dir_all(scratch.join("target").join("debug").join(".fingerprint")).unwrap();
    let test_binary = look(RUNNING, Some(&deps.join("aterm_gui-0123.exe")), &path);
    assert!(
        test_binary
            .blocked
            .as_deref()
            .is_some_and(|why| why.contains("build tree")),
        "{test_binary:?}"
    );
}

#[test]
fn status_names_the_running_build_then_the_staged_one_and_how_to_apply_it() {
    let lines = |standing: &Standing| status_lines(RUNNING, &here(standing.clone()));

    let newer = lines(&Standing::Newer(staged(101, Smoke::Pass)));
    assert_eq!(
        newer[0],
        "aterm 0.199.0 (commit 1a37de31a34a, build 100) is running"
    );
    assert!(
        newer[1].starts_with("aterm 0.200.0 (commit 0123456789ab, build 101) is staged at "),
        "{newer:?}"
    );
    for word in [DIR, "staged 2026-11-11T10:00:00Z", "smoke test pass"] {
        assert!(newer[1].contains(word), "{word}: {newer:?}");
    }
    assert!(
        newer[2].contains("`aterm update apply` installs it"),
        "{newer:?}"
    );
    assert!(newer[2].contains("--restart"), "{newer:?}");

    for failed in [
        Standing::Newer(staged(101, Smoke::Fail)),
        Standing::Differs(staged(100, Smoke::Fail)),
    ] {
        let failed = lines(&failed).join("\n");
        assert!(failed.contains("smoke test failed"), "{failed}");
        assert!(!failed.contains("aterm update apply"), "{failed}");
        assert!(failed.contains(STAGE_COMMAND), "{failed}");
    }

    for current in [
        Standing::Same(staged(100, Smoke::Pass)),
        Standing::Older(staged(99, Smoke::Pass)),
    ] {
        let said = lines(&current).join("\n");
        assert!(said.contains("aterm is current"), "{said}");
        assert!(!said.contains("aterm update apply"), "{said}");
    }

    // Installed under this copy: a restart, never an install.
    let laid = lines(&Standing::Installed(staged(101, Smoke::Pass))).join("\n");
    assert!(laid.contains("build 101) is installed"), "{laid}");
    assert!(laid.contains("restart aterm"), "{laid}");
    assert!(!laid.contains("aterm update apply"), "{laid}");
    assert!(!laid.contains("aterm is current"), "{laid}");

    // The same number and another build: named as that, and offered.
    let mut other = here(Standing::Differs(staged(100, Smoke::Pass)));
    other.installed = Some(installed(100, true));
    let differs = status_lines(RUNNING, &other).join("\n");
    assert!(differs.contains("0123456789ab-dirty"), "{differs}");
    assert!(differs.contains("same build number"), "{differs}");
    assert!(
        differs.contains("`aterm update apply` installs it"),
        "{differs}"
    );
    assert!(!differs.contains("aterm is current"), "{differs}");

    // A copy apply cannot replace is told why, in place of the line that says it installs.
    let mut blocked = here(Standing::Newer(staged(101, Smoke::Pass)));
    blocked.blocked = Some("this copy is an MSIX package".to_string());
    let said = status_lines(RUNNING, &blocked).join("\n");
    assert!(said.contains("cannot install it"), "{said}");
    assert!(said.contains("MSIX package"), "{said}");
    assert!(!said.contains("installs it and stops nothing"), "{said}");

    let absent = lines(&Standing::Absent).join("\n");
    assert!(absent.contains("no staged build"), "{absent}");
    assert!(absent.contains(&format!("`{STAGE_COMMAND}`")), "{absent}");
    assert!(absent.contains("latest.json"), "{absent}");

    let unreadable = lines(&Standing::Unreadable("schema 2".to_string())).join("\n");
    assert!(unreadable.contains("schema 2"), "{unreadable}");

    for standing in [
        Standing::Absent,
        Standing::Same(staged(100, Smoke::Pass)),
        Standing::Newer(staged(101, Smoke::Pass)),
    ] {
        let said = lines(&standing).join("\n");
        assert!(!said.contains("doesn\u{2019}t update itself"), "{said}");
    }
}

#[test]
fn the_control_line_is_one_ok_line_of_fields() {
    let line = |standing: Standing| control_line(RUNNING, &here(standing));
    let newer = line(Standing::Newer(staged(101, Smoke::Pass)));
    assert!(
        newer.starts_with(
            "OK enabled=false build=100 version=0.199.0 commit=1a37de31a34a staged=newer \
             staged_build=101 staged_version=0.200.0 staged_commit=0123456789ab smoke=pass "
        ),
        "{newer}"
    );
    assert!(
        newer.contains("outcome=\"a newer build is staged: update apply installs it\""),
        "{newer}"
    );
    assert!(!newer.contains("installed_build="), "{newer}");
    let none = line(Standing::Absent);
    assert_eq!(
        none,
        "OK enabled=false build=100 version=0.199.0 commit=1a37de31a34a staged=none \
         outcome=\"no staged build\""
    );

    let mut laid = here(Standing::Installed(staged(101, Smoke::Pass)));
    laid.installed = Some(installed(101, false));
    let laid = control_line(RUNNING, &laid);
    assert!(laid.contains(" staged=installed "), "{laid}");
    assert!(laid.contains(" installed_build=101 "), "{laid}");
    assert_eq!(
        quoted_field(&laid, "outcome").as_deref(),
        Some("the staged build is installed: restart aterm to use it")
    );

    let mut blocked = here(Standing::Newer(staged(101, Smoke::Pass)));
    blocked.blocked = Some(r"this copy is an MSIX package (C:\Program Files\WindowsApps\x)".into());
    let blocked = control_line(RUNNING, &blocked);
    let outcome = quoted_field(&blocked, "outcome").expect(&blocked);
    assert!(outcome.contains("cannot install it"), "{outcome}");
    assert!(
        outcome.ends_with(r"(C:\Program Files\WindowsApps\x)"),
        "{outcome}"
    );

    for standing in [
        Standing::Absent,
        Standing::Unreadable("a \"quoted\"\nreason".to_string()),
        Standing::Newer(staged(101, Smoke::Fail)),
        Standing::Differs(staged(100, Smoke::Pass)),
        Standing::Installed(staged(101, Smoke::Pass)),
        Standing::Same(staged(100, Smoke::Pass)),
        Standing::Older(staged(99, Smoke::Pass)),
        Standing::Unstamped(staged(101, Smoke::Pass)),
    ] {
        let line = line(standing.clone());
        assert!(!line.contains('\n'), "{line}");
        assert!(!line.contains("no updater on this platform"), "{line}");
        assert!(
            line.contains(&format!(" staged={} ", standing.word())),
            "{line}"
        );
        assert!(quoted_field(&line, "outcome").is_some(), "{line}");
    }
}

/// The quoted fields carry the wire's escaping, so a reader that unescapes (as it must for
/// the macOS line of the same verb) gets a Windows path and a quoted reason back whole. The
/// control: the same path written raw does not survive that reader.
#[test]
fn the_control_line_s_quoted_fields_survive_an_unescaping_reader() {
    let newer = control_line(RUNNING, &here(Standing::Newer(staged(101, Smoke::Pass))));
    assert!(newer.contains(&format!("staged_dir={DIR:?}")), "{newer}");
    assert_eq!(quoted_field(&newer, "staged_dir").as_deref(), Some(DIR));
    assert_eq!(
        quoted_field(&newer, "staged_at").as_deref(),
        Some("2026-11-11T10:00:00Z")
    );
    let raw = format!(" staged_dir=\"{DIR}\" outcome=\"x\"");
    assert_ne!(
        quoted_field(&raw, "staged_dir").as_deref(),
        Some(DIR),
        "a raw backslash is an escape to the reader"
    );

    // A path whose separators spell escapes (`\n`, `\t`, `\u`) and one that ends in a
    // backslash, which written raw would escape the closing quote and swallow `outcome=`.
    for dir in [r"C:\users\name\tools\dist\0123456789ab", r"D:\"] {
        let mut build = staged(101, Smoke::Pass);
        build.dir = PathBuf::from(dir);
        let line = control_line(RUNNING, &here(Standing::Newer(build)));
        assert_eq!(quoted_field(&line, "staged_dir").as_deref(), Some(dir));
        assert_eq!(
            quoted_field(&line, "outcome").as_deref(),
            Some("a newer build is staged: update apply installs it"),
            "{line}"
        );
    }

    let reason = "a \"quoted\"\nreason";
    let unreadable = control_line(RUNNING, &here(Standing::Unreadable(reason.to_string())));
    assert!(!unreadable.contains('\n'), "{unreadable}");
    assert_eq!(
        quoted_field(&unreadable, "outcome"),
        Some(format!(
            "can\u{2019}t read the staged build pointer: {reason}"
        ))
    );
}

#[test]
fn the_installer_is_run_staged_into_the_running_copy_s_folder() {
    let build = staged(101, Smoke::Pass);
    let dest = Path::new(r"C:\Users\someone\AppData\Local\Programs\aterm");
    let args = |mode| -> Vec<String> {
        installer_args(&build, dest, mode)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };
    let installer = build.installer().display().to_string();
    assert_eq!(
        args(ApplyMode::Staged),
        [
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            installer.as_str(),
            "-Staged",
            "-Dest",
            r"C:\Users\someone\AppData\Local\Programs\aterm",
            "-NoShortcuts",
            "-NoPath",
        ]
    );
    let restart = args(ApplyMode::Restart);
    assert!(restart.contains(&"-Restart".to_string()), "{restart:?}");
    assert!(!restart.contains(&"-Staged".to_string()), "{restart:?}");
}

#[test]
fn a_failed_installer_s_reason_is_its_own_line() {
    let said = "Installing aterm 0.200.0 from: D:/dist\n\ninstall.ps1: no aterm.exe in D:/dist\n";
    assert_eq!(failure_reason(said), Some("no aterm.exe in D:/dist"));
    assert_eq!(
        failure_reason("first\n  the last line  \n\n"),
        Some("the last line")
    );
    assert_eq!(failure_reason(" \n\n"), None);
}

#[test]
fn apply_refuses_whatever_is_not_an_offered_build_with_its_installer() {
    let scratch = crate::scratch_dir("win-apply");
    let mut build = staged(101, Smoke::Pass);
    build.dir = scratch.join("dist");
    std::fs::create_dir_all(&build.dir).unwrap();

    let missing = apply_refusal(&Standing::Newer(build.clone())).unwrap_err();
    assert!(missing.contains("install.ps1"), "{missing}");
    std::fs::write(build.installer(), "exit 0\n").unwrap();
    assert_eq!(apply_refusal(&Standing::Newer(build.clone())), Ok(&build));
    assert_eq!(apply_refusal(&Standing::Differs(build.clone())), Ok(&build));

    let mut failed = build.clone();
    failed.smoke = Smoke::Fail;
    for (standing, want) in [
        (Standing::Newer(failed.clone()), "smoke test"),
        (Standing::Differs(failed), "smoke test"),
        (Standing::Installed(build.clone()), "already installed"),
        (Standing::Same(build.clone()), "current"),
        (Standing::Older(build.clone()), "current"),
        (Standing::Unstamped(build.clone()), "no build number"),
        (Standing::Absent, "no staged build"),
        (Standing::Unreadable("schema 2".to_string()), "schema 2"),
    ] {
        let why = apply_refusal(&standing).unwrap_err();
        assert!(why.contains(want), "{standing:?}: {why}");
    }

    // `installable` is the same rule plus this copy's own refusal.
    let mut open = here(Standing::Newer(build.clone()));
    assert_eq!(open.installable(), Ok(&build));
    open.blocked = Some("this copy is an MSIX package".to_string());
    assert_eq!(
        open.installable(),
        Err("this copy is an MSIX package".to_string())
    );
}

/// The window's row and `apply` agree: a build the row offers is one apply installs, and one
/// apply refuses is never on the row. The window's click on a row it still shows relies on
/// it: a refusal there always means the row is stale.
#[test]
fn the_row_offers_exactly_what_apply_installs() {
    let scratch = crate::scratch_dir("win-agree");
    let mut build = staged(101, Smoke::Pass);
    build.dir = scratch.join("0123456789ab");
    std::fs::create_dir_all(&build.dir).unwrap();
    std::fs::write(build.installer(), "exit 0\n").unwrap();
    let mut failed = build.clone();
    failed.smoke = Smoke::Fail;
    let mut offered = 0;
    for standing in [
        Standing::Newer(build.clone()),
        Standing::Newer(failed.clone()),
        Standing::Differs(build.clone()),
        Standing::Differs(failed),
        Standing::Installed(build.clone()),
        Standing::Same(build.clone()),
        Standing::Older(build.clone()),
        Standing::Unstamped(build.clone()),
        Standing::Absent,
        Standing::Unreadable("why".to_string()),
    ] {
        for refusal in [None, Some("this copy is an MSIX package".to_string())] {
            // `look` sets `blocked` only for an offered build.
            let mut here = here(standing.clone());
            here.blocked = standing.offered().and(refusal);
            let on_the_row = matches!(here.notice(), Notice::Offered(_));
            assert_eq!(on_the_row, here.installable().is_ok(), "{here:?}");
            offered += usize::from(on_the_row);
        }
    }
    assert_eq!(
        offered, 2,
        "the control: an open Newer and an open Differs are offered"
    );
}

#[test]
fn apply_installs_only_over_an_install_folder() {
    let scratch = crate::scratch_dir("win-dest");
    let dist_root = scratch.join("aterm").join("dist");
    let mut build = staged(101, Smoke::Pass);
    build.dir = dist_root.join("0123456789ab");
    std::fs::create_dir_all(&build.dir).unwrap();

    let install = scratch.join("Programs").join("aterm");
    std::fs::create_dir_all(&install).unwrap();
    assert_eq!(
        install_dest(&install.join("aterm.exe"), &build),
        Ok(install.clone())
    );

    // A cargo target folder is not an install.
    let target = scratch.join("target").join("debug");
    std::fs::create_dir_all(target.join("deps")).unwrap();
    let why = install_dest(&target.join("aterm.exe"), &build).unwrap_err();
    assert!(why.contains("build tree"), "{why}");
    // Nor the folders inside a profile a test or an example runs from.
    std::fs::create_dir_all(target.join(".fingerprint")).unwrap();
    for inner in ["deps", "examples"] {
        std::fs::create_dir_all(target.join(inner)).unwrap();
        let why = install_dest(&target.join(inner).join("aterm-0123.exe"), &build).unwrap_err();
        assert!(why.contains("build tree"), "{inner}: {why}");
    }
    // The control: a folder that is only named `deps` is an install.
    let named = scratch.join("tools").join("deps");
    std::fs::create_dir_all(&named).unwrap();
    assert_eq!(
        install_dest(&named.join("aterm.exe"), &build),
        Ok(named.clone())
    );

    // Neither is the staged dist folder itself.
    let why = install_dest(&build.dir.join("aterm.exe"), &build).unwrap_err();
    assert!(why.contains("staged dist folder itself"), "{why}");

    // Nor another dist folder the build loop keeps beside it.
    let kept = dist_root.join("ba9876543210");
    std::fs::create_dir_all(&kept).unwrap();
    let why = install_dest(&kept.join("aterm.exe"), &build).unwrap_err();
    assert!(why.contains("a staged dist folder"), "{why}");
    assert!(why.contains("install.ps1"), "{why}");
    // The control: a folder beside the dists that is not named as one is an install.
    let beside = dist_root.join("portable");
    std::fs::create_dir_all(&beside).unwrap();
    assert_eq!(
        install_dest(&beside.join("aterm.exe"), &build),
        Ok(beside.clone())
    );

    // Nor an MSIX package, which only Windows can replace; the folder's case is Windows'.
    for apps in ["WindowsApps", "windowsapps"] {
        let package = scratch
            .join("Program Files")
            .join(apps)
            .join("aterm_1.0_x64");
        std::fs::create_dir_all(&package).unwrap();
        let why = install_dest(&package.join("aterm.exe"), &build).unwrap_err();
        assert!(why.contains("MSIX package"), "{why}");
        assert!(!why.contains('\n'), "one line: {why}");
    }
}

/// The watch reports each change of what the window should say, once: a build that becomes
/// offered, its withdrawal (the pointer now names a build that failed its smoke test, or
/// cannot be read), and a build laid under the window. A look that changes nothing is silent.
#[test]
fn the_watch_tells_each_change_once_and_withdraws_what_is_no_longer_offered() {
    let offered = |build| Notice::Offered(staged(build, Smoke::Pass));
    let look_at = |pointer, on_disk: Option<InstalledBuild>| {
        here(classify(RUNNING, on_disk.as_ref(), pointer)).notice()
    };
    let mut looks = vec![
        // Nothing staged, and a build older than this copy: nothing to say, and no call.
        look_at(Ok(None), None),
        look_at(Ok(Some(staged(99, Smoke::Pass))), None),
        // 101 is staged: offered, and a second look at it is silent.
        look_at(Ok(Some(staged(101, Smoke::Pass))), None),
        look_at(Ok(Some(staged(101, Smoke::Pass))), None),
        // 102 fails its smoke test: the offer of 101 is withdrawn, once.
        look_at(Ok(Some(staged(102, Smoke::Fail))), None),
        look_at(Err("torn".to_string()), None),
        // 103 is staged, then laid under the window by another installer.
        look_at(Ok(Some(staged(103, Smoke::Pass))), None),
        look_at(
            Ok(Some(staged(103, Smoke::Pass))),
            Some(installed(103, false)),
        ),
        look_at(
            Ok(Some(staged(103, Smoke::Pass))),
            Some(installed(103, false)),
        ),
        // 104 is staged over it.
        look_at(
            Ok(Some(staged(104, Smoke::Pass))),
            Some(installed(103, false)),
        ),
    ]
    .into_iter();
    let rounds = looks.len();
    let mut told = Vec::new();
    let mut waits = Vec::new();
    watch_with(
        &mut || looks.next().expect("one look per round"),
        &mut |notice| told.push(notice),
        &mut |period| {
            waits.push(period);
            waits.len() < rounds
        },
    );
    assert_eq!(
        told,
        [
            offered(101),
            Notice::Nothing,
            offered(103),
            Notice::Installed(staged(103, Smoke::Pass)),
            offered(104),
        ]
    );
    assert_eq!(waits.len(), rounds);
    assert!(
        waits
            .iter()
            .all(|period| *period >= Duration::from_secs(600)),
        "the window reads the pointer no more often than every ten minutes: {waits:?}"
    );
}

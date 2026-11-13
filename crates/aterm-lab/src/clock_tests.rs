// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lab(clock: &str) -> Run {
    Run {
        tag: "c1".into(),
        run: "/s/runs/c1".into(),
        w: "/s/runs/c1/scratch".into(),
        clock: clock.into(),
        ..Run::default()
    }
}

/// THE CLOCK'S VARIABLES ARE ITS LAB'S: a lab up with it loads the
/// library from its run directory, reads its offset there, and records
/// in its scratch (the one place a jailed process writes); a lab without
/// it hands none.
#[test]
fn a_lab_up_with_the_clock_hands_its_own_files_and_one_without_it_none() {
    assert_eq!(
        vars(&lab("1")),
        [
            (
                "DYLD_INSERT_LIBRARIES",
                "/s/runs/c1/clock/libaterm_lab_clock.dylib"
            ),
            ("LABCLOCK_FILE", "/s/runs/c1/clock/offset"),
            ("LABCLOCK_DIR", "/s/runs/c1/scratch/clock"),
            ("LABCLOCK_CONTINUOUS", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    assert!(vars(&lab("0")).is_empty() && vars(&lab("")).is_empty());
    for (k, _) in vars(&lab("1")) {
        assert!(is_clock_var(&k), "{k}");
    }
    assert!(!is_clock_var("DYLD_LIBRARY_PATH") && !is_clock_var("TMPDIR"));
}

/// A HARDENED RUNTIME IS READ OFF ITS FLAG (Claude Code 2.1.303 as the
/// store holds it; the ad hoc clone and a linker-signed build have none).
#[test]
fn a_hardened_runtime_is_read_off_the_signature_flags() {
    let store = "Executable=/S/claude\nIdentifier=com.anthropic.claude-code\nFormat=Mach-O thin (arm64)\n\
                 CodeDirectory v=20500 size=522693 flags=0x10000(runtime) hashes=16323+7 location=embedded\n";
    assert!(hardened(store));
    assert!(hardened(
        "CodeDirectory v=20500 flags=0x12000(library-validation,runtime) x"
    ));
    assert!(!hardened(
        "CodeDirectory v=20400 size=522514 flags=0x2(adhoc) hashes=16323+2"
    ));
    assert!(!hardened(
        "CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed) hashes=66192+0"
    ));
    assert!(!hardened("code object is not signed at all"));
}

/// THE BUILD'S LIBRARY IS ITS CDYLIB'S `.dylib`, whatever else it printed.
#[test]
fn the_build_names_its_library_in_its_artifact_line() {
    let lines = concat!(
        r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"lab-clock-probe"},"filenames":["/t/debug/lab-clock-probe"]}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"kind":["cdylib"],"crate_types":["cdylib"],"name":"aterm_lab_clock"},"filenames":["/t/lab-clock/debug/libaterm_lab_clock.dylib"]}"#,
        "\n",
        r#"{"reason":"build-finished","success":true}"#,
        "\n"
    );
    assert_eq!(
        artifact_of(lines),
        Some(PathBuf::from("/t/lab-clock/debug/libaterm_lab_clock.dylib"))
    );
    assert_eq!(
        artifact_of(r#"{"reason":"build-finished","success":true}"#),
        None
    );
    assert_eq!(artifact_of("not json\n"), None);
}

/// A RECORD SAYS ITS PROCESS AND THE OFFSET IT TOOK (`model::render`'s
/// text, an executable path with a space in it); the offset file holds
/// whole seconds.
#[test]
fn a_record_and_the_offset_read_back() {
    let text = "pid=95226 ppid=1 exe=/lab/bin dir/aterm\n\
                offset_ns=1800000000000 continuous=1 file_reads=2\n\
                gettimeofday=0 time=3 mach_continuous_time=19 mach_continuous_approximate_time=0\n\
                clock 0: clock_gettime=11698 clock_gettime_nsec_np=0\n";
    let r = parse_record(text).expect("a record");
    assert_eq!(
        (r.pid, r.ppid, r.exe.as_str(), r.name()),
        (95226, 1, "/lab/bin dir/aterm", "aterm")
    );
    assert!(r.at(1800) && !r.at(0));
    assert_eq!(parse_record("pid=1 ppid=0\n"), None);
    assert_eq!(parse_record("pid=x ppid=0 exe=/a\noffset_ns=0\n"), None);
    assert_eq!(parse_offset(&offset_text(1800)), Some(1800));
    assert_eq!(parse_offset(&offset_text(0)), Some(0));
    assert_eq!(parse_offset("1800.5\n"), None);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

fn mac_frame(argc: i32, tail: &[u8]) -> Vec<u8> {
    let mut bytes = argc.to_ne_bytes().to_vec();
    bytes.extend_from_slice(b"/bin/aterm\0\0");
    bytes.extend_from_slice(tail);
    bytes
}

#[test]
fn a_kernel_argv_buffer_keeps_words_and_stops_before_the_environment() {
    let expected = vec![
        OsString::from("/bin/aterm"),
        OsString::from("link"),
        OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
        OsString::new(),
    ];
    let bytes = mac_frame(4, b"/bin/aterm\0link\0name\xff with --durability relaxed\0\0--durability=relaxed\0SECRET=not-argv\0\0");
    assert_eq!(argv_from_procargs2(&bytes), Some(expected));
    let ordinary = mac_frame(1, b"/bin/aterm\0SECRET=not-argv\0");
    assert_eq!(
        argv_from_procargs2(&ordinary),
        Some(vec![OsString::from("/bin/aterm")])
    );
    let mut broken = Vec::new();
    for argc in [-1, 0, PROCESS_ARGV_WORDS as i32 + 1] {
        broken.push(mac_frame(argc, b"/bin/aterm\0"));
    }
    broken.push(mac_frame(2, b"/bin/aterm\0"));
    broken.push(mac_frame(1, b"/bin/aterm"));
    broken.push(vec![1, 0, 0]);
    let got: Vec<_> = broken
        .iter()
        .map(|bytes| argv_from_procargs2(bytes))
        .collect();
    assert!(got.iter().all(Option::is_none));
}

#[test]
fn a_proc_cmdline_keeps_empty_and_non_utf8_words_but_refuses_a_prefix() {
    assert_eq!(
        argv_from_cmdline(b"aterm\0\0name\xff with --durability relaxed\0\0"),
        Some(vec![
            OsString::from("aterm"),
            OsString::new(),
            OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
            OsString::new()
        ])
    );
    assert_eq!(
        argv_from_cmdline(b"aterm\0"),
        Some(vec![OsString::from("aterm")])
    );
    assert_eq!(argv_from_cmdline(b""), None);
    assert_eq!(argv_from_cmdline(b"aterm\0truncated"), None);
    let at_limit = vec![0u8; PROCESS_ARGV_WORDS];
    assert_eq!(
        argv_from_cmdline(&at_limit).unwrap().len(),
        PROCESS_ARGV_WORDS
    );
    let over_limit = vec![0u8; PROCESS_ARGV_WORDS + 1];
    assert_eq!(argv_from_cmdline(&over_limit), None);

    let raw = b"/i\0t\xff\0\0".to_vec();
    let limit = raw.len();
    let (words, translated) =
        cmdline_from_read(raw.clone(), limit, || Some(b"/i".to_vec())).unwrap();
    assert!(translated, "the exact translator word was removed");
    assert_eq!(
        argv_from_cmdline(&words),
        Some(vec![OsString::from_vec(b"t\xff".to_vec()), OsString::new()])
    );
    assert!(words.len() < limit, "trimming could hide the sentinel");
    assert!(words.ends_with(b"\0"), "the prefix would look complete");
    assert_eq!(
        cmdline_from_read(raw.clone(), limit - 1, || {
            panic!("an oversized read must refuse before observing the interpreter")
        }),
        None
    );
    assert_eq!(
        cmdline_from_read(raw.clone(), limit, || None),
        Some((raw, false)),
        "a native read at the bound keeps every word"
    );
    let other = b"/ix\0t\0".to_vec();
    assert_eq!(
        cmdline_from_read(other.clone(), other.len(), || Some(b"/i".to_vec())),
        Some((other, false)),
        "a byte prefix without the NUL boundary names no translator word"
    );
}

#[test]
fn an_argv_observation_needs_the_same_known_process_start_on_both_sides() {
    let old = ProcessStart {
        boot: BootClock(17),
        wall: None,
    };
    let new = ProcessStart {
        boot: BootClock(18),
        wall: None,
    };
    let words = || Some(vec![OsString::from("aterm"), OsString::new()]);
    let mut count = 0;
    assert_eq!(
        argv_of_same_process(
            71,
            |_| {
                count += 1;
                Some(old)
            },
            |_| words()
        ),
        words()
    );
    assert_eq!(count, 2);
    for after in [None, Some(new)] {
        let mut n = 0;
        assert_eq!(
            argv_of_same_process(
                71,
                |_| {
                    n += 1;
                    if n == 1 { Some(old) } else { after }
                },
                |_| words()
            ),
            None
        );
        assert_eq!(n, 2);
    }
    assert_eq!(
        argv_of_same_process(
            71,
            |_| None,
            |_| panic!("unknown identity must not read argv")
        ),
        None
    );
    for pid in [-1, 0, 1] {
        assert_eq!(
            argv_of_same_process(pid, |_| panic!("invalid pid"), |_| panic!("invalid pid")),
            None
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_childs_program_and_argv_read_as_given_and_leave_no_descriptor_behind() {
    const NAME: &str = "process::argv_tests::\
        a_childs_program_and_argv_read_as_given_and_leave_no_descriptor_behind";
    const PARK_ENV: &str = "ATERM_UDS_TEST_ARGV_PARK";
    if std::env::var_os(PARK_ENV).is_some() {
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let args = ["--exact", NAME, "--nocapture", "--test-threads=1"];
    let mut child = crate::spawnfd::command(&exe)
        .args(args)
        .env(PARK_ENV, "1")
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("the parked copy");
    let pid = i32::try_from(child.id()).expect("a pid");
    let given: Vec<OsString> = std::iter::once(exe.clone().into_os_string())
        .chain(args.iter().map(OsString::from))
        .collect();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut read = read_process_argv(pid);
    while read.as_ref() != Some(&given) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
        read = read_process_argv(pid);
    }
    let again = (0..64).all(|_| read_process_argv(pid) == read);
    let program = read_exec_path(child.id());
    let auxv = std::path::PathBuf::from(format!("/proc/{pid}/auxv"));
    let kept = std::fs::read_dir("/proc/self/fd")
        .expect("this process's descriptors")
        .flatten()
        .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|to| to == auxv))
        .count();
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(read, Some(given), "the parked copy's argv");
    assert_eq!(program, Some(exe), "the parked copy's program");
    assert!(again, "64 more reads answered the same");
    assert_eq!(kept, 0, "descriptors left open on {}", auxv.display());
}

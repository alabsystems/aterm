// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A read of a nonblocking stream says `WouldBlock` at once, so a line that arrives here was
/// waited for in `poll(2)`. This read has no deadline once the command runs.
#[cfg(unix)]
#[test]
fn a_line_is_waited_for_in_poll() {
    use std::io::Write as _;
    let (near, far) = aterm_uds::kept_out(crate::CtlStream::pair).unwrap();
    near.set_nonblocking(true).unwrap();
    let answering = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        (&far).write_all(b"OK sudo stdio\n").unwrap();
        far
    });
    assert_eq!(read_line(&near).unwrap(), "OK sudo stdio");
    drop(answering.join().unwrap());
}

fn parts(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn only_a_sudo_request_is_a_command_and_its_words_are_untouched() {
    let request = parts(&["sudo", "--", "scutil", "--help", "@self"]);
    assert_eq!(
        command_words(&request).map(<[String]>::len),
        Some(4),
        "the words stay as typed, `--help` and `@self` included"
    );
    assert!(command_words(&parts(&["sudo", "--help"])).is_none());
    assert!(command_words(&parts(&["status", "sudo"])).is_none());
}

#[test]
fn a_program_resolves_to_what_the_shell_would_run() {
    let cwd = Path::new("/tmp/here");
    assert_eq!(
        resolve_program("./tool", cwd, None).expect("a path"),
        "/tmp/here/./tool"
    );
    assert_eq!(
        resolve_program("/bin/ls", cwd, None).expect("absolute stays"),
        "/bin/ls"
    );
    let path = std::ffi::OsString::from("/nonexistent-aterm-dir:/bin");
    assert_eq!(
        resolve_program("ls", cwd, Some(&path)).expect("on PATH"),
        "/bin/ls"
    );
    assert!(resolve_program("no-such-aterm-program", cwd, Some(&path)).is_err());
    assert!(
        resolve_program("ls", cwd, None).is_err(),
        "no PATH, nothing found"
    );
}

#[test]
fn the_final_line_sets_the_exit_code() {
    assert_eq!(final_line_code("OK sudo exit=7"), Ok(7));
    assert_eq!(final_line_code("OK sudo signal=15"), Ok(143));
    assert!(final_line_code("OK sudo exit=x").is_err());
    assert_eq!(
        final_line_code("ERR sudo refused off"),
        Err("ERR sudo refused off".to_string())
    );
}

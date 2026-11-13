// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A child's working directory ([`cwd_of`]) and the file it holds ([`open_files`]) read
/// from outside it; once it is reaped neither names anything.
#[test]
fn a_childs_directory_and_open_file_read_from_outside_it() {
    let dir = crate::TestScratch::new("relaunch");
    let lock = dir.join("presence.lock");
    std::fs::write(&lock, b"").unwrap();
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .current_dir(&*dir)
        .stdin(std::fs::File::open(&lock).unwrap())
        .spawn()
        .expect("spawn a holder");
    let pid = i32::try_from(child.id()).unwrap();
    let (cwd, files) = (cwd_of(pid), open_files(pid));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(cwd.as_deref(), Some(&*dir.to_string_lossy()));
    assert!(
        files.contains(&lock.to_string_lossy().into_owned()),
        "{files:?}"
    );
    assert_eq!((cwd_of(pid), open_files(pid)), (None, Vec::new()), "reaped");
}

#[test]
fn a_programs_words_are_read_by_its_path() {
    let bin = "/Users//u/Library/Application Support/aterm/pkg/store/agy/1/bin/agy";
    let w = words_of(&format!("{bin} --effort high"), bin).expect("its");
    assert_eq!(w, ["--effort", "high"]);
    assert_eq!(words_of(bin, bin), Some(Vec::new()));
    assert_eq!(words_of("/bin/zsh -l", bin), None);
    assert_eq!(words_of(&format!("{bin}-x --effort high"), bin), None);
}

#[test]
fn a_blocks_exit_is_read_by_its_command() {
    let json = r#"{"blocks":[{"id":1,"state":"complete","exit":137,"cmdline":"sh /r/muse.sh"},{"id":2,"state":"complete","exit":0,"cmdline":" ( cd -- '/w' && exec '/p/agents/muse' 'resume' 's1' )"},{"id":3,"state":"executing","exit":null,"cmdline":"sh /r/muse.sh resume s1"}]}"#;
    assert_eq!(block_exit(json, "muse.sh"), Some(137));
    assert_eq!(block_exit(json, "resume' 's1"), Some(0));
    assert_eq!(block_exit(json, "make"), None);
    assert_eq!(block_exit(r#"{"blocks":[]}"#, "muse"), None);
}

#[test]
fn the_hosts_log_is_read_from_a_stamp_on() {
    let log = "1792253947.285 96443 INFO h: harness @s-1: the agent exited; read at the exit: removed\n\
               continued without a stamp\n\
               1792254007.290 96443 INFO h: harness @s-1: relaunch pid=1 step=ended:graceful-exit\n";
    let since = lines_since(log, 1_792_253_947_300);
    assert_eq!(since.len(), 1, "{since:?}");
    assert!(since[0].contains("step=ended:graceful-exit"));
    assert_eq!(lines_since(log, 1_792_253_947_285).len(), 2);
    assert!(lines_since(log, 1_792_254_007_291).is_empty());
}

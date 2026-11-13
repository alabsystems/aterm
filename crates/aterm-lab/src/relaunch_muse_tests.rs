// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::relaunch_check::words_of;

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// A Muse's command line is read by its program's path, spaces and all;
/// the session it resumes off its words.
#[test]
fn a_relaunched_muse_names_the_session_it_resumes() {
    let bin =
        "/Users//u/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";
    let cmd = format!("{bin} --base-url http://127.0.0.1:9 --trust-workspace resume 01a1-s");
    let w = words_of(&cmd, bin).expect("muse's");
    assert_eq!(resumed(&w), Some("01a1-s"));
    assert_eq!(words_of("/bin/zsh -l", bin), None);
    assert_eq!(words_of(&format!("{bin}-x resume 01a1-s"), bin), None);
    assert_eq!(resumed(&words("--base-url u")), None);
    assert_eq!(resumed(&words("resume")), None);
}

/// A session log's newest workspace. NEGATIVE CONTROL: a log naming none.
#[test]
fn the_newest_workspace_a_log_names() {
    let log = r#"{"workspace_root":"/a"}
{"x":1,"workspace_root":"/private/tmp/at.x/proj","y":2}"#;
    assert_eq!(workspace_in(log).as_deref(), Some("/private/tmp/at.x/proj"));
    assert_eq!(workspace_in("{}"), None);
}

/// A lock names its holder's pid; a truncated one names none.
#[test]
fn a_lock_names_its_holder() {
    assert_eq!(lock_pid("pid=42\nhost=h\n"), Some(42));
    assert_eq!(lock_pid(""), None);
}

/// The shell gets the launcher's environment but the data and config
/// homes, which the relaunch line carries; the twin execs the build.
#[test]
fn the_shell_reaches_the_stub_and_the_twin_execs_the_build() {
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        ..Run::default()
    };
    let s = shell_exports(&run);
    for want in [
        "META_API_KEY=",
        "TBH_AUTH_BASE_URL=http://127.0.0.1:5",
        "HTTPS_PROXY=http://127.0.0.1:6",
        "HOME=/s/runs/t/scratch/home",
    ] {
        assert!(s.contains(want), "{want}: {s}");
    }
    assert!(
        !s.contains("XDG_DATA_HOME") && !s.contains("XDG_CONFIG_HOME"),
        "{s}"
    );
    let t = twin_script("/a b/muse");
    assert!(
        t.starts_with("#!/bin/sh\n") && t.contains("exec '/a b/muse' \"$@\"\n"),
        "{t}"
    );
}

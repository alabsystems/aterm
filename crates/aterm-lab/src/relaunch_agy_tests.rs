// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// An agy's words name the conversation it resumes, any spelling.
#[test]
fn a_relaunched_agy_names_the_conversation_it_resumes() {
    assert_eq!(
        resumed(&words("--effort high --conversation=c1")),
        Some("c1")
    );
    assert_eq!(resumed(&words("-conversation c1")), Some("c1"));
    assert_eq!(resumed(&words("--effort high")), None);
    assert_eq!(resumed(&words("-c")), None);
    assert_eq!(resumed(&words("--conversations=c1")), None);
}

/// A presence lock names its conversation under its config. NEGATIVE
/// CONTROLS: another directory, another config.
#[test]
fn a_presence_lock_names_its_conversation() {
    let config = Path::new("/s/agy-home/.gemini/antigravity-cli");
    let c = "0a9c7e21-3b5d-4f60-9e18-2c4d6f8a0b1c";
    let lock = format!("{}/presence/{c}.lock", config.display());
    assert_eq!(lock_conversation(config, &lock).as_deref(), Some(c));
    let db = format!("{}/conversations/{c}.db", config.display());
    assert_eq!(lock_conversation(config, &db), None);
    assert_eq!(
        lock_conversation(config, &format!("/elsewhere/presence/{c}.lock")),
        None
    );
}

/// The shell gets the launcher's environment but its `HOME`, which the
/// relaunch line carries; the twin exports what atpkg's does and execs
/// the build; a stand-in build runs the lab's with its flag first.
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
        "GEMINI_API_KEY=",
        "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5",
        "HTTPS_PROXY=http://127.0.0.1:6",
        "HOME=/s/runs/t/scratch/home ",
    ] {
        assert!(s.contains(want), "{want}: {s}");
    }
    assert!(!s.contains("agy-home"), "{s}");
    let t = twin_script("/a b/agy");
    assert!(
        t.starts_with("#!/bin/sh\n")
            && t.contains("export AGY_CLI_DISABLE_AUTO_UPDATE='true'\n")
            && t.contains("exec '/a b/agy' \"$@\"\n"),
        "{t}"
    );
    assert_eq!(
        build_with("/a/agy", REFUSED_FLAG),
        "#!/bin/sh\nexec '/a/agy' '--lab-refused-flag' \"$@\"\n"
    );
}

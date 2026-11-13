// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{
    host_is_local, informative, informative_under, msys_home_suffix, msys_spelling,
    windows_home_suffix, without_console_program_path, without_identity_of,
};

const FULL: &str = "m17-tower.local";
const SHORT: &str = "m17-tower";

fn shed(title: &str) -> String {
    informative_under(title, None, None).to_string()
}

#[test]
fn windows_titles_read_the_profile_folder_as_a_tilde() {
    let home = Some(r"C:\Users\m6-an");
    let label = |title: &str| informative_under(title, None, home).to_string();
    for (title, want) in [
        (r"C:\Users\m6-an\aterm", r"~\aterm"),
        (r"C:\Users\m6-an", "~"),
        (r"c:\users\M6-AN\src\x", r"~\src\x"),
        (r"C:\Users\m6-an/src", "~/src"),
        (
            r"C:\Users\m6-an\aterm - ping -n 4 127.0.0.1",
            r"~\aterm - ping -n 4 127.0.0.1",
        ),
        (r"C:\Users\m6-an - ping x", "~ - ping x"),
        (r"C:\Users\m6-an2\src", r"C:\Users\m6-an2\src"),
        (r"C:\Users\m6-an.old", r"C:\Users\m6-an.old"),
        (r"D:\work", r"D:\work"),
        (r"C:\Windows", r"C:\Windows"),
        (r"~\aterm", r"~\aterm"),
        (r"vim C:\Users\m6-an\x.rs", r"vim C:\Users\m6-an\x.rs"),
        (r"C:\Windows\system32\cmd.exe", ""),
        ("MINGW64:/c/Users//m6-an/src", "/c/Users//m6-an/src"),
    ] {
        assert_eq!(label(title), want, "{title:?}");
    }
    assert!(!informative_under(r"C:\Users\m6-an", None, home).is_empty());
    assert_eq!(
        informative_under(r"C:\Users\m6-an\x", None, Some(r"C:\Users\m6-an\")).to_string(),
        r"~\x",
        "a profile folder spelled with its separator"
    );
    assert_eq!(
        informative_under("MINGW64:/c/Users//m6-an/src", Some("/c/Users//m6-an"), home).to_string(),
        "~/src",
        "Git Bash keeps its own rule"
    );
    assert_eq!(
        windows_home_suffix(r"C:\x", "C:"),
        None,
        "a drive is no home"
    );
    assert_eq!(windows_home_suffix(r"C:\x", r"C:\"), None, "nor its root");
    assert_eq!(
        shed(r"C:\Users\m6-an\aterm"),
        r"C:\Users\m6-an\aterm",
        "CONTROL"
    );
}

#[test]
fn git_bash_titles_lose_their_environment_prefix() {
    assert_eq!(
        shed("MINGW64:/c/Users//m6-an/src/aterm"),
        "/c/Users//m6-an/src/aterm"
    );
    assert_eq!(shed("UCRT64:/tmp"), "/tmp");
    assert_eq!(shed("MSYS:/home/me"), "/home/me");
    assert_eq!(shed("MINGW64: not a path"), "MINGW64: not a path");
    assert_eq!(shed("vim MINGW64:/x"), "vim MINGW64:/x");
    assert_eq!(
        shed("MINGW64:/c/Users//m6-an/ünï 日本"),
        "/c/Users//m6-an/ünï 日本"
    );
}

#[test]
fn git_bash_titles_read_home_as_a_tilde() {
    let home = Some("/c/Users//m6-an");
    let label = |title: &str| informative_under(title, home, None).to_string();
    assert_eq!(
        label("MINGW64:/c/Users//m6-an/aterm-winperfect"),
        "~/aterm-winperfect"
    );
    assert_eq!(label("MINGW64:/c/Users//m6-an"), "~");
    assert_eq!(label("MINGW64:/C/users/M6-AN/src"), "~/src");
    assert_eq!(label("UCRT64:/c/Users//m6-an/ünï 日本"), "~/ünï 日本");
    assert_eq!(label("MINGW64:/c/Users//m6-an2/src"), "/c/Users//m6-an2/src");
    assert_eq!(label("MINGW64:/d/work"), "/d/work");
    assert_eq!(
        label("/c/Users//m6-an/notes"),
        "/c/Users//m6-an/notes",
        "not Git Bash's title"
    );
    assert!(!informative_under("MINGW64:/c/Users//m6-an", home, None).is_empty());
    assert_eq!(
        shed("MINGW64:/c/Users//m6-an/aterm-winperfect"),
        "/c/Users//m6-an/aterm-winperfect",
        "CONTROL"
    );
    let mut slot = String::from("stale");
    slot.clear();
    informative_under("MINGW64:/c/Users//m6-an/src", home, None).push_to(&mut slot);
    assert_eq!(slot, "~/src");
}

#[test]
fn a_home_is_spelled_as_git_bash_spells_it() {
    assert_eq!(
        msys_spelling("C:\\Users\\m6-an").as_deref(),
        Some("/c/Users//m6-an")
    );
    assert_eq!(msys_spelling("D:/home/me/").as_deref(), Some("/d/home/me"));
    assert_eq!(msys_spelling("/home/me").as_deref(), Some("/home/me"));
    assert_eq!(msys_spelling("C:").as_deref(), Some("/c"));
    assert_eq!(msys_spelling("\\\\server\\share\\me"), None);
    assert_eq!(msys_spelling("relative\\me"), None);
    assert_eq!(msys_home_suffix("/c/users/x/y", "/C/Users//X/"), Some("/y"));
    assert_eq!(msys_home_suffix("/c/Users//xy", "/c/Users//x"), None);
    assert_eq!(msys_home_suffix("/c", "/c/Users//x"), None);
}

#[test]
fn every_name_the_kernel_gave_this_machine_stays_its_own() {
    let mut names = super::MachineNames::default();
    assert!(!names.names("m19Macb128GB2TB"), "nothing read yet");
    names.note(None);
    names.note(Some(String::new()));
    assert!(!names.names(""), "an unreadable name names nothing");
    names.note(Some("m19Macb128GB2TB.localdomain".into()));
    names.note(Some("m19-Macbook-M5-Max-128GB-2TB.local".into()));
    for host in [
        "m19Macb128GB2TB.localdomain",
        "m19Macb128GB2TB",
        "M19MACB128GB2TB",
        "m19-Macbook-M5-Max-128GB-2TB.local",
        "m19-Macbook-M5-Max-128GB-2TB",
    ] {
        assert!(names.names(host), "{host}");
    }
    assert!(!names.names("build-box.example"), "never this machine's");
    assert!(!names.names("m19"), "a prefix is not the name");
    for i in 0..super::MachineNames::KEPT {
        names.note(Some(format!("renamed-{i}.local")));
    }
    assert!(!names.names("m19Macb128GB2TB"), "the oldest name is gone");
    assert!(names.names("renamed-0"), "the kept ones stay");
}

#[test]
fn a_citation_host_is_this_machine_only_by_its_whole_name() {
    for (host, name) in [
        ("m19Macb128GB2TB", "m19Macb128GB2TB.localdomain"),
        ("m19Macb128GB2TB.local", "m19Macb128GB2TB.localdomain"),
        ("M19MACB128GB2TB.LOCAL", "m19Macb128GB2TB"),
        ("m19Macb128GB2TB.localdomain.", "m19Macb128GB2TB"),
        ("10.0.0.5", "10.0.0.5"),
        ("build-box.example", "build-box.example"),
    ] {
        assert!(super::names_machine(host, name), "{host} is {name}");
    }
    for (host, name) in [
        ("m19Macb128GB2TB.other.example", "m19Macb128GB2TB.local"),
        ("10.0.0.7", "10.0.0.5"),
        ("m19", "m19Macb128GB2TB.local"),
        ("build-box", "build-box.example"),
        ("", "m19Macb128GB2TB.local"),
        ("anything", ""),
        ("", ""),
    ] {
        assert!(!super::names_machine(host, name), "{host} is not {name}");
    }
    let mut names = super::MachineNames::default();
    names.note(Some("m17-tower.local".into()));
    assert!(names.names("m17-tower"));
    assert!(
        !names.names("m17-tower.corp.example"),
        "the first label alone"
    );
}

#[test]
fn the_names_a_predecessor_read_are_kept_as_older() {
    let mut names = super::MachineNames::default();
    names.note(Some("new-name.local".into()));
    names.adopt_older([
        "old-name.localdomain".to_string(),
        "NEW-NAME.local".to_string(),
    ]);
    assert_eq!(names.all().len(), 2, "once each: {:?}", names.all());
    assert_eq!(names.all()[0], "old-name.localdomain", "the older first");
    assert!(names.names("old-name"), "the predecessor's name");
    assert!(!names.names("other-box"), "never read by either");
    names.adopt_older((0..super::MachineNames::KEPT).map(|i| format!("gone-{i}")));
    assert_eq!(names.all().len(), super::MachineNames::KEPT);
    assert!(names.names("new-name"), "the newest stay");
    assert!(!names.names("gone-0"), "the oldest went first");
}

fn strip(title: &str) -> &str {
    without_identity_of(title, FULL, SHORT)
}

#[test]
fn only_this_machines_prompt_identity_is_shed() {
    for (title, want) in [
        ("user@m17-tower: ~/aterm", "~/aterm"),
        ("user@m17-tower.local: ~/aterm", "~/aterm"),
        ("user@m17-tower:~/aterm", "~/aterm"),
        ("user@m17-tower", ""),
        ("user@m17-tower:", ""),
        ("  user@m17-tower:   ", ""),
        ("root@prod-db-01: /var/log", "root@prod-db-01: /var/log"),
        ("user@m17-tower-2: ~/aterm", "user@m17-tower-2: ~/aterm"),
        ("vim README.md", "vim README.md"),
        ("~/aterm", "~/aterm"),
        ("", ""),
        (
            "sent to user@m17-tower: ok",
            "sent to user@m17-tower: ok",
        ),
        ("@m17-tower: ~/aterm", "@m17-tower: ~/aterm"),
        ("user@: ~/aterm", "user@: ~/aterm"),
    ] {
        assert_eq!(strip(title), want, "{title:?}");
    }
}

#[test]
fn an_unknown_local_name_matches_nothing() {
    assert_eq!(
        without_identity_of("user@m17-tower: ~/aterm", "", ""),
        "user@m17-tower: ~/aterm"
    );
    assert!(!host_is_local("m17-tower", "", ""));
}

#[test]
fn case_and_domain_spellings_of_this_machine_all_match() {
    assert!(host_is_local("M17-Tower", FULL, SHORT));
    assert!(host_is_local("m17-tower.lan", FULL, SHORT));
    assert!(!host_is_local("m17-towerx", FULL, SHORT));
}

#[test]
fn only_a_bare_console_program_path_is_shed() {
    for path in [
        "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe",
        "C:\\Windows\\system32\\cmd.exe",
        "c:/windows/system32/CMD.EXE",
        "\\\\build-01\\tools\\shell.exe",
        "  C:\\Windows\\system32\\cmd.exe  ",
        "C:\\Windows\\system32\\cmd.exe - ",
    ] {
        assert_eq!(without_console_program_path(path), "", "{path:?}");
        assert!(informative(path).is_empty(), "{path:?}");
    }
    for (title, want) in [
        (
            "C:\\Windows\\system32\\cmd.exe - ping  build-01",
            "ping  build-01",
        ),
        (
            "C:\\Windows\\system32\\cmd.exe - C:\\tools\\run.exe",
            "C:\\tools\\run.exe",
        ),
        ("~\\aterm", "~\\aterm"),
        ("/usr/bin/bash", "/usr/bin/bash"),
        ("pwsh.exe", "pwsh.exe"),
        ("bin\\tool.exe", "bin\\tool.exe"),
        (
            "C:\\apps\\foo.exe.bak\\notes.txt",
            "C:\\apps\\foo.exe.bak\\notes.txt",
        ),
        ("C:\\Users\\x\\aterm", "C:\\Users\\x\\aterm"),
        (
            "running C:\\tools\\run.exe now",
            "running C:\\tools\\run.exe now",
        ),
        ("C:\\tools\\run.exe.", "C:\\tools\\run.exe."),
        ("C:\\tools\\run.exe -v", "C:\\tools\\run.exe -v"),
        ("C:", "C:"),
        ("", ""),
        ("user@m17-tower: ~/aterm", "user@m17-tower: ~/aterm"),
    ] {
        assert_eq!(without_console_program_path(title), want, "{title:?}");
    }
}

/// Windows names this machine from the OS.
#[cfg(windows)]
#[test]
fn windows_names_this_machine_without_a_child() {
    let name = super::resolve_hostname().expect("Windows names this machine");
    assert!(
        !name.is_empty() && !name.contains(char::is_whitespace),
        "{name:?}"
    );
    let short = name
        .split_once('.')
        .map_or(name.as_str(), |(label, _)| label);
    let prompt = format!("me@{}: ~", name.to_ascii_uppercase());
    assert_eq!(without_identity_of(&prompt, &name, short), "~");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn parent() -> Vec<(String, String)> {
    pairs(&[
        ("SYSTEMROOT", r"C:\WINDOWS"),
        ("SystemDrive", "C:"),
        ("Path", r"C:\WINDOWS\system32"),
        ("LOCALAPPDATA", r"C:\Users\owner\AppData\Local"),
        ("APPDATA", r"C:\Users\owner\AppData\Roaming"),
        ("USERPROFILE", r"C:\Users\owner"),
        ("TEMP", r"C:\Users\owner\AppData\Local\Temp"),
        ("ATERM_CHILD", "1"),
        (
            "ATERM_CONTROL_SOCK",
            r"C:\Users\owner\AppData\Local\Temp\aterm\aterm-1.sock",
        ),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("XDG_CONFIG_HOME", r"C:\Users\owner\.config"),
    ])
}

#[test]
fn isolated_keeps_the_machine_and_redirects_every_per_user_root() {
    let roots = Roots::under(Path::new(r"C:\scratch\w"));
    let env = isolated(&parent(), &roots).expect("a whole environment isolates");
    // Windows names differ only in case: the parent's SYSTEMROOT and Path are kept.
    assert_eq!(lookup(&env, "SystemRoot"), Some(r"C:\WINDOWS"));
    assert_eq!(lookup(&env, "SystemDrive"), Some("C:"));
    assert_eq!(lookup(&env, "PATH"), Some(r"C:\WINDOWS\system32"));
    for name in ["TMP", "TEMP", "LOCALAPPDATA", "APPDATA", "USERPROFILE"] {
        let value = lookup(&env, name).expect(name);
        assert!(value.starts_with(r"C:\scratch\w"), "{name}={value}");
    }
    // Negative control: nothing of the owner's session or roots survives.
    for name in [
        "ATERM_CHILD",
        "ATERM_CONTROL_SOCK",
        "CLAUDE_CODE_ENTRYPOINT",
        "XDG_CONFIG_HOME",
    ] {
        assert_eq!(lookup(&env, name), None, "{name} leaked");
    }
    assert!(env.iter().all(|(_, v)| !v.contains("owner")), "{env:?}");
    // Each name once: a second TEMP would leave the winner to the OS.
    for (i, (name, _)) in env.iter().enumerate() {
        assert!(
            env.iter()
                .skip(i + 1)
                .all(|(other, _)| !other.eq_ignore_ascii_case(name)),
            "{name} twice"
        );
    }
}

#[test]
fn isolated_refuses_an_environment_without_the_system_drive() {
    let roots = Roots::under(Path::new(r"C:\scratch\w"));
    let stripped: Vec<_> = parent()
        .into_iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("SystemDrive"))
        .collect();
    let refused = isolated(&stripped, &roots).expect_err("no SystemDrive");
    assert!(refused.contains("SystemDrive"), "{refused}");
    let empty = pairs(&[("SystemRoot", ""), ("SystemDrive", "C:")]);
    assert!(
        isolated(&empty, &roots).is_err(),
        "an empty SystemRoot is no SystemRoot"
    );
    // Control: the same environment with both isolates.
    assert!(isolated(&parent(), &roots).is_ok());
}

#[test]
fn sock_path_stays_inside_a_short_root_and_leaves_a_long_one() {
    let short = Path::new(r"C:\Temp");
    let (sock, extra) = sock_path(Path::new(r"C:\scratch\w"), short, "aws-1").expect("short root");
    assert!(sock.starts_with(r"C:\scratch\w"), "{}", sock.display());
    assert_eq!(extra, None);

    let long = PathBuf::from(format!(r"C:\{}", "d".repeat(SOCK_PATH_MAX)));
    let (sock, extra) = sock_path(&long, short, "aws-1").expect("falls back");
    assert!(sock.starts_with(short), "{}", sock.display());
    assert!(sock.as_os_str().len() <= SOCK_PATH_MAX);
    assert_eq!(extra.as_deref(), sock.parent());

    // Negative control: no fallback that fits is refused, never truncated.
    assert!(sock_path(&long, &long, "aws-1").is_err());
}

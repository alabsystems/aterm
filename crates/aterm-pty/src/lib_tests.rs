// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn build_child_env_drops_denylisted_and_keeps_overrides() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let inherited = vec![
        (os("PATH"), os("/usr/bin")),
        (os("ATERM_SESSION_ID"), os("s-0123456789abcdef0123")),
        (os("ANTHROPIC_API_KEY"), os("secret")),
        (os("CLAUDECODE"), os("1")),
        (os("CURSOR_TRACE_ID"), os("xyz")),
        (os("TERM"), os("dumb")),
    ];
    let env_add = vec![("TERM".to_string(), "xterm-256color".to_string())];
    let out = build_child_env(inherited.into_iter(), &env_add);
    let keys: Vec<String> = out
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for denied in [
        "ATERM_SESSION_ID",
        "ANTHROPIC_API_KEY",
        "CLAUDECODE",
        "CURSOR_TRACE_ID",
    ] {
        assert!(
            !keys.contains(&denied.to_string()),
            "{denied} must be filtered from the child env, got {keys:?}"
        );
    }
    assert!(
        keys.contains(&"PATH".to_string()),
        "PATH must pass through to the child"
    );
    let terms: Vec<&OsString> = out
        .iter()
        .filter(|(k, _)| k == &os("TERM"))
        .map(|(_, v)| v)
        .collect();
    assert_eq!(terms.len(), 1, "TERM must appear exactly once");
    assert_eq!(
        terms[0],
        &os("xterm-256color"),
        "env_add must override inherited TERM"
    );
}

/// The default-editor hint is per hop (docs/DESIGN-amacs-2026-10-19.md §8.3, W1): an
/// aterm started from a tab drops the hint it inherited and hands its own, while the
/// person's `EDITOR` and `VISUAL` pass through.
/// NEGATIVE CONTROL: with no hint of its own in `env_add`, the child gets none, though one
/// was inherited.
#[test]
fn build_child_env_rederives_the_default_editor_hint_per_hop() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let hint = aterm_types::domain::ENV_DEFAULT_EDITOR;
    let inherited = || {
        vec![
            (os(hint), os("amacs")),
            (os("EDITOR"), os("vi")),
            (os("VISUAL"), os("nano")),
        ]
        .into_iter()
    };
    let value = |out: &[(OsString, OsString)], key: &str| {
        out.iter()
            .filter(|(k, _)| k == &os(key))
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let own = vec![(hint.to_string(), "amacs".to_string())];
    let out = build_child_env(inherited(), &own);
    assert_eq!(value(&out, hint), ["amacs"], "this hop's own hint, once");
    assert_eq!(value(&out, "EDITOR"), ["vi"]);
    assert_eq!(value(&out, "VISUAL"), ["nano"]);
    let out = build_child_env(inherited(), &[]);
    assert!(value(&out, hint).is_empty(), "an inherited hint is dropped");
    assert_eq!(value(&out, "EDITOR"), ["vi"]);
}

/// An `EDITOR=amacs` an outer aterm's tab exported is that aterm's default, not the
/// person's: an aterm started from such a tab drops it, so its own setting (the opt-out
/// included) decides again, while a person's `EDITOR` passes through.
/// NEGATIVE CONTROL: the same `EDITOR=amacs` with no `ATERM_CHILD` (an aterm started from
/// the Dock or another terminal) reaches the child, as do `EDITOR=vi` under `ATERM_CHILD`
/// and an `EDITOR` this hop sets itself.
#[test]
fn build_child_env_drops_an_editor_an_outer_aterm_tab_exported() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let editor = |out: &[(OsString, OsString)]| {
        out.iter()
            .filter(|(k, _)| k == "EDITOR")
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let from_tab =
        |value: &str| vec![(os("ATERM_CHILD"), os("1")), (os("EDITOR"), os(value))].into_iter();
    let out = build_child_env(from_tab("amacs"), &[]);
    assert!(editor(&out).is_empty(), "{out:?}");
    assert!(out.iter().any(|(k, _)| k == "ATERM_CHILD"), "{out:?}");
    assert_eq!(editor(&build_child_env(from_tab("vi"), &[])), ["vi"]);
    let own = vec![("EDITOR".to_string(), "amacs".to_string())];
    assert_eq!(editor(&build_child_env(from_tab("amacs"), &own)), ["amacs"]);
    let outside = vec![(os("EDITOR"), os("amacs"))].into_iter();
    assert_eq!(editor(&build_child_env(outside, &[])), ["amacs"]);
}

/// aterm launched from another terminal's tab: that terminal's window, pane and
/// socket names do not reach aterm's shell, aterm's own identity does, and a
/// multiplexer's or a user's configuration variable is left alone.
#[test]
fn build_child_env_drops_another_terminals_identity() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let inherited = vec![
        (os("PATH"), os("/usr/bin")),
        (os("WINDOWID"), os("4242")),
        (os("VTE_VERSION"), os("7600")),
        (
            os("GNOME_TERMINAL_SCREEN"),
            os("/org/gnome/Terminal/screen/x"),
        ),
        (os("GNOME_TERMINAL_SERVICE"), os(":1.99")),
        (os("KITTY_WINDOW_ID"), os("3")),
        (os("KONSOLE_DBUS_SERVICE"), os(":1.7")),
        (os("TERM_PROGRAM"), os("WezTerm")),
        (os("TMUX"), os("/tmp/tmux-1000/default,9,0")),
        (os("KITTY_CONFIG_DIRECTORY"), os("/home/u/kitty")),
    ];
    let env_add = vec![("TERM_PROGRAM".to_string(), "aterm".to_string())];
    let out = build_child_env(inherited.into_iter(), &env_add);
    let pairs: Vec<(String, String)> = out
        .iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    assert_eq!(
        pairs,
        vec![
            pair("PATH", "/usr/bin"),
            pair("TERM_PROGRAM", "aterm"),
            pair("TMUX", "/tmp/tmux-1000/default,9,0"),
            pair("KITTY_CONFIG_DIRECTORY", "/home/u/kitty"),
        ]
    );
}

/// aterm launched with its compositor connection handed down (`WAYLAND_SOCKET`): no
/// shell gets the variable, with or without the terminal claimed, since the
/// descriptor it names is closed in every child, and a Wayland client run from the
/// shell would take that number as its connection; `WAYLAND_DISPLAY` reaches it.
#[test]
fn build_child_env_drops_a_compositor_connection_handed_down_by_number() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    for env_add in [
        vec![("TERM_PROGRAM".to_string(), "aterm".to_string())],
        Vec::new(),
    ] {
        let inherited = vec![
            (os("WAYLAND_SOCKET"), os("5")),
            (os("WAYLAND_DISPLAY"), os("wayland-0")),
            (os("PATH"), os("/usr/bin")),
        ];
        let keys: Vec<String> = build_child_env(inherited.into_iter(), &env_add)
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .filter(|k| k != "TERM_PROGRAM")
            .collect();
        assert_eq!(keys, ["WAYLAND_DISPLAY", "PATH"], "env_add {env_add:?}");
    }
}

/// The passthrough session of an `aterm` typed at a prompt: its `env_add` holds the
/// session PATH and no `TERM_PROGRAM`, so the outer terminal stays the shell's
/// terminal and its identity variables reach the shell unchanged.
#[test]
fn build_child_env_keeps_the_outer_terminals_identity_without_a_claim() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let inherited = vec![
        (os("WINDOWID"), os("4242")),
        (os("VTE_VERSION"), os("7600")),
        (os("KITTY_WINDOW_ID"), os("3")),
        (os("KONSOLE_DBUS_SERVICE"), os(":1.7")),
        (os("TERM_PROGRAM"), os("kitty")),
        (os("PATH"), os("/usr/bin")),
    ];
    let env_add = vec![("PATH".to_string(), "/opt/aterm/bin:/usr/bin".to_string())];
    let out = build_child_env(inherited.into_iter(), &env_add);
    let pairs: Vec<(String, String)> = out
        .iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    assert_eq!(
        pairs,
        vec![
            pair("WINDOWID", "4242"),
            pair("VTE_VERSION", "7600"),
            pair("KITTY_WINDOW_ID", "3"),
            pair("KONSOLE_DBUS_SERVICE", ":1.7"),
            pair("TERM_PROGRAM", "kitty"),
            pair("PATH", "/opt/aterm/bin:/usr/bin"),
        ]
    );
}

/// A non-UTF-8 key with a denied prefix is still denied; the last key is the control.
#[cfg(unix)]
#[test]
fn a_non_utf8_key_with_a_denied_prefix_is_still_denied() {
    use std::os::unix::ffi::OsStringExt as _;
    let raw = |b: &[u8]| std::ffi::OsString::from_vec(b.to_vec());
    let inherited = vec![
        (raw(b"ANTHROPIC_\xffX"), raw(b"secret")),
        (raw(b"CLAUDE\xfe"), raw(b"1")),
        (raw(b"PLAIN_\xffKEY"), raw(b"kept")),
    ];
    let keys: Vec<Vec<u8>> = build_child_env(inherited.into_iter(), &[])
        .into_iter()
        .map(|(k, _)| k.into_vec())
        .collect();
    assert_eq!(keys, vec![b"PLAIN_\xffKEY".to_vec()]);
}

/// An identity's agent-home variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) arrives through
/// `env_add`: it replaces the parent's, and no other `CLAUDE*`/`CODEX_*` key survives.
#[test]
fn build_child_env_replaces_the_parents_agent_home_with_the_identitys() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let table = [
        ("CLAUDE_CONFIG_DIR", "/state/identities/worker/.claude"),
        ("CODEX_HOME", "/state/identities/worker/.codex"),
    ];
    for (var, identity_value) in table {
        let inherited = vec![
            (os("HOME"), os("/Users//human")),
            (os(var), os("/Users//human/agent-config-of-the-human")),
            (os("CLAUDECODE"), os("1")),
            (os("CLAUDE_CODE_ENTRYPOINT"), os("cli")),
            (os("CODEX_SANDBOX"), os("seatbelt")),
            (os("PATH"), os("/usr/bin")),
        ];
        let env_add = vec![(var.to_string(), identity_value.to_string())];
        let out = build_child_env(inherited.into_iter(), &env_add);
        let values: Vec<(String, String)> = out
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let of_var: Vec<&str> = values
            .iter()
            .filter(|(k, _)| k == var)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            of_var,
            vec![identity_value],
            "{var}: exactly once, and it is the identity's value, not the parent's"
        );
        assert!(
            !values
                .iter()
                .any(|(_, v)| v.contains("agent-config-of-the-human")),
            "{var}: the parent's value is gone from the child env: {values:?}"
        );
        let agent_keys: Vec<&str> = values
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| k.starts_with("CLAUDE") || k.starts_with("CODEX_"))
            .collect();
        assert_eq!(
            agent_keys,
            vec![var],
            "{var}: no other CLAUDE*/CODEX_* key of the parent's rides along"
        );
        assert!(
            values
                .iter()
                .any(|(k, v)| k == "HOME" && v == "/Users//human"),
            "HOME is the human's: an identity moves the agent's config, not the user"
        );
    }
}

#[cfg(windows)]
#[test]
fn build_child_env_denies_mixed_case_keys_on_windows() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let inherited = vec![
        (os("Path"), os("C:\\Windows")),
        (os("anthropic_api_key"), os("secret")),
        (os("Claude_Code"), os("1")),
        (os("Aterm_Update_Token"), os("secret")),
        (os("cursor_trace_id"), os("xyz")),
        (os("PATH"), os("C:\\StaleDuplicate")),
    ];
    let env_add = vec![("PATH".to_string(), "C:\\Tools;C:\\Windows".to_string())];
    let out = build_child_env(inherited.into_iter(), &env_add);
    let pairs: Vec<(String, String)> = out
        .iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![("PATH".to_string(), "C:\\Tools;C:\\Windows".to_string())],
        "mixed-case deny-listed keys must be filtered and mixed-case overrides collapsed on Windows"
    );
}

#[cfg(unix)]
#[test]
fn build_child_env_stays_case_sensitive_on_unix() {
    use std::ffi::OsString;
    let os = |s: &str| OsString::from(s);
    let inherited = vec![
        (os("anthropic_api_key"), os("distinct-var")),
        (os("Path"), os("/custom")),
        (os("PATH"), os("/old-a")),
        (os("PATH"), os("/old-b")),
    ];
    let env_add = vec![("PATH".to_string(), "/new".to_string())];
    let out = build_child_env(inherited.into_iter(), &env_add);
    assert_eq!(
        out,
        vec![
            (os("anthropic_api_key"), os("distinct-var")),
            (os("Path"), os("/custom")),
            (os("PATH"), os("/new")),
        ],
        "exact-case deny and override must stay case-sensitive while collapsing duplicate exact keys on unix"
    );
}

#[test]
fn pty_echo_signals_identity_and_exit_const_helpers() {
    let sigs = TtySignals::posix_defaults();
    assert!(sigs.is_intr(0x03) && sigs.is_quit(0x1c) && sigs.is_susp(0x1a));
    assert!(sigs.signals(0x03) && !sigs.signals(b'a'));
    assert!(!TtySignals::disabled().signals(0x03));

    let cooked = TtyEcho {
        echo: true,
        canonical: true,
        signals: Some(sigs),
    };
    assert!(cooked.is_cooked_echo() && !cooked.is_raw_mode() && !cooked.swallows_input());
    assert!(cooked.intercepts_signal_byte(0x03));

    let raw = TtyEcho {
        echo: false,
        canonical: false,
        signals: None,
    };
    assert!(raw.is_raw_mode() && !raw.intercepts_signal_byte(0x03));

    let id = ShellIdentity::from_parts(42, Some(1000), true);
    assert!(id.has_positive_pid() && id.has_birth_record());
    assert!(id.matches_kernel_birth(42, 1000));
    assert!(!id.matches_kernel_birth(42, 999));
    assert!(!ShellIdentity::unproven(-1).has_positive_pid());

    assert_eq!(ShellHangup::Shell(42).signalled_pgid(), Some(42));
    assert!(ShellHangup::Foreground(9).did_signal());
    assert!(!ShellHangup::Nothing.did_signal());

    assert!(ChildExit::Code(0).is_success());
    assert_eq!(ChildExit::Code(2).exit_code(), Some(2));
    assert_eq!(ChildExit::Signal(9).signal_number(), Some(9));
    assert_eq!(ChildExit::Signal(9).shell_status_code(), 137);
    assert!(
        SpawnedShell {
            master: 3,
            pid: 100
        }
        .has_valid_handles()
    );
    assert_eq!(clamp_winsize_cells(0, 80), 80);
    assert_eq!(clamp_winsize_cells(0, 0), 1);
    let ws = encode_winsize_le(80, 24, 640, 384);
    assert_eq!(decode_winsize_le(ws), (80, 24, 640, 384));
}

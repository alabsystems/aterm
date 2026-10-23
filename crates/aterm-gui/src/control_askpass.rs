// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Handler for the `askpass` control verb: verifies `Scope::Owner`, attests that the peer is `aterm` under `/usr/bin/sudo`, and forwards the prompt to the window (`Wake::Askpass`).

#[cfg(target_os = "macos")]
use std::time::Instant;

use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
#[cfg(target_os = "macos")]
use crate::askpass::{ASKPASS_DEADLINE, ASKPASS_TICK, AskpassRequest, await_answer};
use crate::control::Scope;

/// Build a bounded public reply, including refusals on unsupported platforms.
pub(crate) fn public_reply(line: &str) -> aterm_uds::sudo::WireBuffer {
    aterm_uds::sudo::WireBuffer::from_bytes(line.as_bytes()).unwrap_or_else(|| {
        aterm_uds::sudo::WireBuffer::from_bytes(b"ERR askpass refused response too long\n")
            .expect("bounded public refusal")
    })
}

/// Serve one `askpass` line. Returns the reply line to write to the peer.
#[cfg(target_os = "macos")]
pub(crate) fn serve(
    stream: &aterm_uds::CtlStream,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    store: &crate::session_store::Store,
) -> aterm_uds::sudo::WireBuffer {
    if scope != Scope::Owner {
        return public_reply("ERR askpass refused owner-only\n");
    }

    // Refuse escalation while any session is held.
    if let Some(refusal) = crate::fabric::app_halt_refusal(store, "askpass") {
        return public_reply(&refusal);
    }

    if crate::askpass::relay_off() {
        return public_reply("ERR askpass refused off\n");
    }

    let Ok(peer) = aterm_uds::fdpass::peer_pid(stream) else {
        return public_reply("ERR askpass refused the peer is unreadable\n");
    };
    let peer = peer as i32;
    if !peer_is_our_binary(peer) {
        return public_reply("ERR askpass refused the peer is not aterm\n");
    }
    let Some(sudo_pid) = aterm_uds::sudo::parent_of(peer) else {
        return public_reply("ERR askpass refused the peer has no parent\n");
    };
    if let Err(why) = aterm_uds::sudo::attest_sudo(sudo_pid) {
        return public_reply(&format!("ERR askpass refused {why}\n"));
    }

    // Classify the command from exact kernel argv (`KERN_PROCARGS2`), failing closed when unreadable.
    let argv = u32::try_from(sudo_pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
        .map(|args| args.argv);
    match sudo_target_is_debugger(argv.as_deref()) {
        Ok(true) => return public_reply("ERR askpass refused debugger-as-root\n"),
        Err(()) => return public_reply("ERR askpass refused command-unreadable\n"),
        Ok(false) => {}
    }

    let invoker_pid = aterm_uds::sudo::parent_of(sudo_pid).unwrap_or(sudo_pid);
    if crate::spare::askpass_refused(invoker_pid) {
        return public_reply("ERR askpass refused no such session\n");
    }
    let command = ps_command_line(sudo_pid);
    let invoker = ps_command_line(invoker_pid);
    if command.is_empty() {
        return public_reply("ERR askpass refused command-unreadable\n");
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let ticket = std::sync::Arc::new(crate::askpass::AskpassTicket::new(
        crate::alert_keys::next_confirm_id(),
        Instant::now() + ASKPASS_DEADLINE,
    ));
    let request = AskpassRequest {
        ticket: std::sync::Arc::clone(&ticket),
        attribute_from: invoker_pid,
        command,
        invoker,
        reply: tx,
    };
    if proxy.send_event(Wake::Askpass(Box::new(request))).is_err() {
        return public_reply("ERR askpass refused the window is gone\n");
    }

    let mut hung = || crate::control::caller_hung_up();
    let mut now = Instant::now;
    let response = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    let _ = proxy.send_event(Wake::AskpassWithdraw { id: ticket.id });
    response
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn serve(
    _stream: &aterm_uds::CtlStream,
    _scope: Scope,
    _proxy: &EventLoopProxy<Wake>,
    _store: &crate::session_store::Store,
) -> aterm_uds::sudo::WireBuffer {
    public_reply("ERR askpass refused unsupported platform\n")
}

/// Returns true if `pid`'s canonical executable path matches this instance's binary.
#[cfg(target_os = "macos")]
fn peer_is_our_binary(pid: i32) -> bool {
    let canon = |p: std::path::PathBuf| p.canonicalize().ok();
    let ours = std::env::current_exe().ok().and_then(canon);
    let theirs = aterm_uds::sudo::exe_path(pid).and_then(canon);
    match (ours, theirs) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Reads and escapes `pid`'s command line via `/bin/ps` for display on the sheet.
#[cfg(target_os = "macos")]
fn ps_command_line(pid: i32) -> String {
    if pid <= 0 {
        return String::new();
    }
    let out = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-ww", "-o", "args=", "-p", &pid.to_string()])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .output();
    let line = out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    crate::askpass::escape_display(&line)
}

/// Classifies whether `sudo`'s direct target executable in `argv` is a debugger, returning `Err(())` if `argv` is absent or ambiguous.
#[cfg(any(target_os = "macos", test))]
fn sudo_target_is_debugger(argv: Option<&[String]>) -> Result<bool, ()> {
    let mut args = argv.ok_or(())?.iter();
    args.next().filter(|arg| !arg.is_empty()).ok_or(())?;
    let mut options = true;
    while let Some(arg) = args.next() {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && let Some(long) = arg.strip_prefix("--") {
            let (name, value) = long
                .split_once('=')
                .map_or((long, None), |(name, value)| (name, Some(value)));
            match name {
                "auth-type" | "close-from" | "login-class" | "chdir" | "group" | "host"
                | "prompt" | "chroot" | "role" | "type" | "other-user" | "command-timeout"
                | "user" => {
                    if value.is_none() {
                        args.next().ok_or(())?;
                    }
                }
                "preserve-env" => {} // Optional list must be attached with '='.
                "askpass" | "bell" | "background" | "edit" | "set-home" | "help" | "login"
                | "remove-timestamp" | "reset-timestamp" | "list" | "no-update"
                | "non-interactive" | "preserve-groups" | "stdin" | "shell" | "version"
                | "validate"
                    if value.is_none() => {}
                _ => return Err(()),
            }
            continue;
        }
        if options && arg.starts_with('-') && arg != "-" {
            let short = &arg[1..];
            for (at, flag) in short.char_indices() {
                match flag {
                    'a' | 'C' | 'c' | 'D' | 'g' | 'h' | 'p' | 'R' | 'r' | 't' | 'T' | 'U' | 'u' => {
                        if at + flag.len_utf8() == short.len() {
                            args.next().ok_or(())?;
                        }
                        break; // The remainder, if any, is this option's value.
                    }
                    'A' | 'B' | 'b' | 'E' | 'e' | 'H' | 'i' | 'K' | 'k' | 'l' | 'N' | 'n' | 'P'
                    | 'S' | 's' | 'V' | 'v' => {}
                    _ => return Err(()),
                }
            }
            continue;
        }
        if let Some((name, _)) = arg.split_once('=') {
            if name.is_empty() || name.contains('/') {
                return Err(());
            }
            continue; // sudo's command environment, not the executable.
        }
        let base = arg
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or(())?;
        return Ok(matches!(
            base,
            "lldb" | "debugserver" | "lldb-server" | "gdb" | "dtrace"
        ));
    }
    Err(())
}

#[cfg(test)]
mod debugger_tests {
    use super::sudo_target_is_debugger;

    fn classify(argv: &[&str]) -> bool {
        let argv = argv
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        sudo_target_is_debugger(Some(&argv)).expect("complete, supported exact argv")
    }

    /// Recognizes a root debugger past `sudo` flags and environment assignments only when it is the target command.
    #[test]
    fn debugger_target_is_recognized_only_as_the_command() {
        let cases: &[(&str, &[&str], bool)] = &[
            ("bare debugger", &["sudo", "lldb", "./a.out"], true),
            (
                "after -u and a path",
                &["sudo", "-u", "root", "/usr/bin/debugserver", "-", "./a.out"],
                true,
            ),
            (
                "absolute sudo, flags",
                &["/usr/bin/sudo", "-A", "-k", "gdb"],
                true,
            ),
            ("dtrace", &["sudo", "dtrace", "-n", "syscall:::"], true),
            (
                "after --user's value",
                &["sudo", "--user", "root", "lldb"],
                true,
            ),
            (
                "after --prompt's value",
                &["sudo", "--prompt", "Password:", "gdb"],
                true,
            ),
            (
                "after clustered -Anu's value",
                &["sudo", "-Anu", "root", "/usr/bin/debugserver"],
                true,
            ),
            ("after an assignment", &["sudo", "FOO=bar", "lldb"], true),
            (
                "prompt value holding a space",
                &["sudo", "--prompt", "hello make", "lldb"],
                true,
            ),
            (
                "whitespace inside the path",
                &["sudo", "/tmp/private tools/lldb", "./a.out"],
                true,
            ),
            (
                "after --user= and --",
                &["sudo", "--user=root", "--", "lldb"],
                true,
            ),
            ("after attached -uroot", &["sudo", "-uroot", "lldb"], true),
            ("path argument", &["sudo", "rm", "-rf", "/tmp/lldb"], false),
            (
                "package argument",
                &["sudo", "apt", "install", "gdb"],
                false,
            ),
            ("ordinary command", &["sudo", "make", "install"], false),
            (
                "argument after --user's value",
                &["sudo", "--user", "root", "make", "lldb"],
                false,
            ),
            (
                "assignment value",
                &["sudo", "FOO=lldb", "make", "install"],
                false,
            ),
            (
                "same text, split prompt",
                &["sudo", "--prompt", "hello", "make", "lldb"],
                false,
            ),
            (
                "argument after a spaced path",
                &["sudo", "/tmp/private tools/make", "lldb"],
                false,
            ),
            ("option after --", &["sudo", "--", "--user", "lldb"], false),
            (
                "argument after --preserve-env=",
                &["sudo", "--preserve-env=PATH,HOME", "make", "lldb"],
                false,
            ),
        ];
        let wrong = cases
            .iter()
            .filter(|(_, argv, debugger)| classify(argv) != *debugger)
            .map(|(case, argv, debugger)| format!("{case}: {argv:?} should be {debugger}"))
            .collect::<Vec<_>>();
        assert!(wrong.is_empty(), "misclassified: {wrong:#?}");
    }

    #[test]
    fn absent_or_unclassifiable_argv_never_admits_a_sheet() {
        assert!(sudo_target_is_debugger(None).is_err());
        for argv in [
            vec![],
            vec!["(sudo)".into()],
            vec!["sudo".into(), "--user".into()],
            vec!["sudo".into(), "--new-option".into(), "lldb".into()],
            vec!["sudo".into(), "-Z".into(), "make".into()],
            vec!["sudo".into(), "FOO=bar".into()],
            vec!["sudo".into(), "odd/name=value".into(), "lldb".into()],
            vec!["sudo".into(), "=value".into(), "lldb".into()],
        ] {
            assert!(sudo_target_is_debugger(Some(&argv)).is_err(), "{argv:?}");
        }
    }
}

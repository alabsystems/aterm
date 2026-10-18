// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `askpass` control verb's lane handler (`docs/DESIGN-sudo-askpass-2026-10-15.md`
//! §3.4). It runs on a wait lane, holding the connecting peer's stream, so it
//! reads the peer once and gates on `Scope::Owner` EXACTLY (not owner-class, so
//! a bridge is refused like an edge). It attests the peer is aterm's own binary
//! under Apple's `/usr/bin/sudo`, reads what to show from the kernel, hands the
//! request to the window (`Wake::Askpass`), and parks for the owner's answer —
//! returning the one reply, which reaches the peer through the lane's reply
//! buffer (`Outbox`) like any other and is zeroed there by `Outbox::scrub`
//! (`write_volatile`, once delivered and on drop), and never logging the secret.

#[cfg(target_os = "macos")]
use std::time::Instant;

use winit::event_loop::EventLoopProxy;

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
    // Owner EXACTLY: every in-session client holds the Owner token, but a child
    // edge or a bridge must never reach a surface that releases the owner's
    // password.
    if scope != Scope::Owner {
        return public_reply("ERR askpass refused owner-only\n");
    }

    // A held session must not escalate to root: `askpass` is `is_pty_reaching`,
    // but it is intercepted before the generic halt gate, so enforce it here
    // (scope-blind, as `app_halt_refusal` is — if any session is held, refuse).
    if let Some(refusal) = crate::fabric::app_halt_refusal(store, "askpass") {
        return public_reply(&refusal);
    }

    // The off marker (design §3.1): no sheet when it exists or cannot be
    // checked. Read synchronously on the lane; no control verb writes it, and
    // the owner's Settings button is still planned.
    if crate::askpass::relay_off() {
        return public_reply("ERR askpass refused off\n");
    }

    // The peer is the `aterm-askpass` helper. It must be aterm's own binary, and
    // its parent must be the genuine setuid-root `/usr/bin/sudo`.
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

    // Policy needs real argument boundaries: ps joins argv, so a prompt,
    // environment value or executable path with spaces cannot be classified
    // from its display text. KERN_PROCARGS2 is commonly refused for setuid-root
    // sudo; that is an unavailable proof, never permission to use the display.
    let argv = u32::try_from(sudo_pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
        .map(|args| args.argv);
    match sudo_target_is_debugger(argv.as_deref()) {
        Ok(true) => return public_reply("ERR askpass refused debugger-as-root\n"),
        Err(()) => return public_reply("ERR askpass refused command-unreadable\n"),
        Ok(false) => {}
    }

    // The ps line remains a best-effort DISPLAY, never an argv witness.
    // `attribute_from` is sudo's parent: the window attributes from there,
    // past sudo (whose PROC_PIDTBSDINFO the session walker cannot read).
    let invoker_pid = aterm_uds::sudo::parent_of(sudo_pid).unwrap_or(sudo_pid);
    // A warm spare's rc asks nobody (`crate::spare`): refused, and the spare
    // tainted; one from a session before its start record is counted.
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
    // Cancelling the shared ticket also fences a request the main loop has not
    // presented yet. This wake retires an already-visible sheet by exact id.
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

/// Whether `pid`'s executable is this instance's own aterm binary (so the peer
/// is the real `aterm-askpass`, not some other same-uid process). Canonicalised
/// both sides, so a symlink-named launch still matches.
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

/// `sudo`'s and the invoker's command line for the sheet, read with setuid
/// `/bin/ps` (joined and lossy — the sheet labels it "the command line sudo
/// was given"), then escaped so no control or bidi byte can forge the sheet.
/// This display is never used to classify the command. Empty string when `ps`
/// cannot be read.
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

/// Classify only sudo's DIRECT target from exact kernel argv. Shells and other
/// wrappers can run further programs; this is not a sandbox for those programs.
/// The option spellings/arity are from the installed sudo(8) command synopsis.
/// Unknown options, incomplete argv and unavailable kernel reads refuse rather
/// than guessing which operand names the command. Never pass joined ps text.
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
            // Do not mistake an unusual sudo environment operand for a
            // harmless command. A path or absent name is ambiguous here.
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

    #[test]
    fn debugger_options_and_environment_values_do_not_become_the_command() {
        for argv in [
            vec!["sudo", "--user", "root", "lldb"],
            vec!["sudo", "--prompt", "Password:", "gdb"],
            vec!["sudo", "-Anu", "root", "/usr/bin/debugserver"],
            vec!["sudo", "FOO=bar", "lldb"],
        ] {
            assert!(classify(&argv), "a direct debugger stays refused: {argv:?}");
        }
        assert!(!classify(&["sudo", "--user", "root", "make", "lldb"]));
        assert!(!classify(&["sudo", "FOO=lldb", "make", "install"]));
    }

    #[test]
    fn debugger_classification_preserves_prompt_argument_boundaries() {
        let debugger = ["sudo", "--prompt", "hello make", "lldb"];
        let ordinary = ["sudo", "--prompt", "hello", "make", "lldb"];
        assert_eq!(debugger.join(" "), ordinary.join(" "));
        assert!(classify(&debugger));
        assert!(!classify(&ordinary));
    }

    #[test]
    fn a_debugger_path_keeps_whitespace_inside_its_argument() {
        assert!(classify(&["sudo", "/tmp/private tools/lldb", "./a.out"]));
        assert!(!classify(&["sudo", "/tmp/private tools/make", "lldb"]));
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
            assert!(sudo_target_is_debugger(Some(&argv)).is_err());
        }
        assert!(classify(&["sudo", "--user=root", "--", "lldb"]));
        assert!(classify(&["sudo", "-uroot", "lldb"]));
        assert!(!classify(&["sudo", "--", "--user", "lldb"]));
        assert!(!classify(&[
            "sudo",
            "--preserve-env=PATH,HOME",
            "make",
            "lldb"
        ]));
    }

    /// A root debugger is recognised past sudo's options and a path, and only as
    /// the command — not as a mere argument to another tool.
    #[test]
    fn debugger_target_is_recognized_only_as_the_command() {
        assert!(classify(&["sudo", "lldb", "./a.out"]));
        assert!(classify(&[
            "sudo",
            "-u",
            "root",
            "/usr/bin/debugserver",
            "-",
            "./a.out"
        ]));
        assert!(classify(&["/usr/bin/sudo", "-A", "-k", "gdb"]));
        assert!(classify(&["sudo", "dtrace", "-n", "syscall:::"]));
        assert!(!classify(&["sudo", "rm", "-rf", "/tmp/lldb"]));
        assert!(!classify(&["sudo", "apt", "install", "gdb"]));
        assert!(!classify(&["sudo", "make", "install"]));
    }
}

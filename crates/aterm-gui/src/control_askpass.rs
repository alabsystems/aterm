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

    // A fixed app-owned operation is authorized by the PID generation bound
    // before suspended sudo was resumed. Unbound requests still take the
    // unchanged generic argv/refusal path below.
    let owned_request = match crate::cpu_policy_authorization::request(sudo_pid) {
        Ok(request) => request,
        Err(()) => {
            return public_reply(
                "ERR askpass refused owned operation already requested or withdrawn\n",
            );
        }
    };
    if let Some((owned, ticket)) = owned_request {
        let (tx, rx) = std::sync::mpsc::channel();
        let request = AskpassRequest {
            ticket: std::sync::Arc::clone(&ticket),
            attribute_from: std::process::id() as i32,
            owned: Some(std::sync::Arc::clone(&owned)),
            exact: false,
            purpose: None,
            command: "Read CPU policy flags (Game Mode, CarPlay, runaway mitigation)".into(),
            invoker: "aterm system resource diagnostics".into(),
            reply: tx,
        };
        if proxy.send_event(Wake::Askpass(Box::new(request))).is_err() {
            ticket.cancel();
            return public_reply("ERR askpass refused the window is gone\n");
        }
        let mut hung = || crate::control::caller_hung_up();
        let response = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut Instant::now);
        let _ = proxy.send_event(Wake::AskpassWithdraw { id: ticket.id });
        if !owned.matches_live_sudo() {
            return public_reply("ERR askpass refused owned process changed or withdrawn\n");
        }
        return response;
    }

    // A sudo aterm started: the words are the ones aterm passed to exec, so nothing is read back.
    let started = crate::control_sudo::bound(sudo_pid);
    let (command, invoker_pid) = if let Some(started) = &started {
        (started.command.clone(), started.client_pid)
    } else {
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
        let command = argv.as_deref().map(display_line).unwrap_or_default();
        if command.is_empty() {
            return public_reply("ERR askpass refused command-unreadable\n");
        }
        (
            command,
            aterm_uds::sudo::parent_of(sudo_pid).unwrap_or(sudo_pid),
        )
    };
    if crate::spare::askpass_refused(invoker_pid) {
        return public_reply("ERR askpass refused no such session\n");
    }
    let invoker = command_line(invoker_pid);

    let (tx, rx) = std::sync::mpsc::channel();
    let ticket = std::sync::Arc::new(crate::askpass::AskpassTicket::new(
        crate::alert_keys::next_confirm_id(),
        Instant::now() + ASKPASS_DEADLINE,
    ));
    if let Some(started) = &started {
        started.watch(&ticket);
    }
    let request = AskpassRequest {
        ticket: std::sync::Arc::clone(&ticket),
        attribute_from: invoker_pid,
        owned: None,
        exact: started.is_some(),
        purpose: started.as_ref().and_then(|bound| bound.purpose.clone()),
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
    if started.is_some_and(|started| !started.is_live()) {
        return public_reply("ERR askpass refused the sudo aterm started has gone\n");
    }
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

/// `words` as the sheet shows a command line: joined by spaces and escaped as
/// `/bin/ps -ww -o args=` escaped it (`aterm_sysprobe::kproc::vis`), then escaped
/// again so no control or bidi byte can forge the sheet. A display only, never a
/// classification.
#[cfg(target_os = "macos")]
fn display_line(words: &[String]) -> String {
    let joined = words.join(" ");
    let shown = aterm_sysprobe::kproc::vis(joined.as_bytes(), aterm_sysprobe::kproc::Locale::Utf8)
        .unwrap_or(joined);
    crate::askpass::escape_display(shown.trim())
}

/// The invoker's command line for the sheet: what `/bin/ps -ww -o args=`
/// printed, read in process (`aterm_sysprobe::kproc::ps_args`: its argv where
/// the kernel hands it over, its image's path for another user's process),
/// then escaped as [`display_line`] escapes. Empty when no process has `pid`.
#[cfg(target_os = "macos")]
fn command_line(pid: i32) -> String {
    let Some(record) = u32::try_from(pid)
        .ok()
        .filter(|&pid| pid > 0)
        .and_then(aterm_sysprobe::kproc::record)
    else {
        return String::new();
    };
    let line = aterm_sysprobe::kproc::ps_args(&record, aterm_sysprobe::kproc::Locale::Utf8);
    crate::askpass::escape_display(line.trim())
}

/// Classifies whether `sudo`'s direct target executable in `argv` is a debugger, returning `Err(())` if `argv` is absent or ambiguous.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn sudo_target_is_debugger(argv: Option<&[String]>) -> Result<bool, ()> {
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
#[path = "control_askpass_debugger_tests.rs"]
mod debugger_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "control_askpass_display_tests.rs"]
mod display_tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The linked agh transport, served by each executable hosting verifier remote reads.
//!
//! Both the verifier and release cutter dispatch this entry before their ordinary CLI.
//! Reads retain their caller's deadline. Admitted native writes have a cancellation owner
//! retained through settlement. Both use linked agh; an older installed application cannot
//! select a different transport implementation.

use std::ffi::OsString;
use std::process::ExitCode;

pub(crate) const FLAG: &str = "--remote-agh";

enum Invocation<'a> {
    Transport(u32, &'a [OsString]),
    Owned(u32, &'a [OsString]),
    Credential(&'a [OsString]),
}

fn invocation(args: &[OsString]) -> Option<Invocation<'_>> {
    if let [mode, auth, helper, _operation] = args
        && mode == "credential"
        && auth == "auth"
        && helper == "git-credential"
    {
        return Some(Invocation::Credential(&args[1..]));
    }
    if let Some(args) = args.strip_prefix(&[OsString::from("owned")]) {
        let (parent, tail) = args.split_first()?;
        let parent = parent.to_str()?.parse::<u32>().ok()?;
        if parent <= 1 || parent.to_string() != args[0].to_str()? {
            return None;
        }
        // This private transport dedicates stdin to cancellation, never to an Agh body.
        return (tail.len() == 6
            && tail[0] == "repo"
            && tail[1] == "push"
            && tail[4] == "--input"
            && tail[5] != "-")
            .then_some(Invocation::Owned(parent, tail));
    }
    let (parent, args) = args.split_first()?;
    let text = parent.to_str()?;
    let parent = text.parse::<u32>().ok()?;
    (parent > 1 && parent.to_string() == text && !args.is_empty())
        .then_some(Invocation::Transport(parent, args))
}

/// Serve a remote child using the host's linked agh implementation.
///
/// Hosts of the verifier's native remote boundary must call this before their normal
/// argument dispatch. Ordinary invocations return `None`; agh retains its own routing,
/// credential policy, redaction and exit statuses.
/// Primary transport invocations carry the caller PID captured before spawning. Git's
/// credential callback uses a separate, exact grammar inside its already-owned Git group.
///
/// # Safety
/// Call before starting any other thread: the native parent-death watcher forks.
#[must_use]
pub unsafe fn entry(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|arg| arg != FLAG) {
        return None;
    }
    let Some(invocation) = invocation(&args[1..]) else {
        eprintln!(
            "verify remote: expected a captured parent PID or the credential callback grammar"
        );
        return Some(ExitCode::from(crate::exit::COULD_NOT_RUN as u8));
    };
    let (parent, args, owned) = match invocation {
        // Git's group is owned by agh's Fx tether. A callback cannot choose another CLI verb.
        Invocation::Credential(args) => (None, args, false),
        Invocation::Transport(parent, args) => (Some(parent), args, false),
        Invocation::Owned(parent, args) => (Some(parent), args, true),
    };
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        // SAFETY: the host carries the fresh single-threaded entry obligation.
        let held = unsafe {
            match parent {
                Some(parent) => match i32::try_from(parent) {
                    Ok(parent) => aterm_uds::tether::hold_parent_of(parent),
                    Err(_) => Err("captured parent is outside this host's PID range".into()),
                },
                None => aterm_uds::tether::hold_parent(),
            }
        };
        if let Err(why) = held {
            eprintln!("verify remote: cannot own transport lifetime: {why}");
            return Some(ExitCode::from(crate::exit::COULD_NOT_RUN as u8));
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    let _ = parent;
    #[cfg(unix)]
    if owned {
        // SAFETY: this exact private entry owns the inherited cancellation socket on stdin,
        // before any thread or Rust stdin owner starts. Credential callbacks never take it.
        let socket = unsafe { aterm_uds::spawnfd::adopt_inherited_socket(0) };
        let result = socket.map_err(|why| why.to_string()).and_then(|socket| {
            cancellation(aterm_uds::CtlStream::from(socket), |stop| {
                agh::cli::main_entry_with_git_stop(args.to_vec(), stop)
            })
        });
        return Some(result.unwrap_or_else(|why| {
            eprintln!("verify remote: cannot own mutation cancellation: {why}");
            ExitCode::from(crate::exit::COULD_NOT_RUN as u8)
        }));
    }
    #[cfg(not(unix))]
    if owned {
        return Some(ExitCode::from(crate::exit::COULD_NOT_RUN as u8));
    }
    Some(agh::cli::main_entry(args.to_vec()))
}

/// Arm before entering Agh and join the watcher on every return or unwind. A broken watcher
/// requests stop; it never leaves a native mutation running without its cancellation owner.
#[cfg(unix)]
pub(super) fn cancellation<T>(
    stream: aterm_uds::CtlStream,
    work: impl FnOnce(std::sync::Arc<std::sync::atomic::AtomicBool>) -> T,
) -> Result<T, String> {
    cancellation_with(aterm_uds::hangup::Bell::process(), stream, work)
}

#[cfg(unix)]
fn cancellation_with<T>(
    bell: &aterm_uds::hangup::Bell,
    stream: aterm_uds::CtlStream,
    work: impl FnOnce(std::sync::Arc<std::sync::atomic::AtomicBool>) -> T,
) -> Result<T, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    let (ring, heard) = mpsc::sync_channel(1);
    let armed = bell
        .arm(&stream, ring.clone())
        .map_err(|why| why.to_string())?;
    let finished = AtomicBool::new(false);
    let stop = Arc::new(AtomicBool::new(false));
    struct Finish<'a>(&'a AtomicBool, mpsc::SyncSender<()>);
    impl Drop for Finish<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            let _ = self.1.try_send(());
        }
    }
    std::thread::scope(|scope| {
        let requested = stop.clone();
        let stream = &stream;
        let armed = &armed;
        let finished = &finished;
        std::thread::Builder::new()
            .name("verify-mutation-stop".into())
            .spawn_scoped(scope, move || {
                while heard.recv().is_ok() {
                    if finished.load(Ordering::Acquire) {
                        return;
                    }
                    if !armed.ringing() || aterm_uds::hangup::peer_closed(stream) {
                        requested.store(true, Ordering::Release);
                        return;
                    }
                }
                if !finished.load(Ordering::Acquire) {
                    requested.store(true, Ordering::Release);
                }
            })
            .map_err(|why| why.to_string())?;
        let _finish = Finish(finished, ring);
        Ok(work(stop))
    })
}

#[cfg(test)]
#[path = "remote_cli_tests.rs"]
mod tests;

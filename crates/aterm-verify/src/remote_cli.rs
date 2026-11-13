// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The linked agh transport, served by each executable hosting verifier remote reads.
//!
//! Both the verifier and release cutter dispatch this entry before their ordinary CLI.
//! Remote children keep the caller's deadline and use the same linked agh protocol;
//! an older installed application cannot select a different transport implementation.

use std::ffi::OsString;
use std::process::ExitCode;

pub(crate) const FLAG: &str = "--remote-agh";

enum Invocation<'a> {
    Transport(u32, &'a [OsString]),
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
    let (parent, args) = match invocation {
        // Git's group is owned by agh's Fx tether. A callback cannot choose another CLI verb.
        Invocation::Credential(args) => (None, args),
        Invocation::Transport(parent, args) => (Some(parent), args),
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
    Some(agh::cli::main_entry(args.to_vec()))
}

#[cfg(test)]
#[path = "remote_cli_tests.rs"]
mod tests;

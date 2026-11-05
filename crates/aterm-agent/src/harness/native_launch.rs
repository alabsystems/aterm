// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one-shot, in-place native exec constructor. It is the shell's existing foreground
//! child, never a supervisor wrapper or a source of new command-line authority.

use std::ffi::OsString;
use std::process::ExitCode;

use super::native_act::{self, Binding, Image, ImagePhase, Reply, Request};

struct Args {
    socket: String,
    session: String,
    binding: Binding,
    phase: ImagePhase,
}

fn arguments(argv: &[OsString]) -> Option<Args> {
    if argv.len() != 6 {
        return None;
    }
    let mut words = std::collections::BTreeMap::new();
    for arg in argv {
        let (key, value) = arg.to_str()?.split_once('=')?;
        if words.insert(key, value).is_some() {
            return None;
        }
    }
    let socket = words.remove("socket")?;
    if socket.len() > native_act::MAX_PATH * 2 || !aterm_codec::hex::is_lower(socket, socket.len())
    {
        return None;
    }
    let socket = String::from_utf8(aterm_codec::hex::decode_vec(socket.as_bytes())?).ok()?;
    if !std::path::Path::new(&socket).is_absolute() || socket.contains('\0') {
        return None;
    }
    let session = words.remove("session")?.to_string();
    let binding = Binding {
        id: words.remove("id")?.to_string(),
        capability: words.remove("cap")?.to_string(),
    };
    let phase = match words.remove("phase")? {
        "forward" => ImagePhase::Forward,
        "rollback" => ImagePhase::Rollback,
        _ => return None,
    };
    // The sixth word pins the native protocol, so a stale installed launcher refuses before
    // asking for a command rather than interpreting an incompatible payload.
    if words.remove("contract")? != "1" || !words.is_empty() {
        return None;
    }
    Request::Attach(binding.clone(), phase).encode()?;
    Some(Args {
        socket,
        session,
        binding,
        phase,
    })
}

/// Checkpoints supplied only by an explicit Rust fixture caller. Installed entry
/// points never accept a hook or consult a configuration/environment switch.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug)]
pub enum FixtureStage {
    BeforeAttach,
    AfterAttach,
    AfterGo,
}

fn local_environment(raw: &[(OsString, OsString)]) -> std::io::Result<Vec<String>> {
    let mut names = std::collections::BTreeSet::new();
    raw.iter()
        .map(|(name, value)| {
            if name.is_empty() || !names.insert(name) {
                return Err(std::io::Error::other(
                    "cold environment has ambiguous names",
                ));
            }
            let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                return Err(std::io::Error::other("cold environment is not Unicode"));
            };
            if name.contains(['=', '\0']) || value.contains('\0') {
                return Err(std::io::Error::other("cold environment is malformed"));
            }
            Ok(format!("{name}={value}"))
        })
        .collect()
}

fn run(
    args: Args,
    #[cfg(debug_assertions)] hook: Option<&dyn Fn(FixtureStage, ImagePhase) -> std::io::Result<()>>,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut client = super::upgrade_drive::harness_client(&args.socket, None)?;
    // SAFETY: the client exclusively owns this live socket; only its descriptor flags are read.
    let flags = unsafe { libc::fcntl(client.transport().as_raw_fd(), libc::F_GETFD) };
    if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
        return Err(std::io::Error::other(
            "native operation channel is not close-on-exec",
        ));
    }
    #[cfg(debug_assertions)]
    if let Some(hook) = hook {
        hook(FixtureStage::BeforeAttach, args.phase)?;
    }
    let attached = native_act::exchange(
        &mut client,
        &args.session,
        &Request::Attach(args.binding.clone(), args.phase),
    )?;
    let mut inherited = None;
    let (command, deadline_us) = match attached {
        Reply::Launch {
            command,
            deadline_us,
        } => (command, deadline_us),
        Reply::NeedEnvironment {
            seed,
            intent,
            deadline_us,
        } => {
            if args.phase != ImagePhase::Forward {
                return Err(std::io::Error::other("cold rollback refused"));
            }
            let raw = std::env::vars_os().collect::<Vec<_>>();
            let cwd = std::env::current_dir()?;
            let env = local_environment(&raw)?;
            let facts =
                native_act::ColdEnvironmentFacts::observe_for_command(&env, &intent, &seed, &cwd)
                    .ok_or_else(|| std::io::Error::other("cold environment facts unavailable"))?;
            let Reply::ResolvedLaunch {
                command,
                deadline_us: resolved_deadline,
            } = native_act::exchange(
                &mut client,
                &args.session,
                &Request::Resolve(args.binding.clone(), args.phase, facts),
            )?
            else {
                return Err(std::io::Error::other("cold launch was not admitted"));
            };
            if resolved_deadline != deadline_us || !seed.cold_resolution_agrees(&command) {
                return Err(std::io::Error::other("cold launch intent changed"));
            }
            inherited = Some(raw);
            (command, deadline_us)
        }
        _ => return Err(std::io::Error::other("native launcher was not admitted")),
    };
    #[cfg(debug_assertions)]
    if let Some(hook) = hook {
        hook(FixtureStage::AfterAttach, args.phase)?;
    }
    // All preparation happens before Execute. The child supplies only interpreted account
    // facts, never a command or argv. Its raw environment stays local; the host has already
    // registered its exact birth/death watch and selects the permitted export delta.
    if Image::at(std::path::Path::new(&command.program)).as_ref() != Some(&command.image)
        || inherited.is_none()
            && (command
                .env
                .iter()
                .any(|(name, value)| std::env::var(name).ok().as_deref() != Some(value.as_str()))
                || command
                    .unset
                    .as_ref()
                    .is_some_and(|names| names.iter().any(|name| std::env::var_os(name).is_some())))
    {
        return Err(std::io::Error::other("native launch context changed"));
    }
    let mut exec = aterm_uds::spawnfd::transparent(&command.program);
    exec.args(&command.argv);
    if let Some(mut raw) = inherited {
        if let Some(names) = &command.unset {
            raw.retain(|(name, _)| {
                !names
                    .iter()
                    .any(|unset| name == std::ffi::OsStr::new(unset))
            });
        }
        for (name, value) in &command.env {
            raw.retain(|(key, _)| key != std::ffi::OsStr::new(name));
            raw.push((OsString::from(name), OsString::from(value)));
        }
        exec.env_exactly(raw);
    }
    let exec = exec.prepare_exec()?;
    let Reply::Execute {
        deadline_us: original,
    } = native_act::exchange(
        &mut client,
        &args.session,
        &Request::Execute(args.binding.clone(), args.phase),
    )?
    else {
        return Err(std::io::Error::other("native exec was not admitted"));
    };
    if original != deadline_us {
        return Err(std::io::Error::other("native operation deadline changed"));
    }
    #[cfg(debug_assertions)]
    if let Some(hook) = hook {
        hook(FixtureStage::AfterGo, args.phase)?;
    }
    let refused = exec.exec_until(original);
    // An exec error is terminal for this helper. Even a failed error report leaves the exact
    // child and kernel witness owned by the original host operation; EOF never retires it.
    if let Some(errno) = refused.raw_os_error().filter(|errno| *errno > 0) {
        let _recorded = native_act::exchange(
            &mut client,
            &args.session,
            &Request::ExecFailed(args.binding, args.phase, errno),
        );
    }
    Err(refused)
}

/// Intercept this internal constructor before normal harness setup, account reads or lanes.
pub(super) fn entry(argv: &[OsString]) -> Option<ExitCode> {
    if argv.first().is_none_or(|arg| arg != "native-launch") {
        return None;
    }
    if let Some(args) = arguments(&argv[1..]) {
        let _terminal = run(
            args,
            #[cfg(debug_assertions)]
            None,
        );
    }
    Some(ExitCode::from(126))
}

/// Exercise the actual native launcher with a fixture-owned checkpoint callback.
/// This is never called by the installed CLI and has no runtime opt-in.
#[cfg(debug_assertions)]
pub fn fixture_entry(
    argv: &[OsString],
    hook: &dyn Fn(FixtureStage, ImagePhase) -> std::io::Result<()>,
) -> Option<ExitCode> {
    if argv.first().is_none_or(|arg| arg != "native-launch") {
        return None;
    }
    if let Some(args) = arguments(&argv[1..]) {
        let _terminal = run(args, Some(hook));
    }
    Some(ExitCode::from(126))
}

#[cfg(test)]
mod cold_environment_tests {
    use super::*;
    #[test]
    fn owned_environment_rejects_duplicate_names_without_disclosing_values() {
        assert_eq!(local_environment(&[]).unwrap(), Vec::<String>::new());
        let raw = vec![(OsString::from("PROBE"), OsString::from("first"))];
        assert_eq!(local_environment(&raw).unwrap(), ["PROBE=first"]);
        let mut duplicate = raw;
        duplicate.push((
            OsString::from("PROBE"),
            OsString::from("different-private-value"),
        ));
        let refusal = local_environment(&duplicate).unwrap_err().to_string();
        assert!(!refusal.contains("different-private-value"));
        assert!(local_environment(&[(OsString::from("bad=name"), OsString::new())]).is_err());
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A process-root registration followed by one exact, identity-preserving exec.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

const USAGE: &str = "usage: aterm resource-run [--sock PATH] [--session @self|@<sid>|@<local-id>] [--ai WORD] --job LABEL [--cpu-units N --memory-bytes M [--timeout-ms N] [--status-fd N] [--accounted] [--expect-original-store-fd N]] -- PROGRAM [ARGS...]\n       aterm resource-run [--sock PATH] [--session @self|@<sid>] --capabilities";

#[cfg(unix)]
#[path = "resource_run_admission.rs"]
mod admission;

#[path = "resource_run_early.rs"]
mod early;
pub use early::{OriginalRootStore, capture_original_root_store};

#[cfg(any(unix, test))]
struct Resources {
    cpu_units: u32,
    memory_bytes: u64,
    timeout_ms: Option<u64>,
    status_fd: Option<i32>,
}

#[cfg(not(any(unix, test)))]
struct Resources;

impl Resources {
    #[cfg(any(unix, test))]
    fn validated(
        cpu_units: u32,
        memory_bytes: u64,
        timeout_ms: Option<u64>,
        status_fd: Option<i32>,
    ) -> Self {
        Self {
            cpu_units,
            memory_bytes,
            timeout_ms,
            status_fd,
        }
    }

    // Unsupported platforms retain successful argument validation, not a request
    // payload that no native admission transport can consume.
    #[cfg(not(any(unix, test)))]
    fn validated(_: u32, _: u64, _: Option<u64>, _: Option<i32>) -> Self {
        Self
    }
}

const ABOUT: &str = "Registers this process with the session's aterm as job LABEL, then execs PROGRAM in its\n\
                     place (same pid, descriptors and process group): its CPU and memory are charged to\n\
                     that tab. --ai names the agent the job runs for (default: the agent the tab\n\
                     runs, else `unknown`). `aterm help resource-run` says more.";

struct Args {
    sock: Option<String>,
    session: String,
    ai: Option<String>,
    job: String,
    command: Vec<OsString>,
    resources: Option<Resources>,
    capabilities: bool,
    accounted: bool,
    #[cfg(unix)]
    expected_store: Option<std::fs::File>,
}

#[cfg(test)]
fn parse(argv: Vec<OsString>) -> io::Result<Option<Args>> {
    parse_with_original_store(argv, None)
}

fn parse_with_original_store(
    argv: Vec<OsString>,
    original: Option<OriginalRootStore>,
) -> io::Result<Option<Args>> {
    #[cfg(unix)]
    if let Some(original) = &original {
        original.check_arguments(&argv)?;
    }
    #[cfg(unix)]
    let mut original = original;
    #[cfg(not(unix))]
    let _ = original;
    #[cfg(unix)]
    let captured_arguments = argv.clone();
    let mut args = argv.into_iter();
    let (mut sock, mut session, mut ai, mut job) = (None, None, None, None);
    let (mut cpus, mut memory, mut timeout, mut status) = (None, None, None, None);
    let mut capabilities = false;
    let mut accounted = false;
    let mut expected_store_fd: Option<String> = None;
    while let Some(arg) = args.next() {
        let Some(word) = arg.to_str() else {
            return Err(super::usage(USAGE));
        };
        if word == "--help" || word == "-h" {
            return Ok(None);
        }
        if word == "--accounted" {
            if accounted {
                return Err(super::usage(USAGE));
            }
            accounted = true;
            continue;
        }
        if word == "--capabilities" {
            if capabilities {
                return Err(super::usage(USAGE));
            }
            capabilities = true;
            continue;
        }
        if word == "--" {
            let resources = resources(cpus, memory, timeout, status)?;
            let command: Vec<_> = args.collect();
            if command.is_empty()
                || capabilities
                || (accounted && resources.is_none())
                || (resources.is_some() && ai.is_some())
            {
                return Err(super::usage(USAGE));
            }
            let expected_store: Option<std::fs::File> = if let Some(value) = expected_store_fd {
                if !accounted
                    || !matches!(command.as_slice(), [_, entry] if entry == "--resource-workload-v1")
                {
                    return Err(super::usage(USAGE));
                }
                let fd = value.parse::<i32>().map_err(|_| super::usage(USAGE))?;
                if fd < 6 {
                    return Err(super::usage(USAGE));
                }
                #[cfg(unix)]
                {
                    if resources.as_ref().is_none_or(|value| {
                        value.timeout_ms.is_none() || value.status_fd == Some(fd)
                    }) {
                        return Err(super::usage(USAGE));
                    }
                    Some(
                        original
                            .take()
                            .ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::PermissionDenied,
                                    "original Store expectation was not captured at native startup",
                                )
                            })?
                            .into_file(&captured_arguments, fd)?,
                    )
                }
                #[cfg(not(unix))]
                {
                    return Err(io::ErrorKind::Unsupported.into());
                }
            } else {
                None
            };
            #[cfg(not(unix))]
            let _ = expected_store;
            return Ok(Some(Args {
                sock,
                session: match session {
                    Some(session) => session,
                    None if resources.is_some() => "@self".into(),
                    None => return Err(super::usage(USAGE)),
                },
                ai,
                job: job.ok_or_else(|| super::usage(USAGE))?,
                command,
                resources,
                capabilities: false,
                accounted,
                #[cfg(unix)]
                expected_store,
            }));
        }
        let slot = match word {
            "--sock" => &mut sock,
            "--session" => &mut session,
            "--ai" => &mut ai,
            "--job" => &mut job,
            "--cpu-units" => &mut cpus,
            "--memory-bytes" => &mut memory,
            "--timeout-ms" => &mut timeout,
            "--status-fd" => &mut status,
            "--expect-original-store-fd" => &mut expected_store_fd,
            _ => return Err(super::usage(USAGE)),
        };
        if slot.is_some() {
            return Err(super::usage(USAGE));
        }
        *slot = Some(
            args.next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| super::usage(USAGE))?,
        );
    }
    if capabilities
        && !accounted
        && ai.is_none()
        && job.is_none()
        && cpus.is_none()
        && memory.is_none()
        && timeout.is_none()
        && status.is_none()
        && expected_store_fd.is_none()
    {
        return Ok(Some(Args {
            sock,
            session: session.unwrap_or_else(|| "@self".into()),
            ai,
            job: String::new(),
            command: Vec::new(),
            resources: None,
            capabilities: true,
            accounted: false,
            #[cfg(unix)]
            expected_store: None,
        }));
    }
    Err(super::usage(USAGE))
}

fn resources(
    cpus: Option<String>,
    memory: Option<String>,
    timeout: Option<String>,
    status: Option<String>,
) -> io::Result<Option<Resources>> {
    fn positive(value: &str) -> io::Result<u64> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(super::usage(USAGE));
        }
        value
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| super::usage(USAGE))
    }
    match (cpus, memory) {
        (None, None) if timeout.is_none() && status.is_none() => Ok(None),
        (Some(cpus), Some(memory)) => Ok(Some(Resources::validated(
            u32::try_from(positive(&cpus)?).map_err(|_| super::usage(USAGE))?,
            positive(&memory)?,
            timeout.as_deref().map(positive).transpose()?,
            status
                .as_deref()
                .map(positive)
                .transpose()?
                .map(|fd| {
                    i32::try_from(fd)
                        .ok()
                        .filter(|fd| *fd > 2)
                        .ok_or_else(|| super::usage(USAGE))
                })
                .transpose()?,
        ))),
        _ => Err(super::usage(USAGE)),
    }
}

/// Register this process, then become the exact requested command.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    main_entry_with_original_store(argv, None)
}

/// Consume an original startup capture; the late parser never reads its raw FD.
pub fn main_entry_with_original_store(
    argv: Vec<OsString>,
    original: Option<OriginalRootStore>,
) -> ExitCode {
    let admitted = argv
        .iter()
        .take_while(|word| word.as_os_str() != "--")
        .any(|word| {
            matches!(
                word.to_str(),
                Some(
                    "--cpu-units"
                        | "--memory-bytes"
                        | "--timeout-ms"
                        | "--status-fd"
                        | "--capabilities"
                        | "--expect-original-store-fd"
                )
            )
        });
    match run_registered_command_with_original_store(argv, original) {
        Ok(code) => code,
        Err(error) => {
            let _ = crate::stderr_prefixed("aterm resource-run: ", &error.to_string());
            if admitted {
                ExitCode::from(75)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(all(test, unix))]
fn run_registered_command(argv: Vec<OsString>) -> io::Result<ExitCode> {
    run_registered_command_with_original_store(argv, None)
}

fn run_registered_command_with_original_store(
    argv: Vec<OsString>,
    original: Option<OriginalRootStore>,
) -> io::Result<ExitCode> {
    let Some(args) = parse_with_original_store(argv, original)? else {
        use std::io::Write;
        let stdout = io::stdout();
        let mut output = crate::StdoutSink::new(stdout.lock());
        writeln!(output, "{USAGE}\n{ABOUT}")?;
        return Ok(ExitCode::SUCCESS);
    };
    let self_sid = std::env::var(crate::SELF_SID_ENV)
        .ok()
        .filter(|sid| !sid.is_empty());
    let target_sid = match super::classify_selector(&args.session)? {
        super::SelTok::Sid(sid) => Some(sid.to_string()),
        _ => self_sid.clone(),
    };
    let (path, origin) = crate::resolve_target(args.sock.clone(), None, target_sid)?;
    let wire = super::ConnWire { path, origin };
    let sid = super::resolve_selector(
        &args.session,
        self_sid.as_deref(),
        &mut super::LazySessions::new(&wire),
    )?;
    if args.capabilities || args.resources.is_some() || args.accounted {
        #[cfg(unix)]
        return admission::run(&wire, &sid, &args);
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retained resource admission requires Unix",
        ));
    }
    let own = std::process::id();
    let start = aterm_uds::process::read_process_start(i32::try_from(own).unwrap_or(-1))
        .ok_or_else(|| io::Error::other("kernel process identity unavailable"))?
        .boot
        .0;
    let mut request = vec![
        format!("@{sid}"),
        "resources".into(),
        "register".into(),
        format!("start={start}"),
        format!("job={}", aterm_codec::pct::encode(&args.job)),
    ];
    if let Some(ai) = args.ai {
        request.push(format!("ai={}", aterm_codec::pct::encode(&ai)));
    }
    let reply = super::require_ok(wire.request(&request)?)?;
    if !registered(&reply.status, own, start, &sid) {
        return Err(io::Error::other(
            "host did not confirm this process's resource registration",
        ));
    }
    exec_command(&args.command)
}

fn exec_command(command: &[OsString]) -> io::Result<ExitCode> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        #[expect(
            clippy::disallowed_types,
            reason = "a registered pass-through becomes its command; exec preserves all caller descriptors and process identity"
        )]
        let error = std::process::Command::new(&command[0])
            .args(&command[1..])
            .exec();
        Err(error)
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "resource-run requires a Unix process identity",
        ))
    }
}

fn registered(status: &str, pid: u32, start: u64, sid: &str) -> bool {
    let words: Vec<_> = status.split_whitespace().collect();
    if words.get(..2) != Some(&["OK", "resources"]) {
        return false;
    }
    let one = |name: &str, value: &str| {
        let prefix = format!("{name}=");
        let values: Vec<_> = words
            .iter()
            .filter_map(|word| word.strip_prefix(&prefix))
            .collect();
        values == [value]
    };
    one("version", "1")
        && one("root_pid", &pid.to_string())
        && one("start", &start.to_string())
        && one("session", sid)
}

#[cfg(test)]
#[path = "resource_run_own_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "resource_run_tests.rs"]
mod process_tests;

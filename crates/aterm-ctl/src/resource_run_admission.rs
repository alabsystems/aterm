// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public CLI to the authenticated host; this process never allocates or spawns.
use super::Args;
use crate::resource_admission_transport as transport;
use aterm_types::resource_admission::{
    AccountedEventFrame, AccountingObservation, AccountingScope, Binding, CompletionCause, Event,
    EventFrame, Outcome, Submission,
};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    fs::File,
    io::{self, Read as _, Write as _},
    os::{
        fd::{AsFd as _, BorrowedFd},
        unix::{ffi::OsStrExt as _, fs::FileTypeExt as _},
    },
    process::ExitCode,
    time::{Duration, Instant},
};

const HANDOFF: Duration = Duration::from_secs(10);
const STATUS_MAX: usize = 8192;

pub(super) fn run(wire: &super::super::ConnWire, sid: &str, args: &Args) -> io::Result<ExitCode> {
    if args.capabilities {
        return capabilities(wire);
    }
    let resources = args
        .resources
        .as_ref()
        .ok_or_else(|| io::Error::other("missing resource request"))?;
    let mut status = resources.status_fd.map(status_file).transpose()?;
    let mut attempt = [0; 16];
    aterm_uds::rand::fill(&mut attempt)?;
    let peer_pid = std::process::id();
    let peer_birth =
        aterm_uds::process::read_birth(i32::try_from(peer_pid).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("submitting process birth unavailable"))?
            .0;
    let request = Submission {
        binding: Binding {
            session: sid.to_owned(),
            attempt,
        },
        peer_pid,
        peer_birth,
        argv: args
            .command
            .iter()
            .map(|arg| arg.as_os_str().as_bytes().to_vec())
            .collect(),
        cwd: std::env::current_dir()?.as_os_str().as_bytes().to_vec(),
        env: std::env::vars_os()
            .map(|(key, value)| {
                (
                    key.as_os_str().as_bytes().to_vec(),
                    value.as_os_str().as_bytes().to_vec(),
                )
            })
            .collect(),
        cpu_units: resources.cpu_units,
        memory_bytes: resources.memory_bytes,
        label: args.job.clone(),
        timeout_ms: resources.timeout_ms,
    };
    request.validate()?;
    if !capability_value(wire).is_ok_and(|value| {
        value["ready"].as_bool() == Some(true)
            && (!args.accounted
                || value["accounting"].as_str() == Some("owning-wait-accounting-v1"))
            && (args.expected_store.is_none()
                || value["root_store_expectation"].as_str()
                    == Some(transport::store_expectation::CAPABILITY))
    }) {
        emit(
            &mut status,
            &EventFrame {
                binding: request.binding.clone(),
                event: Event::Refused {
                    reason: "retained host custody unavailable before submission".into(),
                },
            },
        )?;
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "retained host custody unavailable before submission",
        ));
    }
    let (events, host_events) = aterm_uds::kept_out(CtlStream::pair)?;
    let fds = transport::Attachments {
        stdin: std::io::stdin().as_fd().try_clone_to_owned()?,
        stdout: std::io::stdout().as_fd().try_clone_to_owned()?,
        stderr: std::io::stderr().as_fd().try_clone_to_owned()?,
        events: host_events.into(),
    };
    let attached = if let Some(expected) = &args.expected_store {
        transport::store_expectation::attach(
            &wire.path,
            &request,
            fds,
            expected.as_fd().try_clone_to_owned()?,
            Instant::now() + HANDOFF,
        )
    } else if args.accounted {
        transport::attach_accounted(&wire.path, &request, fds, Instant::now() + HANDOFF)
    } else {
        transport::attach(&wire.path, &request, fds, Instant::now() + HANDOFF)
    }
    .is_ok();
    // A failed ATTACHED exchange can follow real descriptor delivery and
    // actual launch. The private lifecycle stream remains authoritative.
    match receive_mode(
        &events,
        &request.binding,
        &mut status,
        (!attached).then(|| Instant::now() + HANDOFF),
        args.accounted,
    ) {
        Ok(code) => Ok(code),
        Err(error) => {
            emit_unknown(&mut status, &request.binding)?;
            let _ = transport::send_cancel(
                &events,
                aterm_types::resource_admission::Cancel {
                    binding: request.binding.clone(),
                },
                Instant::now() + HANDOFF,
            );
            Err(error)
        }
    }
}

fn capabilities(wire: &super::super::ConnWire) -> io::Result<ExitCode> {
    let value = capability_value(wire)?;
    writeln!(crate::StdoutSink::new(io::stdout().lock()), "{value}")?;
    Ok(ExitCode::SUCCESS)
}

fn capability_value(wire: &super::super::ConnWire) -> io::Result<aterm_json::Value> {
    let reply = wire.request(&["resource-admission-v3".into(), "capabilities".into()]);
    let value = match reply {
        Ok(reply)
            if reply.status == "OK resource-admission-v3 capabilities"
                && reply.lines.len() == 1 =>
        {
            let value: aterm_json::Value = aterm_json::from_str(&reply.lines[0])
                .map_err(|_| io::Error::other("invalid host capability response"))?;
            if value["version"].as_u64() != Some(3) || value["ready"].as_bool().is_none() {
                return Err(io::Error::other("invalid host capability response"));
            }
            value
        }
        _ => aterm_json::json!({"version":3,"ready":false,"reason":"host protocol unavailable"}),
    };
    Ok(value)
}

fn status_file(fd: i32) -> io::Result<File> {
    if fd <= 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "status fd must be a separate pipe or socket",
        ));
    }
    let metadata = aterm_uds::spawnfd::descriptor_metadata(fd)?;
    if !metadata.file_type().is_fifo() && !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "status fd must be a pipe or socket",
        ));
    }
    // SAFETY: descriptor_metadata just established this inherited descriptor is
    // live. This CLI has started no threads or descriptor-mutating callbacks;
    // kept_out excludes coordinated spawn activity while it is duplicated.
    let owned = aterm_uds::kept_out(|| unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned())?;
    Ok(File::from(owned))
}
pub(super) fn store_file(fd: i32) -> io::Result<File> {
    if fd < 6 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // Capture before resolving a GUI or allocating transport descriptors. The
    // CLI has started no threads or descriptor-mutating callbacks. This is
    // solely an expectation; native backend validates readonly original identity.
    aterm_uds::kept_out(|| {
        if !aterm_uds::spawnfd::descriptor_metadata(fd)?.is_dir() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        // SAFETY: the preceding kernel read established this inherited source
        // is live; no descriptor mutation can intervene in this CLI boundary.
        unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map(File::from)
    })
}

fn receive_mode(
    events: &CtlStream,
    binding: &Binding,
    status: &mut Option<File>,
    mut first_event_deadline: Option<Instant>,
    accounted: bool,
) -> io::Result<ExitCode> {
    let mut started = false;
    let mut queued = false;
    loop {
        if first_event_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "host attachment outcome unknown; do not retry this attempt",
            ));
        }
        // Waiting for the first byte consumes no frame state. A frame itself
        // then has one bounded deadline; partial timeouts are never retried.
        match ready::until(
            events.as_fd(),
            Interest::Read,
            Instant::now() + Duration::from_secs(1),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error),
        }
        let bytes = transport::read_frame(events, Instant::now() + HANDOFF)?;
        let (frame, observation) = if accounted {
            match AccountedEventFrame::decode(&bytes, binding) {
                Ok(frame) => (frame.finished, Some(frame.observation)),
                Err(_) => {
                    let frame = EventFrame::decode(&bytes, binding)?;
                    if matches!(frame.event, Event::Finished { .. } | Event::Refused { .. }) {
                        return Err(io::Error::other(
                            "accounted root requires an accounted terminal",
                        ));
                    }
                    (frame, None)
                }
            }
        } else {
            (EventFrame::decode(&bytes, binding)?, None)
        };
        first_event_deadline = None;
        match &frame.event {
            Event::Queued if !started && !queued => {
                queued = true;
                emit(status, &frame)?;
            }
            Event::Started { .. } if !started => {
                started = true;
                emit(status, &frame)?;
            }
            Event::Finished { outcome, .. } if started => {
                if accounted {
                    clean_eof(events, Instant::now() + HANDOFF)?;
                }
                emit_mode(status, &frame, observation)?;
                return Ok(match outcome {
                    Outcome::Exit(code) if (0..=255).contains(code) => ExitCode::from(*code as u8),
                    Outcome::Signal(signal) if (1..=127).contains(signal) => {
                        ExitCode::from((128 + signal) as u8)
                    }
                    _ => return Err(io::Error::other("invalid host completion status")),
                });
            }
            Event::Refused { .. } if !started => {
                if accounted {
                    clean_eof(events, Instant::now() + HANDOFF)?;
                }
                emit_mode(status, &frame, observation)?;
                return Ok(ExitCode::from(75));
            }
            _ => return Err(io::Error::other("invalid host lifecycle sequence")),
        }
    }
}

fn clean_eof(mut events: &CtlStream, deadline: Instant) -> io::Result<()> {
    loop {
        match events.read(&mut [0]) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(io::Error::other("bytes after accounted root terminal")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(events.as_fd(), Interest::Read, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}
fn emit(output: &mut Option<File>, frame: &EventFrame) -> io::Result<()> {
    emit_mode(output, frame, None)
}
fn emit_mode(
    output: &mut Option<File>,
    frame: &EventFrame,
    observation: Option<AccountingObservation>,
) -> io::Result<()> {
    let Some(output) = output else { return Ok(()) };
    let mut value = aterm_json::json!({"version":3,"session":frame.binding.session,"attempt":aterm_uds::rand::hex_encode(&frame.binding.attempt)});
    let object = value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("invalid status object"))?;
    let facts = match &frame.event {
        Event::Queued => aterm_json::json!({"event":"queued"}),
        Event::Started {
            pid,
            birth,
            monotonic_ns,
            unix_ns,
        } => {
            aterm_json::json!({"event":"started","pid":pid,"birth":birth,"monotonic_ns":monotonic_ns,"unix_ns":unix_ns})
        }
        Event::Finished {
            outcome,
            cause,
            runtime_ns,
        } => {
            let outcome = match outcome {
                Outcome::Exit(code) => aterm_json::json!({"exit":code}),
                Outcome::Signal(signal) => aterm_json::json!({"signal":signal}),
            };
            let cause = match cause {
                CompletionCause::Exited => "exited",
                CompletionCause::Timeout => "timeout",
                CompletionCause::Cancelled => "cancelled",
                CompletionCause::ResourceLimit => "resource_limit",
            };
            aterm_json::json!({"event":"finished","outcome":outcome,"cause":cause,"runtime_ns":runtime_ns})
        }
        Event::Refused { reason } => aterm_json::json!({"event":"refused","reason":reason}),
    };
    for (key, value) in facts
        .as_object()
        .ok_or_else(|| io::Error::other("invalid status facts"))?
    {
        object.insert(key.clone(), value.clone());
    }
    if let Some(observation) = observation {
        let scope = match observation.scope {
            AccountingScope::ChildAndWaitedDescendantsV1 => "child-and-waited-descendants-v1",
        };
        object.insert("accounting".into(), aterm_json::json!({
            "scope":scope,
            "cpu_times":observation.cpu_times.map(|cpu| aterm_json::json!({"user_ns":cpu.user_ns,"system_ns":cpu.system_ns})),
            "peak_rss_bytes":observation.peak_rss_bytes
        }));
    }
    let text = value.to_string();
    if text.len() + 1 > STATUS_MAX {
        return Err(io::Error::other("oversized status event"));
    }
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")
}

fn emit_unknown(output: &mut Option<File>, binding: &Binding) -> io::Result<()> {
    let Some(output) = output else { return Ok(()) };
    let value = aterm_json::json!({"version":3,"session":binding.session,"attempt":aterm_uds::rand::hex_encode(&binding.attempt),"event":"transport_unknown","reason":"launch outcome unconfirmed; this attempt must not be retried"});
    writeln!(output, "{value}")
}

#[cfg(test)]
#[path = "resource_run_admission_tests.rs"]
mod tests;

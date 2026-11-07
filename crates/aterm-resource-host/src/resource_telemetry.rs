// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One host sampler and one host-only writer. Neither an observation nor its
//! read-only liveness end can allocate, control or settle a resource claim.
use crate::resource_machine::{self as machine, CachedSampler};
use aterm_ctl::resource_admission_transport::{self as wire, Peer};
use aterm_uds::CtlStream;
use std::{
    io,
    os::fd::{AsFd as _, AsRawFd as _},
    time::Instant,
};
use trust_resource_controller::{
    Channel, ContextQuery, ContextReply, ContextUnavailable, Pressure, Snapshot, TelemetryQuery,
    TelemetryReply, TelemetryResponse, Thermal,
};

pub(crate) struct Telemetry {
    sampler: CachedSampler,
    reader: io::PipeReader,
    // Never cloned or offered to a child. Read-end copies cannot keep this alive.
    _writer: io::PipeWriter,
}
impl Telemetry {
    pub(crate) fn new() -> io::Result<Self> {
        let (reader, writer) = host_pipe()?;
        Ok(Self {
            sampler: CachedSampler::new()?,
            reader,
            _writer: writer,
        })
    }
    fn snapshot(&self) -> io::Result<Snapshot> {
        let measured = self
            .sampler
            .latest()
            .filter(|sample| sample.cpus > 0)
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(Snapshot {
            cpus: measured.cpus,
            busy_millicores: measured.busy_millicores,
            available_memory_bytes: measured.available_memory_bytes,
            pressure: match measured.pressure {
                machine::Pressure::Unknown => Pressure::Unknown,
                machine::Pressure::Normal => Pressure::Normal,
                machine::Pressure::Warn => Pressure::Warn,
                machine::Pressure::Critical => Pressure::Critical,
            },
            thermal: match measured.thermal {
                machine::Thermal::Unknown => Thermal::Unknown,
                machine::Thermal::Nominal => Thermal::Nominal,
                machine::Thermal::Fair => Thermal::Fair,
                machine::Thermal::Serious => Thermal::Serious,
                machine::Thermal::Critical => Thermal::Critical,
            },
            sample_age_ns: u64::try_from(measured.sample_age.as_nanos()).unwrap_or(u64::MAX),
            window_ns: u64::try_from(measured.window.as_nanos()).unwrap_or(u64::MAX),
        })
    }
}
/// Set the mode only while both fresh descriptions are private. Every receiver
/// can then verify live-empty with EAGAIN without mutating a shared description.
fn host_pipe() -> io::Result<(io::PipeReader, io::PipeWriter)> {
    aterm_uds::kept_out(|| {
        let (reader, writer) = io::pipe()?;
        // SAFETY: this is the fresh owned read description, not a received slot.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: no copy has escaped; set nonblocking before any sharing.
        if unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((reader, writer))
    })
}

#[cfg(test)]
#[path = "resource_telemetry/tests.rs"]
mod tests;
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

pub(crate) fn serve(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    telemetry: &Telemetry,
    observe: impl FnOnce(Peer, ContextQuery) -> io::Result<ContextReply>,
) -> io::Result<()> {
    let session = hello["session"].as_str().ok_or_else(refused)?;
    if hello["version"].as_u64() != Some(3)
        || hello["role"].as_str() != Some("native-telemetry")
        || hello.as_object().is_none_or(|object| object.len() != 3)
    {
        return Err(refused());
    }
    let query = TelemetryQuery::decode(&wire::read_frame(stream, deadline)?)?;
    if query.query.binding.session != session || wire::local_peer(stream)? != peer {
        return Err(refused());
    }
    let observed = observe(peer, query.query.clone()).and_then(|context| {
        telemetry
            .snapshot()
            .map(|snapshot| TelemetryReply { context, snapshot })
    });
    let response = match observed {
        Ok(reply) => TelemetryResponse::Ready(reply),
        Err(_) => TelemetryResponse::Unavailable(ContextUnavailable::for_query(
            &query.query,
            "no authenticated live native machine observation".into(),
        )?),
    };
    if wire::local_peer(stream)? != peer {
        return Err(refused());
    }
    wire::write_frame(stream, &response.encode()?, deadline)?;
    if wire::read_frame(stream, deadline)?.as_slice() != [1] {
        return Err(refused());
    }
    if matches!(response, TelemetryResponse::Unavailable(_)) {
        return Ok(());
    }
    if wire::local_peer(stream)? != peer {
        return Err(refused());
    }
    // One sequential handler owns every read/write. This protected duplicate
    // bridges the shared canonical SCM codec; it introduces no competing reader.
    let duplicate = aterm_uds::kept_out(|| stream.as_fd().try_clone_to_owned())?;
    let mut channel = Channel::new(std::os::unix::net::UnixStream::from(duplicate))?;
    channel.send_read_lifeline(telemetry.reader.as_fd(), deadline)?;
    // ACK retains the original validated server through the client's final
    // provenance check. A peer may close immediately afterward on Darwin.
    if wire::read_frame(stream, deadline)?.as_slice() != [2] {
        return Err(refused());
    }
    Ok(())
}

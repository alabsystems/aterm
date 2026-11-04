// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owner-authorized local descriptor attachment, before proxy dispatch.
//! Socket handoff does not authorize execution or computer-wide resource policy.

use crate::{Wake, control::Scope, session_store::Store};
use aterm_capacity::host_protocol::Generation;
use aterm_types::allocation_monitor::{ADMISSION, FD_MARKER, parse_attach, queued};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use aterm_window::event_loop::EventLoopProxy;
use atpkg::build_admit::service::monitor::Client;
use std::{
    io::{self, Write as _},
    os::fd::AsFd as _,
    time::{Duration, Instant},
};

const HANDOFF: Duration = Duration::from_secs(10);

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "allocation monitor descriptor refused",
    )
}

fn write_until(mut stream: &CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn receive(stream: &CtlStream, deadline: Instant) -> io::Result<CtlStream> {
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // Each receive is nonblocking. Only descriptor creation/validation/mark
        // holds the spawn lock; waiting for the sender happens outside it.
        let received = aterm_uds::kept_out(|| {
            let mut payload = [0; 2];
            let received = aterm_uds::fdpass::recv_with_fds(stream, &mut payload, 1)?;
            if received.bytes != 1 || payload[0] != FD_MARKER || received.fds.len() != 1 {
                return Err(refused());
            }
            let reader = CtlStream::from(received.fds.into_iter().next().expect("one descriptor"));
            if aterm_uds::peer::peer_uid(&reader) != Some(aterm_uds::peer::our_uid()) {
                return Err(refused());
            }
            Ok(reader)
        });
        match received {
            Ok(stream) => return Ok(stream),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
}

fn allowed(scope: Scope, selected: bool, remote: bool) -> bool {
    scope == Scope::Owner && !selected && !remote
}

pub(crate) fn serve(
    stream: &CtlStream,
    rest: &str,
    scope: Scope,
    selected: bool,
    store: &Store,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    // Drive admission can promote a remote scope to Owner. Its provenance
    // remains remote and must never enter a local SCM_RIGHTS handoff.
    if !allowed(scope, selected, crate::fabric::serving_driver().is_some()) {
        return "ERR allocation monitor refused local-owner-only\n".into();
    }
    let Some(generation) = parse_attach(rest) else {
        return "ERR usage: allocation attach generation=<32 lowercase hex>\n".into();
    };
    let deadline = Instant::now() + HANDOFF;
    let attached = (|| {
        stream.set_nonblocking(true)?;
        write_until(stream, format!("{ADMISSION}\n").as_bytes(), deadline)?;
        let reader = receive(stream, deadline)?;
        let client = Client::from_trusted_stream(reader, Generation(generation))?;
        let host = store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .allocation_monitor();
        let notify = crate::allocation_monitor_host::Notify::EventLoop(proxy.clone());
        host.attach(client, Generation(generation), notify)
    })();
    // This disposition closes the control connection. Its final Outbox write
    // stays bounded after the direct admission/descriptor handshake.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)),
    ));
    match attached {
        Ok(revision) => queued(revision),
        Err(_) => "ERR allocation monitor attachment refused\n".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_local_unselected_owner_can_receive_a_descriptor() {
        assert!(allowed(Scope::Owner, false, false));
        assert!(!allowed(Scope::Owner, true, false));
        assert!(!allowed(Scope::Owner, false, true));
        assert!(!allowed(Scope::Bridge, false, false));
        for content in [false, true] {
            for drive in [false, true] {
                assert!(!allowed(Scope::Fleet { content, drive }, false, false));
            }
        }
    }
    #[test]
    fn handoff_requires_exact_marker_and_one_descriptor() {
        use std::os::fd::AsFd as _;
        for mode in 0..4 {
            let (send, recv) = aterm_uds::kept_out(CtlStream::pair).unwrap();
            recv.set_nonblocking(true).unwrap();
            let (capability, _server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
            let data = if mode == 0 {
                vec![FD_MARKER]
            } else if mode == 1 {
                vec![b'?']
            } else {
                vec![FD_MARKER, 1]
            };
            let fds = if mode == 3 {
                Vec::new()
            } else {
                vec![capability.as_fd()]
            };
            aterm_uds::fdpass::send_with_fds(&send, &data, &fds).unwrap();
            assert_eq!(receive(&recv, Instant::now() + HANDOFF).is_ok(), mode == 0);
        }
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A physical bootstrap-channel challenge bound to the actual constructor member.
//! The challenge travels only on the native-owned socket; its acknowledgement
//! arrives on that member's authenticated SDK connection. Neither is SDK readiness.
use super::*;
use std::io::{Read, Write};

pub(super) struct Control {
    stream: aterm_uds::CtlStream,
    state: Mutex<State>,
    delivery: Mutex<()>,
    receipt_failed: std::sync::atomic::AtomicBool,
}
enum State {
    Fresh,
    Pending(Proof),
    Bound(Proof),
    Failed,
}
struct Proof {
    connection: Connection,
    member: String,
    operation: String,
    nonce: String,
}
impl Proof {
    fn matches(&self, connection: &Connection, member: &str, operation: &str) -> bool {
        self.connection.same(connection) && self.member == member && self.operation == operation
    }
}
impl Control {
    pub(super) fn new(stream: aterm_uds::CtlStream) -> std::io::Result<Self> {
        stream.set_write_timeout(Some(std::time::Duration::from_secs(60)))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
        Ok(Self {
            stream,
            state: Mutex::new(State::Fresh),
            delivery: Mutex::new(()),
            receipt_failed: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub(super) fn bound(&self) -> Result<(Connection, String), Error> {
        if self
            .receipt_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Error::Stale);
        }
        match &*lock(&self.state)? {
            State::Bound(proof) | State::Pending(proof) if !proof.connection.is_live() => {
                Err(Error::Stale)
            }
            State::Bound(proof) => Ok((proof.connection.clone(), proof.member.clone())),
            State::Fresh | State::Pending(_) => Err(Error::Busy),
            _ => Err(Error::Stale),
        }
    }
    pub(super) fn send_attachment(&self, stream: &aterm_uds::CtlStream) -> Result<String, Error> {
        use std::os::fd::AsFd;
        let _delivery = lock(&self.delivery)?;
        self.bound()?;
        let nonce = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let header = format!("A{nonce}");
        let result = (|| {
            let sent = loop {
                match aterm_uds::fdpass::send_with_fds(
                    &self.stream,
                    header.as_bytes(),
                    &[stream.as_fd()],
                ) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => break result.map_err(|_| Error::Unavailable)?,
                }
            };
            if sent == 0 || sent > header.len() {
                return Err(Error::Unavailable);
            }
            let mut socket = &self.stream;
            socket
                .write_all(&header.as_bytes()[sent..])
                .map_err(|_| Error::Unavailable)?;
            // Retain the sender's actual endpoint through physical reception.
            // A queued SCM_RIGHTS write alone is not a completed handoff.
            let mut receipt = [0; 32];
            socket
                .read_exact(&mut receipt)
                .map_err(|_| Error::Unavailable)?;
            if !aterm_digest::ct_eq(&receipt, nonce.as_bytes()) {
                return Err(Error::Denied);
            }
            // The matching physical receipt completes transfer custody. A
            // concurrent metadata reader must not turn that completed transfer
            // into an uncertain send. OwnedService separately rechecks the
            // original constructor, SDK and foreground before exposing a right.
            Ok(nonce.clone())
        })();
        if result.is_err() {
            // An uncertain receipt cannot be consumed by a different delivery.
            self.receipt_failed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }
    pub(super) fn send_packet(&self, packet: &[u8]) -> Result<(), Error> {
        let _delivery = lock(&self.delivery)?;
        self.bound()?;
        let mut stream = &self.stream;
        stream.write_all(packet).map_err(|_| Error::Unavailable)
    }
    fn begin(&self, connection: &Connection, member: &str, operation: &str) -> Result<(), Error> {
        if self
            .receipt_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        match &*state {
            State::Pending(proof) | State::Bound(proof) => {
                return if proof.matches(connection, member, operation) {
                    Ok(())
                } else {
                    Err(Error::Stale)
                };
            }
            State::Failed => return Err(Error::Stale),
            State::Fresh => {}
        }
        let nonce = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        // A partial or uncertain write poisons this bootstrap channel. It must
        // never start a second challenge on the same byte stream.
        *state = State::Failed;
        let mut stream = &self.stream;
        stream
            .write_all(format!("ALABCTL1{nonce}").as_bytes())
            .map_err(|_| Error::Unavailable)?;
        *state = State::Pending(Proof {
            connection: connection.clone(),
            member: member.into(),
            operation: operation.into(),
            nonce,
        });
        Ok(())
    }
    fn acknowledge(
        &self,
        connection: &Connection,
        member: &str,
        operation: &str,
        nonce: &str,
    ) -> Result<(), Error> {
        if self
            .receipt_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        let matches = match &*state {
            State::Pending(proof) | State::Bound(proof) => {
                proof.matches(connection, member, operation)
                    && aterm_digest::ct_eq(proof.nonce.as_bytes(), nonce.as_bytes())
            }
            _ => false,
        };
        if !matches {
            return Err(Error::Denied);
        }
        if matches!(&*state, State::Bound(_)) {
            return Ok(());
        }
        if let State::Pending(proof) = std::mem::replace(&mut *state, State::Failed) {
            *state = State::Bound(proof);
        } else {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

impl Registry {
    fn owned_control(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        controller_operation: &str,
    ) -> Result<Arc<Control>, Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
        // This role is issued independently of credential members. Its opaque
        // id cannot preload credentials or assert SDK readiness.
        self.frontend_controller(peer, connection, member, controller_operation)?;
        let state = lock(&self.state)?;
        let launch = state
            .launches
            .get(peer.launch.as_ref().ok_or(Error::Denied)?)
            .ok_or(Error::Stale)?;
        let LaunchBinding::Native(binding) = &launch.binding else {
            return Err(Error::Denied);
        };
        binding.entry()?;
        if launch.process.as_ref() != Some(&peer.process)
            || launch.sid != peer.caller.sid
            || launch.native_restart.is_some()
        {
            return Err(Error::Denied);
        }
        let issued = state.controllers.get(member).ok_or(Error::Stale)?;
        if !issued.bound_to(&peer.caller, &peer.process, connection) {
            return Err(Error::Denied);
        }
        let control = lock(&binding.1)?.clone();
        control.ok_or(Error::Unsupported)
    }
    /// Begin a physical control-socket challenge for this native-owned child.
    /// No nonce is returned on the SDK connection. Exact retries write nothing.
    ///
    /// # Errors
    /// The child, independent controller or connection is stale, foreign, unbound,
    /// or has no owned socket; another operation owns the proof; socket IO fails.
    pub fn begin_owned_control(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        controller_operation: &str,
        operation: &str,
    ) -> Result<(), Error> {
        if !hex(operation, 32) {
            return Err(Error::Denied);
        }
        let control = self.owned_control(peer, connection, member, controller_operation)?;
        control.begin(connection, member, operation)?;
        if !Arc::ptr_eq(
            &control,
            &self.owned_control(peer, connection, member, controller_operation)?,
        ) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    /// Acknowledge the challenge received on the original owned socket through
    /// the same independently issued controller and SDK connection. This grants no
    /// source use, frontend attachment, or SDK readiness.
    ///
    /// # Errors
    /// As `begin_owned_control`, or the exact challenge has not been received.
    pub fn acknowledge_owned_control(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        controller_operation: &str,
        operation: &str,
        nonce: &str,
    ) -> Result<(), Error> {
        if !hex(operation, 32) || !hex(nonce, 32) {
            return Err(Error::Denied);
        }
        let control = self.owned_control(peer, connection, member, controller_operation)?;
        control.acknowledge(connection, member, operation, nonce)?;
        if !Arc::ptr_eq(
            &control,
            &self.owned_control(peer, connection, member, controller_operation)?,
        ) {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "codex_source_owned_control_tests.rs"]
mod tests;

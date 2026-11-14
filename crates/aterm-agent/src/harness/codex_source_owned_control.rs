// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A physical bootstrap-channel challenge bound to the actual constructor member.
//! The challenge travels only on the native-owned socket; its acknowledgement
//! arrives on that member's authenticated SDK connection. Neither is SDK readiness.
use super::*;
use std::io::Write;

pub(super) struct Control {
    stream: aterm_uds::CtlStream,
    state: Mutex<State>,
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
        Ok(Self {
            stream,
            state: Mutex::new(State::Fresh),
        })
    }
    pub(super) fn bound(&self) -> Result<(Connection, String), Error> {
        match &*lock(&self.state)? {
            State::Bound(proof) | State::Pending(proof) if !proof.connection.is_live() => {
                Err(Error::Stale)
            }
            State::Bound(proof) => Ok((proof.connection.clone(), proof.member.clone())),
            State::Fresh | State::Pending(_) => Err(Error::Busy),
            _ => Err(Error::Stale),
        }
    }
    fn begin(&self, connection: &Connection, member: &str, operation: &str) -> Result<(), Error> {
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
    ) -> Result<Arc<Control>, Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
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
        let actor = state
            .actors
            .get(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?;
        let issued = actor.issued.get(member).ok_or(Error::Stale)?;
        if actor.process != peer.process
            || issued.target.is_some()
            || issued.parent_use.is_some()
            || !issued.connection.same(connection)
        {
            return Err(Error::Denied);
        }
        let control = lock(&binding.1)?.clone();
        control.ok_or(Error::Unsupported)
    }
    /// Begin a physical control-socket challenge for this native-owned child.
    /// No nonce is returned on the SDK connection. Exact retries write nothing.
    ///
    /// # Errors
    /// The child, constructor member or connection is stale, foreign, unbound,
    /// or has no owned socket; another operation owns the proof; socket IO fails.
    pub fn begin_owned_control(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        operation: &str,
    ) -> Result<(), Error> {
        if !hex(operation, 32) {
            return Err(Error::Denied);
        }
        let control = self.owned_control(peer, connection, member)?;
        control.begin(connection, member, operation)?;
        if !Arc::ptr_eq(&control, &self.owned_control(peer, connection, member)?) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    /// Acknowledge the challenge received on the original owned socket through
    /// the same admitted constructor member and SDK connection. This grants no
    /// source use, frontend attachment, or SDK readiness.
    ///
    /// # Errors
    /// As `begin_owned_control`, or the exact challenge has not been received.
    pub fn acknowledge_owned_control(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        operation: &str,
        nonce: &str,
    ) -> Result<(), Error> {
        if !hex(operation, 32) || !hex(nonce, 32) {
            return Err(Error::Denied);
        }
        let control = self.owned_control(peer, connection, member)?;
        control.acknowledge(connection, member, operation, nonce)?;
        if !Arc::ptr_eq(&control, &self.owned_control(peer, connection, member)?) {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "codex_source_owned_control_tests.rs"]
mod tests;

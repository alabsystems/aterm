// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native producer transport proof, never CPU/memory admission or image-role authority.
use aterm_ctl::resource_admission_transport::Peer;
use std::{
    io,
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, Instant},
};
use trust_resource_controller::{BirthKind, Channel, Identity as WireIdentity, ProducerChallenge};
use trust_resource_provenance::{
    PinnedProgram,
    process::{self, Identity},
};

/// Possession establishes only this original native helper/parent/socket exchange.
/// The owner separately authorizes producer image, live job, private capability
/// and current retained resources. Deliberately has no Debug or serialization.
pub(crate) struct TransportProof {
    _channel: Channel,
    peer_socket: UnixStream,
    program: Arc<PinnedProgram>,
    helper: Identity,
    producer: Identity,
    root: [u8; 16],
    call: [u8; 16],
}
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native producer transport refused",
    )
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
fn capture(
    program: &PinnedProgram,
    peer: Peer,
    deadline: Instant,
) -> io::Result<(Identity, Identity)> {
    time(deadline)?;
    let helper = process::read_identity(peer.pid)?;
    if helper.birth != peer.birth {
        return Err(refused());
    }
    program.validate_cached_process(helper, deadline)?;
    let producer = process::read_identity(helper.parent)?;
    #[cfg(target_os = "macos")]
    if helper.parent_unique_id == 0 || helper.parent_unique_id != producer.unique_id {
        return Err(refused());
    }
    if process::read_identity(helper.pid)? != helper
        || process::read_identity(producer.pid)? != producer
    {
        return Err(refused());
    }
    time(deadline)?;
    Ok((helper, producer))
}
impl TransportProof {
    /// Challenge the actual creator peer of the received socket and retain the
    /// original exchange. JSON identities and decoded challenge bytes grant nothing.
    /// # Errors
    /// Missing/stale peer, helper image/parent mismatch, unsupported credentials,
    /// deadline, malformed answer or any identity change refuses the exchange.
    pub(crate) fn authenticate(
        proof: UnixStream,
        helper: Peer,
        expected_program: Arc<PinnedProgram>,
        root: [u8; 16],
        call: [u8; 16],
        deadline: Instant,
    ) -> io::Result<Self> {
        time(deadline)?;
        let (identity, producer) = capture(&expected_program, helper, deadline)?;
        process::check_peer(&proof, producer)?;
        aterm_uds::keep_out_of_children(&proof)?;
        let peer_socket = aterm_uds::kept_out(|| proof.try_clone())?;
        let mut channel = Channel::new(proof)?;
        let mut nonce = [0; 32];
        aterm_uds::rand::fill(&mut nonce)?;
        let challenge = ProducerChallenge {
            root_attempt: root,
            member_attempt: call,
            helper: WireIdentity {
                pid: identity.pid,
                birth: identity.birth,
                #[cfg(target_os = "macos")]
                birth_kind: BirthKind::MacosWallMicroseconds,
                #[cfg(target_os = "linux")]
                birth_kind: BirthKind::LinuxStartTicks,
            },
            nonce,
        };
        time(deadline)?;
        channel.send(&challenge.encode()?, deadline)?;
        challenge.verify_answer(&channel.receive(deadline)?)?;
        let bound = Self {
            _channel: channel,
            peer_socket,
            program: expected_program,
            helper: identity,
            producer,
            root,
            call,
        };
        bound.validate_until(deadline)?;
        time(deadline)?;
        Ok(bound)
    }
    /// Fresh non-interactive validation before GO. This performs no challenge or
    /// resource mutation and never interprets identity as permission to signal.
    pub(crate) fn validate(&self) -> io::Result<()> {
        self.validate_at(Instant::now())
    }
    fn validate_at(&self, started: Instant) -> io::Result<()> {
        // Recheck the original native identities, image and peer socket. Wall
        // time detects a hang; scheduler delay alone is no changed provenance.
        self.validate_until(started + Duration::from_secs(60))
    }
    fn validate_until(&self, deadline: Instant) -> io::Result<()> {
        let (helper, producer) = capture(
            &self.program,
            Peer {
                pid: self.helper.pid,
                birth: self.helper.birth,
            },
            deadline,
        )?;
        if helper != self.helper || producer != self.producer {
            return Err(refused());
        }
        process::check_peer(&self.peer_socket, self.producer)?;
        if capture(
            &self.program,
            Peer {
                pid: self.helper.pid,
                birth: self.helper.birth,
            },
            deadline,
        )? != (self.helper, self.producer)
        {
            return Err(refused());
        }
        time(deadline)
    }
    pub(crate) fn producer(&self) -> Identity {
        self.producer
    }
    pub(crate) fn helper(&self) -> Identity {
        self.helper
    }
    pub(crate) fn binding(&self) -> ([u8; 16], [u8; 16]) {
        (self.root, self.call)
    }
}
#[cfg(test)]
#[path = "resource_producer/tests.rs"]
mod tests;

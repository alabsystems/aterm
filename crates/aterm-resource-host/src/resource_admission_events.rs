// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental private lifecycle transport. A slow reader cannot stall admission
//! or prevent the host from servicing another job's cancellation or timeout.
use aterm_types::resource_admission::{
    AccountedEventFrame, AccountingObservation, Binding, Cancel, Event, EventFrame,
};
use aterm_uds::CtlStream;
use std::{
    collections::VecDeque,
    io::{self, IoSlice, Read as _},
    os::fd::OwnedFd,
};

pub(in super::super) struct Events {
    stream: CtlStream,
    binding: Binding,
    output: VecDeque<Vec<u8>>,
    sent: usize,
    input: Vec<u8>,
    failed: bool,
    cancel_requested: bool,
    metadata_root: Option<[u8; 16]>,
    prepared: bool,
    inputs_unchanged: Option<Result<(), String>>,
    accounted: bool,
    observe_accounting: bool,
    workload: bool,
    linker: Option<trust_resource_controller::Loan>,
    produced: Option<(
        trust_resource_controller::workload::host::produced::Key,
        trust_resource_controller::workload::host::MemberKind,
    )>,
    contained: Option<trust_resource_controller::workload::host::contained::Request>,
    contained_terminal: bool,
    #[cfg(all(test, target_os = "macos"))]
    sent_frames: Vec<Vec<u8>>,
    #[cfg(all(test, target_os = "macos"))]
    audit: Option<std::fs::File>,
}
impl Events {
    pub(in super::super) fn new(fd: OwnedFd, binding: Binding) -> io::Result<Self> {
        let stream = CtlStream::from(fd);
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            binding,
            output: VecDeque::new(),
            sent: 0,
            input: Vec::new(),
            failed: false,
            cancel_requested: false,
            metadata_root: None,
            prepared: false,
            inputs_unchanged: None,
            accounted: false,
            observe_accounting: false,
            workload: false,
            linker: None,
            produced: None,
            contained: None,
            contained_terminal: false,
            #[cfg(all(test, target_os = "macos"))]
            sent_frames: Vec::new(),
            #[cfg(all(test, target_os = "macos"))]
            audit: None,
        })
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(in super::super) fn audit_to(&mut self, mut file: std::fs::File) {
        use std::io::Write as _;
        assert!(self.audit.is_none());
        for frame in &self.sent_frames {
            file.write_all(frame).unwrap();
        }
        self.audit = Some(file);
    }
    pub(in super::super) fn emit(&mut self, event: Event) -> io::Result<()> {
        if self.accounted {
            match event {
                Event::Refused { .. } => {
                    return self.emit_terminal(event, AccountingObservation::uncollected());
                }
                Event::Finished { .. } => {
                    return Err(io::Error::other(
                        "accounted completion requires the original owning-wait observation",
                    ));
                }
                _ => {}
            }
        }
        // Queued, Started and Finished are bounded lifecycle facts, not logs.
        // A refusal replaces execution, so no attempt needs more than three.
        if self.output.len() >= 4 || self.failed {
            return Err(io::Error::other("private lifecycle unavailable"));
        }
        let bytes = if let Some(root) = self.metadata_root {
            self.metadata_event(root, event)?
        } else {
            EventFrame {
                binding: self.binding.clone(),
                event,
            }
            .encode()?
        };
        self.enqueue(bytes)
    }
    pub(in super::super) fn account(&mut self) -> io::Result<()> {
        if self.metadata_root.is_some() || self.prepared || self.accounted {
            return Err(io::Error::other("accounting purpose or phase differs"));
        }
        self.accounted = true;
        Ok(())
    }
    pub(in super::super) fn requires_accounting(&self) -> bool {
        self.accounted
    }
    pub(in super::super) fn observe_accounting(&mut self) {
        self.observe_accounting = true;
    }
    pub(in super::super) fn collect_accounting(&self) -> bool {
        self.accounted || self.observe_accounting
    }
    pub(in super::super) fn emit_terminal(
        &mut self,
        event: Event,
        observation: AccountingObservation,
    ) -> io::Result<()> {
        if self.contained.is_some() && matches!(event, Event::Finished { .. }) {
            return Err(io::Error::other(
                "contained terminal requires actual native issuer",
            ));
        }
        if !self.accounted {
            return self.emit(event);
        }
        if self.output.len() >= 4 || self.failed {
            return Err(io::Error::other("private lifecycle unavailable"));
        }
        if self.workload || self.linker.is_some() {
            let root = self
                .metadata_root
                .ok_or_else(|| io::Error::other("workload root binding missing"))?;
            let bytes = self.metadata_event(root, event)?;
            let event = trust_resource_controller::MetadataEvent::decode(
                &bytes,
                root,
                self.binding.attempt,
            )?;
            let bytes = match event {
                trust_resource_controller::MetadataEvent::Completed(completed) => {
                    if let Some(loan) = self.linker {
                        return self.enqueue(
                            trust_resource_controller::linker::Completion {
                                loan,
                                completed,
                                observation,
                            }
                            .encode()?,
                        );
                    }
                    trust_resource_controller::workload::host::Completion {
                        phase: self.produced.as_ref().map_or(0, |(key, _)| key.ordinal),
                        kind: self.produced.as_ref().map_or(
                            trust_resource_controller::workload::host::MemberKind::CompilerQuery,
                            |(_, kind)| *kind,
                        ),
                        completed,
                        observation,
                    }
                    .encode()?
                }
                _ => bytes,
            };
            return self.enqueue(bytes);
        }
        let bytes = AccountedEventFrame {
            finished: EventFrame {
                binding: self.binding.clone(),
                event,
            },
            observation,
        }
        .encode()?;
        self.enqueue(bytes)
    }
    pub(in super::super) fn metadata(&mut self, root: [u8; 16]) {
        self.metadata_root = Some(root);
    }
    pub(in super::super) fn workload(&mut self, root: [u8; 16]) -> io::Result<()> {
        self.account()?;
        self.metadata_root = Some(root);
        self.workload = true;
        Ok(())
    }
    pub(in super::super) fn linker(
        &mut self,
        request: &trust_resource_controller::linker::Request,
    ) -> io::Result<()> {
        self.account()?;
        self.metadata_root = Some(request.member.binding.root_attempt);
        self.linker = Some(request.loan()?);
        Ok(())
    }
    pub(in super::super) fn linker_prepared(
        &mut self,
        prepared: &trust_resource_controller::linker::Prepared,
    ) -> io::Result<()> {
        if self.prepared
            || self.metadata_root != Some(prepared.root_attempt)
            || self.binding.attempt != prepared.member_attempt
            || self.linker != Some(prepared.loan)
        {
            return Err(io::Error::other(
                "linker inventory binding or phase differs",
            ));
        }
        self.enqueue(prepared.encode()?)?;
        self.prepared = true;
        Ok(())
    }
    pub(in super::super) fn prepared(
        &mut self,
        inventory: &trust_resource_controller::MetadataPrepared,
    ) -> io::Result<()> {
        if self.metadata_root != Some(inventory.root_attempt)
            || self.binding.attempt != inventory.member_attempt
            || self.prepared
        {
            return Err(io::Error::other(
                "metadata inventory binding or phase differs",
            ));
        }
        self.enqueue(inventory.encode()?)?;
        self.prepared = true;
        Ok(())
    }
    pub(in super::super) fn produced(
        &mut self,
        key: trust_resource_controller::workload::host::produced::Key,
        kind: trust_resource_controller::workload::host::MemberKind,
    ) -> io::Result<()> {
        key.validate()?;
        self.workload(key.root_attempt)?;
        self.produced = Some((key, kind));
        Ok(())
    }
    pub(in super::super) fn produced_prepared(
        &mut self,
        inventory: &trust_resource_controller::workload::host::produced::ProducedPrepared,
    ) -> io::Result<()> {
        if self.prepared
            || self.binding.attempt != inventory.member_attempt
            || self
                .produced
                .as_ref()
                .is_none_or(|(key, _)| key != &inventory.key)
        {
            return Err(io::Error::other(
                "produced preparation binding or phase differs",
            ));
        }
        self.enqueue(inventory.encode()?)?;
        self.prepared = true;
        Ok(())
    }
    pub(in super::super) fn contained(
        &mut self,
        request: trust_resource_controller::workload::host::contained::Request,
    ) -> io::Result<()> {
        request.validate()?;
        if self.contained.is_some()
            || self.prepared
            || self.binding.attempt != request.produced.member.member_attempt
            || self.produced.as_ref()
                != Some(&(request.produced.key.clone(), request.produced.kind))
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.contained = Some(request);
        Ok(())
    }
    pub(in super::super) fn is_contained(&self) -> bool {
        self.contained.is_some()
    }
    pub(in super::super) fn emit_contained_refused(&mut self) -> io::Result<()> {
        if !self.is_contained() || self.contained_terminal || self.failed {
            return Err(io::Error::other(
                "contained refusal unavailable or already issued",
            ));
        }
        self.emit(Event::Refused {
            reason: "contained target did not reach GO".into(),
        })?;
        self.contained_terminal = true;
        Ok(())
    }
    pub(in super::super) fn emit_contained_terminal(
        &mut self,
        returned: &super::super::member_custody::ReturnedProducer,
        event: Event,
        observation: AccountingObservation,
    ) -> io::Result<()> {
        let request = self
            .contained
            .as_ref()
            .ok_or_else(|| io::Error::other("contained request missing"))?;
        if self.contained_terminal || self.failed {
            return Err(io::Error::other(
                "contained terminal unusable or already issued",
            ));
        }
        let root = request.produced.key.root_attempt;
        let bytes = self.metadata_event(root, event)?;
        let trust_resource_controller::MetadataEvent::Completed(completed) =
            trust_resource_controller::MetadataEvent::decode(&bytes, root, self.binding.attempt)?
        else {
            return Err(io::ErrorKind::InvalidData.into());
        };
        let completion = trust_resource_controller::workload::host::Completion {
            phase: request.produced.key.ordinal,
            kind: request.produced.kind,
            completed,
            observation,
        };
        let bytes = returned.completion(request, completion)?.encode()?;
        self.enqueue(bytes)?;
        self.contained_terminal = true;
        Ok(())
    }
    pub(in super::super) fn inputs_unchanged(&mut self, result: Result<(), String>) {
        if !self.inputs_unchanged.as_ref().is_some_and(Result::is_err) {
            self.inputs_unchanged = Some(result);
        }
    }
    fn enqueue(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        if self.output.len() >= 4 || self.failed {
            return Err(io::Error::other("private lifecycle unavailable"));
        }
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(&bytes);
        self.output.push_back(frame);
        self.flush();
        Ok(())
    }
    pub(in super::super) fn flush(&mut self) {
        while let Some(frame) = self.output.front() {
            match aterm_uds::fdpass::send_vectored(
                &self.stream,
                &[IoSlice::new(&frame[self.sent..])],
            ) {
                Ok(0) => {
                    self.failed = true;
                    break;
                }
                Ok(n) => {
                    self.sent += n;
                    if self.sent == frame.len() {
                        #[cfg(all(test, target_os = "macos"))]
                        {
                            use std::io::Write as _;
                            assert!(self.sent_frames.len() < 4);
                            if let Some(audit) = &mut self.audit {
                                audit.write_all(frame).unwrap();
                            }
                            self.sent_frames.push(frame.clone());
                        }
                        self.output.pop_front();
                        self.sent = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.failed = true;
                    break;
                }
            }
        }
    }
    fn metadata_event(&self, root_attempt: [u8; 16], event: Event) -> io::Result<Vec<u8>> {
        use trust_resource_controller::{self as wire, MetadataCompletion, MetadataEvent};
        let status = match event {
            Event::Queued => wire::Status::Attached,
            Event::Started {
                pid,
                birth,
                monotonic_ns,
                unix_ns,
            } if self.prepared => wire::Status::Started(wire::Started {
                identity: wire::Identity {
                    pid,
                    birth,
                    birth_kind: if cfg!(target_os = "macos") {
                        wire::BirthKind::MacosWallMicroseconds
                    } else {
                        wire::BirthKind::LinuxStartTicks
                    },
                },
                monotonic_ns,
                unix_ns,
            }),
            Event::Finished {
                outcome,
                cause,
                runtime_ns,
            } => wire::Status::Finished {
                outcome: match outcome {
                    aterm_types::resource_admission::Outcome::Exit(code) => {
                        wire::Outcome::Exit(code)
                    }
                    aterm_types::resource_admission::Outcome::Signal(signal) => {
                        wire::Outcome::Signal(signal)
                    }
                },
                cause: match cause {
                    aterm_types::resource_admission::CompletionCause::Exited => {
                        wire::CompletionCause::Exited
                    }
                    aterm_types::resource_admission::CompletionCause::Timeout => {
                        wire::CompletionCause::Timeout
                    }
                    aterm_types::resource_admission::CompletionCause::Cancelled => {
                        wire::CompletionCause::Cancelled
                    }
                    aterm_types::resource_admission::CompletionCause::ResourceLimit => {
                        wire::CompletionCause::ResourceLimit
                    }
                },
                runtime_ns,
            },
            Event::Refused { reason } => wire::Status::Refused { reason },
            _ => {
                return Err(io::Error::other(
                    "metadata started without held input inventory",
                ));
            }
        };
        let frame = wire::StatusFrame {
            root_attempt,
            member_attempt: self.binding.attempt,
            status,
        };
        if matches!(frame.status, wire::Status::Finished { .. }) {
            MetadataEvent::Completed(MetadataCompletion {
                finished: frame,
                inputs_unchanged: self.inputs_unchanged.clone().ok_or_else(|| {
                    io::Error::other("metadata inputs were not rechecked before resource return")
                })?,
            })
            .encode()
        } else {
            MetadataEvent::Status(frame).encode()
        }
    }
    pub(in super::super) fn abandon(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        self.failed = true;
    }
    pub(in super::super) fn drained(&mut self) -> bool {
        self.flush();
        self.failed || self.output.is_empty()
    }
    pub(in super::super) fn cancelled(&mut self) -> bool {
        self.flush();
        if self.failed {
            return true;
        }
        if self.cancel_requested {
            // A valid cancellation keeps its observation channel alive for
            // the actual outcome. Any extra byte/replay withdraws that channel.
            let mut extra = [0; 1];
            match (&self.stream).read(&mut extra) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                _ => self.failed = true,
            }
            return true;
        }
        // The sole inbound record is an exact cancellation for this attempt.
        // Read at most its exact encoded length, preserving partial arrivals.
        let expected = match if let Some(root_attempt) = self.metadata_root {
            trust_resource_controller::Cancel {
                root_attempt,
                member_attempt: self.binding.attempt,
            }
            .encode()
        } else {
            (Cancel {
                binding: self.binding.clone(),
            })
            .encode()
        } {
            Ok(bytes) => bytes.len(),
            Err(_) => {
                self.failed = true;
                return true;
            }
        };
        let mut buffer = [0u8; 256];
        loop {
            let room = (expected + 4).saturating_sub(self.input.len());
            if room == 0 {
                break;
            }
            match (&self.stream).read(&mut buffer[..room.min(256)]) {
                Ok(0) => {
                    self.failed = true;
                    return true;
                }
                Ok(n) => self.input.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.failed = true;
                    return true;
                }
            }
        }
        if self.input.len() < 4 {
            return false;
        }
        let mut header = [0u8; 4];
        header.copy_from_slice(&self.input[..4]);
        if u32::from_be_bytes(header) as usize != expected {
            self.failed = true;
            return true;
        }
        if self.input.len() < expected + 4 {
            return false;
        }
        // Malformed input also withdraws this controller's interest. It never
        // names another job, frees a claim, or authorizes numeric signaling.
        let valid = if let Some(root_attempt) = self.metadata_root {
            trust_resource_controller::Cancel::decode(
                &self.input[4..],
                trust_resource_controller::Cancel {
                    root_attempt,
                    member_attempt: self.binding.attempt,
                },
            )
            .is_ok()
        } else {
            Cancel::decode(&self.input[4..], &self.binding).is_ok()
        };
        self.cancel_requested = valid;
        self.failed = !valid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    fn pair() -> (Events, CtlStream) {
        let (host, client) = CtlStream::pair().unwrap();
        let binding = Binding {
            session: "s-a".into(),
            attempt: [7; 16],
        };
        (Events::new(host.into(), binding).unwrap(), client)
    }
    #[test]
    fn cancellation_preserves_partial_frame_without_blocking_another_tick() {
        let (mut events, mut client) = pair();
        let cancel = Cancel {
            binding: events.binding.clone(),
        }
        .encode()
        .unwrap();
        let header = (cancel.len() as u32).to_be_bytes();
        client.write_all(&header[..2]).unwrap();
        assert!(!events.cancelled());
        client.write_all(&header[2..]).unwrap();
        client.write_all(&cancel[..3]).unwrap();
        assert!(!events.cancelled());
        client.write_all(&cancel[3..]).unwrap();
        assert!(events.cancelled());
    }
    #[test]
    fn disconnect_and_oversized_header_withdraw_only_the_controller() {
        let (mut events, client) = pair();
        drop(client);
        assert!(events.cancelled());
        let (mut events, mut client) = pair();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert!(events.cancelled());
        assert!(events.input.len() <= 256);
    }
    #[test]
    fn exact_cancel_keeps_actual_terminal_observable_but_replay_refuses() {
        let (mut events, mut client) = pair();
        let payload = Cancel {
            binding: events.binding.clone(),
        }
        .encode()
        .unwrap();
        client
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        client.write_all(&payload).unwrap();
        assert!(events.cancelled());
        assert!(!events.failed);
        events
            .emit(Event::Finished {
                outcome: aterm_types::resource_admission::Outcome::Signal(15),
                cause: aterm_types::resource_admission::CompletionCause::Cancelled,
                runtime_ns: 1,
            })
            .unwrap();
        let bytes = aterm_ctl::resource_admission_transport::read_frame(
            &client,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert!(matches!(
            EventFrame::decode(&bytes, &events.binding).unwrap().event,
            Event::Finished {
                cause: aterm_types::resource_admission::CompletionCause::Cancelled,
                ..
            }
        ));
        client.write_all(&[0]).unwrap();
        assert!(events.cancelled());
        assert!(events.failed);
        assert!(events.emit(Event::Queued).is_err());
    }
}

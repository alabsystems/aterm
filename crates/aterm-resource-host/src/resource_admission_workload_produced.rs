// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live trusted SDK registration is accepted only inside the original paid,
//! quiescent foreign phase. Its data and descriptor cannot create that phase.
use super::*;
use std::{
    fs::Metadata,
    os::{fd::OwnedFd, unix::fs::MetadataExt as _},
    sync::mpsc,
};
use trust_resource_controller::{MetadataFile, workload::host::produced as wire};
use trust_resource_provenance::program::produced::{HeldOutputRoot, HeldProducedProgram};

#[path = "resource_admission_workload_produced_member.rs"]
mod member_owner;
pub(in crate::actor::owner) use member_owner::Member;

const MAX_OUTPUTS: usize = 4096;
pub(crate) struct RegistrationPending {
    receive: mpsc::Receiver<wire::RegistrationReply>,
}
impl RegistrationPending {
    pub(crate) fn poll(&self) -> io::Result<Option<wire::RegistrationReply>> {
        match self.receive.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

#[derive(Default)]
pub(super) struct Outputs {
    seen: std::collections::BTreeSet<[u8; 16]>,
    entries: BTreeMap<[u8; 16], Output>,
}
enum Output {
    Preparing(Box<Preparing>),
    Registered(Arc<Registered>),
    Refused,
}
struct Preparing {
    // Join before any original guard is released, including actor unwind.
    worker: preparation::Worker<io::Result<Registered>>,
    request: wire::Registration,
    peer: Peer,
    reply: mpsc::SyncSender<wire::RegistrationReply>,
    _primary: Arc<Primary>,
    pub(super) _sdk: Arc<sdk::Keeper>,
    _output: Arc<HeldOutputRoot>,
    _locks: Arc<output::OutputLocks>,
}
pub(super) struct Registered {
    pub(super) request: wire::Registration,
    pub(super) program: Arc<HeldProducedProgram>,
    // Keep the SDK's actual offered original description as well as the
    // independently captured SAME image; it is never reconstructed by labels.
    _original: File,
    _before: Metadata,
    _primary: Arc<Primary>,
    _sdk: Arc<sdk::Keeper>,
    _locks: Arc<output::OutputLocks>,
}
impl Outputs {
    pub(super) fn registered(&self, key: &wire::Key) -> io::Result<Arc<Registered>> {
        match self.entries.get(&key.output) {
            Some(Output::Registered(registered)) if registered.request.key == *key => {
                Ok(Arc::clone(registered))
            }
            _ => Err(refused("live original output registration unavailable")),
        }
    }
    pub(super) fn preparing(&self) -> bool {
        self.entries
            .values()
            .any(|entry| matches!(entry, Output::Preparing(_)))
    }
    pub(super) fn leave(&mut self, phase: [u8; 16]) {
        self.entries.retain(|_, entry| match entry {
            Output::Registered(entry) => entry.request.key.phase != phase,
            Output::Preparing(_) => true,
            Output::Refused => false,
        });
    }
}
fn readonly(file: &File) -> io::Result<Metadata> {
    let meta = file.metadata()?;
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o022 != 0
        || flags < 0
        || flags & libc::O_ACCMODE != libc::O_RDONLY
        || descriptor_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
    {
        return Err(refused(
            "registered executable description is not original read-only private input",
        ));
    }
    Ok(meta)
}
fn same_stamp(before: &Metadata, after: &Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mode() == after.mode()
        && before.uid() == after.uid()
        && before.gid() == after.gid()
        && before.nlink() == after.nlink()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
fn observed_file(
    item: &trust_resource_provenance::program::FileObservation,
) -> io::Result<MetadataFile> {
    let mut sha256 = [0; 32];
    if !aterm_codec::hex::decode_into(item.sha256.as_bytes(), &mut sha256) {
        return Err(refused("original produced digest unavailable"));
    }
    Ok(MetadataFile {
        path: item.path.as_os_str().as_bytes().to_vec(),
        sha256,
        bytes: item.bytes,
        device: item.device,
        inode: item.inode,
    })
}
fn capture(
    request: wire::Registration,
    original: File,
    primary: Arc<Primary>,
    sdk: Arc<sdk::Keeper>,
    output: Arc<HeldOutputRoot>,
    locks: Arc<output::OutputLocks>,
    deadline: Instant,
) -> io::Result<Registered> {
    let before = readonly(&original)?;
    let program = std::path::Path::new(std::ffi::OsStr::from_bytes(&request.program.path));
    let roots: Vec<_> = request
        .runtime_roots
        .iter()
        .map(|path| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
        .collect();
    let held = output.program(
        sdk.original_inputs(),
        program,
        &request.program.sha256,
        &roots,
        deadline,
    )?;
    // The hash/runtime capture belongs to the native owner. This equality
    // check binds it to the offered original open object, not just its path.
    let item = &held.observation().executable;
    if item.device != before.dev() || item.inode != before.ino() || item.bytes != before.len() {
        return Err(refused(
            "registered descriptor differs from current original UnitOutput",
        ));
    }
    held.validate(deadline)?;
    if !same_stamp(&before, &readonly(&original)?) {
        return Err(refused(
            "registered original executable changed during capture",
        ));
    }
    Ok(Registered {
        request,
        program: Arc::new(held),
        _original: original,
        _before: before,
        _primary: primary,
        _sdk: sdk,
        _locks: locks,
    })
}
fn validate(job: &mut Job, peer: Peer, request: &wire::Registration) -> io::Result<Instant> {
    request.validate()?;
    if !job.launched
        || job.reaped
        || job.is_settled()
        || job.cleanup_at.is_some()
        || peer.pid != job.child.child_mut().id()
        || peer.birth != job.root_birth
        || !matches!(job.custody, Custody::Cooperative(_))
    {
        return Err(refused(
            "registration peer is not its original live cooperative root",
        ));
    }
    let sdk = job
        .metadata_sdk
        .as_ref()
        .ok_or_else(|| refused("original registration SDK unavailable"))?;
    sdk.validate_loaded(peer.pid)?;
    let state = job
        .workload
        .as_ref()
        .ok_or_else(|| refused("original registration owner unavailable"))?;
    let ready = state
        .ready
        .as_ref()
        .ok_or_else(|| refused("original registration root not ready"))?;
    let active = state
        .phase
        .active()
        .ok_or_else(|| refused("no original active foreign phase"))?;
    if request.key.root_attempt != job.binding.attempt
        || request.key.invocation_sha256 != ready.sha256
        || request.key.phase != active.id
        || request.key.ordinal != active.ordinal
        || state.control.as_ref().is_none_or(|link| link.closed())
    {
        return Err(refused(
            "registration is foreign, replayed or outside its original phase",
        ));
    }
    let deadline = (job.started
        + job
            .timeout
            .ok_or_else(|| refused("registration root deadline unavailable"))?)
    .min(native_inputs::deadline(Instant::now()));
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    ready._output.validate(deadline)?;
    state
        .output
        .as_ref()
        .ok_or_else(|| refused("original graph output locks not retained"))?
        .validate(&ready._output, deadline)?;
    Ok(deadline)
}
fn reason(error: &io::Error) -> String {
    let text: String = error
        .to_string()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(512)
        .collect();
    if text.is_empty() {
        "original output registration refused".into()
    } else {
        text
    }
}
impl Owner {
    pub(in crate::actor) fn workload_register(
        &mut self,
        peer: Peer,
        session: &str,
        request: wire::Registration,
        original: OwnedFd,
    ) -> io::Result<RegistrationPending> {
        self.fence.check_current()?;
        let job = self
            .jobs
            .get_mut(&(session.to_owned(), request.key.root_attempt))
            .ok_or_else(|| refused("original registration root unavailable"))?;
        let deadline = validate(job, peer, &request)?;
        job::inspection(&self.fence, &self.debt, job)?;
        if self.faulted || self.debt.reserved()? != job.cpu_ceiling {
            return Err(refused(
                "registration original durable domain custody changed",
            ));
        }
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original output owner unavailable"))?;
        if !job.members.is_empty()
            || !job.loans.is_empty()
            || !state.bank.drained()
            || state.produced.preparing()
            || state.produced.seen.len() >= MAX_OUTPUTS
            || !state.produced.seen.insert(request.key.output)
        {
            return Err(refused(
                "registration needs original quiescence and a fresh bounded output",
            ));
        }
        if let Some(producer) = request.producer_attempt {
            let source = state
                .census
                .get(&producer)
                .ok_or_else(|| refused("cold output omitted its actual producer"))?;
            if !matches!(source.kind, CensusKind::Compiler)
                || source.uncertain
                || !source.retired
                || !source.drained
                || !matches!(
                    source.completion.as_ref(),
                    Some(member::Completed {
                        outcome: aterm_types::resource_admission::Outcome::Exit(0),
                        cause: CompletionCause::Exited,
                        ..
                    })
                )
            {
                return Err(refused("cold output producer has not positively completed"));
            }
        }
        let output = Arc::clone(
            &state
                .ready
                .as_ref()
                .ok_or_else(|| refused("original output owner missing"))?
                ._output,
        );
        let locks = state
            .output
            .as_ref()
            .ok_or_else(|| refused("original graph output locks missing"))?
            .clone();
        let primary = Arc::clone(&state.primary);
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original SDK missing"))?
            .clone();
        let (reply, receive) = mpsc::sync_channel(1);
        let worker_request = request.clone();
        let worker_primary = Arc::clone(&primary);
        let worker_sdk = Arc::clone(&sdk);
        let worker_output = Arc::clone(&output);
        let worker_locks = Arc::clone(&locks);
        let worker = std::thread::Builder::new()
            .name("resource-produced-register".into())
            .spawn(move || {
                capture(
                    worker_request,
                    File::from(original),
                    worker_primary,
                    worker_sdk,
                    worker_output,
                    worker_locks,
                    deadline,
                )
            })?;
        state.produced.entries.insert(
            request.key.output,
            Output::Preparing(Box::new(Preparing {
                worker: preparation::Worker::new(worker),
                request,
                peer,
                reply,
                _primary: primary,
                _sdk: sdk,
                _output: output,
                _locks: locks,
            })),
        );
        Ok(RegistrationPending { receive })
    }
    pub(in crate::actor) fn tick_workload_registrations(&mut self) {
        for job in self.jobs.values_mut() {
            let finished: Vec<_> = job
                .workload
                .as_ref()
                .into_iter()
                .flat_map(|state| state.produced.entries.iter())
                .filter_map(|(output, entry)| match entry {
                    Output::Preparing(preparing) if preparing.worker.is_finished() => Some(*output),
                    _ => None,
                })
                .collect();
            for output in finished {
                let Some(Output::Preparing(preparing)) = job
                    .workload
                    .as_mut()
                    .and_then(|state| state.produced.entries.remove(&output))
                else {
                    continue;
                };
                let Preparing {
                    worker,
                    request,
                    peer,
                    reply,
                    ..
                } = *preparing;
                let result = worker
                    .join()
                    .map_err(|_| io::Error::other("native registration worker panicked"))
                    .and_then(|value| value)
                    .and_then(|registered| {
                        job::inspection(&self.fence, &self.debt, job)?;
                        if self.faulted || self.debt.reserved()? != job.cpu_ceiling {
                            return Err(refused(
                                "registration original durable domain custody changed",
                            ));
                        }
                        let deadline = validate(job, peer, &request)?;
                        registered.program.validate(deadline)?;
                        if !same_stamp(&registered._before, &readonly(&registered._original)?) {
                            return Err(refused("original registered descriptor changed"));
                        }
                        Ok(registered)
                    });
                let (entry, response) = match result {
                    Ok(registered) => {
                        match observed_file(&registered.program.observation().executable) {
                            Ok(executable) => (
                                Output::Registered(Arc::new(registered)),
                                wire::RegistrationReply::Registered(wire::RegistrationAck {
                                    key: request.key.clone(),
                                    executable,
                                }),
                            ),
                            Err(error) => (
                                Output::Refused,
                                wire::RegistrationReply::Refused {
                                    key: request.key.clone(),
                                    reason: reason(&error),
                                },
                            ),
                        }
                    }
                    Err(error) => (
                        Output::Refused,
                        wire::RegistrationReply::Refused {
                            key: request.key.clone(),
                            reason: reason(&error),
                        },
                    ),
                };
                if let Some(state) = job.workload.as_mut() {
                    state.produced.entries.insert(output, entry);
                }
                let _ = reply.try_send(response);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn actual_registration_descriptor_requires_readonly_unaliased_unchanged_input() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("unit-output");
        std::fs::write(&path, b"actual native input descriptor").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original = File::open(&path).unwrap();
        let before = readonly(&original).unwrap();
        let copy = original.try_clone().unwrap();
        assert!(same_stamp(&before, &readonly(&copy).unwrap()));
        let writable = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(readonly(&writable).is_err());
        let alias = scratch.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(
            readonly(&original).is_err(),
            "hardlinked output cannot become original executable custody"
        );
        std::fs::remove_file(alias).unwrap();
        let held = readonly(&original).unwrap();
        std::fs::rename(&path, scratch.path().join("retained-original")).unwrap();
        std::fs::write(&path, b"substituted output").unwrap();
        let substituted = File::open(&path).unwrap();
        assert!(!same_stamp(&held, &readonly(&substituted).unwrap()));
        assert_eq!(
            original.metadata().unwrap().ino(),
            held.ino(),
            "the offered original stays owned across pathname substitution"
        );
        std::fs::set_permissions(
            scratch.path().join("retained-original"),
            std::fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        assert!(
            !same_stamp(&held, &readonly(&original).unwrap()),
            "same-object permission drift is not adopted as a new baseline"
        );
    }
}

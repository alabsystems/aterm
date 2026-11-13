// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private fixed preflight attempts. The recipe worker owns artifact selection;
//! the Host owns the original paid child and its durable retirement.

use super::*;
use aterm_types::resource_admission::{Event, Outcome};
use std::{
    io::Read as _,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
};

pub(super) struct Prepared {
    command: Option<spawnfd::Command>,
    prelude: crate::metadata_prelude::Owner,
    program: PathBuf,
    image: File,
    before: std::fs::Metadata,
    deadline: Instant,
    limit: usize,
}

pub(super) fn output_limit(command: &spawnfd::Command) -> io::Result<usize> {
    let mut args = command.get_args();
    match (args.next(), args.next()) {
        (Some(arg), None) if arg == "-vV" => Ok(16384),
        (Some(arg), None) if arg == "--resource-capabilities" => Ok(8192),
        _ => Err(refused("native preflight needs an exact selected query")),
    }
}

impl Prepared {
    pub(super) fn new(command: spawnfd::Command, deadline: Instant) -> io::Result<Self> {
        if !cfg!(target_os = "macos") {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let limit = output_limit(&command)?;
        if command.guard_targets().len() != 1 {
            return Err(refused(
                "native preflight requires the original selected backing",
            ));
        }
        let program = PathBuf::from(command.get_program());
        if !program.is_absolute() || Instant::now() >= deadline {
            return Err(refused("native preflight selected executable unavailable"));
        }
        let image = aterm_uds::kept_out(|| File::open(&program))?;
        let before = image.metadata()?;
        if !before.is_file() || before.mode() & 0o111 == 0 {
            return Err(refused("native preflight selected image is not executable"));
        }
        let targets = command
            .guard_targets()
            .map(|fd| fd as u32)
            .collect::<Vec<_>>();
        let (mut prelude, mut child) =
            crate::metadata_prelude::Owner::new(&command, &targets, deadline)?;
        child
            .append_guards_from(&command)?
            .append_guards_at(vec![prelude.extra()?])?;
        prelude.configured(&mut child)?;
        let prepared = Self {
            command: Some(child),
            prelude,
            program,
            image,
            before,
            deadline,
            limit,
        };
        prepared.validate_image()?;
        Ok(prepared)
    }

    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) fn take_command(&mut self) -> io::Result<spawnfd::Command> {
        self.validate_image()?;
        self.command
            .take()
            .ok_or_else(|| refused("native preflight command already consumed"))
    }

    fn validate_image(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if stamp(&self.before) != stamp(&self.image.metadata()?)
            || stamp(&self.before) != stamp(&std::fs::metadata(&self.program)?)
        {
            return Err(refused("selected native preflight image changed"));
        }
        Ok(())
    }

    fn loaded(&self, pid: u32) -> io::Result<()> {
        self.validate_image()?;
        loaded(pid, &self.before)
    }

    fn poll(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        self.validate_image()?;
        let before = &self.before;
        self.prelude.poll(child, |pid| loaded(pid, before))
    }
}

fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mode(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

fn loaded(pid: u32, expected: &std::fs::Metadata) -> io::Result<()> {
    if !aterm_uds::process::LoadedImage::read(pid as i32)
        .is_some_and(|image| image.matches_metadata(expected))
        || !crate::metadata_prelude::no_fork(pid)?
    {
        return Err(refused(
            "native preflight final image or confinement changed",
        ));
    }
    Ok(())
}

impl Owner {
    pub(super) fn tick_preflight_roots(&mut self) {
        use aterm_capacity::cohort::launch::CohortSignal;
        for job in self.jobs.values_mut() {
            if job.launched || job.reaped || job.cleanup_at.is_some() || job.preflight.is_none() {
                continue;
            }
            let result = (|| -> io::Result<()> {
                if job.events.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let preflight = job
                    .preflight
                    .as_mut()
                    .ok_or_else(|| refused("native preflight missing"))?;
                if !preflight.poll(job.child.child_mut())? {
                    return Ok(());
                }
                let deadline = preflight.deadline();
                let (monotonic_ns, unix_ns) = start::clocks()?;
                let started = Instant::now();
                let remaining = start::remaining_workload_timeout(deadline, started)
                    .ok_or(io::ErrorKind::TimedOut)?;
                self.journal.update(
                    &job.binding,
                    "running",
                    aterm_json::json!({"monotonic_ns":monotonic_ns,"unix_ns":unix_ns}),
                )?;
                job::inspection(&self.fence, &self.debt, job)?;
                let pid = job.child.child_mut().id();
                job.preflight
                    .as_ref()
                    .ok_or_else(|| refused("native preflight missing"))?
                    .loaded(pid)?;
                if started
                    .checked_add(remaining)
                    .is_none_or(|end| Instant::now() >= end)
                {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                if job.events.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                job.child.child_mut().resume()?;
                job.started = started;
                job.timeout = Some(remaining);
                job.launched = true;
                let _ = job.events.emit(Event::Started {
                    pid,
                    birth: job.root_birth,
                    monotonic_ns,
                    unix_ns,
                });
                Ok(())
            })();
            if let Err(error) = result {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) {
                    self.journal.record_fault("preflight-prepare", &error);
                }
                job.events.abandon();
                job.cause = match error.kind() {
                    io::ErrorKind::TimedOut => CompletionCause::Timeout,
                    io::ErrorKind::Interrupted => CompletionCause::Cancelled,
                    _ => CompletionCause::HostFault,
                };
                job.cleanup_at = Some(Instant::now());
                if let Err(error) = job.child.request_group(CohortSignal::Kill) {
                    self.journal
                        .fault(&mut self.faulted, "preflight-kill", error);
                    let _ = job.child.child_mut().kill();
                }
            }
        }
    }
}

pub(super) struct Scope {
    pending: Option<Box<Pending>>,
    binding: Binding,
    events: job::LocalEvents,
    read: aterm_uds::CtlStream,
    stdout: Vec<u8>,
    eof: bool,
    limit: usize,
    deadline: Instant,
    terminal: Option<Event>,
    error: Option<io::Error>,
    reply: Option<std::sync::mpsc::SyncSender<io::Result<Vec<u8>>>>,
}

impl Scope {
    pub(super) fn new(owner: &Owner, root: &Pending, call: recipe::ProbeCall) -> io::Result<Self> {
        let recipe::ProbeCall { prepared, reply } = call;
        let limit = prepared.limit;
        let mut attempt = [0; 16];
        aterm_uds::rand::fill(&mut attempt)?;
        let binding = Binding {
            session: root.submission.request.binding.session.clone(),
            attempt,
        };
        if attempt == [0; 16] || !owner.accepts_with_queued(&binding, 1) {
            return Err(refused("native preflight original attempt unavailable"));
        }
        let (read, write) = aterm_uds::kept_out(aterm_uds::CtlStream::pair)?;
        read.set_nonblocking(true)?;
        let (mut events, local) = job::Events::local(binding.clone());
        let deadline = prepared.deadline();
        let peer = root.submission.peer;
        let program = prepared.program.clone();
        let request = Submission {
            binding: binding.clone(),
            peer_pid: peer.pid,
            peer_birth: peer.birth,
            argv: vec![program.as_os_str().as_bytes().to_vec()],
            cwd: root.submission.request.cwd.clone(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: root.submission.request.memory_bytes,
            label: "selected tool preflight".into(),
            timeout_ms: None,
        };
        let null = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
        })?;
        let stderr = aterm_uds::kept_out(|| null.try_clone())?;
        request.validate()?;
        owner.journal.create(&binding)?;
        let _ = events.emit(Event::Queued);
        let pending = Box::new(Pending {
            queued_at: Instant::now(),
            submission: PendingSubmission {
                request,
                peer,
                fds: PendingIo {
                    stdin: null.into(),
                    stdout: write.into(),
                    stderr: stderr.into(),
                },
            },
            events,
            recipe: Some(recipe::Recipe {
                kind: recipe::Kind::Preflight,
                program,
                arguments: Vec::new(),
                environment: Vec::new(),
                cwd: PathBuf::new(),
                width: 1,
                metadata_sdk: None,
                compiler: None,
                ay: None,
            }),
            probe: None,
            primary: None,
            sdk: None,
            waiter: None,
            claim: None,
            recovery: None,
            admitted: None,
            scope_reference: None,
            cancelled: false,
            resolver: None,
            bootstrap_graph: None,
            preflight: Some(prepared),
            probe_scope: None,
        });
        Ok(Self {
            pending: Some(pending),
            binding,
            events: local,
            read,
            stdout: Vec::new(),
            eof: false,
            limit,
            deadline,
            terminal: None,
            error: None,
            reply: Some(reply),
        })
    }

    pub(super) fn cancel(&self) {
        self.events.cancel();
    }

    fn capture(&mut self) {
        if self.eof || self.error.is_some() {
            return;
        }
        let mut bytes = [0; 8192];
        let n = bytes.len().min(self.limit + 1 - self.stdout.len());
        match (&self.read).read(&mut bytes[..n]) {
            Ok(0) => self.eof = true,
            Ok(n) if self.stdout.len() + n <= self.limit => {
                self.stdout.extend_from_slice(&bytes[..n])
            }
            Ok(_) => self.error = Some(refused("selected tool preflight exceeded stdout limit")),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn poll(&mut self, owner: &mut Owner) -> bool {
        if self.reply.is_none() {
            return true;
        }
        self.capture();
        if self.error.is_some() || Instant::now() >= self.deadline {
            self.cancel();
        }
        if let Some(pending) = self.pending.take() {
            self.pending = owner.tick_pending(*pending).map(Box::new);
        }
        while let Ok(event) = self.events.receive.try_recv() {
            if matches!(event, Event::Finished { .. } | Event::Refused { .. }) {
                self.terminal = Some(event);
            }
        }
        if self.pending.is_some()
            || owner
                .jobs
                .contains_key(&(self.binding.session.clone(), self.binding.attempt))
            || owner
                .failed
                .iter()
                .any(|failed| failed.binding == self.binding)
        {
            return false;
        }
        let exited = matches!(
            self.terminal,
            Some(Event::Finished {
                outcome: Outcome::Exit(0),
                cause: CompletionCause::Exited,
                ..
            })
        );
        if exited && self.error.is_none() && !self.eof && Instant::now() < self.deadline {
            return false;
        }
        let result = if let Some(error) = self.error.take() {
            Err(error)
        } else if exited && self.eof {
            Ok(std::mem::take(&mut self.stdout))
        } else if let Some(Event::Refused { reason }) = &self.terminal {
            Err(refused(reason))
        } else {
            Err(refused(
                "selected tool preflight has no successful settled result",
            ))
        };
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(result);
        }
        true
    }
}

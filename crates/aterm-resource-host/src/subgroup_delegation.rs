// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Custody of original subgroup scheduling descriptions. No memory or root admission authority.

use aterm_capacity::build_cpu::{self, ExternalDebt, Lease, Owner, Pool};
use aterm_ctl::resource_subgroup_client::Delegations;
use aterm_types::resource_subgroup::{CpuDelegation, FifoWitness, GnuTransport, Request};
use aterm_uds::spawnfd;
use jobserver_utils::ScopedHandoff;
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{
            ffi::OsStringExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::PathBuf,
};

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn duplicate(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<OwnedFd> {
    aterm_uds::kept_out(|| fd.try_clone_to_owned())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh,
    Configured,
    Bound,
    Failed,
}

/// Scheduling descriptions retained by the host until the member's scoped drain.
/// This owns no memory grant, root admission, process launcher or GO permission.
pub struct Prepared {
    lease: Option<Lease>,
    previous: Option<Owner>,
    reported: Option<Owner>,
    cpu: Option<ScopedHandoff>,
    gnu: Option<ScopedHandoff>,
    named: Option<NamedFifo>,
    _gnu_files: Vec<OwnedFd>,
    mapped: Option<Vec<(OwnedFd, i32)>>,
    flags: Option<std::ffi::OsString>,
    workload_original: bool,
    phase: Phase,
}
impl Prepared {
    /// Borrow the same retained GNU descriptions for a native retirement
    /// bookend. This duplicates custody, never reads or replenishes tokens.
    pub(crate) fn retirement_gnu(&self) -> io::Result<Vec<OwnedFd>> {
        if self.phase != Phase::Configured && self.phase != Phase::Bound {
            return Err(refused("GNU retirement requires configured originals"));
        }
        if let Some(named) = &self.named {
            named.validate()?;
            named
                .files
                .iter()
                .map(|file| duplicate(file.as_fd()))
                .collect()
        } else {
            self._gnu_files
                .iter()
                .map(|file| duplicate(file.as_fd()))
                .collect()
        }
    }
    /// Retain another host-owned lifetime guard without replacing existing
    /// CPU/GNU mappings. This is transport, not allocation or SDK validation.
    pub(crate) fn retain_guard(&mut self, guard: OwnedFd) -> io::Result<u32> {
        if self.phase != Phase::Fresh {
            return Err(refused("lifetime guard requires an unconfigured member"));
        }
        let mapped = self
            .mapped
            .as_mut()
            .ok_or_else(|| refused("missing member maps"))?;
        if mapped.len() >= spawnfd::MAX_GUARD_FDS {
            return Err(refused("too many member lifetime guards"));
        }
        let targets = mapped
            .iter()
            .map(|(_, target)| *target)
            .collect::<std::collections::BTreeSet<_>>();
        let mut excluded = Vec::new();
        let mut candidate = guard;
        loop {
            let target = candidate.as_raw_fd();
            // Keep the private bootstrap lanes and all existing target slots.
            if target > 4 && !targets.contains(&target) {
                mapped.push((candidate, target));
                return u32::try_from(target).map_err(|_| refused("invalid lifetime guard"));
            }
            let next = duplicate(candidate.as_fd())?;
            excluded.push(candidate);
            candidate = next;
        }
    }
    /// Actual adopted kernel unit's observational key, never standalone CPU authority.
    pub fn cpu_slot(&self) -> Option<u32> {
        self.lease.as_ref().map(Lease::slot)
    }
    /// Adopt original descriptions under canonical mixed-debt admission.
    /// Ordinary busy preserves every borrowed input right and creates no handoff.
    /// V3 requires permanent prior-boot retirement; legacy fences are not a v3 entitlement.
    /// # Errors
    /// Invalid layout, any legacy fence, absent/unowned CPU lease or invalid GNU transport.
    #[cfg(test)]
    pub fn try_new(
        _pool: &Pool,
        debt: &ExternalDebt,
        global_limit: u32,
        request: &Request,
        rights: &Delegations,
    ) -> io::Result<Option<Self>> {
        Self::try_new_mapped(_pool, debt, global_limit, request, rights, None)
    }
    /// Preserve the private controller's explicit named-FIFO child targets.
    /// # Errors
    /// Invalid or overlapping targets, or any ordinary descriptor admission failure.
    pub fn try_new_mapped(
        _pool: &Pool,
        debt: &ExternalDebt,
        global_limit: u32,
        request: &Request,
        rights: &Delegations,
        named_targets: Option<[u32; 2]>,
    ) -> io::Result<Option<Self>> {
        Self::try_new_inner(debt, global_limit, request, rights, named_targets, None)
    }
    /// Borrow an actual original guard retained by the SAME workload host.
    /// The offered description is equality input only; it never becomes the
    /// guard, and no native acquire/headroom exception is performed here.
    pub(crate) fn from_workload_original(
        debt: &ExternalDebt,
        request: &Request,
        rights: &Delegations,
        named_targets: Option<[u32; 2]>,
        original: Lease,
    ) -> io::Result<Self> {
        let offered = rights
            .cpu
            .as_ref()
            .ok_or_else(|| refused("missing offered workload CPU"))?;
        let held = File::from(duplicate(original.as_fd())?).metadata()?;
        let offered = File::from(duplicate(offered.as_fd())?).metadata()?;
        if !matches!(request.cpu, CpuDelegation::SharedBorrow { .. })
            || held.dev() != offered.dev()
            || held.ino() != offered.ino()
        {
            return Err(refused("workload CPU differs from actual original guard"));
        }
        Self::try_new_inner(debt, 0, request, rights, named_targets, Some(original))?
            .ok_or_else(|| refused("original workload guard unexpectedly unavailable"))
    }
    fn try_new_inner(
        debt: &ExternalDebt,
        global_limit: u32,
        request: &Request,
        rights: &Delegations,
        named_targets: Option<[u32; 2]>,
        original: Option<Lease>,
    ) -> io::Result<Option<Self>> {
        let workload_original = original.is_some();
        request.validate()?;
        if let Some(targets) = named_targets
            && (!matches!(request.gnu, GnuTransport::NamedFifo { descriptors: 2, .. })
                || targets[0] == targets[1]
                || targets
                    .iter()
                    .any(|target| *target < 3 || *target > i32::MAX as u32)
                || matches!(request.cpu, CpuDelegation::SharedBorrow { target_fd } if targets.contains(&target_fd)))
        {
            return Err(refused("named GNU child targets overlap or differ"));
        }
        if !request.fence_targets.is_empty() || !rights.fences.is_empty() {
            return Err(refused("v3 subgroup cannot inherit legacy fences"));
        }
        if rights.gnu.len() != request.gnu.fd_count()
            || rights.cpu.is_some() != matches!(request.cpu, CpuDelegation::SharedBorrow { .. })
        {
            return Err(refused("subgroup scheduling descriptor layout differs"));
        }
        let lease = if let Some(original) = original {
            Some(original)
        } else if matches!(request.cpu, CpuDelegation::SharedBorrow { .. }) {
            let source = File::from(duplicate(
                rights
                    .cpu
                    .as_ref()
                    .ok_or_else(|| refused("missing shared CPU description"))?
                    .as_fd(),
            )?);
            match debt.try_adopt_inherited(&source, move |_| global_limit)? {
                Some(lease) => Some(lease),
                None => return Ok(None),
            }
        } else {
            None
        };
        let gnu = rights
            .gnu
            .iter()
            .map(|fd| duplicate(fd.as_fd()))
            .collect::<io::Result<Vec<_>>>()?;
        let mut prepared = Self {
            lease: None,
            previous: None,
            reported: None,
            cpu: None,
            gnu: None,
            named: None,
            _gnu_files: Vec::new(),
            mapped: Some(Vec::new()),
            flags: None,
            workload_original,
            phase: Phase::Fresh,
        };
        if let CpuDelegation::SharedBorrow { target_fd } = request.cpu {
            let lease = lease.ok_or_else(|| refused("missing checked CPU adoption"))?;
            let handoff = if workload_original {
                None
            } else {
                Some(ScopedHandoff::cpu_bundle(
                    duplicate(lease.as_fd())?,
                    Vec::new(),
                )?)
            };
            prepared
                .mapped
                .as_mut()
                .unwrap()
                .push((duplicate(lease.as_fd())?, target_fd as i32));
            prepared.previous = Some(lease.owner());
            prepared.cpu = handoff;
            prepared.lease = Some(lease);
        }
        match &request.gnu {
            GnuTransport::None => {}
            GnuTransport::NumericPipe {
                read_target,
                write_target,
            } => {
                let [reader, writer]: [OwnedFd; 2] = gnu
                    .try_into()
                    .map_err(|_| refused("missing GNU pipe pair"))?;
                for (fd, read) in [(&reader, true), (&writer, false)] {
                    let file = File::from(duplicate(fd.as_fd())?);
                    if !file.metadata()?.file_type().is_fifo()
                        || access(&file)? != if read { libc::O_RDONLY } else { libc::O_WRONLY }
                    {
                        return Err(refused("GNU numeric pipe access differs"));
                    }
                }
                let targets = [*read_target as i32, *write_target as i32];
                prepared.gnu = Some(ScopedHandoff::jobserver(
                    [duplicate(reader.as_fd())?, duplicate(writer.as_fd())?],
                    targets,
                )?);
                prepared.mapped.as_mut().unwrap().extend([
                    (duplicate(reader.as_fd())?, targets[0]),
                    (duplicate(writer.as_fd())?, targets[1]),
                ]);
                prepared._gnu_files = vec![reader, writer];
                prepared.flags = Some(
                    format!(
                        "-j --jobserver-fds={},{} --jobserver-auth={},{}",
                        targets[0], targets[1], targets[0], targets[1]
                    )
                    .into(),
                );
            }
            GnuTransport::NamedFifo {
                locator, witness, ..
            } => {
                let named = NamedFifo::new(locator.clone(), witness.clone(), gnu)?;
                prepared.gnu = Some(ScopedHandoff::named_fifo(named.witnesses()?)?);
                let mapped = prepared.mapped.as_mut().unwrap();
                if let Some(targets) = named_targets {
                    for (file, target) in named.files.iter().zip(targets) {
                        mapped.push((duplicate(file.as_fd())?, target as i32));
                    }
                } else {
                    let mut next = 3;
                    for file in &named.files {
                        while mapped.iter().any(|(_, target)| *target == next) {
                            next += 1;
                        }
                        mapped.push((duplicate(file.as_fd())?, next));
                        next += 1;
                    }
                }
                prepared.flags = Some(named.flags());
                prepared.named = Some(named);
            }
        }
        Ok(Some(prepared))
    }
    /// Install one explicit mapped inheritance set and canonical scoped advertisements.
    /// Retain this Prepared through heldspawn, bind, GO and actual scoped drain.
    /// # Errors
    /// Repeated use, existing guards, changed FIFO identity or invalid native mapping.
    pub fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        self.configure_with_extra(command, Vec::new())
    }
    /// Descriptor TARGET observations for the held one-use mapping. No source
    /// descriptor, lease constructor or new resource escapes this owner.
    pub(crate) fn child_targets(&self) -> io::Result<Vec<u32>> {
        self.mapped
            .as_ref()
            .ok_or_else(|| refused("original child mapping already consumed"))?
            .iter()
            .map(|(_, target)| {
                u32::try_from(*target).map_err(|_| refused("invalid original child target"))
            })
            .collect()
    }
    /// One owner-private preparation channel. Extras confer no scheduling
    /// authority and must be removed before the selected image's exec.
    pub(crate) fn configure_with_extra(
        &mut self,
        command: &mut spawnfd::Command,
        extra: Vec<(OwnedFd, i32)>,
    ) -> io::Result<()> {
        self.configure_original(command, extra, false)
    }
    /// Owner-private original parked compiler continuation. It receives only
    /// CPU5 and cannot select compiler startup or acquire/delegate more units.
    pub(crate) fn configure_linker(
        &mut self,
        command: &mut spawnfd::Command,
        extra: Vec<(OwnedFd, i32)>,
    ) -> io::Result<()> {
        if !self.workload_original
            || self.gnu.is_some()
            || self.named.is_some()
            || self.flags.is_some()
        {
            return Err(refused(
                "linker does not retain an original CPU-only continuation",
            ));
        }
        self.configure_original(command, extra, true)
    }
    fn configure_original(
        &mut self,
        command: &mut spawnfd::Command,
        extra: Vec<(OwnedFd, i32)>,
        linker: bool,
    ) -> io::Result<()> {
        if self.phase != Phase::Fresh {
            return Err(refused("scheduling setup already consumed"));
        }
        self.phase = Phase::Failed;
        if command.guard_targets().len() != 0 {
            return Err(refused("scheduling setup would replace existing guards"));
        }
        if let Some(named) = &self.named {
            named.validate()?;
        }
        let mut mapped = self
            .mapped
            .take()
            .ok_or_else(|| refused("missing scheduling inheritance"))?;
        let cpu_target = if self.lease.is_some() {
            mapped.first().map(|(_, target)| *target)
        } else {
            None
        };
        mapped.extend(extra);
        if !mapped.is_empty() {
            command.inherit_guards_at(mapped)?;
        }
        for key in [
            jobserver_utils::CPU_HANDOFF_ENV,
            jobserver_utils::DESCENDANT_HANDOFF_ENV,
            build_cpu::IMPLICIT_FD_ENV,
            trust_resource_controller::workload::host::CPU_SET_FDS_ENV,
            trust_resource_controller::workload::host::HOST_STORE_FD_ENV,
            "TRUST_JOBSERVER_HANDOFF",
            "BUILD_SLOT_FENCE_FDS",
            "MAKEFLAGS",
            "MFLAGS",
            "CARGO_MAKEFLAGS",
        ] {
            command.env_remove(key);
        }
        if let Some(cpu) = &self.cpu {
            command.env(jobserver_utils::CPU_HANDOFF_ENV, cpu.advertisement());
            command.env(
                build_cpu::IMPLICIT_FD_ENV,
                cpu_target
                    .ok_or_else(|| refused("missing CPU child target"))?
                    .to_string(),
            );
        }
        if self.workload_original {
            let target =
                cpu_target.ok_or_else(|| refused("missing original workload member target"))?;
            if (linker && target != 5) || (!linker && target < 6) {
                return Err(refused("unsupported original workload member target"));
            }
            if !linker {
                command.env(
                    trust_resource_controller::workload::host::CPU_SET_FDS_ENV,
                    target.to_string(),
                );
            }
        }
        if let Some(gnu) = &self.gnu {
            command.env(jobserver_utils::DESCENDANT_HANDOFF_ENV, gnu.advertisement());
        }
        if let Some(flags) = &self.flags {
            for key in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
                command.env(key, flags);
            }
        }
        self.phase = Phase::Configured;
        Ok(())
    }
    /// Arm canonical handoffs for one actual direct held child before the host permits GO.
    /// Diagnostic owner uses the CPU core's native Mach/startticks identity, never BSD birth.
    /// # Errors
    /// Wrong phase, changed FIFO, invalid actual child or unavailable CPU identity/record update.
    pub fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        if self.phase != Phase::Configured {
            return Err(refused("scheduling handoff is not configurable"));
        }
        self.phase = Phase::Failed;
        if let Some(named) = &self.named {
            named.validate()?;
        }
        // Every mode binds through the CPU/GNU endpoint when one exists. Fixed/no-GNU
        // still requires a native same-user direct child, not a numerical assertion.
        let lineage = aterm_uds::process::read_lineage(
            i32::try_from(pid).map_err(|_| refused("invalid child PID"))?,
        )
        .ok_or_else(|| refused("child identity unavailable"))?;
        if lineage.parent != std::process::id() {
            return Err(refused("scheduling member is not an actual direct child"));
        }
        let owner = if self.lease.is_some() {
            Some(
                build_cpu::process_cpu::read(pid)
                    .ok_or_else(|| refused("child CPU identity unavailable"))?
                    .owner,
            )
        } else {
            None
        };
        if let Some(cpu) = &self.cpu {
            cpu.bind_child(pid)?;
        }
        if let Some(gnu) = &self.gnu {
            gnu.bind_child(pid)?;
        }
        if aterm_uds::process::read_lineage(pid as i32).is_none_or(|after| {
            after.identity != lineage.identity || after.parent != lineage.parent
        }) || owner.is_some_and(|owner| {
            build_cpu::process_cpu::read(pid).is_none_or(|after| after.owner != owner)
        }) {
            return Err(refused("scheduling child changed during binding"));
        }
        if let (Some(lease), Some(owner)) = (&self.lease, owner) {
            lease.report_owner(owner)?;
            self.reported = Some(owner);
        }
        self.phase = Phase::Bound;
        Ok(())
    }
    fn restore_owner(&mut self) {
        let (Some(lease), Some(previous), Some(reported)) =
            (&self.lease, self.previous, self.reported)
        else {
            return;
        };
        if previous.birth.is_none()
            || build_cpu::process_cpu::read(previous.pid).is_none_or(|now| now.owner != previous)
        {
            return;
        }
        // A newer diagnostic attribution must not be overwritten by this older scope.
        use std::os::unix::fs::FileExt as _;
        let Ok(file) = lease.try_clone().map(Lease::into_file) else {
            return;
        };
        let mut bytes = [0; build_cpu::MAX_RECORD_BYTES + 1];
        let Ok(count) = file.read_at(&mut bytes, 0) else {
            return;
        };
        let Some(record) = std::str::from_utf8(&bytes[..count])
            .ok()
            .and_then(|text| build_cpu::Record::parse(text).ok())
        else {
            return;
        };
        if record.slot == lease.slot()
            && record.owner == reported
            && build_cpu::process_cpu::read(previous.pid).is_some_and(|now| now.owner == previous)
        {
            let _ = lease.report_owner(previous);
        }
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        self.restore_owner();
    }
}
fn access(file: &File) -> io::Result<i32> {
    // SAFETY: F_GETFL observes this owned live description and changes no flags.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    if flags & libc::O_PATH != 0 {
        return Err(refused("GNU description is a path handle"));
    }
    Ok(flags & libc::O_ACCMODE)
}
fn fifo_metadata(meta: &Metadata, witness: &FifoWitness) -> io::Result<()> {
    if !meta.file_type().is_fifo()
        || meta.dev() != witness.device
        || meta.ino() != witness.inode
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.nlink() != 1
        || meta.mode() & 0o7077 != 0
    {
        return Err(refused(
            "GNU FIFO identity, ownership or private mode differs",
        ));
    }
    Ok(())
}

// Retain originals even when a matching read-only description is needed for a
// write-only inherited FIFO. No operation here reads, creates or replenishes tokens.
struct NamedFifo {
    locator: PathBuf,
    witness: FifoWitness,
    files: Vec<File>,
}
impl NamedFifo {
    fn new(locator: Vec<u8>, witness: FifoWitness, files: Vec<OwnedFd>) -> io::Result<Self> {
        if locator.first() != Some(&b'/')
            || locator.contains(&0)
            || ![1, 2].contains(&files.len())
            || locator
                .iter()
                .any(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'\\' | b'\'' | b'"'))
        {
            return Err(refused("invalid named GNU FIFO layout"));
        }
        let mut named = Self {
            locator: PathBuf::from(std::ffi::OsString::from_vec(locator)),
            witness,
            files: files.into_iter().map(File::from).collect(),
        };
        named.validate()?;
        let modes = named
            .files
            .iter()
            .map(access)
            .collect::<io::Result<Vec<_>>>()?;
        if modes.len() == 1 {
            match modes[0] {
                libc::O_RDWR => {}
                libc::O_WRONLY => {
                    // The held original, never the locator alone, fixes the FIFO.
                    let reader = aterm_uds::kept_out(|| {
                        OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                            .open(&named.locator)
                    })?;
                    fifo_metadata(&reader.metadata()?, &named.witness)?;
                    if access(&reader)? != libc::O_RDONLY {
                        return Err(refused("GNU FIFO reader has wrong access"));
                    }
                    named.files.push(reader);
                }
                _ => return Err(refused("single GNU FIFO descriptor cannot return tokens")),
            }
        } else if ![libc::O_RDONLY, libc::O_RDWR].contains(&modes[0])
            || ![libc::O_WRONLY, libc::O_RDWR].contains(&modes[1])
        {
            return Err(refused(
                "GNU FIFO descriptors do not preserve read/write access",
            ));
        }
        named.validate()?;
        Ok(named)
    }
    fn validate(&self) -> io::Result<()> {
        fifo_metadata(&fs::symlink_metadata(&self.locator)?, &self.witness)?;
        for file in &self.files {
            fifo_metadata(&file.metadata()?, &self.witness)?;
        }
        Ok(())
    }
    fn witnesses(&self) -> io::Result<[OwnedFd; 2]> {
        let mut read = None;
        let mut write = None;
        for file in &self.files {
            let mode = access(file)?;
            if mode != libc::O_WRONLY && read.is_none() {
                read = Some(duplicate(file.as_fd())?);
            }
            if mode != libc::O_RDONLY && write.is_none() {
                write = Some(duplicate(file.as_fd())?);
            }
        }
        Ok([
            read.ok_or_else(|| refused("named GNU FIFO has no readable witness"))?,
            write.ok_or_else(|| refused("named GNU FIFO has no writable witness"))?,
        ])
    }
    fn flags(&self) -> std::ffi::OsString {
        use std::os::unix::ffi::OsStrExt as _;
        let mut bytes = b"-j --jobserver-auth=fifo:".to_vec();
        bytes.extend(self.locator.as_os_str().as_bytes());
        std::ffi::OsString::from_vec(bytes)
    }
}

#[cfg(test)]
#[path = "subgroup_delegation/tests.rs"]
mod tests;

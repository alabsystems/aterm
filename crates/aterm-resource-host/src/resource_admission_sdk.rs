// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original manager GC custody, not SDK writer/proof authentication. Files and
//! their selected names remain held and rechecked across the actual lease.
use super::*;
use std::{
    io::Read as _,
    os::{
        fd::OwnedFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    sync::Arc,
};

pub(super) const TARGET: i32 = 5;
pub(super) struct Keeper {
    lease: atpkg::lease::Lease,
    program: File,
    ready: File,
    program_path: PathBuf,
    ready_path: PathBuf,
    program_before: std::fs::Metadata,
    ready_before: std::fs::Metadata,
    execution: PathBuf,
    build: u64,
    held: trust_resource_provenance::program::HeldProgram,
}
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, u32, u32, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mode(),
        meta.uid(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn open(path: &std::path::Path) -> io::Result<(File, std::fs::Metadata)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() == 0
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o022 != 0
    {
        return Err(refused("managed SDK input identity is unsafe"));
    }
    Ok((file, meta))
}
fn current(path: &std::path::Path, file: &File, before: &std::fs::Metadata) -> io::Result<()> {
    let named = std::fs::symlink_metadata(path)?;
    if !named.is_file()
        || stamp(&named) != stamp(before)
        || stamp(&file.metadata()?) != stamp(before)
    {
        return Err(refused(
            "selected managed SDK input changed across its original lease",
        ));
    }
    Ok(())
}
impl Keeper {
    pub(super) fn resolver_compiler(&self) -> PathBuf {
        self.execution.join("bin/trustc")
    }

    pub(super) fn require_resolver_protocol(&self, deadline: Instant) -> io::Result<()> {
        self.validate()?;
        self.held.require_resolver_protocol_marker(
            &trust_resource_controller::resolver::RESOLVER_PROTOCOL_MARKER,
            deadline,
        )?;
        self.validate()
    }

    pub(super) fn capture(
        layout: &atpkg::store::Layout,
        program_path: &std::path::Path,
    ) -> io::Result<Arc<Self>> {
        let directory = program_path
            .parent()
            .ok_or_else(|| refused("SDK program parent missing"))?;
        let subject = atpkg::lease::Subject::of_dir(&layout.prefix, directory)
            .ok_or_else(|| refused("metadata service has no managed SDK subject"))?;
        let atpkg::lease::Subject::Build { build, .. } = subject else {
            return Err(refused("metadata service is not a managed SDK build"));
        };
        let execution = directory
            .parent()
            .ok_or_else(|| refused("SDK execution root missing"))?
            .to_path_buf();
        let ready_path = atpkg::store::ready_marker_path(&subject.path(layout))
            .ok_or_else(|| refused("SDK ready record unavailable"))?;
        let (program, program_before) = open(program_path)?;
        let (ready, ready_before) = open(&ready_path)?;
        let lease = match atpkg::lease::hold_for(
            &layout.prefix,
            directory,
            "native metadata service and retained members",
            Duration::from_secs(2),
        ) {
            Ok(atpkg::lease::Hold::Held(lease)) => lease,
            _ => return Err(refused("original SDK GC lease unavailable")),
        };
        current(program_path, &program, &program_before)?;
        current(&ready_path, &ready, &ready_before)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        if program_before.len() > 1024 * 1024 * 1024 {
            return Err(refused(
                "metadata service image exceeds native profile bound",
            ));
        }
        let mut bytes = program.try_clone()?;
        let mut hash = aterm_digest::Sha256::new();
        let mut observed = 0u64;
        let mut buffer = [0; 65536];
        loop {
            if Instant::now() >= deadline {
                return Err(refused("metadata service capture deadline expired"));
            }
            let read = bytes.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            observed += read as u64;
            if observed > program_before.len() {
                return Err(refused("metadata service image grew while reading"));
            }
            hash.update(&buffer[..read]);
        }
        if observed != program_before.len() {
            return Err(refused("metadata service image changed length"));
        }
        current(program_path, &program, &program_before)?;
        let held = trust_resource_provenance::program::HeldProgram::open(
            &execution,
            program_path,
            &hash.finalize(),
            deadline,
        )?;
        let result = Self {
            lease,
            program,
            ready,
            program_path: program_path.to_path_buf(),
            ready_path,
            program_before,
            ready_before,
            execution,
            build,
            held,
        };
        result.validate()?;
        if !atpkg::store::build_is_complete(&subject.path(layout)) {
            return Err(refused(
                "selected SDK ceased to be a whole manager artifact",
            ));
        }
        result.validate()?;
        Ok(Arc::new(result))
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.lease.check_current()?;
        current(&self.program_path, &self.program, &self.program_before)?;
        current(&self.ready_path, &self.ready, &self.ready_before)?;
        self.held.validate(Instant::now() + Duration::from_secs(2))
    }
    pub(super) fn runtime_environment(
        &self,
    ) -> io::Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
        self.held.runtime_environment()
    }
    pub(super) fn validate_loaded(&self, pid: u32) -> io::Result<()> {
        self.validate()?;
        self.held.validate_loaded(
            trust_resource_provenance::process::read_identity(pid)?,
            Instant::now() + Duration::from_secs(2),
        )
    }
    pub(super) fn descriptor(&self) -> io::Result<OwnedFd> {
        self.validate()?;
        self.lease.try_clone_descriptor()
    }
    pub(super) fn context(&self) -> io::Result<trust_resource_controller::SdkCustody> {
        self.validate()?;
        let descriptor = File::from(self.lease.try_clone_descriptor()?);
        let meta = descriptor.metadata()?;
        Ok(trust_resource_controller::SdkCustody {
            execution: self.execution.as_os_str().as_bytes().to_vec(),
            build: self.build,
            lease_path: self.lease.path().as_os_str().as_bytes().to_vec(),
            lease: trust_resource_controller::FileIdentity {
                device: meta.dev(),
                inode: meta.ino(),
            },
            inherited_fd: TARGET as u32,
        })
    }
}

use std::os::fd::AsRawFd as _;
use trust_resource_controller::SdkCustody;

pub(super) fn root(
    selection: Option<&compiler::Selection>,
    command: &mut spawnfd::Command,
) -> io::Result<Option<SdkCustody>> {
    let Some(selected) = selection else {
        return Ok(None);
    };
    let fd = aterm_uds::kept_out(|| selected.lease_descriptor())?;
    let metadata = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
    command.inherit_guards(vec![fd])?;
    let target = command
        .guard_targets()
        .next()
        .ok_or_else(|| refused("SDK guard target missing"))?;
    let value = description(selected, &metadata, target)?;
    command.env(
        trust_resource_controller::client::sdk::SDK_LEASE_ENV,
        target.to_string(),
    );
    Ok(Some(value))
}

pub(super) fn member(
    selection: Option<&compiler::Selection>,
    prepared: &mut crate::subgroup_delegation::Prepared,
    command: &mut spawnfd::Command,
) -> io::Result<Option<SdkCustody>> {
    let Some(selected) = selection else {
        return Ok(None);
    };
    let fd = aterm_uds::kept_out(|| selected.lease_descriptor())?;
    let metadata = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
    let target = prepared.retain_guard(fd)?;
    let target = i32::try_from(target).map_err(|_| refused("SDK guard target out of range"))?;
    let value = description(selected, &metadata, target)?;
    command.env(
        trust_resource_controller::client::sdk::SDK_LEASE_ENV,
        target.to_string(),
    );
    Ok(Some(value))
}
fn description(
    selected: &compiler::Selection,
    metadata: &std::fs::Metadata,
    target: i32,
) -> io::Result<SdkCustody> {
    selected.check_lease()?;
    Ok(SdkCustody {
        execution: selected.execution().as_os_str().as_bytes().to_vec(),
        build: selected.build(),
        lease_path: selected.lease_path().as_os_str().as_bytes().to_vec(),
        lease: trust_resource_controller::FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        inherited_fd: u32::try_from(target).map_err(|_| refused("invalid SDK guard target"))?,
    })
}
pub(super) fn json(sdk: &Option<SdkCustody>) -> aterm_json::Value {
    sdk.as_ref().map_or(aterm_json::Value::Null, |sdk| {
        aterm_json::json!({
        "build":sdk.build,"execution_hex":aterm_codec::hex::encode_lower(&sdk.execution),
        "lease_path_hex":aterm_codec::hex::encode_lower(&sdk.lease_path),
        "device":sdk.lease.device,"inode":sdk.lease.inode,"target":sdk.inherited_fd })
    })
}

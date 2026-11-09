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
    held: Arc<trust_resource_provenance::program::HeldProgram>,
    bootstrap_query_compiler:
        std::sync::OnceLock<Arc<trust_resource_provenance::program::HeldProgram>>,
    bootstrap_object_driver:
        std::sync::OnceLock<Option<Arc<trust_resource_provenance::program::HeldProgram>>>,
    workload_compiler: std::sync::OnceLock<Arc<trust_resource_provenance::program::HeldProgram>>,
    workload_linker: std::sync::OnceLock<Arc<trust_resource_provenance::program::HeldProgram>>,
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

fn retain_sdk_lease(
    layout: &atpkg::store::Layout,
    directory: &std::path::Path,
    started: Instant,
) -> io::Result<atpkg::lease::Lease> {
    let remaining = native_inputs::deadline(started)
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
    match atpkg::lease::hold_for(
        &layout.prefix,
        directory,
        "native metadata service and retained members",
        remaining,
    ) {
        Ok(atpkg::lease::Hold::Held(lease)) => Ok(lease),
        _ => Err(refused("original SDK GC lease unavailable")),
    }
}
impl Keeper {
    pub(super) fn execution(&self) -> &std::path::Path {
        &self.execution
    }
    pub(super) fn build(&self) -> u64 {
        self.build
    }
    pub(super) fn lease_path(&self) -> &std::path::Path {
        self.lease.path()
    }
    pub(super) fn original_inputs(&self) -> Arc<trust_resource_provenance::program::HeldProgram> {
        Arc::clone(&self.held)
    }
    pub(super) fn program_sha256(&self) -> io::Result<[u8; 32]> {
        self.validate()?;
        let mut digest = [0; 32];
        if !aterm_codec::hex::decode_into(
            self.held.observation().executable.sha256.as_bytes(),
            &mut digest,
        ) {
            return Err(refused("original managed SDK digest is malformed"));
        }
        Ok(digest)
    }
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
    pub(super) fn require_workload_protocol(&self, deadline: Instant) -> io::Result<()> {
        self.validate()?;
        self.held.require_workload_protocol_marker(
            &trust_resource_controller::workload::WORKLOAD_PROTOCOL_MARKER,
            deadline,
        )?;
        self.validate()
    }
    /// Capture the compiler from this SAME selected manager artifact before
    /// the Bootstrap Root can execute. No request-selected directory or marker
    /// creates SDK custody, and this does not enable a producing profile.
    pub(super) fn capture_bootstrap_query_inputs(&self, deadline: Instant) -> io::Result<()> {
        if self.bootstrap_query_compiler.get().is_some() {
            return Err(refused("Bootstrap query inputs already captured"));
        }
        let held = self.capture_workload_program("trustc", deadline)?;
        self.bootstrap_query_compiler
            .set(held)
            .map_err(|_| refused("Bootstrap query compiler capture raced"))?;
        self.validate()?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
    /// Borrow only an original selected input. In particular a missing early
    /// compiler capture never falls back to opening a request pathname.
    pub(super) fn bootstrap_query_program(
        &self,
        tool: trust_resource_controller::bootstrap_graph::query::Tool,
        deadline: Instant,
    ) -> io::Result<Arc<trust_resource_provenance::program::HeldProgram>> {
        self.validate()?;
        let held = match tool {
            trust_resource_controller::bootstrap_graph::query::Tool::Targo => &self.held,
            trust_resource_controller::bootstrap_graph::query::Tool::Compiler => self
                .bootstrap_query_compiler
                .get()
                .ok_or_else(|| refused("Bootstrap query compiler not retained before Root GO"))?,
        };
        held.validate(deadline)?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(Arc::clone(held))
    }

    /// Capture this distinct object's input before the SAME Root can execute.
    /// A genuinely absent optional tool leaves readonly query75 available, but
    /// can never be opened later in response to an object request.
    pub(super) fn capture_bootstrap_object_input(&self, deadline: Instant) -> io::Result<()> {
        if self.bootstrap_object_driver.get().is_some() {
            return Err(refused("Bootstrap object driver already captured"));
        }
        self.validate()?;
        let selected = self.execution.join("bin/tippy-driver");
        let held = match std::fs::symlink_metadata(&selected) {
            Ok(_) => Some(self.capture_workload_program("tippy-driver", deadline)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        self.bootstrap_object_driver
            .set(held)
            .map_err(|_| refused("Bootstrap object driver capture raced"))?;
        self.validate()?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }

    /// Only the actual early-selected driver can issue the distinct object
    /// profile. A pathname, request digest or current file cannot fill absence.
    pub(super) fn bootstrap_object_program(
        &self,
        deadline: Instant,
    ) -> io::Result<Arc<trust_resource_provenance::program::HeldProgram>> {
        self.validate()?;
        let held = self
            .bootstrap_object_driver
            .get()
            .ok_or_else(|| refused("Bootstrap object driver not captured before Root GO"))?
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "original SDK has no captured object driver",
                )
            })?;
        held.validate(deadline)?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(Arc::clone(held))
    }
    fn capture_workload_program(
        &self,
        name: &str,
        deadline: Instant,
    ) -> io::Result<Arc<trust_resource_provenance::program::HeldProgram>> {
        self.validate()?;
        let program = self.execution.join("bin").join(name);
        let (file, before) = open(&program)?;
        if before.len() > 1024 * 1024 * 1024 {
            return Err(refused(
                "workload compiler image exceeds native input bound",
            ));
        }
        use std::os::unix::fs::FileExt as _;
        let mut hash = aterm_digest::Sha256::new();
        let mut bytes = [0u8; 65536];
        let mut offset = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let count = file.read_at(&mut bytes, offset)?;
            if count == 0 {
                break;
            }
            offset += count as u64;
            if offset > before.len() {
                return Err(refused("workload compiler grew during capture"));
            }
            hash.update(&bytes[..count]);
        }
        if offset != before.len() {
            return Err(refused("workload compiler changed during capture"));
        }
        current(&program, &file, &before)?;
        let held = trust_resource_provenance::program::HeldProgram::open(
            &self.execution,
            &program,
            &hash.finalize(),
            deadline,
        )?;
        current(&program, &file, &before)?;
        self.validate()?;
        Ok(Arc::new(held))
    }
    pub(super) fn capture_workload_compiler(&self, deadline: Instant) -> io::Result<()> {
        if self.workload_compiler.get().is_some() || self.workload_linker.get().is_some() {
            return Err(refused("workload compiler/linker inputs already captured"));
        }
        let held = self.capture_workload_program("trustc", deadline)?;
        held.require_workload_member_protocol_marker(
            &trust_resource_controller::workload::WORKLOAD_MEMBER_PROTOCOL_MARKER,
            deadline,
        )?;
        let linker = self.capture_workload_program("rust-lld", deadline)?;
        self.workload_compiler
            .set(held)
            .map_err(|_| refused("workload compiler capture raced"))?;
        self.workload_linker
            .set(linker)
            .map_err(|_| refused("workload linker capture raced"))?;
        self.validate()
    }
    pub(super) fn workload_linker(
        &self,
        deadline: Instant,
    ) -> io::Result<Arc<trust_resource_provenance::program::HeldProgram>> {
        self.validate()?;
        let held = self
            .workload_linker
            .get()
            .ok_or_else(|| refused("original workload linker not captured before GO"))?;
        held.validate(deadline)?;
        Ok(Arc::clone(held))
    }
    pub(super) fn workload_compiler(
        &self,
        deadline: Instant,
    ) -> io::Result<Arc<trust_resource_provenance::program::HeldProgram>> {
        self.validate()?;
        let held = self
            .workload_compiler
            .get()
            .ok_or_else(|| refused("original workload compiler not captured before GO"))?;
        held.validate(deadline)?;
        Ok(Arc::clone(held))
    }

    pub(super) fn capture(
        layout: &atpkg::store::Layout,
        program_path: &std::path::Path,
    ) -> io::Result<Arc<Self>> {
        Self::capture_at(layout, program_path, Instant::now())
    }
    fn capture_at(
        layout: &atpkg::store::Layout,
        program_path: &std::path::Path,
        started: Instant,
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
        let lease = retain_sdk_lease(layout, directory, Instant::now())?;
        current(program_path, &program, &program_before)?;
        current(&ready_path, &ready, &ready_before)?;
        let deadline = native_inputs::deadline(started);
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
            held: Arc::new(held),
            bootstrap_query_compiler: std::sync::OnceLock::new(),
            bootstrap_object_driver: std::sync::OnceLock::new(),
            workload_compiler: std::sync::OnceLock::new(),
            workload_linker: std::sync::OnceLock::new(),
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
        self.validate_at(Instant::now())
    }
    fn validate_at(&self, started: Instant) -> io::Result<()> {
        self.lease.check_current()?;
        current(&self.program_path, &self.program, &self.program_before)?;
        current(&self.ready_path, &self.ready, &self.ready_before)?;
        native_inputs::validate(&self.held, started)?;
        if let Some(compiler) = self.bootstrap_query_compiler.get() {
            native_inputs::validate(compiler, started)?;
        }
        if let Some(Some(driver)) = self.bootstrap_object_driver.get() {
            native_inputs::validate(driver, started)?;
        }
        if let Some(compiler) = self.workload_compiler.get() {
            native_inputs::validate(compiler, started)?;
        }
        if let Some(linker) = self.workload_linker.get() {
            native_inputs::validate(linker, started)?;
        }
        Ok(())
    }
    pub(super) fn runtime_environment(
        &self,
    ) -> io::Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
        self.held.runtime_environment()
    }
    pub(super) fn validate_loaded(&self, pid: u32) -> io::Result<()> {
        self.validate_loaded_at(pid, Instant::now())
    }
    pub(super) fn validate_loaded_at(&self, pid: u32, started: Instant) -> io::Result<()> {
        self.validate_at(started)?;
        native_inputs::validate_loaded(
            &self.held,
            trust_resource_provenance::process::read_identity(pid)?,
            started,
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

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
#[path = "resource_admission_sdk/tests.rs"]
mod tests;

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

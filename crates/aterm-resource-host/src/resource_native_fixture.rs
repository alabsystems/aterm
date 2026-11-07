// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only maintained entry for a real cached host/helper integration. No
//! production role or environment variable can select this private-store seam.
use aterm_capacity::build_cpu::{self, ExternalDebt, Pool};
use aterm_ctl::resource_subgroup_client::Delegations;
use aterm_types::{
    resource_admission::{Binding, Submission},
    resource_subgroup::{Capability, CpuDelegation, GnuTransport, Request},
};
use aterm_uds::spawnfd;
use jobserver_utils::ScopedHandoff;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

const SCOPED_CPU_MEMBER: &str = "__scoped-cpu-held-member";
const SCOPED_CPU_PROOF: &str = "actual-scoped-cpu-recovered";
const RECOVERY_REFUSED: u8 = 78;
const MEMBER_FAILED: u8 = 79;
const HANG: Duration = Duration::from_secs(60);

/// Dispatch the standalone native fixture before any libtest argument parser.
/// This exists only on the currently implemented host-member platform in tests.
pub fn entry(args: &[OsString]) -> ExitCode {
    // Exercise the actual production original-store helper path. The legacy
    // private fixture below intentionally remains separate from this route.
    if args
        .first()
        .is_some_and(|arg| arg == trust_resource_controller::WorkloadHelperFds::ENTRY)
    {
        return crate::resource_proxy::entry(
            args,
            &mut crate::resource_proxy_backend::NativeBackend::account(),
        )
        .unwrap();
    }
    if args.first().is_some_and(|v| v == SCOPED_CPU_MEMBER) {
        return scoped_cpu_member(&args[1..]);
    }
    let pending = trust_resource_controller::client::sdk::snapshot_sdk_lease().unwrap();
    let gnu = if matches!(args, [arg] if arg == "-vV")
        || matches!(args, [worker, subgroup, name, value]
            if worker == "--resource-worker-v3" && subgroup == "--resource-subgroups-v3"
                && name == "--crate-name" && value == "original_store_helper_fixture")
    {
        Some(
            jobserver_utils::recover_named_fifo_resources(
                &std::env::var_os("CARGO_JOBSERVER_HANDOFF").unwrap(),
            )
            .unwrap(),
        )
    } else {
        None
    };
    // SAFETY: this is the single-threaded native fixture entry. Only its fixed
    // named GNU import runs after the original descriptor-only snapshot.
    unsafe { trust_resource_controller::client::sdk::capture_sdk_lease(pending) }.unwrap();

    if args.first().is_some_and(|v| {
        v == "__resource-member-v3" || v == trust_resource_controller::accounting::HELPER_ENTRY
    }) {
        let image = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .unwrap();
        let path = image
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .unwrap();
        let store = trust_resource_provenance::Store::open_at(path).unwrap();
        return crate::resource_proxy::entry(
            args,
            &mut crate::resource_proxy_backend::NativeBackend::for_store(store),
        )
        .unwrap();
    }
    crate::actor::member_fixture(args, gnu)
}

fn scoped_cpu_member(args: &[OsString]) -> ExitCode {
    let [pool_path, legacy_path] = args else {
        eprintln!("scoped CPU member requires its pool and retired legacy namespace");
        return ExitCode::from(MEMBER_FAILED);
    };
    let Some(advertisement) = std::env::var_os(jobserver_utils::CPU_HANDOFF_ENV) else {
        eprintln!("scoped CPU member has no handoff advertisement");
        return ExitCode::from(MEMBER_FAILED);
    };
    let (cpu, fences) = match jobserver_utils::recover_cpu_resources(&advertisement) {
        Ok(resources) => resources,
        Err(error) => {
            eprintln!("scoped CPU recovery refused: {error}");
            return ExitCode::from(RECOVERY_REFUSED);
        }
    };
    let result = (|| -> io::Result<()> {
        if !fences.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "scoped CPU member received unexpected fences",
            ));
        }
        let pool = Pool::open_existing(Path::new(pool_path))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "CPU pool is absent"))?;
        let legacy = build_cpu::LegacyRetirement::open(Path::new(legacy_path))?;
        let debt = ExternalDebt::open(&pool, &legacy)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "external CPU debt is inactive")
        })?;
        let _lease = debt
            .try_adopt_inherited(&fs::File::from(cpu), |_| 1)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "CPU adoption is busy"))?;
        let mut output = io::stdout().lock();
        writeln!(output, "{SCOPED_CPU_PROOF}")?;
        output.flush()
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("scoped CPU member failed: {error}");
            ExitCode::from(MEMBER_FAILED)
        }
    }
}

struct Retired(PathBuf);

impl Drop for Retired {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn pool(root: &Path) -> (Pool, ExternalDebt, Retired) {
    spawnfd::raise_soft_nofile();
    let legacy = root.join("legacy");
    fs::DirBuilder::new().mode(0o700).create(&legacy).unwrap();
    let cleanup = Retired(legacy.clone());
    let retirement = build_cpu::LegacyRetirement::retire(&legacy, 1).unwrap();
    let pool = Pool::open_in(root, std::num::NonZeroU32::new(1).unwrap()).unwrap();
    let debt = ExternalDebt::activate(&pool, &retirement).unwrap();
    (pool, debt, cleanup)
}

fn request() -> Request {
    let job = Binding {
        session: "s-ab12".into(),
        attempt: [1; 16],
    };
    Request {
        job: job.clone(),
        capability: Capability::from_bytes([7; 32]),
        member: Submission {
            binding: Binding {
                attempt: [2; 16],
                ..job
            },
            peer_pid: std::process::id(),
            peer_birth: aterm_uds::process::read_birth(std::process::id() as i32)
                .unwrap()
                .0,
            argv: vec![b"/usr/bin/true".to_vec()],
            cwd: b"/".to_vec(),
            env: vec![],
            cpu_units: 1,
            memory_bytes: 100,
            label: "scoped CPU fixture".into(),
            timeout_ms: None,
        },
        cpu: CpuDelegation::SharedBorrow { target_fd: 900 },
        gnu: GnuTransport::None,
        fence_targets: vec![],
    }
}

fn member_command(program: &Path, root: &Path, legacy: &Path) -> spawnfd::Command {
    let mut command = spawnfd::command(program);
    command.arg(SCOPED_CPU_MEMBER).arg(root).arg(legacy);
    command
}

struct ChildGuard(spawnfd::Child);

impl std::ops::Deref for ChildGuard {
    type Target = spawnfd::Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            aterm_uds::deadline::kill_within(&mut self.0, HANG);
        }
    }
}

fn wait_success(child: &mut ChildGuard) {
    let by = Instant::now() + HANG;
    let mut exit = aterm_uds::deadline::ExitWait::new(child);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "scoped CPU member failed: {status}");
            return;
        }
        assert!(Instant::now() < by, "scoped CPU member exit hang detector");
        exit.wait(by.saturating_duration_since(Instant::now()));
    }
}

/// Exercise the scoped CPU handoff through this harness-free executable. The
/// ordinary fixture calls this before it copies itself for the cached-host case.
pub(crate) fn scoped_cpu_control() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, retired) = pool(root.path());
    let previous = build_cpu::process_cpu::read(std::process::id())
        .unwrap()
        .owner;
    let original = debt.try_acquire_native(previous, 1).unwrap().unwrap();
    let program = std::env::current_exe().unwrap();

    let inert =
        ScopedHandoff::cpu_bundle(original.try_clone().unwrap().into_file().into(), Vec::new())
            .unwrap();
    let mut unbound = member_command(&program, root.path(), &retired.0);
    unbound.env(jobserver_utils::CPU_HANDOFF_ENV, inert.advertisement());
    let output = aterm_uds::deadline::output_within(&mut unbound, HANG).unwrap();
    assert_eq!(
        output.status.code(),
        Some(i32::from(RECOVERY_REFUSED)),
        "unbound scoped CPU member did not reach the inert endpoint: {output:?}"
    );
    assert!(
        !output
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|line| line == SCOPED_CPU_PROOF.as_bytes()),
        "unbound scoped CPU member printed the success proof: {output:?}"
    );
    drop(inert);

    let supplied = Delegations {
        cpu: Some(original.try_clone().unwrap().into_file().into()),
        gnu: Vec::new(),
        fences: Vec::new(),
    };
    let mut prepared =
        crate::subgroup_delegation::Prepared::try_new(&pool, &debt, 1, &request(), &supplied)
            .unwrap()
            .unwrap();
    drop(supplied);
    let mut command = member_command(&program, root.path(), &retired.0);
    command
        .start_suspended(true)
        .stdout(spawnfd::Stdio::piped())
        .stderr(spawnfd::Stdio::inherit());
    prepared.configure(&mut command).unwrap();
    let mut child = ChildGuard(command.spawn().unwrap());
    let native = build_cpu::process_cpu::read(child.id()).unwrap().owner;
    prepared.bind_child(child.id()).unwrap();
    assert!(
        matches!(pool.snapshot().unwrap()[0],build_cpu::SlotState::Held {reported_owner:Some(current),..} if current==native)
    );
    assert!(prepared.bind_child(child.id()).is_err());
    child.resume().unwrap();
    aterm_uds::deadline::prints(&mut child, SCOPED_CPU_PROOF, HANG);
    wait_success(&mut child);
    drop(command);
    drop(prepared);
    assert!(
        matches!(pool.snapshot().unwrap()[0],build_cpu::SlotState::Held {reported_owner:Some(current),..} if current==previous)
    );
    drop(original);
    println!("actual scoped CPU binding passed");
}

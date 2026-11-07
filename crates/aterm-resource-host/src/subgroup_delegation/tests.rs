// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

fn fifo(root: &std::path::Path, name: &str) -> (PathBuf, File, FifoWitness) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = root.join(name);
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: cpath is live and NUL terminated; the explicit temporary path is new.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let file = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
    })
    .unwrap();
    let meta = file.metadata().unwrap();
    let witness = FifoWitness {
        device: meta.dev(),
        inode: meta.ino(),
    };
    (path, file, witness)
}
#[test]
fn named_fifo_keeps_original_kind_and_mints_no_tokens() {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let (path, file, witness) = fifo(root.path(), "fifo-\u{00e9}");
    let original = file.metadata().unwrap();
    let named = NamedFifo::new(
        path.as_os_str().as_bytes().to_vec(),
        witness,
        vec![file.into()],
    )
    .unwrap();
    assert_eq!(named.files.len(), 1);
    assert!(ScopedHandoff::named_fifo(named.witnesses().unwrap()).is_ok());
    assert_eq!(named.files[0].metadata().unwrap().ino(), original.ino());
    assert!(
        named
            .flags()
            .as_os_str()
            .as_bytes()
            .starts_with(b"-j --jobserver-auth=fifo:")
    );
    let mut byte = [0];
    assert_eq!(
        (&named.files[0]).read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fs::remove_file(&path).unwrap();
    let (_other, other, _) = fifo(root.path(), "fifo-\u{00e9}");
    assert!(named.validate().is_err());
    drop(other);
}
#[test]
fn write_only_named_fifo_adds_only_a_matching_reader() {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let (path, held, witness) = fifo(root.path(), "named");
    let writer = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
    })
    .unwrap();
    drop(held);
    let named = NamedFifo::new(
        path.as_os_str().as_bytes().to_vec(),
        witness,
        vec![writer.into()],
    )
    .unwrap();
    assert_eq!(named.files.len(), 2);
    assert!(ScopedHandoff::named_fifo(named.witnesses().unwrap()).is_ok());
    assert_eq!(access(&named.files[0]).unwrap(), libc::O_WRONLY);
    assert_eq!(access(&named.files[1]).unwrap(), libc::O_RDONLY);
    let mut byte = [0];
    assert_eq!(
        (&named.files[1]).read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

struct Retired(std::path::PathBuf);
impl Drop for Retired {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}
fn pool(root: &std::path::Path) -> (Pool, ExternalDebt, Retired) {
    spawnfd::raise_soft_nofile();
    use std::os::unix::fs::DirBuilderExt as _;
    let legacy = root.join("legacy");
    fs::DirBuilder::new().mode(0o700).create(&legacy).unwrap();
    let cleanup = Retired(legacy.clone());
    let retirement = build_cpu::LegacyRetirement::retire(&legacy, 1).unwrap();
    let pool = Pool::open_in(root, std::num::NonZeroU32::new(1).unwrap()).unwrap();
    let debt = ExternalDebt::activate(&pool, &retirement).unwrap();
    (pool, debt, cleanup)
}
fn owner() -> Owner {
    build_cpu::process_cpu::read(std::process::id())
        .unwrap()
        .owner
}
fn request(cpu: CpuDelegation, gnu: GnuTransport) -> Request {
    use aterm_types::resource_admission::{Binding, Submission};
    let job = Binding {
        session: "s-ab12".into(),
        attempt: [1; 16],
    };
    Request {
        job: job.clone(),
        capability: aterm_types::resource_subgroup::Capability::from_bytes([7; 32]),
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
            label: "adapter".into(),
            timeout_ms: None,
        },
        cpu,
        gnu,
        fence_targets: vec![],
    }
}
fn rights() -> Delegations {
    Delegations {
        cpu: None,
        gnu: Vec::new(),
        fences: Vec::new(),
    }
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
            aterm_uds::deadline::kill_within(&mut self.0, std::time::Duration::from_secs(60));
        }
    }
}
#[test]
fn v3_rejects_legacy_fences_and_fixed_mode_cannot_mint_credit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, _retired) = pool(root.path());
    let mut r = request(CpuDelegation::FixedBorrow { units: 1 }, GnuTransport::None);
    let mut prepared = Prepared::try_new(&pool, &debt, 1, &r, &rights())
        .unwrap()
        .unwrap();
    assert!(prepared.lease.is_none());
    assert!(prepared.cpu.is_none());
    let mut command = spawnfd::command("/usr/bin/true");
    command.env(build_cpu::IMPLICIT_FD_ENV, "900");
    prepared.configure(&mut command).unwrap();
    assert_eq!(command.guard_targets().len(), 0);
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == build_cpu::IMPLICIT_FD_ENV && value.is_none())
    );
    assert!(prepared.configure(&mut command).is_err());
    r.fence_targets.push(9);
    assert!(Prepared::try_new(&pool, &debt, 1, &r, &rights()).is_err());
    r.fence_targets.clear();
    let file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.path().join("unheld-fence"))
        .unwrap();
    let mut supplied = rights();
    supplied.fences.push(file.into());
    assert!(Prepared::try_new(&pool, &debt, 1, &r, &supplied).is_err());
    assert!(matches!(
        pool.snapshot().unwrap()[0],
        build_cpu::SlotState::Available { .. }
    ));
}
#[test]
fn checked_adoption_retains_busy_inputs_and_legitimate_held_credit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, _retired) = pool(root.path());
    let lease = debt.try_acquire_native(owner(), 1).unwrap().unwrap();
    let r = request(
        CpuDelegation::SharedBorrow { target_fd: 900 },
        GnuTransport::None,
    );
    let fresh = File::options()
        .read(true)
        .write(true)
        .open(pool.path().join("slot-00000000"))
        .unwrap();
    let mut bad = rights();
    bad.cpu = Some(fresh.into());
    assert!(Prepared::try_new(&pool, &debt, 1, &r, &bad).is_err());
    assert!(
        bad.cpu
            .as_ref()
            .unwrap()
            .as_fd()
            .try_clone_to_owned()
            .is_ok()
    );
    let mut supplied = rights();
    supplied.cpu = Some(lease.try_clone().unwrap().into_file().into());
    let manifest = aterm_uds::file_lock::LockFile::take_existing(
        &pool.path().join("pool"),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        "fixture manifest lock",
    )
    .unwrap();
    assert!(
        Prepared::try_new(&pool, &debt, 1, &r, &supplied)
            .unwrap()
            .is_none()
    );
    assert!(
        supplied
            .cpu
            .as_ref()
            .unwrap()
            .as_fd()
            .try_clone_to_owned()
            .is_ok()
    );
    drop(manifest);
    let mut prepared = Prepared::try_new(&pool, &debt, 1, &r, &supplied)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.cpu_slot(), Some(lease.slot()));
    drop(supplied);
    drop(lease);
    assert!(debt.try_acquire_native(owner(), 1).unwrap().is_none());
    let mut command = spawnfd::command("/usr/bin/true");
    prepared.configure(&mut command).unwrap();
    assert_eq!(command.guard_targets().collect::<Vec<_>>(), [900]);
    assert!(prepared.bind_child(std::process::id()).is_err());
    drop(command);
    drop(prepared);
    assert!(debt.try_acquire_native(owner(), 1).unwrap().is_some());
}
#[test]
fn numeric_gnu_maps_original_pipe_and_flags_without_tokens_or_cpu_credit() {
    use std::io::{Read as _, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, _retired) = pool(root.path());
    let (read, write) = aterm_uds::kept_out(io::pipe).unwrap();
    let mut supplied = rights();
    supplied.gnu = vec![read.into(), write.into()];
    let mut prepared = Prepared::try_new(
        &pool,
        &debt,
        1,
        &request(
            CpuDelegation::FixedBorrow { units: 1 },
            GnuTransport::NumericPipe {
                read_target: 43,
                write_target: 44,
            },
        ),
        &supplied,
    )
    .unwrap()
    .unwrap();
    assert!(prepared.lease.is_none());
    let mut command = spawnfd::command("/usr/bin/true");
    prepared.configure(&mut command).unwrap();
    assert_eq!(command.guard_targets().collect::<Vec<_>>(), [43, 44]);
    for key in ["MAKEFLAGS", "MFLAGS", "CARGO_MAKEFLAGS"] {
        assert!(command.get_envs().any(|(name, value)| name == key
            && value
                == Some(std::ffi::OsStr::new(
                    "-j --jobserver-fds=43,44 --jobserver-auth=43,44"
                ))));
    }
    let watch = aterm_uds::ready::Watch::new(
        prepared._gnu_files[0].as_fd(),
        aterm_uds::ready::Interest::Read,
    );
    assert!(
        aterm_uds::ready::any_interests_now(&[watch])
            .unwrap()
            .is_empty()
    );
    File::from(duplicate(prepared._gnu_files[1].as_fd()).unwrap())
        .write_all(b"+")
        .unwrap();
    let mut byte = [0];
    File::from(duplicate(prepared._gnu_files[0].as_fd()).unwrap())
        .read_exact(&mut byte)
        .unwrap();
    assert_eq!(byte, *b"+");
    assert!(matches!(
        pool.snapshot().unwrap()[0],
        build_cpu::SlotState::Available { .. }
    ));
}

#[test]
fn named_prepared_configures_authenticated_fifo_role_and_original_flags() {
    use std::os::unix::ffi::OsStrExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, _retired) = pool(root.path());
    let (path, file, witness) = fifo(root.path(), "shared-fifo");
    let mut supplied = rights();
    supplied.gnu.push(file.into());
    let mut prepared = Prepared::try_new(
        &pool,
        &debt,
        1,
        &request(
            CpuDelegation::FixedBorrow { units: 1 },
            GnuTransport::NamedFifo {
                locator: path.as_os_str().as_bytes().to_vec(),
                witness,
                descriptors: 1,
            },
        ),
        &supplied,
    )
    .unwrap()
    .unwrap();
    let mut command = spawnfd::command("/usr/bin/true");
    prepared.configure(&mut command).unwrap();
    assert!(command.get_envs().any(|(key, value)| {
        key == jobserver_utils::DESCENDANT_HANDOFF_ENV
            && value.is_some_and(|value| value.as_bytes().starts_with(b"fifo1:"))
    }));
    assert!(command.get_envs().any(|(key, value)| {
        key == "MAKEFLAGS"
            && value.is_some_and(|value| value.as_bytes().ends_with(path.as_os_str().as_bytes()))
    }));
    assert_eq!(command.guard_targets().len(), 1);
    assert!(prepared.lease.is_none());
}

#[test]
fn named_controller_targets_remain_exact_and_cannot_alias() {
    use std::os::unix::ffi::OsStrExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, _retired) = pool(root.path());
    let (path, file, witness) = fifo(root.path(), "mapped-fifo");
    let mut supplied = rights();
    supplied.gnu = vec![file.try_clone().unwrap().into(), file.into()];
    let request = request(
        CpuDelegation::FixedBorrow { units: 1 },
        GnuTransport::NamedFifo {
            locator: path.as_os_str().as_bytes().to_vec(),
            witness,
            descriptors: 2,
        },
    );
    assert!(
        Prepared::try_new_mapped(&pool, &debt, 1, &request, &supplied, Some([71, 71])).is_err()
    );
    let mut prepared =
        Prepared::try_new_mapped(&pool, &debt, 1, &request, &supplied, Some([71, 73]))
            .unwrap()
            .unwrap();
    let mut command = spawnfd::command("/usr/bin/true");
    prepared.configure(&mut command).unwrap();
    assert_eq!(command.guard_targets().collect::<Vec<_>>(), [71, 73]);
}

#[test]
#[ignore = "started only by the held-child adapter fixture"]
fn held_member() {
    if std::env::var_os("ATERM_ADAPTER_MEMBER").is_none() {
        return;
    }
    let advertisement = std::env::var_os(jobserver_utils::CPU_HANDOFF_ENV).unwrap();
    let (cpu, fences) = jobserver_utils::recover_cpu_resources(&advertisement).unwrap();
    assert!(fences.is_empty());
    let pool = Pool::open_existing(std::path::Path::new(
        &std::env::var_os("ATERM_ADAPTER_POOL").unwrap(),
    ))
    .unwrap()
    .unwrap();
    let legacy = build_cpu::LegacyRetirement::open(std::path::Path::new(
        &std::env::var_os("ATERM_ADAPTER_LEGACY").unwrap(),
    ))
    .unwrap();
    let debt = ExternalDebt::open(&pool, &legacy).unwrap().unwrap();
    let _lease = debt
        .try_adopt_inherited(&File::from(cpu), |_| 1)
        .unwrap()
        .unwrap();
    println!("actual-scoped-cpu-recovered");
}

#[test]
#[cfg(target_os = "macos")]
fn actual_held_child_binding_reports_native_owner_and_restores_live_parent() {
    spawnfd::raise_soft_nofile();
    let root = aterm_tempfile::tempdir().unwrap();
    let (pool, debt, retired) = pool(root.path());
    let previous = owner();
    let original = debt.try_acquire_native(previous, 1).unwrap().unwrap();
    let mut supplied = rights();
    supplied.cpu = Some(original.try_clone().unwrap().into_file().into());
    let mut prepared = Prepared::try_new(
        &pool,
        &debt,
        1,
        &request(
            CpuDelegation::SharedBorrow { target_fd: 900 },
            GnuTransport::None,
        ),
        &supplied,
    )
    .unwrap()
    .unwrap();
    drop(supplied);
    assert!(
        jobserver_utils::recover_cpu_resources(prepared.cpu.as_ref().unwrap().advertisement())
            .is_err()
    );
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "subgroup_delegation::tests::held_member",
            "--nocapture",
        ])
        .env("ATERM_ADAPTER_MEMBER", "1")
        .env("ATERM_ADAPTER_POOL", root.path())
        .env("ATERM_ADAPTER_LEGACY", &retired.0)
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
    aterm_uds::deadline::prints(
        &mut child,
        "actual-scoped-cpu-recovered",
        std::time::Duration::from_secs(60),
    );
    let by = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < by, "member exit hang detector");
        exit.wait(by.saturating_duration_since(std::time::Instant::now()));
    }
    drop(command);
    drop(prepared);
    assert!(
        matches!(pool.snapshot().unwrap()[0],build_cpu::SlotState::Held {reported_owner:Some(current),..} if current==previous)
    );
    drop(original);
}

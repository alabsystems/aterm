// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Maintained native host qualification: `epoch_owner_probe --targo /absolute/targo`.
//! Uses a fresh private capacity domain, never the account's production supply.
//! Blocks the original helper's BIND transaction, then checks automatic recovery
//! and final cohort settlement without replacing the child or its backing.

#[cfg(target_os = "macos")]
mod native {
    use std::{
        fs,
        io::{self, Write as _},
        num::NonZeroU32,
        os::unix::fs::{
            DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
        },
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use aterm_capacity::{
        Admit, Ledger, Machine, Pace, Procs, Reading,
        build_cpu::{Owner as CpuOwner, RETAINED_DIRECTORY_NAME, RetainedPool, RetainedSlotState},
        cohort_namespace::Namespace,
        epoch_controller::{Config, RootCancellation},
        host_protocol::FailureKind,
        leadership::Fence,
    };
    use aterm_uds::{deadline, file_lock::LockFile, process, spawnfd};
    use atpkg::{
        build_admit::{
            self, Admission, JobControl, Seat,
            service::{EpochId, Owner},
        },
        build_cache::Tool,
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const HANG: Duration = Duration::from_secs(60);
    const FILE_PACE: Duration = Duration::from_millis(10);

    // Deterministic private admission input; this is no measurement of host
    // capacity, fairness, performance or production allocation policy.
    struct ProbeMachine;
    impl Machine for ProbeMachine {
        fn read(&self, _: Option<&Path>) -> Reading {
            Reading {
                cores: 8,
                load1: Some(0.0),
                load5: Some(0.0),
                available: Some(40 << 30),
                physical: Some(64 << 30),
                ..Reading::default()
            }
        }
        fn procs(&self) -> Option<Procs> {
            None
        }
    }

    struct World(Option<aterm_tempfile::TempDir>);
    impl World {
        fn new() -> io::Result<Self> {
            Ok(Self(Some(aterm_tempfile::tempdir()?)))
        }
        fn path(&self) -> &Path {
            self.0.as_ref().expect("owned fixture").path()
        }
        fn keep(&mut self) -> PathBuf {
            self.0.take().expect("owned fixture").keep()
        }
    }
    impl Drop for World {
        fn drop(&mut self) {
            if let Some(directory) = self.0.take() {
                // Release only this fixture's compiled actors. No saved PID is
                // signalled, and failed capacity/endpoint evidence is preserved.
                let _ = fs::write(directory.path().join("member.abort"), b"probe failed\n");
                let _ = fs::write(directory.path().join("member.release"), b"cleanup\n");
                for side in ["left", "right"] {
                    let _ = fs::write(
                        directory.path().join(format!("cancel-{side}.release")),
                        b"cleanup\n",
                    );
                }
                let path = directory.keep();
                let _ = writeln!(
                    io::stderr(),
                    "kept failed host epoch probe: {}",
                    path.display()
                );
            }
        }
    }

    struct CancelBarriers {
        root: PathBuf,
        armed: bool,
    }
    impl Drop for CancelBarriers {
        fn drop(&mut self) {
            if self.armed {
                // Release only the fixture's known work bodies on every error.
                // Their real parents still drain; no PID is used as control.
                for side in ["left", "right"] {
                    let _ = fs::write(
                        self.root.join(format!("cancel-{side}.release")),
                        b"cleanup\n",
                    );
                }
            }
        }
    }

    fn require(condition: bool, why: &'static str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(io::Error::other(why).into())
        }
    }
    fn private_dir(path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    fn publish(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        let path = root.join(name);
        if path.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "fixture publication exists",
            ));
        }
        let temporary = root.join(format!(".{name}.pending"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        drop(file);
        fs::rename(temporary, path)
    }
    fn tick(owner: &mut Owner) -> io::Result<()> {
        let completed = owner.tick(&mut |_, _| Ok(JobControl::Wait), &mut |line| {
            eprintln!("{line}")
        })?;
        if !completed.is_empty() {
            return Err(io::Error::other(
                "unrelated native job completed in private fixture",
            ));
        }
        Ok(())
    }
    fn until(
        owner: &mut Owner,
        mut ready: impl FnMut(&Owner) -> io::Result<bool>,
        file_barrier: bool,
    ) -> Result<()> {
        let deadline = Instant::now() + HANG;
        loop {
            tick(owner)?;
            if ready(owner)? {
                return Ok(());
            }
            require(
                Instant::now() < deadline,
                "native host epoch stage did not complete",
            )?;
            // Control/child readiness drives ordinary stages. Only actual file
            // barriers need a bounded observation cadence in addition to it.
            let next = if file_barrier {
                deadline.min(Instant::now() + FILE_PACE)
            } else {
                deadline
            };
            owner.wait(next)?;
        }
    }
    fn fifo_identity(path: &Path) -> Result<(u64, u64)> {
        let metadata = fs::symlink_metadata(path)?;
        require(
            metadata.file_type().is_fifo(),
            "original named FIFO is missing or changed type",
        )?;
        Ok((metadata.dev(), metadata.ino()))
    }
    fn original_fifo_removed(path: &Path, pinned: &fs::File) -> Result<()> {
        require(
            matches!(fs::symlink_metadata(path),Err(ref error) if error.kind()==io::ErrorKind::NotFound)
                && pinned.metadata()?.nlink() == 0,
            "original pinned FIFO was not unlinked before CPU settlement",
        )
    }
    fn claim_identity(path: &Path) -> Result<(u64, u64)> {
        let metadata = fs::symlink_metadata(path)?;
        require(
            metadata.is_file() && metadata.nlink() == 1 && metadata.mode() & 0o077 == 0,
            "original claim is not a private single-link regular file",
        )?;
        Ok((metadata.dev(), metadata.ino()))
    }
    fn cpu_records(pool: &RetainedPool, slots: &[u32]) -> io::Result<Vec<Vec<u8>>> {
        slots
            .iter()
            .map(|slot| {
                fs::read(
                    pool.path()
                        .join(RETAINED_DIRECTORY_NAME)
                        .join(format!("slot-{slot:08x}")),
                )
            })
            .collect()
    }
    fn original_cpu_held(pool: &RetainedPool) -> Result<()> {
        let slots = pool.snapshot()?;
        require(
            slots.len() == 2
                && slots
                    .iter()
                    .all(|slot| matches!(slot, RetainedSlotState::Held { .. })),
            "both original CPU capabilities must remain actually held",
        )
    }
    fn member_identity(note: &str) -> Result<(u32, u32)> {
        let mut lines = note.lines();
        let pid = lines
            .next()
            .and_then(|line| line.strip_prefix("pid="))
            .ok_or_else(|| io::Error::other("member PID missing"))?
            .parse()?;
        let group = lines
            .next()
            .and_then(|line| line.strip_prefix("pgid="))
            .ok_or_else(|| io::Error::other("member group missing"))?
            .parse()?;
        require(pid > 1 && group > 1, "invalid compiled member identity")?;
        Ok((pid, group))
    }
    fn live_identity(pid: u32) -> Result<process::Identity> {
        let pid = i32::try_from(pid)?;
        require(
            process::process_presence(pid) == process::ProcessPresence::Present,
            "actual probe process is no longer live",
        )?;
        Ok(process::read_lineage(pid)
            .ok_or_else(|| io::Error::other("probe process native birth unavailable"))?
            .identity)
    }
    fn epoch_row(owner: &Owner, id: &EpochId) -> Result<aterm_capacity::host_protocol::Epoch> {
        let snapshot = publication(owner)?;
        // Exercise the exact public observation codec consumed by the panel.
        let snapshot = aterm_capacity::host_protocol::Snapshot::decode(&snapshot.encode()?)?;
        snapshot
            .epochs
            .into_iter()
            .find(|row| row.sequence == id.sequence())
            .ok_or_else(|| io::Error::other("owner epoch observation missing").into())
    }

    fn publication(
        owner: &Owner,
    ) -> io::Result<std::sync::Arc<aterm_capacity::host_protocol::Snapshot>> {
        owner
            .observer()
            .read(None)
            .map(|read| read.snapshot)
            .map_err(|error| io::Error::other(format!("owner observation refused: {error:?}")))
    }

    fn script(root: &Path) -> Result<String> {
        let root = format!(
            "{:?}",
            root.to_str()
                .ok_or_else(|| io::Error::other("fixture path is not UTF-8"))?
        );
        Ok(r#"
use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
unsafe extern "C" { fn getpgrp() -> i32; }
fn wait(root:&Path,name:&str)->bool {
    let until=Instant::now()+Duration::from_secs(60);
    while !root.join(name).exists() {
        if root.join("member.abort").exists() { return false; }
        assert!(Instant::now()<until,"host fixture did not release member");
        std::thread::sleep(Duration::from_millis(10));
    }
    !root.join("member.abort").exists()
}
fn note(root:&Path,name:&str,body:&str) {
    let temporary=root.join(format!(".{name}.pending"));
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).unwrap();
    // SAFETY: getpgrp only observes this actual executing process's group.
    let group=unsafe{getpgrp()};
    writeln!(file,"pid={}\npgid={group}\n{body}",std::process::id()).unwrap();
    drop(file);
    std::fs::rename(temporary,root.join(name)).unwrap();
}
fn main() {
    let root=PathBuf::from(__ROOT__);
    if std::env::var_os("ATERM_HOST_EPOCH_PROBE_MEMBER").is_some() {
        note(&root,"member.ready","ready");
        if !wait(&root,"member.challenge") { return; }
        let nonce=std::fs::read_to_string(root.join("member.challenge")).unwrap();
        note(&root,"member.alive",&format!("nonce={}",nonce.trim()));
        if !wait(&root,"member.release") { return; }
        note(&root,"member.released",&format!("nonce={}",nonce.trim()));
        return;
    }
    let child=Command::new(std::env::current_exe().unwrap())
        .env("ATERM_HOST_EPOCH_PROBE_MEMBER","1").stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(root.join("member.stdout")).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(root.join("member.stderr")).unwrap()))
        .spawn().unwrap();
    if !wait(&root,"member.ready") { return; }
    assert!(std::fs::read_to_string(root.join("member.ready")).unwrap().starts_with(&format!("pid={}\n",child.id())));
    drop(child); // Native host owns the survivor's fixture release, not its PID.
    println!("cargo:rerun-if-changed=build.rs");
}
"#.replace("__ROOT__",&root))
    }

    fn cancel_script(root: &Path, side: &str) -> Result<String> {
        let root = format!(
            "{:?}",
            root.to_str()
                .ok_or_else(|| io::Error::other("fixture path is not UTF-8"))?
        );
        Ok(r#"
use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _, path::{Path,PathBuf}, time::{Duration,Instant}};
unsafe extern "C" { fn getpgrp() -> i32; }
fn note(root:&Path,side:&str,event:&str,body:&str) {
    let name=format!("cancel-{side}.{event}");
    let temporary=root.join(format!(".{name}.pending"));
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).unwrap();
    // SAFETY: getpgrp observes this actual executing build script's group.
    let group=unsafe{getpgrp()};
    writeln!(file,"pid={}\npgid={group}\n{body}",std::process::id()).unwrap();
    drop(file);
    std::fs::rename(temporary,root.join(name)).unwrap();
}
fn main() {
    let root=PathBuf::from(__ROOT__);
    let side=__SIDE__;
    note(&root,side,"ready","ready");
    let deadline=Instant::now()+Duration::from_secs(60);
    let mut challenged=false;
    while !root.join(format!("cancel-{side}.release")).exists() {
        if !challenged {
            if let Ok(nonce)=std::fs::read_to_string(root.join("cancel.challenge")) {
                note(&root,side,"alive",&format!("nonce={}",nonce.trim()));
                challenged=true;
            }
        }
        assert!(Instant::now()<deadline,"host fixture did not release cancelled work");
        std::thread::sleep(Duration::from_millis(10));
    }
    note(&root,side,"done","normal successful body completion");
    println!("cargo:rerun-if-changed=build.rs");
}
"#.replace("__ROOT__", &root).replace("__SIDE__", &format!("{side:?}")))
    }

    fn qualify_cancellation(
        root: &Path,
        helper: &Path,
        owner: &mut Owner,
        seat: &Seat<'_>,
        ledger: &Ledger,
        pool: &RetainedPool,
    ) -> Result<aterm_json::Value> {
        require(
            owner.is_empty(),
            "first epoch remains active before cancellation phase",
        )?;
        let generation = publication(owner)?.generation;
        let scope = (process::host_id(), process::boot_id());
        require(
            scope.0.is_some() && scope.1.is_some(),
            "native cancellation scope unavailable",
        )?;
        let mut barriers = CancelBarriers {
            root: root.to_owned(),
            armed: true,
        };
        let project = root.join("cancel-project.noindex");
        private_dir(&project)?;
        private_dir(&project.join("src"))?;
        publish(&project,"Cargo.toml",b"[package]\nname='host_epoch_cancelled'\nversion='0.0.0'\nedition='2024'\n[build-dependencies]\nhost_cancel_left={path='left'}\nhost_cancel_right={path='right'}\n[workspace]\n")?;
        publish(
            &project.join("src"),
            "lib.rs",
            b"pub fn cancelled_fixture() {}\n",
        )?;
        let forbidden = format!(
            "fn main(){{std::fs::write({:?},b\"unexpected queued root work\\n\").unwrap();}}\n",
            root.join("cancel-forbidden")
        );
        publish(&project, "build.rs", forbidden.as_bytes())?;
        for side in ["left", "right"] {
            let dependency = project.join(side);
            private_dir(&dependency)?;
            private_dir(&dependency.join("src"))?;
            publish(
                &dependency,
                "Cargo.toml",
                format!("[package]\nname='host_cancel_{side}'\nversion='0.0.0'\nedition='2024'\n")
                    .as_bytes(),
            )?;
            publish(
                &dependency.join("src"),
                "lib.rs",
                b"pub fn independent_dependency() {}\n",
            )?;
            publish(
                &dependency,
                "build.rs",
                cancel_script(root, side)?.as_bytes(),
            )?;
        }
        let caps = (0..2)
            .map(|_| {
                pool.try_acquire(CpuOwner::current(None))?
                    .ok_or_else(|| io::Error::other("second epoch backing missing"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        require(
            caps[0].slot() != caps[1].slot(),
            "cancellation backing aliases",
        )?;
        original_cpu_held(pool)?;
        let build = build_admit::classify(&["--unverified".into(), "build".into()], Tool::Targo)
            .ok_or_else(|| io::Error::other("cancellation build classification failed"))?;
        let held = match build_admit::admit_cohort_job(&build, &project, seat, &mut |line| {
            eprintln!("{line}")
        }) {
            Admission::Claimed(held) => *held,
            other => {
                return Err(
                    io::Error::other(format!("cancellation admission refused: {other:?}")).into(),
                );
            }
        };
        let admitted_bytes = held.claim().record().need.mem;
        let transport = root.join("cancel-transport.noindex");
        let target = root.join("cancel-stable-target.noindex");
        let accepted = owner.submit_epoch(
            held,
            caps,
            Config {
                helper: helper.to_owned(),
                directory: transport.clone(),
                epoch: aterm_uds::rand::hex_token::<16>()?,
                roots: 1,
            },
        )?;
        accepted.result?;
        let id = accepted.id;
        until(owner, |owner| Ok(owner.epoch_status(&id)?.opened), false)?;
        let group = owner
            .epoch_helper_id(&id)?
            .ok_or_else(|| io::Error::other("cancellation helper missing"))?;
        let fifo = transport.join("credits.fifo");
        let original_fifo = fifo_identity(&fifo)?;
        let pinned_fifo = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&fifo)?;
        let pinned = pinned_fifo.metadata()?;
        require(
            (pinned.dev(), pinned.ino()) == original_fifo && pinned.nlink() == 1,
            "cancellation FIFO does not match pinned original",
        )?;
        owner.epoch_spawn_root(&id, 0, &project.join("Cargo.toml"), &target)?;
        until(
            owner,
            |_| {
                Ok(root.join("cancel-left.ready").try_exists()?
                    && root.join("cancel-right.ready").try_exists()?)
            },
            true,
        )?;
        // Two independent plain build scripts executing together need two
        // active units: the actual root's implicit unit plus its acquired P=1.
        // Neither script imports or manufactures any jobserver capability.
        let left = member_identity(&fs::read_to_string(root.join("cancel-left.ready"))?)?;
        let right = member_identity(&fs::read_to_string(root.join("cancel-right.ready"))?)?;
        require(
            left.0 != right.0 && left.1 == group && right.1 == group,
            "independent cancellation bodies do not share the original group",
        )?;
        let left_identity = live_identity(left.0)?;
        let right_identity = live_identity(right.0)?;
        require(
            owner.epoch_cancel_root(&id, 0)? == RootCancellation::Requested,
            "actual Owner did not queue cancellation",
        )?;
        require(
            owner.epoch_cancel_root(&id, 0)? == RootCancellation::Requested,
            "duplicate cancellation changed pending intent",
        )?;
        owner.epoch_close(&id)?;
        until(
            owner,
            |owner| Ok(owner.epoch_root_cancellation(&id, 0)? == Some(RootCancellation::Stopped)),
            false,
        )?;
        let nonce = aterm_uds::rand::hex_token::<16>()?;
        publish(root, "cancel.challenge", nonce.as_bytes())?;
        until(
            owner,
            |_| {
                Ok(root.join("cancel-left.alive").try_exists()?
                    && root.join("cancel-right.alive").try_exists()?)
            },
            true,
        )?;
        for (side, expected) in [("left", left), ("right", right)] {
            let note = fs::read_to_string(root.join(format!("cancel-{side}.alive")))?;
            require(
                member_identity(&note)? == expected
                    && note.lines().any(|line| line == format!("nonce={nonce}")),
                "active body failed fresh challenge after stop acknowledgement",
            )?;
        }
        require(
            live_identity(left.0)? == left_identity && live_identity(right.0)? == right_identity,
            "cancellation killed or replaced an active work body",
        )?;
        let stopped = owner.epoch_status(&id)?;
        require(
            !stopped.retired
                && !stopped.helper_exited
                && !stopped.control_eof
                && !stopped.endpoint_cleaned
                && stopped.completed_roots == 0
                && stopped.settled_units == 0
                && stopped.uncertain_units == 0
                && stopped.remaining_units == 2
                && !stopped.faulted
                && !owner.is_empty(),
            "stop acknowledgement falsely completed or refunded active root work",
        )?;
        require(
            owner.epoch_root_outcome(&id, 0)?.is_none()
                && !owner.epoch_root_eligible_for_activation(&id, 0)?,
            "cancelled active root became eligible or acquired a fake outcome",
        )?;
        require(
            fifo_identity(&fifo)? == original_fifo && pinned_fifo.metadata()?.nlink() == 1,
            "cancellation changed or unlinked live transport",
        )?;
        original_cpu_held(pool)?;
        require(
            pool.try_acquire(CpuOwner::current(None))?.is_none(),
            "active cancelled root backing was reissued",
        )?;
        require(
            !root.join("cancel-forbidden").try_exists()?,
            "queued root work ran before drain",
        )?;
        let held_row = epoch_row(owner, &id)?;
        require(
            held_row.backing == 2
                && held_row.roots == 1
                && held_row.reserved_bytes == admitted_bytes
                && held_row.remaining_units == 2
                && held_row.completed_roots == 0
                && held_row.failed_roots == 0,
            "cancellation publication lost original admission or live work",
        )?;
        publish(
            root,
            "cancel-held.json",
            &aterm_json::to_vec(&aterm_json::json!({
                "epoch_sequence":id.sequence(),"state":"stopped","nonce":nonce,
                "left":left_identity.token(),"right":right_identity.token(),
                "backing":2,"roots":1,"published":1,"remaining_units":2,
                "root_outcome_absent":true,"active_bodies_answered_after_ack":true,
                "fifo_device":original_fifo.0,"fifo_inode":original_fifo.1,
                "pinned_fifo_links":pinned_fifo.metadata()?.nlink(),"admitted_bytes":admitted_bytes
            }))?,
        )?;
        publish(root, "cancel-left.release", b"normal completion\n")?;
        publish(root, "cancel-right.release", b"normal completion\n")?;
        barriers.armed = false;
        let deadline = Instant::now() + HANG;
        let mut first_settlement_observed = false;
        loop {
            tick(owner)?;
            require(
                !root.join("cancel-forbidden").try_exists()?,
                "cancelled root dispatched queued dependent work",
            )?;
            let progress = owner.epoch_status(&id)?;
            if let Some(outcome) = owner.epoch_root_outcome(&id, 0)? {
                require(
                    !outcome.success && outcome.code == Some(101),
                    "cancelled actual root did not preserve its non-success exit",
                )?;
            }
            let attempted = progress.settled_units + progress.uncertain_units;
            if attempted > 0 {
                require(
                    scope == (process::host_id(), process::boot_id())
                        && process::group_presence(group) == process::ProcessPresence::Absent,
                    "cancelled root CPU settlement preceded original group drain",
                )?;
                original_fifo_removed(&fifo, &pinned_fifo)?;
                require(
                    progress.retired
                        && progress.helper_exited
                        && progress.control_eof
                        && progress.endpoint_cleaned
                        && progress.uncertain_units == 0,
                    "cancelled root CPU settlement bypassed retirement or durable cleanup",
                )?;
                if !first_settlement_observed {
                    require(
                        attempted == 1,
                        "cancelled root skipped first CPU settlement turn",
                    )?;
                    publish(
                        root,
                        "cancel-first-settlement.json",
                        &aterm_json::to_vec(&aterm_json::json!({
                            "settled_units":progress.settled_units,"remaining_units":progress.remaining_units,
                            "group_absent":true,"named_fifo_absent":true,
                            "pinned_fifo_links":pinned_fifo.metadata()?.nlink(),"endpoint_cleaned":true
                        }))?,
                    )?;
                    first_settlement_observed = true;
                }
            }
            if owner.is_empty() {
                break;
            }
            require(
                Instant::now() < deadline,
                "cancelled owner epoch did not drain",
            )?;
            owner.wait(deadline)?;
        }
        require(
            first_settlement_observed,
            "cancelled root first settlement was not observed",
        )?;
        for (side, expected) in [("left", left), ("right", right)] {
            require(
                member_identity(&fs::read_to_string(
                    root.join(format!("cancel-{side}.done")),
                )?)? == expected,
                "cancelled active body did not finish normally",
            )?;
        }
        require(
            ledger.list()?.is_empty(),
            "cancellation memory admission remains after retirement",
        )?;
        require(
            scope == (process::host_id(), process::boot_id())
                && process::group_presence(group) == process::ProcessPresence::Absent,
            "cancelled original cohort did not conclusively drain",
        )?;
        original_fifo_removed(&fifo, &pinned_fifo)?;
        let status = owner.epoch_status(&id)?;
        let outcome = owner.epoch_root_outcome(&id, 0)?.ok_or_else(|| {
            io::Error::other("cancelled root actual outcome missing from history")
        })?;
        require(
            status.retired
                && status.helper_exited
                && status.control_eof
                && status.endpoint_cleaned
                && status.completed_roots == 1
                && status.failed_roots == 1
                && status.settled_units == 2
                && status.uncertain_units == 0
                && status.remaining_units == 0
                && !status.faulted
                && !outcome.success
                && outcome.code == Some(101),
            "cancelled root settled history lost actual failed outcome or exact CPU closure",
        )?;
        require(
            owner.epoch_root_cancellation(&id, 0)? == Some(RootCancellation::Stopped)
                && !owner.epoch_root_eligible_for_activation(&id, 0)?,
            "cancelled history lost driver acknowledgement or became eligible",
        )?;
        let completed = epoch_row(owner, &id)?;
        require(
            publication(owner)?.generation == generation
                && completed.backing == 2
                && completed.roots == 1
                && completed.reserved_bytes == admitted_bytes
                && completed.completed_roots == 1
                && completed.failed_roots == 1
                && completed.settled_units == 2
                && completed.remaining_units == 0
                && completed.uncertain_units == 0,
            "cancelled publication lost original generation, admission or failed result",
        )?;
        require(
            pool.snapshot()?
                .iter()
                .filter(|state| matches!(state, RetainedSlotState::Available { .. }))
                .count()
                == 2,
            "cancelled original pool did not restore exactly N",
        )?;
        let mut recovered = Vec::new();
        for _ in 0..2 {
            recovered.push(
                pool.try_acquire(CpuOwner::current(None))?
                    .ok_or_else(|| io::Error::other("cancelled backing was not restored"))?,
            );
        }
        require(
            pool.try_acquire(CpuOwner::current(None))?.is_none(),
            "cancellation minted a third credit",
        )?;
        for cap in recovered {
            cap.settle()?;
        }
        Ok(aterm_json::json!({
            "epoch_sequence":id.sequence(),"backing":2,"roots":1,"published":1,
            "admitted_bytes":admitted_bytes,"cancel_state":"stopped",
            "active_bodies_answered_after_ack":true,"active_bodies_completed_normally":true,
            "dependent_never_started":true,"root_code":outcome.code,"root_success":outcome.success,
            "history_preserved":true,"eligible_for_activation":false,
            "retired":status.retired,"helper_exited":status.helper_exited,"control_eof":status.control_eof,
            "group_drained":true,"endpoint_cleaned":status.endpoint_cleaned,
            "settled_units":status.settled_units,"uncertain_units":status.uncertain_units,
            "first_settlement_observed":first_settlement_observed,"pinned_fifo_links":pinned_fifo.metadata()?.nlink(),
            "exact_n_reacquired":true,"no_third_credit":true,"memory_admission_empty_after_entry_retirement":true
        }))
    }

    fn qualify(root: &Path, helper: &Path) -> Result<()> {
        let mut version = spawnfd::command(helper);
        version.arg0("targo").args(["--unverified", "--version"]);
        let version = deadline::output_until(&mut version, Instant::now() + HANG)?;
        require(
            version.status.success() && version.stdout.starts_with(b"targo "),
            "selected helper is not genuine Targo",
        )?;
        publish(root, "helper.version", &version.stdout)?;
        let home = root.join("home");
        private_dir(&home)?;
        let namespace = Namespace::create_private(root, &home)?;
        let ledger = Ledger::at(namespace.fence());
        let fence = Fence::acquire(&namespace, Instant::now() + HANG)?;
        let generation = fence.generation();
        let mut owner = Owner::fenced(fence, 2, 8)?;
        let pool = RetainedPool::create_private(
            &root.join("cpu.noindex"),
            NonZeroU32::new(2).expect("fixed nonzero width"),
        )?;
        let caps = (0..2)
            .map(|_| {
                pool.try_acquire(CpuOwner::current(None))?
                    .ok_or_else(|| io::Error::other("original CPU backing missing"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let original_slots: Vec<_> = caps.iter().map(|cap| cap.slot()).collect();
        require(
            original_slots[0] != original_slots[1],
            "original CPU capabilities alias one slot",
        )?;
        let original_cpu = cpu_records(&pool, &original_slots)?;
        original_cpu_held(&pool)?;
        for (slot, record) in original_slots.iter().zip(&original_cpu) {
            publish(root, &format!("cpu-{slot:08x}.record"), record)?;
        }
        let project = root.join("project.noindex");
        private_dir(&project)?;
        private_dir(&project.join("src"))?;
        publish(&project,"Cargo.toml",b"[package]\nname='host_epoch_artifact'\nversion='0.0.0'\nedition='2024'\n[workspace]\n")?;
        publish(&project.join("src"),"main.rs",b"fn main(){assert_eq!((0..10).sum::<u64>(),45);println!(\"host-epoch-artifact-ok\");}\n")?;
        publish(&project, "build.rs", script(root)?.as_bytes())?;
        let machine = ProbeMachine;
        let seat = Seat {
            ledger: Some(Ledger::at(namespace.fence())),
            machine: &machine,
            own: None,
            verify: None,
            mode: Admit::Wait,
            pace: Pace::default(),
            pid: std::process::id(),
            marker: None,
        };
        let build = build_admit::classify(&["--unverified".into(), "build".into()], Tool::Targo)
            .ok_or_else(|| io::Error::other("Targo build classification failed"))?;
        let held = match build_admit::admit_cohort_job(&build, &project, &seat, &mut |line| {
            eprintln!("{line}")
        }) {
            Admission::Claimed(held) => *held,
            other => {
                return Err(io::Error::other(format!(
                    "private cohort admission refused: {other:?}"
                ))
                .into());
            }
        };
        let admitted_bytes = held.claim().record().need.mem;
        let claim_path = held.claim().path().to_owned();
        let original_claim = claim_identity(&claim_path)?;
        let description = fs::read(&claim_path)?;
        publish(root, "claim-description.record", &description)?;
        let transport = root.join("transport.noindex");
        let target = root.join("stable-target.noindex");
        let manifest = project.join("Cargo.toml");
        let accepted = owner.submit_epoch(
            held,
            caps,
            Config {
                helper: helper.to_owned(),
                directory: transport.clone(),
                epoch: aterm_uds::rand::hex_token::<16>()?,
                roots: 1,
            },
        )?;
        accepted.result?;
        let id = accepted.id;
        let dispatched = owner.epoch_status(&id)?;
        require(
            dispatched.unbound
                && !dispatched.opened
                && !dispatched.not_spawned
                && !dispatched.faulted
                && dispatched.settled_units == 0
                && dispatched.uncertain_units == 0
                && dispatched.remaining_units == 2,
            "successful dispatch must retain an actual unbound helper before the first tick",
        )?;
        let helper_pid = owner
            .epoch_helper_id(&id)?
            .ok_or_else(|| io::Error::other("dispatched actual helper is missing"))?;
        let helper_identity = live_identity(helper_pid)?;
        let armed = fs::read(&claim_path)?;
        require(
            claim_identity(&claim_path)? == original_claim
                && armed.starts_with(&description)
                && armed.len() > description.len(),
            "dispatch did not durably ARM the original admitted claim",
        )?;
        require(
            cpu_records(&pool, &original_slots)? == original_cpu,
            "dispatch changed original retained CPU nonces",
        )?;
        publish(root, "claim-armed.record", &armed)?;
        let refused_root = owner.epoch_spawn_root(&id, 0, &manifest, &target);
        require(
            refused_root.is_err()
                && epoch_row(&owner, &id)?
                    .failure
                    .is_some_and(|failure| failure.kind == FailureKind::Invalid),
            "unopened unbound helper accepted a root or hid its refusal",
        )?;
        // Dispatch has already committed ARM and produced the actual child.
        // This fresh, independently locked description makes the next BIND
        // transaction fail by real flock contention, without changing bytes,
        // racing the child's scheduling or replacing a production callback.
        let mutation = claim_path
            .parent()
            .ok_or_else(|| io::Error::other("original claim has no directory"))?
            .join("mutation.lock");
        let obstruction =
            LockFile::take_existing(&mutation, Instant::now(), "owned BIND obstruction")?;
        tick(&mut owner)?;
        let blocked = owner.epoch_status(&id)?;
        let blocked_row = epoch_row(&owner, &id)?;
        require(
            blocked.unbound
                && !blocked.opened
                && !blocked.faulted
                && blocked.settled_units == 0
                && blocked.uncertain_units == 0
                && blocked.remaining_units == 2
                && blocked_row
                    .failure
                    .is_some_and(|failure| failure.kind == FailureKind::Other),
            "automatic BIND did not observe real contention and retain original custody",
        )?;
        let explicit = owner
            .epoch_retry_binding(&id)
            .err()
            .ok_or_else(|| io::Error::other("explicit retry bypassed blocked original BIND"))?;
        require(
            explicit.kind() == io::ErrorKind::WouldBlock,
            "explicit retry bypassed actual BIND contention or its shared retry pace",
        )?;
        require(
            owner.epoch_helper_id(&id)? == Some(helper_pid)
                && live_identity(helper_pid)? == helper_identity
                && claim_identity(&claim_path)? == original_claim
                && fs::read(&claim_path)? == armed
                && cpu_records(&pool, &original_slots)? == original_cpu
                && !owner.is_empty()
                && pool.try_acquire(CpuOwner::current(None))?.is_none(),
            "blocked BIND lost the actual original child, ARM or CPU backing",
        )?;
        original_cpu_held(&pool)?;
        publish(
            root,
            "bind-blocked.json",
            &aterm_json::to_vec(&aterm_json::json!({
                "helper":helper_identity.token(),"claim_device":original_claim.0,
                "claim_inode":original_claim.1,"arm_bytes":armed.len(),
                "original_cpu_slots":original_slots,"cpu_nonces_unchanged":true,
                "automatic_bind_contended":true,"explicit_retry_would_block":true,
                "unbound":blocked.unbound,"opened":blocked.opened,
                "settled_units":blocked.settled_units,"remaining_units":blocked.remaining_units
            }))?,
        )?;
        drop(obstruction); // Unlock and close only this fixture's mutation lock.
        until(
            &mut owner,
            |owner| Ok(owner.epoch_status(&id)?.opened),
            false,
        )?;
        require(
            !owner.epoch_status(&id)?.unbound
                && owner.epoch_helper_id(&id)? == Some(helper_pid)
                && live_identity(helper_pid)? == helper_identity,
            "automatic BIND retry replaced its actual original helper",
        )?;
        let bound = fs::read(&claim_path)?;
        require(
            claim_identity(&claim_path)? == original_claim
                && bound.starts_with(&armed)
                && bound.len() > armed.len()
                && cpu_records(&pool, &original_slots)? == original_cpu,
            "automatic BIND did not extend the original ARM while retaining CPU backing",
        )?;
        publish(root, "claim-bound.record", &bound)?;
        publish(
            root,
            "bind-recovered.json",
            &aterm_json::to_vec(&aterm_json::json!({
                "same_helper":helper_identity.token(),"automatic_binding_recovered":true,
                "opened":true,"remaining_units":owner.epoch_status(&id)?.remaining_units
            }))?,
        )?;
        let fifo = transport.join("credits.fifo");
        let original_fifo = fifo_identity(&fifo)?;
        // Holding the original read-only node prevents an unlink/recreate inode
        // ABA from satisfying the named dev/inode comparison. Never read a GNU
        // credit from this independent observer, or pass its FD to any child.
        let pinned_fifo = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&fifo)?;
        let pinned = pinned_fifo.metadata()?;
        require(
            (pinned.dev(), pinned.ino()) == original_fifo && pinned.nlink() == 1,
            "opened FIFO does not match the original pinned node",
        )?;
        owner.epoch_spawn_root(&id, 0, &manifest, &target)?;
        until(&mut owner, |_| root.join("member.ready").try_exists(), true)?;
        let ready = fs::read_to_string(root.join("member.ready"))?;
        let (member, group) = member_identity(&ready)?;
        require(
            owner.epoch_helper_id(&id)? == Some(group),
            "compiled member escaped the actual helper group",
        )?;
        let identity = live_identity(member)?;
        let scope = (process::host_id(), process::boot_id());
        require(
            scope.0.is_some() && scope.1.is_some(),
            "original host/boot identity unavailable",
        )?;
        owner.epoch_close(&id)?;
        until(
            &mut owner,
            |owner| {
                let s = owner.epoch_status(&id)?;
                Ok(s.retired && s.helper_exited && s.control_eof)
            },
            false,
        )?;
        let outcome = owner
            .epoch_root_outcome(&id, 0)?
            .ok_or_else(|| io::Error::other("actual root outcome missing"))?;
        require(
            outcome.success && outcome.code == Some(0),
            "genuine Targo root did not succeed",
        )?;
        let nonce = aterm_uds::rand::hex_token::<16>()?;
        publish(root, "member.challenge", nonce.as_bytes())?;
        until(&mut owner, |_| root.join("member.alive").try_exists(), true)?;
        let alive = fs::read_to_string(root.join("member.alive"))?;
        let expected_nonce = format!("nonce={nonce}");
        require(
            member_identity(&alive)? == (member, group)
                && alive.lines().any(|line| line == expected_nonce),
            "surviving member failed fresh post-retirement challenge",
        )?;
        require(
            live_identity(member)? == identity,
            "actual compiled member identity changed",
        )?;
        require(
            process::group_presence(group) == process::ProcessPresence::Present,
            "original group is not live at withheld-capacity control",
        )?;
        require(
            !owner.is_empty(),
            "owner forgot the unresolved epoch after root completion",
        )?;
        let status = owner.epoch_status(&id)?;
        require(
            !status.endpoint_cleaned
                && status.settled_units == 0
                && status.remaining_units == 2
                && status.uncertain_units == 0,
            "owner returned backing while original member remained live",
        )?;
        require(
            pool.try_acquire(CpuOwner::current(None))?.is_none(),
            "live epoch backing was reissued",
        )?;
        require(
            fifo_identity(&fifo)? == original_fifo && pinned_fifo.metadata()?.nlink() == 1,
            "original FIFO was removed or replaced before cohort drain",
        )?;
        let held = epoch_row(&owner, &id)?;
        require(
            held.backing == 2
                && held.roots == 1
                && held.reserved_bytes == admitted_bytes
                && held.retired
                && held.remaining_units == 2
                && held.settled_units == 0
                && held.uncertain_units == 0
                && held.completed_roots == 1
                && held.failed_roots == 0,
            "owner publication lost actual retained state",
        )?;
        publish(
            root,
            "held.json",
            &aterm_json::to_vec(
                &aterm_json::json!({"helper_group":group,"member":identity.token(),"nonce":nonce,"retired":true,"helper_exited":true,"control_eof":true,"settled_units":status.settled_units,"remaining_units":status.remaining_units,"fifo_device":original_fifo.0,"fifo_inode":original_fifo.1,"pinned_fifo_links":pinned_fifo.metadata()?.nlink(),"admitted_bytes":admitted_bytes,"backing":held.backing,"roots":held.roots,"completed_roots":held.completed_roots,"failed_roots":held.failed_roots}),
            )?,
        )?;
        publish(root, "member.release", nonce.as_bytes())?;
        let deadline = Instant::now() + HANG;
        let mut first_settlement_observed = false;
        loop {
            tick(&mut owner)?;
            let progress = owner.epoch_status(&id)?;
            let attempted = progress.settled_units + progress.uncertain_units;
            if attempted > 0 {
                require(
                    scope == (process::host_id(), process::boot_id()),
                    "native scope changed before first CPU settlement",
                )?;
                require(
                    process::group_presence(group) == process::ProcessPresence::Absent,
                    "CPU settlement preceded original cohort drain",
                )?;
                original_fifo_removed(&fifo, &pinned_fifo)?;
                if !first_settlement_observed {
                    require(
                        attempted == 1,
                        "owner skipped observable first-unit settlement turn",
                    )?;
                    publish(
                        root,
                        "first-settlement.json",
                        &aterm_json::to_vec(
                            &aterm_json::json!({"settled_units":progress.settled_units,"uncertain_units":progress.uncertain_units,"remaining_units":progress.remaining_units,"same_native_scope":true,"group_absent":true,"named_fifo_absent":true,"pinned_fifo_links":pinned_fifo.metadata()?.nlink(),"endpoint_cleaned":progress.endpoint_cleaned}),
                        )?,
                    )?;
                    first_settlement_observed = true;
                }
                require(
                    progress.uncertain_units == 0,
                    "native CPU settlement became uncertain",
                )?;
            }
            if owner.is_empty() {
                break;
            }
            require(
                Instant::now() < deadline,
                "original host epoch did not finish settlement",
            )?;
            owner.wait(deadline)?;
        }
        require(
            first_settlement_observed,
            "first CPU settlement was not observed",
        )?;
        require(
            scope == (process::host_id(), process::boot_id()),
            "native closure scope changed",
        )?;
        require(
            process::group_presence(group) == process::ProcessPresence::Absent,
            "original actual cohort did not drain",
        )?;
        original_fifo_removed(&fifo, &pinned_fifo)?;
        // The original claim guard conservatively keeps its memory admission
        // listed through individual CPU settlements. Only full settled-entry
        // retirement drops that guard and durably removes the original row;
        // ledger emptiness is therefore checked here, not at the first unit.
        require(
            ledger.list()?.is_empty(),
            "original memory admission remains after actual closure",
        )?;
        let status = owner.epoch_status(&id)?;
        require(
            status.endpoint_cleaned
                && status.settled_units == 2
                && status.remaining_units == 0
                && status.uncertain_units == 0,
            "exact original CPU settlement did not complete",
        )?;
        require(
            owner.epoch_root_outcome(&id, 0)? == Some(outcome),
            "settled history lost the actual root result",
        )?;
        let snapshot = publication(&owner)?;
        require(
            snapshot.generation == generation,
            "owner publication changed incarnation",
        )?;
        let completed = epoch_row(&owner, &id)?;
        require(
            completed.endpoint_cleaned
                && completed.backing == 2
                && completed.roots == 1
                && completed.reserved_bytes == admitted_bytes
                && completed.completed_roots == 1
                && completed.settled_units == 2
                && completed.remaining_units == 0
                && completed.uncertain_units == 0
                && completed.failed_roots == 0,
            "settled history projection is incorrect",
        )?;
        require(
            pool.snapshot()?
                .iter()
                .filter(|state| matches!(state, RetainedSlotState::Available { .. }))
                .count()
                == 2,
            "original pool did not restore exactly N",
        )?;
        let mut recovered = Vec::new();
        for _ in 0..2 {
            recovered.push(
                pool.try_acquire(CpuOwner::current(None))?
                    .ok_or_else(|| io::Error::other("restored CPU backing unavailable"))?,
            );
        }
        require(
            pool.try_acquire(CpuOwner::current(None))?.is_none(),
            "private pool grew a third credit",
        )?;
        for cap in recovered {
            cap.settle()?;
        }
        let cancellation = qualify_cancellation(root, helper, &mut owner, &seat, &ledger, &pool)?;
        require(
            owner.epoch_root_outcome(&id, 0)? == Some(outcome),
            "second epoch changed the original successful root history",
        )?;
        let mut artifact = spawnfd::command(target.join("debug/host_epoch_artifact"));
        let artifact = deadline::output_until(&mut artifact, Instant::now() + HANG)?;
        publish(root, "artifact.stdout", &artifact.stdout)?;
        publish(root, "artifact.stderr", &artifact.stderr)?;
        require(
            artifact.status.success() && artifact.stdout == b"host-epoch-artifact-ok\n",
            "actual emitted artifact did not execute successfully",
        )?;
        publish(root, "snapshot.json", &publication(&owner)?.encode()?)?;
        publish(
            root,
            "result.json",
            &aterm_json::to_vec(
                &aterm_json::json!({"scope":"private-native-host-epoch","passed":true,"epoch_sequence":id.sequence(),"backing":2,"roots":completed.roots,"admitted_bytes":admitted_bytes,"completed_roots":completed.completed_roots,"failed_roots":completed.failed_roots,"root_success":outcome.success,"root_code":outcome.code,"automatic_bind_contended":true,"automatic_bind_recovered":true,"original_helper":helper_identity.token(),"group_drained":true,"endpoint_cleaned":status.endpoint_cleaned,"settled_units":status.settled_units,"uncertain_units":status.uncertain_units,"first_settlement_observed":first_settlement_observed,"pinned_fifo_links":pinned_fifo.metadata()?.nlink(),"memory_admission_empty_after_entry_retirement":true,"memory_admission_check_stage":"after-full-entry-retirement","artifact_executed":true,"cancellation":cancellation}),
            )?,
        )?;
        Ok(())
    }

    pub fn run() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        let usage = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: epoch_owner_probe --targo /absolute/genuine/targo",
            )
        };
        require(
            args.next().as_deref() == Some(std::ffi::OsStr::new("--targo")),
            "expected explicit --targo path",
        )?;
        let helper = PathBuf::from(args.next().ok_or_else(usage)?);
        if !helper.is_absolute() || args.next().is_some() {
            return Err(usage().into());
        }
        let helper = fs::canonicalize(helper)?;
        let mut world = World::new()?;
        if let Err(error) = qualify(world.path(), &helper) {
            let _ = publish(world.path(), "failure.txt", error.to_string().as_bytes());
            return Err(error);
        }
        println!("host epoch probe PASS; evidence {}", world.keep().display());
        Ok(())
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = native::run() {
        eprintln!("epoch_owner_probe: {error}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("epoch_owner_probe requires the native macOS host adapter");
        std::process::exit(2);
    }
}

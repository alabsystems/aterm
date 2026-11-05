// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native qualification of real registered harness production through an explicit
//! private Owner. No installed selection, signed release or global supply changes.

#[cfg(all(debug_assertions, target_os = "macos"))]
mod native {
    use std::{
        collections::BTreeSet,
        ffi::OsStr,
        fs,
        io::{self, Read as _, Write as _},
        num::NonZeroU32,
        os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _},
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };

    use aterm_capacity::{
        Admit, Ledger, Machine, Pace, Procs, Reading,
        build_cpu::{Owner as CpuOwner, RetainedPool, RetainedSlotState},
        cache_work::{self, Availability, Collection},
        cohort_namespace::Namespace,
        epoch_controller::{Config, RootCancellation},
        epoch_recipe::{RootOutput, RootProfile, RootRecipe},
        epoch_target::{TargetLease, TargetLoan},
        leadership::Fence,
    };
    use aterm_uds::{deadline, process, spawnfd};
    use atpkg::{
        build_admit::{
            self, Admission, JobControl, Seat,
            service::{EpochId, Owner, monitor},
        },
        build_cache::Tool,
        local_harness::{self, LocalAuthority},
        store::Layout,
    };
    use atpkg_publish::{
        artifact::{
            build_local::{self, CacheFreshness, Plan},
            tools::{HostTools, Run, Tools as _},
        },
        ctx::Ctx,
        host::{Hosts, Slug, fake::FakeHost},
        testkit::harness::{BIN, PROGRAM, make_plan, project},
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const HANG: Duration = Duration::from_secs(60);

    fn require(condition: bool, reason: &str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(io::Error::other(reason).into())
        }
    }

    fn private_dir(path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(path)
    }

    fn publish(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        let temporary = root.join(format!(".{name}.pending"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        drop(file);
        fs::rename(temporary, root.join(name))
    }

    struct World(Option<aterm_tempfile::TempDir>);
    impl World {
        fn new() -> io::Result<Self> {
            Ok(Self(Some(aterm_tempfile::tempdir()?)))
        }
        fn path(&self) -> &Path {
            self.0.as_ref().unwrap().path()
        }
        fn keep(&mut self) -> PathBuf {
            self.0.take().unwrap().keep()
        }
    }
    impl Drop for World {
        fn drop(&mut self) {
            if let Some(world) = self.0.take() {
                let _ = fs::write(
                    world.path().join("cancel.release"),
                    b"owned fixture cleanup\n",
                );
                let _ = fs::write(
                    world.path().join("survivor.release"),
                    b"owned fixture cleanup\n",
                );
                let path = world.keep();
                eprintln!("kept unfinished native harness world: {}", path.display());
            }
        }
    }

    // Deliberately private admission input. This does not measure host resources
    // or claim that the account's actual global compilation supply was used.
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

    fn git(checkout: &Path, args: &[&str]) -> Result<String> {
        let mut command = atpkg_publish::testkit::git();
        command.arg("-C").arg(checkout).args(args);
        let output = deadline::output_until(&mut command, Instant::now() + HANG)?;
        require(
            output.status.success(),
            &format!(
                "fixture Git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        Ok(String::from_utf8(output.stdout)?
            .trim_end_matches('\n')
            .to_owned())
    }

    fn plan_target(plan: &Plan) -> Result<PathBuf> {
        Ok(PathBuf::from(
            plan.command
                .env_of("CARGO_TARGET_DIR")
                .ok_or_else(|| io::Error::other("original default plan target missing"))?,
        ))
    }

    fn dependency_artifact(logs: &Path, target: &Path) -> Result<PathBuf> {
        let mut artifacts = BTreeSet::new();
        for line in fs::read_to_string(logs.join("build.stdout"))?.lines() {
            let Ok(frame) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            if frame["reason"].as_str() != Some("compiler-artifact")
                || frame["target"]["name"].as_str() != Some("unchanged_probe")
            {
                continue;
            }
            let filenames = frame["filenames"]
                .as_array()
                .ok_or_else(|| io::Error::other("actual dependency artifact filenames missing"))?;
            for filename in filenames {
                let path = PathBuf::from(
                    filename
                        .as_str()
                        .ok_or_else(|| io::Error::other("dependency filename is not a string"))?,
                );
                if path.extension() == Some(OsStr::new("rlib")) {
                    require(
                        path.starts_with(target),
                        "actual dependency artifact left original default target",
                    )?;
                    artifacts.insert(path);
                }
            }
        }
        require(
            artifacts.len() == 1,
            "genuine Cargo output did not identify one actual unchanged dependency rlib",
        )?;
        Ok(artifacts.into_iter().next().unwrap())
    }

    fn tick(owner: &mut Owner) -> io::Result<()> {
        let jobs = owner.tick(&mut |_, _| Ok(JobControl::Wait), &mut |line| {
            eprintln!("{line}")
        })?;
        if jobs.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(
                "unexpected unrelated job in private Owner",
            ))
        }
    }

    fn until(
        owner: &mut Owner,
        mut ready: impl FnMut(&Owner) -> io::Result<bool>,
        files: bool,
    ) -> Result<()> {
        let deadline = Instant::now() + HANG;
        loop {
            tick(owner)?;
            if ready(owner)? {
                return Ok(());
            }
            require(
                Instant::now() < deadline,
                "native harness phase exceeded its hang detector",
            )?;
            // Files are deliberate fixture barriers, not host readiness data.
            let next = if files {
                deadline.min(Instant::now() + Duration::from_millis(10))
            } else {
                deadline
            };
            owner.wait(next)?;
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Artifact {
        identity: [u64; 2],
        size: u64,
        modified: [i64; 2],
        bytes: Vec<u8>,
    }
    fn artifact(path: &Path) -> io::Result<Artifact> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::other("compiler output is not a regular file"));
        }
        Ok(Artifact {
            identity: [metadata.dev(), metadata.ino()],
            size: metadata.len(),
            modified: [metadata.mtime(), metadata.mtime_nsec()],
            bytes: fs::read(path)?,
        })
    }

    struct Host {
        namespace: Namespace,
        ledger: Ledger,
        owner: Owner,
        pool: RetainedPool,
        machine: ProbeMachine,
        last_epoch: Option<EpochId>,
    }
    impl Host {
        fn new(root: &Path) -> Result<Self> {
            let capacity = root.join("capacity.noindex");
            private_dir(&capacity)?;
            let capacity_home = capacity.join("home");
            private_dir(&capacity_home)?;
            let namespace = Namespace::create_private(&capacity, &capacity_home)?;
            let ledger = Ledger::at(namespace.fence());
            let fence = Fence::acquire(&namespace, Instant::now() + HANG)?;
            let owner = Owner::fenced(fence, 2, 16)?;
            let pool = RetainedPool::create_private(
                &root.join("cpu.noindex"),
                NonZeroU32::new(2).unwrap(),
            )?;
            Ok(Self {
                namespace,
                ledger,
                owner,
                pool,
                machine: ProbeMachine,
                last_epoch: None,
            })
        }

        fn compile(
            &mut self,
            root: &Path,
            helper: &Path,
            phase: &str,
            plan: &Plan,
            recipe: RootRecipe,
            target_loan: Option<TargetLoan>,
        ) -> Result<(RootOutput, aterm_json::Value)> {
            require(self.owner.is_empty(), "previous private epoch still owned")?;
            let capabilities = (0..2)
                .map(|_| {
                    self.pool
                        .try_acquire(CpuOwner::current(None))?
                        .ok_or_else(|| io::Error::other("original private backing missing"))
                })
                .collect::<io::Result<Vec<_>>>()?;
            require(
                capabilities[0].slot() != capabilities[1].slot(),
                "original CPU units alias",
            )?;
            require(
                self.pool
                    .snapshot()?
                    .iter()
                    .all(|slot| matches!(slot, RetainedSlotState::Held { .. }))
                    && self.pool.try_acquire(CpuOwner::current(None))?.is_none(),
                "CPU backing not all held before helper grant",
            )?;
            let seat = Seat {
                ledger: Some(Ledger::at(self.namespace.fence())),
                machine: &self.machine,
                own: None,
                verify: None,
                mode: Admit::Wait,
                pace: Pace::default(),
                pid: std::process::id(),
                marker: None,
            };
            let class =
                build_admit::classify(&["--unverified".into(), "build".into()], Tool::Targo)
                    .ok_or_else(|| {
                        io::Error::other("real build admission classification failed")
                    })?;
            let held = match build_admit::admit_cohort_job(
                &class,
                &plan.command.cwd.clone().unwrap(),
                &seat,
                &mut |line| eprintln!("{line}"),
            ) {
                Admission::Claimed(held) => *held,
                other => {
                    return Err(io::Error::other(format!(
                        "private native admission refused: {other:?}"
                    ))
                    .into());
                }
            };
            let transport = root.join(format!("{phase}-transport.noindex"));
            let accepted = self.owner.submit_epoch_with_recipes(
                held,
                capabilities,
                Config {
                    helper: helper.to_owned(),
                    directory: transport.clone(),
                    epoch: aterm_uds::rand::hex_token::<16>()?,
                    roots: 1,
                },
                vec![recipe],
            )?;
            accepted.result?;
            let id = accepted.id;
            until(
                &mut self.owner,
                |owner| Ok(owner.epoch_status(&id)?.opened),
                false,
            )?;
            let helper_pid = self
                .owner
                .epoch_helper_id(&id)?
                .ok_or_else(|| io::Error::other("actual helper child missing"))?;
            let scope = (process::host_id(), process::boot_id());
            require(
                scope.0.is_some() && scope.1.is_some(),
                "native process scope unavailable",
            )?;
            let fifo = transport.join("credits.fifo");
            let pinned = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&fifo)?;
            let identity = pinned.metadata()?;
            let stdout_path = transport.join("root-0.stdout");
            let pinned_stdout = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&stdout_path)?;
            let original_stdout = pinned_stdout.metadata()?;
            let target = PathBuf::from(
                plan.command
                    .env_of("CARGO_TARGET_DIR")
                    .ok_or_else(|| io::Error::other("original target missing"))?,
            );
            let manifest = plan.command.cwd.as_ref().unwrap().join("Cargo.toml");
            if let Some(loan) = target_loan {
                self.owner
                    .epoch_spawn_root_with_target(&id, 0, &manifest, &target, loan)?;
            } else {
                self.owner.epoch_spawn_root(&id, 0, &manifest, &target)?;
            }
            let mut survivor_proof = None;
            if phase == "cancel" {
                until(
                    &mut self.owner,
                    |_| root.join("cancel.ready").try_exists(),
                    true,
                )?;
                let ready = fs::read_to_string(root.join("cancel.ready"))?;
                let pid: i32 = ready
                    .trim()
                    .strip_prefix("pid=")
                    .ok_or_else(|| io::Error::other("real active body identity missing"))?
                    .parse()?;
                let body = process::read_identity(pid)
                    .ok_or_else(|| io::Error::other("actual active body birth missing"))?;
                require(
                    self.owner.epoch_cancel_root(&id, 0)? == RootCancellation::Requested,
                    "cancel intent refused",
                )?;
                self.owner.epoch_close(&id)?;
                until(
                    &mut self.owner,
                    |owner| {
                        Ok(owner.epoch_root_cancellation(&id, 0)?
                            == Some(RootCancellation::Stopped))
                    },
                    false,
                )?;
                let nonce = aterm_uds::rand::hex_token::<16>()?;
                publish(root, "cancel.challenge", nonce.as_bytes())?;
                until(
                    &mut self.owner,
                    |_| root.join("cancel.alive").try_exists(),
                    true,
                )?;
                let alive = fs::read_to_string(root.join("cancel.alive"))?;
                require(
                    alive == format!("pid={pid}\nnonce={nonce}\n")
                        && process::read_identity(pid) == Some(body),
                    "original active body did not answer fresh post-ACK challenge",
                )?;
                let status = self.owner.epoch_status(&id)?;
                require(
                    status.settled_units == 0
                        && status.remaining_units == 2
                        && !status.retired
                        && pinned.metadata()?.nlink() == 1
                        && self.pool.try_acquire(CpuOwner::current(None))?.is_none(),
                    "active cancellation acknowledged closure or refunded backing",
                )?;
                require(
                    matches!(self.owner.epoch_take_root_output(&id, 0), Err(error) if error.kind()==io::ErrorKind::WouldBlock),
                    "active cancellation exposed consumable output or falsely reported absence",
                )?;
                publish(
                    root,
                    "cancel.release",
                    b"normal successful active body completion\n",
                )?;
            } else {
                self.owner.epoch_close(&id)?;
            }
            if phase == "survivor" {
                // Retired/EOF/helper exit close only owned roots. The same
                // original group still contains a live writer of this root's
                // actual stdout file, so neither output nor backing is final.
                until(
                    &mut self.owner,
                    |owner| {
                        let state = owner.epoch_status(&id)?;
                        Ok(owner.epoch_root_outcome(&id, 0)?.is_some()
                            && state.retired
                            && state.helper_exited
                            && state.control_eof)
                    },
                    false,
                )?;
                let ready = fs::read_to_string(root.join("survivor.ready"))?;
                let field = |name: &str| -> Result<u32> {
                    Ok(ready
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .ok_or_else(|| io::Error::other("surviving writer identity missing"))?
                        .parse()?)
                };
                let pid = i32::try_from(field("pid=")?)?;
                require(
                    field("pgid=")? == helper_pid
                        && process::group_presence(helper_pid) == process::ProcessPresence::Present,
                    "surviving actual writer left the original helper group",
                )?;
                let member = process::read_identity(pid)
                    .ok_or_else(|| io::Error::other("surviving writer native birth missing"))?;
                let nonce = aterm_uds::rand::hex_token::<16>()?;
                publish(root, "survivor.challenge", nonce.as_bytes())?;
                until(
                    &mut self.owner,
                    |_| root.join("survivor.alive").try_exists(),
                    true,
                )?;
                let alive = fs::read_to_string(root.join("survivor.alive"))?;
                require(
                    alive == format!("pid={pid}\npgid={helper_pid}\nnonce={nonce}\n")
                        && process::read_identity(pid) == Some(member)
                        && process::process_presence(pid) == process::ProcessPresence::Present
                        && scope == (process::host_id(), process::boot_id()),
                    "original same-group writer failed fresh challenge after actual root/helper exit",
                )?;
                let status = self.owner.epoch_status(&id)?;
                require(
                    status.settled_units == 0
                        && status.remaining_units == 2
                        && !status.endpoint_cleaned
                        && pinned.metadata()?.nlink() == 1
                        && fifo.try_exists()?
                        && self.pool.try_acquire(CpuOwner::current(None))?.is_none(),
                    "surviving actual log writer was mistaken for complete cohort closure",
                )?;
                require(
                    matches!(self.owner.epoch_take_root_output(&id,0),Err(error) if error.kind()==io::ErrorKind::WouldBlock),
                    "surviving actual writer exposed a falsely complete RootOutput",
                )?;
                let named = fs::symlink_metadata(&stdout_path)?;
                require(
                    [named.dev(), named.ino()] == [original_stdout.dev(), original_stdout.ino()]
                        && named.nlink() == 1,
                    "original root stdout changed before the controlled rename",
                )?;
                fs::rename(&stdout_path, transport.join("root-0.original-stdout"))?;
                let mut replacement = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&stdout_path)?;
                replacement
                    .write_all(b"unrelated replacement pathname must never be consumed\n")?;
                drop(replacement);
                require(
                    pinned_stdout.metadata()?.nlink() == 1
                        && fs::metadata(&stdout_path)?.ino() != original_stdout.ino()
                        && process::read_identity(pid) == Some(member)
                        && process::process_presence(pid) == process::ProcessPresence::Present,
                    "controlled stdout replacement did not preserve the pinned original and live original writer",
                )?;
                publish(
                    root,
                    "survivor.release",
                    b"normal original writer closure\n",
                )?;
                survivor_proof = Some(
                    aterm_json::json!({"member":member.token(),"nonce":nonce,"root_and_helper_exited_before_challenge":true,"retired_with_live_original_writer":true,"output_withheld_until_group_absent":true,"original_stdout":[original_stdout.dev(),original_stdout.ino()],"original_stdout_renamed_while_writer_live":true}),
                );
            }
            let deadline = Instant::now() + HANG;
            let mut first = false;
            loop {
                tick(&mut self.owner)?;
                let status = self.owner.epoch_status(&id)?;
                if status.settled_units > 0 {
                    require(
                        scope == (process::host_id(), process::boot_id())
                            && process::group_presence(helper_pid)
                                == process::ProcessPresence::Absent,
                        "CPU returned before actual original helper cohort disappeared",
                    )?;
                    require(
                        status.retired
                            && status.helper_exited
                            && status.control_eof
                            && status.endpoint_cleaned
                            && pinned.metadata()?.nlink() == 0
                            && !fifo.try_exists()?,
                        "CPU returned before real retirement/EOF/reap/node unlink",
                    )?;
                    if !first {
                        require(
                            status.settled_units == 1 && status.remaining_units == 1,
                            "first native settlement boundary skipped",
                        )?;
                        first = true;
                    }
                }
                require(
                    status.uncertain_units == 0,
                    "CPU settlement became uncertain",
                )?;
                if self.owner.is_empty() {
                    break;
                }
                require(
                    Instant::now() < deadline,
                    "native compiler cohort did not drain",
                )?;
                self.owner.wait(deadline)?;
            }
            let status = self.owner.epoch_status(&id)?;
            require(
                first
                    && status.settled_units == 2
                    && status.remaining_units == 0
                    && self.ledger.list()?.is_empty(),
                "original CPU or memory admission did not fully settle",
            )?;
            let outcome = self
                .owner
                .epoch_root_outcome(&id, 0)?
                .ok_or_else(|| io::Error::other("actual original root outcome missing"))?;
            let output = self
                .owner
                .epoch_take_root_output(&id, 0)?
                .ok_or_else(|| io::Error::other("settled original root output missing"))?;
            require(
                output.outcome() == outcome,
                "captured output belongs to a different root outcome",
            )?;
            require(
                matches!(TargetLease::acquire(&target),Err(error) if error.kind()==io::ErrorKind::WouldBlock),
                "another cooperative build acquired the target before original output consumption",
            )?;
            require(
                self.owner.epoch_take_root_output(&id, 0)?.is_none(),
                "original output was consumable twice",
            )?;
            let restored = (0..2)
                .map(|_| {
                    self.pool
                        .try_acquire(CpuOwner::current(None))?
                        .ok_or_else(|| io::Error::other("exact original CPU backing not restored"))
                })
                .collect::<io::Result<Vec<_>>>()?;
            require(
                self.pool.try_acquire(CpuOwner::current(None))?.is_none(),
                "native epoch minted a third CPU unit",
            )?;
            for capability in restored {
                capability.settle()?;
            }
            self.last_epoch = Some(id.clone());
            Ok((
                output,
                aterm_json::json!({"phase":phase,"epoch":id.sequence(),"root_code":outcome.code,"root_success":outcome.success,"backing":2,"roots":1,"published":1,"first_settlement_observed":first,"helper_pid":helper_pid,"original_fifo":[identity.dev(),identity.ino()],"pinned_links":pinned.metadata()?.nlink(),"settled_units":status.settled_units,"uncertain_units":status.uncertain_units,"group_absent":true,"private_memory_ledger_empty":true,"target_retained_until_original_output_consumed":true,"survivor":survivor_proof}),
            ))
        }

        fn observe(&self) -> Result<monitor::Observation> {
            let until = Instant::now() + HANG;
            let (client, mut exchange) = self.owner.monitor_source()?.pair(until)?;
            std::thread::scope(|scope| {
                let server = scope.spawn(move || -> io::Result<()> {
                    while exchange.step()? == monitor::Progress::Pending {
                        exchange.wait(until)?;
                    }
                    Ok(())
                });
                let observation = client.read_until(until);
                server
                    .join()
                    .map_err(|_| io::Error::other("actual monitor worker panicked"))??;
                observation.map_err(Into::into)
            })
        }

        fn record_work(&mut self, collection: &Collection) -> Result<aterm_json::Value> {
            let id = self
                .last_epoch
                .as_ref()
                .ok_or_else(|| io::Error::other("original epoch ticket missing"))?;
            let before = self.observe()?;
            self.owner.epoch_record_cache_work(id, 0, collection)?;
            let observed = self.observe()?;
            require(
                observed.snapshot.generation == before.snapshot.generation
                    && observed.snapshot.revision > before.snapshot.revision,
                "cache-work publication changed generation or failed to advance revision",
            )?;
            let epoch = observed
                .snapshot
                .epochs
                .iter()
                .find(|epoch| epoch.sequence == id.sequence())
                .ok_or_else(|| io::Error::other("original completed epoch absent from monitor"))?;
            require(
                epoch.cache_work.len() == 1 && epoch.cache_work[0].root == 0,
                "monitor lost original root cost attribution",
            )?;
            let row = &epoch.cache_work[0];
            match collection {
                Collection::Report(report) => {
                    let summary = report.summary();
                    require(
                        row.summary.as_ref() == Some(&summary) && row.unavailable.is_none(),
                        "monitor changed validated compiler cost observations",
                    )?;
                    Ok(
                        aterm_json::json!({"generation_preserved":true,"epoch":id.sequence(),"root":0,"run_id":summary.run_id,"complete":summary.availability==Availability::Complete,"registered_units":summary.denominators.registered_units,"captures":summary.denominators.attempts_finished,"same_generation_monitor_observed":true}),
                    )
                }
                Collection::Unavailable(reason) => {
                    require(
                        row.summary.is_none() && row.unavailable == Some(*reason),
                        "monitor converted unavailable costs to measured zeros",
                    )?;
                    Ok(
                        aterm_json::json!({"generation_preserved":true,"epoch":id.sequence(),"root":0,"unavailable":format!("{reason:?}"),"same_generation_monitor_observed":true}),
                    )
                }
            }
        }
    }

    fn execute(root: &Path, label: &str, path: &Path, version: &str) -> Result<()> {
        execute_runtime(root, label, path, version, "native-harness-ok")
    }

    fn execute_runtime(
        root: &Path,
        label: &str,
        path: &Path,
        version: &str,
        runtime: &str,
    ) -> Result<()> {
        for (suffix, args, wanted) in [
            (
                "version",
                vec!["--version"],
                format!("{PROGRAM} {version}\n"),
            ),
            ("program", Vec::new(), format!("{runtime} {version}\n")),
        ] {
            let mut run = Run::new(path, args).bounded();
            run.clear_env = true;
            run.env.push(("HOME".into(), root.to_owned().into()));
            let output = HostTools
                .run(&run)
                .map_err(|error| io::Error::other(error.to_string()))?;
            publish(root, &format!("{label}-{suffix}.stdout"), &output.stdout)?;
            publish(root, &format!("{label}-{suffix}.stderr"), &output.stderr)?;
            require(
                output.success() && output.stdout == wanted.as_bytes(),
                "actual emitted/installed harness did not report its feature/version proof",
            )?;
        }
        Ok(())
    }

    fn execute_data(root: &Path, label: &str, path: &Path, expected: u32) -> Result<()> {
        let mut run = Run::new(path, ["--data"]).bounded();
        run.clear_env = true;
        run.env.push(("HOME".into(), root.to_owned().into()));
        let output = HostTools
            .run(&run)
            .map_err(|error| io::Error::other(error.to_string()))?;
        publish(root, &format!("{label}-data.stdout"), &output.stdout)?;
        publish(root, &format!("{label}-data.stderr"), &output.stderr)?;
        require(
            output.success() && output.stdout == format!("data={expected}\n").as_bytes(),
            "actual harness executed stale generated data",
        )
    }

    fn build_count(root: &Path, name: &str) -> Result<u64> {
        Ok(fs::read_to_string(root.join(name))?.trim().parse()?)
    }

    fn qualify(root: &Path, sdk: &Path, helper: &Path, require_cache_work: bool) -> Result<()> {
        let mut genuine = spawnfd::command(helper);
        genuine.arg0("targo").args(["--unverified", "--version"]);
        let genuine = deadline::output_until(&mut genuine, Instant::now() + HANG)?;
        require(
            genuine.status.success() && genuine.stdout.starts_with(b"targo "),
            "selected helper is not genuine Targo",
        )?;
        publish(root, "helper.version", &genuine.stdout)?;
        private_dir(&root.join("output.noindex"))?;
        let host = Arc::new(FakeHost::new());
        let ctx = Ctx::new(
            root,
            None,
            None,
            aterm_time::unix_secs()
                .ok_or_else(|| io::Error::other("fixture clock unavailable"))?
                .try_into()?,
            Hosts::single(host.clone()),
        );
        let mut capacity = Host::new(root)?;
        let checkout = project(root, &ctx, &host, sdk, "positive")?;
        let (request, plan) = make_plan(root, &ctx, &checkout, sdk)?;
        // Qualify the actual producer cache key, including across a committed
        // root edit; a fixture-selected target would conceal a plan defect.
        let target = plan_target(&plan)?;
        let driver_supports_content =
            matches!(plan.cache_freshness, CacheFreshness::ContentInputsV1);
        let usable_checkout_birthtime = match fs::symlink_metadata(&checkout)?.created() {
            Ok(created) => created
                .duration_since(std::time::UNIX_EPOCH)
                .is_ok_and(|age| !age.is_zero()),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => false,
            Err(error) => return Err(error.into()),
        };
        let can_reuse_target = driver_supports_content && usable_checkout_birthtime;
        let freshness = match plan.cache_freshness {
            CacheFreshness::ExactSource => "exact-source",
            CacheFreshness::ContentInputsV1 => "rust-and-build-input-content-v1",
        };
        let mut prepared = build_local::prepare_harness(&ctx, &request, &plan)?;
        require(
            !require_cache_work || plan.analysis_session,
            "original immutable driver lacks genuine analysis-session capability",
        )?;
        let warm_collector = prepared.cache_work_collector(0)?;
        require(
            target.is_dir(),
            "production preparation did not establish its real target ancestry",
        )?;
        let recipe = prepared.recipe(0)?;
        let warm_recipe = prepared.recipe(0)?;
        let version = plan.harness.as_ref().unwrap().version.clone();
        let arguments = plan.command.argv();
        let content_config = arguments
            .windows(2)
            .any(|pair| pair == ["--config", "build.fingerprint=\"content\""]);
        let checksum_flag = arguments.iter().any(|arg| arg == "-Zchecksum-freshness");
        require(
            matches!(recipe.profile, RootProfile::Dev)
                && recipe.bin == BIN
                && recipe.jobs == 2
                && recipe.features == ["probe-flag"]
                && recipe.offline
                && recipe.version == version
                && recipe.content_inputs_v1 == driver_supports_content
                && content_config == driver_supports_content
                && checksum_flag == driver_supports_content,
            "production recipe lost native profile/bin/jobs/features/version/offline intent",
        )?;
        let (output, positive) = capacity.compile(
            root,
            helper,
            "positive",
            &plan,
            recipe,
            Some(prepared.target_loan()?),
        )?;
        require(
            output.outcome().success && output.cancellation().is_none(),
            "genuine successful root was ineligible",
        )?;
        let completion = prepared.finish_root_output_with_cache_work(&ctx, &HostTools, output)?;
        let positive_cache_work = capacity.record_work(&completion.cache_work)?;
        require(
            !require_cache_work || positive_cache_work["complete"].as_bool() == Some(true),
            "genuine positive build did not produce complete exact-session costs",
        )?;
        let built = completion.built?;
        drop(TargetLease::acquire(&target)?);
        let receipt = fs::read(built.dest.join("local-build.json"))?;
        let record: aterm_json::Value = aterm_json::from_slice(&receipt)?;
        require(
            record["schema"].as_u64() == Some(2)
                && record["program"].as_str() == Some(PROGRAM)
                && record["command"].as_str() == Some(BIN)
                && record["profile"].as_str() == Some("dev")
                && record["version"].as_str() == Some(version.as_str())
                && record["source"].as_str() == Some(plan.source.as_str())
                && record["compiler"].as_str() == Some(plan.compiler.as_str())
                && record["target"].as_str() == Some(plan.triple.as_str()),
            "actual complete schema2 receipt lost original production identity",
        )?;
        execute(
            root,
            "published",
            &built.dest.join("bin").join(BIN),
            &version,
        )?;
        execute_data(root, "published", &built.dest.join("bin").join(BIN), 21)?;
        let layout = Layout {
            prefix: root.join("private-store"),
        };
        let authority = LocalAuthority::new(PROGRAM, BIN, &plan.source, plan.triple.as_str())?;
        let staged = {
            let lock = atpkg::lock::try_lock_store(&layout)?;
            local_harness::stage(&lock, &authority, &built.dest)?
        };
        require(
            staged.identity().source == plan.source && staged.identity().version == version,
            "installer admission changed actual source/version",
        )?;
        execute(
            root,
            "installed",
            &staged.directory().join("bin").join(BIN),
            &version,
        )?;
        execute_data(
            root,
            "installed",
            &staged.directory().join("bin").join(BIN),
            21,
        )?;
        let compiler_output = target.join("debug").join(BIN);
        execute(root, "target-before-warm", &compiler_output, &version)?;
        let original_target = artifact(&compiler_output)?;
        let original_artifact = artifact(&built.dest.join("bin").join(BIN))?;
        let dependency = dependency_artifact(&built.logs, &target)?;
        let original_dependency = artifact(&dependency)?;
        require(
            fs::read(root.join("positive-dependency.build-count"))? == b"1\n",
            "first genuine dependency build did not execute exactly once",
        )?;
        require(
            build_count(root, "positive-data.build-count")? == 1,
            "first declared-data dependency build did not execute once",
        )?;
        require(
            fs::read(root.join("positive.build-count"))? == b"1\n",
            "first genuine build did not execute exactly once",
        )?;
        // The immutable artifact still exists. A second prepare must preserve
        // its refusal; warm execution is a fresh transport of the same recipe.
        require(
            build_local::prepare_harness(&ctx, &request, &plan).is_err(),
            "completed immutable artifact was accepted for replacement",
        )?;
        let (warm, warm_receipt) =
            capacity.compile(root, helper, "warm", &plan, warm_recipe, None)?;
        let warm = warm.read(64 << 20)?;
        let warm_logs = root.join("warm-cache-work.noindex");
        private_dir(&warm_logs)?;
        let warm_costs = warm_collector.collect(&HostTools, &warm, &warm_logs);
        let warm_cache_work = capacity.record_work(&warm_costs)?;
        require(
            !require_cache_work || warm_cache_work["complete"].as_bool() == Some(true),
            "genuine warm build did not produce complete exact-session costs",
        )?;
        require(
            matches!(TargetLease::acquire(&target),Err(error) if error.kind()==io::ErrorKind::WouldBlock),
            "reading original warm output released its target before output bytes were consumed",
        )?;
        publish(root, "warm.stdout", &warm.stdout)?;
        publish(root, "warm.stderr", &warm.stderr)?;
        require(
            warm.outcome.success && warm.cancellation.is_none(),
            "warm original root did not succeed",
        )?;
        require(
            fs::read(root.join("positive.build-count"))? == b"1\n"
                && artifact(&compiler_output)? == original_target,
            "fresh transport reran build.rs or rewrote the same compiler output",
        )?;
        require(
            fs::read(built.dest.join("local-build.json"))? == receipt
                && artifact(&built.dest.join("bin").join(BIN))? == original_artifact,
            "warm qualification changed immutable first publication",
        )?;
        execute(root, "target-after-warm", &compiler_output, &version)?;
        drop(warm);
        drop(warm_collector);
        drop(TargetLease::acquire(&target)?);
        let main_path = checkout.join("src/main.rs");
        let previous_main = fs::read_to_string(&main_path)?;
        require(
            previous_main.matches("native-harness-ok").count() == 1,
            "root edit no longer identifies one actual runtime behavior",
        )?;
        fs::write(
            &main_path,
            previous_main.replace("native-harness-ok", "native-harness-edited"),
        )?;
        git(&checkout, &["add", "src/main.rs"])?;
        git(
            &checkout,
            &[
                "commit",
                "-m",
                "change actual root runtime while preserving dependency inputs",
            ],
        )?;
        git(&checkout, &["push", "origin", "main"])?;
        let bare = PathBuf::from(git(&checkout, &["remote", "get-url", "origin"])?);
        host.add_clone_source(&Slug::new(&ctx.accounts.staging, PROGRAM), &bare);
        let (edited_request, edited_plan) = make_plan(root, &ctx, &checkout, sdk)?;
        let edited_version = edited_plan.harness.as_ref().unwrap().version.clone();
        let edited_target = plan_target(&edited_plan)?;
        require(
            (edited_target == target) == can_reuse_target
                && matches!(edited_plan.cache_freshness, CacheFreshness::ContentInputsV1)
                    == driver_supports_content
                && edited_plan.source != plan.source
                && edited_version != version,
            "committed root edit contradicted the selected freshness mode or failed to change artifact identity",
        )?;
        let mut edited_prepared =
            build_local::prepare_harness(&ctx, &edited_request, &edited_plan)?;
        let edited_recipe = edited_prepared.recipe(0)?;
        let (edited_output, edited_epoch) = capacity.compile(
            root,
            helper,
            "edited",
            &edited_plan,
            edited_recipe,
            Some(edited_prepared.target_loan()?),
        )?;
        require(
            edited_output.outcome().success && edited_output.cancellation().is_none(),
            "actual edited root failed or was cancelled",
        )?;
        let edited_completion =
            edited_prepared.finish_root_output_with_cache_work(&ctx, &HostTools, edited_output)?;
        let edited_cache_work = capacity.record_work(&edited_completion.cache_work)?;
        let edited_built = edited_completion.built?;
        require(
            edited_built.dest != built.dest,
            "new source replaced its original immutable artifact address",
        )?;
        let edited_receipt = fs::read(edited_built.dest.join("local-build.json"))?;
        let edited_record: aterm_json::Value = aterm_json::from_slice(&edited_receipt)?;
        require(
            edited_record["source"].as_str() == Some(edited_plan.source.as_str())
                && edited_record["version"].as_str() == Some(edited_version.as_str())
                && edited_record["build_number"].as_u64() == Some(2),
            "edited artifact lost the second actual clean source/version",
        )?;
        let expected_after_root = if can_reuse_target { 1 } else { 2 };
        require(
            build_count(root, "positive.build-count")? == 2
                && build_count(root, "positive-dependency.build-count")? == expected_after_root
                && build_count(root, "positive-data.build-count")? == expected_after_root,
            "root edit build counts contradicted selected freshness mode",
        )?;
        let edited_dependency = dependency_artifact(&edited_built.logs, &edited_target)?;
        require(
            (edited_dependency == dependency) == can_reuse_target
                && artifact(&dependency)? == original_dependency,
            "root edit dependency address or preserved bytes contradicted selected freshness mode",
        )?;
        if can_reuse_target {
            require(
                artifact(&edited_dependency)? == original_dependency,
                "content mode rewrote unchanged dependency after root edit",
            )?;
        }
        require(
            fs::read(built.dest.join("local-build.json"))? == receipt
                && artifact(&built.dest.join("bin").join(BIN))? == original_artifact,
            "edited root changed its first immutable publication",
        )?;
        execute_runtime(
            root,
            "edited-published",
            &edited_built.dest.join("bin").join(BIN),
            &edited_version,
            "native-harness-edited",
        )?;
        execute_data(
            root,
            "edited-published",
            &edited_built.dest.join("bin").join(BIN),
            21,
        )?;
        let edited_authority = LocalAuthority::new(
            PROGRAM,
            BIN,
            &edited_plan.source,
            edited_plan.triple.as_str(),
        )?;
        let edited_stage = {
            let lock = atpkg::lock::try_lock_store(&layout)?;
            local_harness::stage(&lock, &edited_authority, &edited_built.dest)?
        };
        require(
            edited_stage.identity().source == edited_plan.source
                && edited_stage.identity().version == edited_version,
            "installer changed the edited artifact's actual identity",
        )?;
        execute_runtime(
            root,
            "edited-installed",
            &edited_stage.directory().join("bin").join(BIN),
            &edited_version,
            "native-harness-edited",
        )?;
        execute_data(
            root,
            "edited-installed",
            &edited_stage.directory().join("bin").join(BIN),
            21,
        )?;
        execute(
            root,
            "original-after-edit",
            &built.dest.join("bin").join(BIN),
            &version,
        )?;
        execute(
            root,
            "original-installed-after-edit",
            &staged.directory().join("bin").join(BIN),
            &version,
        )?;
        let original_edited_artifact = artifact(&edited_built.dest.join("bin").join(BIN))?;
        // The third real source commit changes a declared build-script input
        // without changing its length or mtime. Root version recompilation must
        // not conceal stale generated behavior in this separate dependency.
        let data_path = checkout.join("data-probe/value.txt");
        let data_before = fs::metadata(&data_path)?;
        let original_data_mtime = data_before.modified()?;
        require(
            fs::read(&data_path)? == b"21\n",
            "original declared data fixture changed",
        )?;
        let mut data_file = fs::OpenOptions::new().write(true).open(&data_path)?;
        data_file.write_all(b"22\n")?;
        data_file.set_times(fs::FileTimes::new().set_modified(original_data_mtime))?;
        drop(data_file);
        git(&checkout, &["add", "data-probe/value.txt"])?;
        git(
            &checkout,
            &[
                "commit",
                "-m",
                "change same-size declared build input while preserving its mtime",
            ],
        )?;
        git(&checkout, &["push", "origin", "main"])?;
        host.add_clone_source(&Slug::new(&ctx.accounts.staging, PROGRAM), &bare);
        let data_after = fs::metadata(&data_path)?;
        require(
            data_after.len() == data_before.len()
                && data_after.modified()? == original_data_mtime
                && fs::read(&data_path)? == b"22\n"
                && git(&checkout, &["show", "HEAD:data-probe/value.txt"])? == "22"
                && git(&bare, &["show", "main:data-probe/value.txt"])? == "22",
            "same-size declared input mtime/bytes were not preserved through actual commit and push",
        )?;
        let (data_request, data_plan) = make_plan(root, &ctx, &checkout, sdk)?;
        let data_target = plan_target(&data_plan)?;
        let data_version = data_plan.harness.as_ref().unwrap().version.clone();
        require(
            matches!(data_plan.cache_freshness, CacheFreshness::ContentInputsV1)
                == driver_supports_content
                && (if can_reuse_target {
                    data_target == target
                } else {
                    data_target != target && data_target != edited_target
                })
                && data_plan.source != edited_plan.source
                && data_version != edited_version,
            "third actual commit contradicted freshness mode or source/version identity",
        )?;
        let mut data_prepared = build_local::prepare_harness(&ctx, &data_request, &data_plan)?;
        let data_recipe = data_prepared.recipe(0)?;
        let (data_output, data_epoch) = capacity.compile(
            root,
            helper,
            "declared-data",
            &data_plan,
            data_recipe,
            Some(data_prepared.target_loan()?),
        )?;
        require(
            data_output.outcome().success && data_output.cancellation().is_none(),
            "actual declared-input root did not succeed",
        )?;
        let data_completion =
            data_prepared.finish_root_output_with_cache_work(&ctx, &HostTools, data_output)?;
        let data_cache_work = capacity.record_work(&data_completion.cache_work)?;
        require(
            !require_cache_work || data_cache_work["complete"].as_bool() == Some(true),
            "actual declared-data edit did not produce complete exact-session costs",
        )?;
        let data_built = data_completion.built?;
        let data_receipt = fs::read(data_built.dest.join("local-build.json"))?;
        let data_record: aterm_json::Value = aterm_json::from_slice(&data_receipt)?;
        require(
            data_built.dest != built.dest
                && data_built.dest != edited_built.dest
                && data_record["source"].as_str() == Some(data_plan.source.as_str())
                && data_record["version"].as_str() == Some(data_version.as_str())
                && data_record["build_number"].as_u64() == Some(3),
            "declared-input artifact lost third immutable source/version identity",
        )?;
        let unrelated_count_after_data = if can_reuse_target { 1 } else { 3 };
        let affected_count_after_data = if can_reuse_target { 2 } else { 3 };
        require(
            build_count(root, "positive.build-count")? == 3
                && build_count(root, "positive-dependency.build-count")?
                    == unrelated_count_after_data
                && build_count(root, "positive-data.build-count")? == affected_count_after_data,
            "declared-input edit did not rebuild exactly the inputs required by selected freshness mode",
        )?;
        let data_dependency = dependency_artifact(&data_built.logs, &data_target)?;
        require(
            (data_dependency == dependency) == can_reuse_target
                && artifact(&dependency)? == original_dependency,
            "declared data edit contradicted unrelated dependency address or preserved bytes",
        )?;
        if can_reuse_target {
            require(
                artifact(&data_dependency)? == original_dependency,
                "declared data edit rewrote the unrelated dependency",
            )?;
        }
        execute_runtime(
            root,
            "data-published",
            &data_built.dest.join("bin").join(BIN),
            &data_version,
            "native-harness-edited",
        )?;
        execute_data(
            root,
            "data-published",
            &data_built.dest.join("bin").join(BIN),
            22,
        )?;
        let data_authority =
            LocalAuthority::new(PROGRAM, BIN, &data_plan.source, data_plan.triple.as_str())?;
        let data_stage = {
            let lock = atpkg::lock::try_lock_store(&layout)?;
            local_harness::stage(&lock, &data_authority, &data_built.dest)?
        };
        execute_runtime(
            root,
            "data-installed",
            &data_stage.directory().join("bin").join(BIN),
            &data_version,
            "native-harness-edited",
        )?;
        execute_data(
            root,
            "data-installed",
            &data_stage.directory().join("bin").join(BIN),
            22,
        )?;
        execute_data(
            root,
            "original-after-data",
            &built.dest.join("bin").join(BIN),
            21,
        )?;
        execute_data(
            root,
            "edited-after-data",
            &edited_built.dest.join("bin").join(BIN),
            21,
        )?;
        let original_data_artifact = artifact(&data_built.dest.join("bin").join(BIN))?;
        let survivor_checkout = project(root, &ctx, &host, sdk, "survivor")?;
        let (survivor_request, survivor_plan) = make_plan(root, &ctx, &survivor_checkout, sdk)?;
        require(
            plan_target(&survivor_plan)? != target,
            "an independent survivor checkout shared the original checkout cache key",
        )?;
        let mut survivor_prepared =
            build_local::prepare_harness(&ctx, &survivor_request, &survivor_plan)?;
        let survivor_recipe = survivor_prepared.recipe(0)?;
        let (survivor_output, mut survivor) = capacity.compile(
            root,
            helper,
            "survivor",
            &survivor_plan,
            survivor_recipe,
            Some(survivor_prepared.target_loan()?),
        )?;
        require(
            survivor_output.outcome().success && survivor_output.cancellation().is_none(),
            "original survivor root did not preserve successful outcome",
        )?;
        let survivor_completion = survivor_prepared.finish_root_output_with_cache_work(
            &ctx,
            &HostTools,
            survivor_output,
        )?;
        let survivor_cache_work = capacity.record_work(&survivor_completion.cache_work)?;
        survivor
            .as_object_mut()
            .ok_or_else(|| io::Error::other("survivor proof was not an object"))?
            .insert("cache_work".into(), survivor_cache_work);
        let survivor_built = survivor_completion.built?;
        let survivor_version = &survivor_plan.harness.as_ref().unwrap().version;
        let captured = fs::read_to_string(survivor_built.logs.join("build.stdout"))?;
        let nonce = fs::read_to_string(root.join("survivor.challenge"))?;
        require(
            captured.lines().any(|line| {
                aterm_json::from_str::<aterm_json::Value>(line)
                    .is_ok_and(|frame| frame["reason"].as_str() == Some("compiler-artifact"))
            }) && captured.lines().any(|line| {
                aterm_json::from_str::<aterm_json::Value>(line).is_ok_and(|frame| {
                    frame["reason"].as_str() == Some("harness-probe-survivor")
                        && frame["nonce"].as_str() == Some(nonce.as_str())
                })
            }) && !captured.contains("unrelated replacement pathname"),
            "one-use original RootOutput did not retain compiler JSON and the late original writer pulse",
        )?;
        execute(
            root,
            "survivor-published",
            &survivor_built.dest.join("bin").join(BIN),
            survivor_version,
        )?;
        let survivor_authority = LocalAuthority::new(
            PROGRAM,
            BIN,
            &survivor_plan.source,
            survivor_plan.triple.as_str(),
        )?;
        let survivor_stage = {
            let lock = atpkg::lock::try_lock_store(&layout)?;
            local_harness::stage(&lock, &survivor_authority, &survivor_built.dest)?
        };
        execute(
            root,
            "survivor-installed",
            &survivor_stage.directory().join("bin").join(BIN),
            survivor_version,
        )?;
        require(
            fs::read(root.join("survivor.done"))? == b"normal writer closure\n"
                && fs::read(built.dest.join("local-build.json"))? == receipt
                && artifact(&built.dest.join("bin").join(BIN))? == original_artifact,
            "surviving writer did not close normally or changed first immutable artifact",
        )?;
        let mut negatives = Vec::new();
        for phase in ["failure", "cancel"] {
            let checkout = project(root, &ctx, &host, sdk, phase)?;
            let (request, mut plan) = make_plan(root, &ctx, &checkout, sdk)?;
            require(
                plan_target(&plan)? != target,
                "an independent negative checkout shared the original checkout cache key",
            )?;
            // Deliberately reuse an existing executable for this adversarial
            // refusal control. This is separate from the genuine default-key
            // reuse proof across commits in the one positive checkout above.
            plan.command
                .env
                .push(("CARGO_TARGET_DIR".into(), target.clone().into()));
            let mut prepared = build_local::prepare_harness(&ctx, &request, &plan)?;
            let destination = build_local::harness_artifact_path(&request, &plan)?;
            let recipe = prepared.recipe(0)?;
            let (output, mut proof) = capacity.compile(
                root,
                helper,
                phase,
                &plan,
                recipe,
                Some(prepared.target_loan()?),
            )?;
            require(
                !output.outcome().success && output.outcome().code == Some(101),
                "negative actual root did not preserve non-success",
            )?;
            require(
                (phase == "cancel") == output.cancellation().is_some(),
                "failed root and actual cancelled root became indistinguishable",
            )?;
            let completion =
                prepared.finish_root_output_with_cache_work(&ctx, &HostTools, output)?;
            let cache_work = capacity.record_work(&completion.cache_work)?;
            require(
                completion.built.is_err(),
                "failed/cancelled original output became consumable artifact",
            )?;
            require(
                !destination.join("local-build.json").try_exists()?,
                "failed/cancelled root committed a complete receipt",
            )?;
            let authority = LocalAuthority::new(PROGRAM, BIN, &plan.source, plan.triple.as_str())?;
            let lock = atpkg::lock::try_lock_store(&layout)?;
            require(
                local_harness::stage(&lock, &authority, &destination).is_err(),
                "installer admitted failed/cancelled artifact",
            )?;
            drop(lock);
            require(
                fs::read(built.dest.join("local-build.json"))? == receipt
                    && artifact(&built.dest.join("bin").join(BIN))? == original_artifact,
                "negative root changed original immutable publication",
            )?;
            let fields = proof
                .as_object_mut()
                .ok_or_else(|| io::Error::other("native epoch proof is not an object"))?;
            fields.insert("cache_work".into(), cache_work);
            fields.insert("complete_receipt_absent".into(), true.into());
            fields.insert("installer_refused".into(), true.into());
            negatives.push(proof);
        }
        require(
            root.join("cancel.done").is_file(),
            "cancelled active body was not normally released",
        )?;
        require(
            fs::read(edited_built.dest.join("local-build.json"))? == edited_receipt
                && artifact(&edited_built.dest.join("bin").join(BIN))? == original_edited_artifact,
            "later survivor/failure/cancellation changed the edited immutable publication",
        )?;
        require(
            fs::read(data_built.dest.join("local-build.json"))? == data_receipt
                && artifact(&data_built.dest.join("bin").join(BIN))? == original_data_artifact,
            "later survivor/failure/cancellation changed generated-data immutable publication",
        )?;
        publish(
            root,
            "cache-work-monitor.json",
            &aterm_json::to_vec(
                &aterm_json::json!({"required":require_cache_work,"original_driver_capability":plan.analysis_session,"positive":positive_cache_work,"warm":warm_cache_work,"edited":edited_cache_work,"declared_data":data_cache_work}),
            )?,
        )?;
        publish(
            root,
            "result.json",
            &aterm_json::to_vec(
                &aterm_json::json!({"scope":"private-native-owner-harness-production","passed":true,"compiler":plan.compiler,"source":plan.source,"profile":"dev","bin":BIN,"features":["probe-flag"],"version":version,"cache_freshness":freshness,"driver_supports_content":driver_supports_content,"usable_checkout_birthtime":usable_checkout_birthtime,"can_reuse_target":can_reuse_target,"schema2_receipt":true,"checkout_config_required_to_compile":true,"actual_published_and_installed_execution":true,"warm_build_count":1,"warm_exact_target_unchanged":true,"immutable_first_publication_unchanged":true,"positive":positive,"warm":warm_receipt,"edited_source":edited_plan.source,"edited_version":edited_version,"after_edit_root_build_count":2,"after_data_root_build_count":3,"unchanged_dependency_build_count":unrelated_count_after_data,"unchanged_dependency_reused_after_root_and_data_edits":can_reuse_target,"original_dependency_artifact":dependency,"original_dependency_dev_ino":original_dependency.identity,"reused_dependency_exact_bytes_and_mtime":can_reuse_target.then_some(true),"data_source":data_plan.source,"data_version":data_version,"data_dependency_build_count":affected_count_after_data,"declared_input_same_size_and_mtime_after_commit_push":true,"generated_runtime_before":21,"generated_runtime_after":22,"all_three_immutable_artifacts_executed":true,"default_plan_target":target,"edited_plan_target":edited_target,"data_plan_target":data_target,"edited_epoch":edited_epoch,"data_epoch":data_epoch,"surviving_writer":survivor,"original_output_nonce_pulse_captured":true,"negative":negatives}),
            )?,
        )?;
        Ok(())
    }

    pub fn run() -> Result<()> {
        let mut arguments = std::env::args_os().skip(1);
        let first = arguments.next();
        if first.as_deref() == Some(OsStr::new("--cache-work-report")) {
            let path = PathBuf::from(
                arguments
                    .next()
                    .ok_or_else(|| io::Error::other("report path missing"))?,
            );
            let flag = arguments.next();
            require(
                flag.as_deref() == Some(OsStr::new("--run-id")),
                "expected --run-id",
            )?;
            let run = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| io::Error::other("run ID missing"))?;
            require(
                arguments.next().is_none() && path.is_absolute(),
                "unexpected report probe arguments",
            )?;
            let mut bytes = Vec::new();
            fs::File::open(&path)?
                .take(cache_work::MAX_REPORT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            let report = cache_work::parse(&bytes, &run)?;
            let summary = report.summary();
            require(
                summary.availability == Availability::Complete
                    && summary.denominators.registered_units > 0
                    && summary.denominators.attempts_finished > 0,
                "actual report lacks a complete nonzero denominator",
            )?;
            println!(
                "{}",
                aterm_json::json!({"scope":"actual-saved-cache-work-report","bytes":bytes.len(),"run_id":run,"units":report.units().len(),"captures":summary.denominators.attempts_finished,"complete":true})
            );
            return Ok(());
        }
        let usage = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: harness_owner_probe --sdk-bin /absolute/immutable/sdk/bin --targo /absolute/qualified/targo [--require-cache-work], or --cache-work-report /absolute/report.json --run-id ID",
            )
        };
        if first.as_deref() != Some(OsStr::new("--sdk-bin")) {
            return Err(usage().into());
        }
        let sdk = PathBuf::from(arguments.next().ok_or_else(usage)?);
        if arguments.next().as_deref() != Some(OsStr::new("--targo")) {
            return Err(usage().into());
        }
        let helper = PathBuf::from(arguments.next().ok_or_else(usage)?);
        let require_cache_work = match arguments.next() {
            None => false,
            Some(flag) if flag == OsStr::new("--require-cache-work") => true,
            _ => return Err(usage().into()),
        };
        if !sdk.is_absolute() || !helper.is_absolute() || arguments.next().is_some() {
            return Err(usage().into());
        }
        let sdk = fs::canonicalize(sdk)?;
        let helper = fs::canonicalize(helper)?;
        let mut world = World::new()?;
        let root = world.path().canonicalize()?;
        if let Err(error) = qualify(&root, &sdk, &helper, require_cache_work) {
            let _ = publish(&root, "failure.txt", error.to_string().as_bytes());
            return Err(error);
        }
        println!(
            "native Owner harness probe PASS; evidence {}",
            world.keep().display()
        );
        Ok(())
    }
}

fn main() {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Err(error) = native::run() {
        eprintln!("harness_owner_probe: {error}");
        std::process::exit(1);
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    {
        eprintln!("harness_owner_probe requires a native macOS development build");
        std::process::exit(2);
    }
}

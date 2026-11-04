// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native qualification: `epoch_probe --targo /absolute/genuine/targo
//! --jobserver-source /absolute/trust/vendor/jobserver`.
//! No unavailable helper is counted as a passing test.

#[cfg(unix)]
mod native {
    use std::{
        fs,
        io::{self, Write as _},
        num::NonZeroU32,
        os::unix::fs::DirBuilderExt as _,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use aterm_capacity::{
        Ledger, Machine, Need, Polled, Procs, Reading, Waiter,
        build_cpu::{Owner, RetainedPool, RetainedSlotState},
        cohort_namespace::Namespace,
        epoch_controller::{Config, EpochController},
        leadership::Fence,
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const HANG: Duration = Duration::from_secs(60);

    // This private qualification namespace has a deterministic admission input;
    // it neither changes the account's global allocator nor measures throughput.
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

    struct World {
        directory: Option<aterm_tempfile::TempDir>,
        passed: bool,
    }

    impl World {
        fn new() -> io::Result<Self> {
            Ok(Self {
                directory: Some(aterm_tempfile::tempdir()?),
                passed: false,
            })
        }
        fn path(&self) -> &Path {
            self.directory.as_ref().unwrap().path()
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            if !self.passed {
                let _ = fs::write(self.path().join("abort"), b"native qualification stopped\n");
                let _ = fs::write(self.path().join("probe.release"), b"cleanup\n");
                if let Some(directory) = self.directory.take() {
                    let path = directory.keep();
                    let _ = writeln!(
                        io::stderr(),
                        "kept failed native epoch probe: {}",
                        path.display()
                    );
                }
            }
        }
    }

    fn private_dir(path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(path)
    }

    fn wait_note(
        world: &Path,
        stage: &str,
        controller: &mut EpochController,
        fence: &Fence,
    ) -> Result<String> {
        let deadline = Instant::now() + HANG;
        loop {
            // Root requests are queued, not synchronously written. Keep the
            // actual private protocol moving while awaiting the child's barrier.
            controller.poll(fence)?;
            match fs::read_to_string(world.join(format!("probe.{stage}"))) {
                Ok(note) => return Ok(note),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual member barrier did not arrive",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn require(condition: bool, why: &'static str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(io::Error::other(why).into())
        }
    }

    fn execute_artifact(path: &Path, world: &Path) -> Result<()> {
        use aterm_uds::spawnfd::{self, Stdio};
        let stdout = world.join("artifact.stdout");
        let mut child = spawnfd::command(path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fs::File::create(&stdout)?))
            .stderr(Stdio::from(fs::File::create(
                world.join("artifact.stderr"),
            )?))
            .spawn()?;
        let deadline = Instant::now() + HANG;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual emitted artifact did not exit",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        require(
            status.success() && fs::read(&stdout)? == b"native-artifact-ok\n",
            "actual emitted artifact failed",
        )
    }

    fn script(world: &Path) -> String {
        let root = format!("{:?}", world.to_str().unwrap());
        r#"
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
unsafe extern "C" { fn getpgrp() -> i32; }
fn note(root: &Path, stage: &str, body: &str) {
    let path = root.join(format!("probe.{stage}"));
    let temporary = path.with_extension("tmp");
    // SAFETY: getpgrp only observes the actual executing process's group.
    let group = unsafe { getpgrp() };
    std::fs::write(&temporary, format!("pid={}\npgid={group}\n{body}\n", std::process::id())).unwrap();
    std::fs::rename(temporary, path).unwrap();
}
fn wait(root: &Path, stage: &str) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(!root.join("abort").exists(), "native probe aborted");
        if root.join(format!("probe.{stage}")).exists() { return; }
        assert!(Instant::now() < until, "native probe barrier timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn main() {
    let root = PathBuf::from(__ROOT__);
    if std::env::var_os("ATERM_EPOCH_PROBE_MEMBER").is_some() {
        let flags = std::env::var("CARGO_MAKEFLAGS").unwrap();
        let expected = format!("--jobserver-auth=fifo:{}", root.join("transport.noindex/credits.fifo").display());
        assert!(flags.split_ascii_whitespace().any(|flag| flag == expected));
        note(&root, "ready", &format!("flags={flags}"));
        wait(&root, "import");
        // SAFETY: the inherited metadata names the original known FIFO, not
        // descriptors that earlier opens could reuse. The helper has exited.
        let server = unsafe { jobserver::Client::from_env_ext(true) }.client.unwrap();
        assert_eq!(server.available().unwrap(), 0);
        assert!(server.try_acquire().unwrap().is_none());
        note(&root, "imported", &format!("flags={flags}\nempty=true"));
        wait(&root, "release");
        note(&root, "released", "released");
        return;
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .env("ATERM_EPOCH_PROBE_MEMBER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(root.join("member.stdout")).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(root.join("member.stderr")).unwrap()))
        .spawn().unwrap();
    wait(&root, "ready");
    assert!(std::fs::read_to_string(root.join("probe.ready")).unwrap().starts_with(&format!("pid={}\n", child.id())));
    drop(child); // The outer native probe owns the actual survivor's release barrier.
    println!("cargo:rerun-if-changed=build.rs");
}
"#.replace("__ROOT__", &root)
    }

    pub fn run() -> Result<()> {
        let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
        if arguments.len() != 4 || arguments[0] != "--targo" || arguments[2] != "--jobserver-source"
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: epoch_probe --targo /absolute/genuine/targo --jobserver-source /absolute/trust/vendor/jobserver",
            )
            .into());
        }
        let helper = PathBuf::from(&arguments[1]).canonicalize()?;
        require(
            helper.is_absolute() && helper.is_file(),
            "native Targo helper is unavailable",
        )?;
        let jobserver = PathBuf::from(&arguments[3]).canonicalize()?;
        require(
            jobserver.join("Cargo.toml").is_file(),
            "explicit jobserver source is unavailable",
        )?;
        let mut world = World::new()?;
        let root = world.path().to_owned();
        let home = root.join("home");
        private_dir(&home)?;
        let namespace = Namespace::create_private(&root, &home)?;
        let ledger = Ledger::at(namespace.fence());
        let machine = ProbeMachine;
        let claim = match Waiter::enter(
            &ledger,
            &machine,
            Need::new("native GNU epoch probe").mem_gib(1),
        )?
        .poll(&machine)
        {
            Polled::Admitted(claim) => claim,
            _ => {
                return Err(io::Error::other("private native probe claim was not admitted").into());
            }
        };
        let fence = Fence::acquire(&namespace, Instant::now() + HANG)?;
        let pool =
            RetainedPool::create_private(&root.join("cpu.noindex"), NonZeroU32::new(2).unwrap())?;
        let mut backing = Vec::new();
        for _ in 0..2 {
            backing.push(
                pool.try_acquire(Owner::current(None))?
                    .ok_or_else(|| io::Error::other("original CPU backing missing"))?,
            );
        }
        let transport = root.join("transport.noindex");
        let mut controller = EpochController::new(
            &fence,
            claim,
            backing,
            Config {
                helper: helper.clone(),
                directory: transport.clone(),
                epoch: aterm_uds::rand::hex_token::<16>()?,
                roots: 1,
            },
        )?;
        let project = root.join("project");
        private_dir(&project)?;
        private_dir(&project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            format!(
                "[package]\nname='epoch_probe_native'\nversion='0.0.0'\nedition='2024'\n[workspace]\n[build-dependencies]\njobserver={{path={}}}\n",
                aterm_json::to_string(
                    jobserver
                        .to_str()
                        .ok_or_else(|| io::Error::other("jobserver source path is not UTF-8"))?
                )?
            ),
        )?;
        fs::write(
            project.join("src/main.rs"),
            "fn main() { assert_eq!((0..10).sum::<u64>(), 45); println!(\"native-artifact-ok\"); }\n",
        )?;
        fs::write(project.join("build.rs"), script(&root))?;
        controller.launch(&fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            if controller.poll(&fence)?.opened {
                break;
            }
            if Instant::now() >= deadline {
                return Err(
                    io::Error::new(io::ErrorKind::TimedOut, "actual helper did not open").into(),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        controller.spawn_root(
            &fence,
            0,
            &project.join("Cargo.toml"),
            &root.join("stable-target.noindex"),
        )?;
        let ready = wait_note(&root, "ready", &mut controller, &fence)?;
        let group = controller
            .helper_id()
            .ok_or_else(|| io::Error::other("actual helper birth missing"))?;
        require(
            ready.contains(&format!("\npgid={group}\n")),
            "actual survivor escaped the original helper group",
        )?;
        controller.close(&fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            let status = controller.poll(&fence)?;
            if status.retired && status.helper_exited && status.control_eof {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native helper did not retire and exit",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        require(
            controller
                .root_outcome(0)
                .is_some_and(|outcome| outcome.success && outcome.code == Some(0))
                && controller.status().failed_roots == 0,
            "actual Targo root compilation failed despite helper retirement",
        )?;
        require(
            controller.try_finish(&fence).is_err(),
            "retirement released CPU while the actual member survived",
        )?;
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "backing returned before actual group drain",
        )?;
        require(
            transport.join("credits.fifo").exists(),
            "FIFO disappeared before actual group drain",
        )?;
        fs::write(
            root.join("probe.import"),
            b"import after actual helper exit\n",
        )?;
        let imported = wait_note(&root, "imported", &mut controller, &fence)?;
        let identity = ready.lines().take(2).collect::<Vec<_>>();
        require(
            imported.lines().take(2).collect::<Vec<_>>() == identity
                && imported.ends_with("\nempty=true\n"),
            "the original actual member did not import the empty FIFO",
        )?;
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "late FIFO import released CPU backing",
        )?;
        fs::write(root.join("probe.release"), b"actual controller release\n")?;
        wait_note(&root, "released", &mut controller, &fence)?;
        let deadline = Instant::now() + HANG;
        loop {
            match controller.try_finish(&fence) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "actual cohort never drained and settled",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        require(
            controller.is_settled() && controller.status().settled_units == 2,
            "controller did not confirm all original units",
        )?;
        require(
            !transport.join("credits.fifo").exists(),
            "original endpoint was not removed after actual drain",
        )?;
        require(
            pool.snapshot()?
                .iter()
                .all(|slot| matches!(slot, RetainedSlotState::Available { .. })),
            "original CPU budget did not return exactly",
        )?;
        let mut restored = Vec::new();
        for _ in 0..2 {
            restored.push(
                pool.try_acquire(Owner::current(None))?
                    .ok_or_else(|| io::Error::other("restored unit missing"))?,
            );
        }
        require(
            pool.try_acquire(Owner::current(None))?.is_none(),
            "extra CPU capacity was minted",
        )?;
        for capability in restored {
            capability.settle()?;
        }
        execute_artifact(
            &root.join("stable-target.noindex/debug/epoch_probe_native"),
            &root,
        )?;
        writeln!(
            io::stdout(),
            "{}",
            aterm_json::json!({"qualified":"native-targo-epoch-controller","helper":helper,"jobserver_source":jobserver,"retained_while_member_alive":true,"late_original_fifo_imported_empty":true,"endpoint_removed_after_group_drain":true,"restored_units":2,"artifact_executed":true})
        )?;
        world.passed = true;
        Ok(())
    }
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(not(unix))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native GNU epoch qualification requires Unix",
    ))
}

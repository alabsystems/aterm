// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development-only native scheduler qualification. No shipped front door or LAN service.

#[cfg(all(target_os = "macos", debug_assertions))]
#[path = "../../src/subscriber_scheduling.rs"]
mod subscriber_scheduling;

fn main() -> std::process::ExitCode {
    #[cfg(all(target_os = "macos", debug_assertions))]
    return native::enter(std::env::args_os().collect());
    #[cfg(not(all(target_os = "macos", debug_assertions)))]
    {
        eprintln!("subscriber scheduling qualification requires a macOS debug build");
        std::process::ExitCode::from(2)
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use aterm_json::{Value, json};
    use aterm_uds::spawnfd::{Child, Command, Stdio};
    use std::ffi::OsString;
    use std::io::{self, BufRead as _, Read as _, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const ROOT: &str = "ATERM_SUBSCRIBER_PROBE_ROOT";
    const CHILD: &str = "ATERM_SUBSCRIBER_PROBE_CHILD";
    const BASELINE: &str = "ATERM_SUBSCRIBER_PROBE_BASELINE";
    const DEFAULT_PRIORITY: &str = "ATERM_SUBSCRIBER_PROBE_DEFAULT_PRIORITY";
    const FALLBACK: &str = "ATERM_SUBSCRIBER_PROBE_FALLBACK";
    const LEGACY: &str = "subscriber-old-destination";
    const HANG: Duration = Duration::from_secs(60);
    // A sampling interval, never an acceptance ceiling or a first-use delay.
    const IDLE: Duration = Duration::from_secs(5);

    pub(super) fn enter(mut argv: Vec<OsString>) -> ExitCode {
        // The old-destination control deliberately reaches the unchanged CLI parser,
        // whose unsupported argument is refused before it constructs any vault Paths.
        if argv.first().is_some_and(|arg| {
            Path::new(arg)
                .file_name()
                .is_some_and(|name| name == LEGACY)
        }) {
            return aterm_secrets::cli::main_entry(argv.into_iter().skip(2).collect());
        }
        if argv.get(1).is_some_and(|arg| arg == "secrets")
            && argv.get(2).is_some_and(|arg| arg == "watch")
        {
            // Match the unchanged unit's inherited Nice=5 before the first image
            // replaces itself. Lowering priority requires no extra authority.
            // SAFETY: modifies this owned process alone, without escalation.
            if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 5) } != 0 {
                return ExitCode::FAILURE;
            }
        }
        if std::env::var_os(BASELINE).is_none() {
            if std::env::var_os(FALLBACK).is_some()
                && let Some(code) = fallback_entry(&argv)
            {
                return code;
            }
            super::subscriber_scheduling::enter(&mut argv);
        }
        let args = argv.get(1..).unwrap_or_default();
        let result = if args == ["--probe"] || args == ["--setexec-only"] {
            controller(args == ["--setexec-only"])
        } else if std::env::var_os(CHILD).is_some() {
            child_priority()
        } else if args == ["--entry-contract"] {
            entry_contract()
        } else if args.len() == 2 && args[0] == "--replace" {
            replace(Path::new(&args[1]))
        } else if args.len() == 2 && args[0] == "--after-replace" {
            after_replace(Path::new(&args[1]))
        } else if args.len() == 3 && args[0] == "--guard-job" {
            guard_job(args[1].to_string_lossy().as_ref(), Path::new(&args[2]))
        } else if args == ["secrets", "watch"] {
            watch()
        } else if args.first().is_some_and(|arg| arg == "init") {
            // The actual local subscriber starts its ordinary converge runner. This
            // qualification has no settings to apply and never dispatches a front door.
            Ok(())
        } else {
            Err(io::Error::other("use --probe or --setexec-only"))
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("subscriber scheduling probe: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn own_root() -> io::Result<PathBuf> {
        std::env::var_os(ROOT)
            .map(PathBuf::from)
            .filter(|root| root.is_absolute())
            .ok_or_else(|| io::Error::other("no owned fixture root"))
    }

    fn fallback_entry(argv: &[OsString]) -> Option<ExitCode> {
        let root = match own_root() {
            Ok(root) => root,
            Err(_) => return Some(ExitCode::FAILURE),
        };
        let args = argv.get(1..).unwrap_or_default();
        if args == ["secrets", super::subscriber_scheduling::WATCH_REEXEC] {
            let refused = json!({"pid":std::process::id(),"args":["secrets",super::subscriber_scheduling::WATCH_REEXEC]});
            if aterm_json::to_vec(&refused).ok().is_none_or(|bytes| {
                std::fs::write(root.join("marker-refused.json"), bytes).is_err()
            }) {
                return Some(ExitCode::FAILURE);
            }
            // Model replacement by the old image at this exact entry, using its
            // unchanged parser. No setup handler, state, keeper or key is entered.
            return Some(aterm_secrets::cli::main_entry(
                argv.iter().skip(2).cloned().collect(),
            ));
        }
        if args == ["secrets", "watch"] && root.join("marker-refused.json").exists() {
            let parked = || -> io::Result<()> {
                let mut stream = UnixStream::connect(root.join("fallback.sock"))?;
                stream.set_read_timeout(Some(HANG))?;
                stream.set_write_timeout(Some(HANG))?;
                send(
                    &mut stream,
                    &json!({"stage":"plain-restarted","pid":std::process::id(),
                    "args":["secrets","watch"],"priorities":priorities(std::process::id())?,
                    "nice":usage(std::process::id())?.scheduling.and_then(|scheduling|scheduling.nice.ok())}),
                )?;
                let mut release = [0];
                stream.read_exact(&mut release)?;
                Ok(())
            };
            return Some(if parked().is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        None
    }

    fn priorities(pid: u32) -> io::Result<Vec<i32>> {
        aterm_uds::qos::thread_base_priorities(i32::try_from(pid).map_err(io::Error::other)?)
            .filter(|read| !read.is_empty())
            .ok_or_else(|| io::Error::other("the owned process's priorities are unavailable"))
    }

    fn send(stream: &mut UnixStream, value: &Value) -> io::Result<()> {
        let mut bytes = aterm_json::to_vec(value).map_err(io::Error::other)?;
        bytes.push(b'\n');
        stream.write_all(&bytes)
    }

    fn read(stream: &mut io::BufReader<UnixStream>) -> io::Result<Value> {
        let mut line = String::new();
        stream.read_line(&mut line)?;
        aterm_json::from_str(&line).map_err(io::Error::other)
    }

    fn child_priority() -> io::Result<()> {
        // SAFETY: retain the unit's Nice=5 in every paired child and outside-band
        // reference, lowering only this owned child's priority.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 5) } != 0 {
            return Err(io::Error::last_os_error());
        }
        aterm_gui::hold_process_as_housekeeping();
        let _lift = aterm_uds::qos::lift();
        println!("{}", json!({"priorities":priorities(std::process::id())?}));
        Ok(())
    }

    fn child_read(mut command: Command) -> io::Result<Vec<i32>> {
        command.env(CHILD, "1");
        let output =
            aterm_uds::deadline::output_within(&mut command, HANG).map_err(io::Error::other)?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "priority child failed: {:?}",
                output.status
            )));
        }
        let value: Value = aterm_json::from_slice(&output.stdout).map_err(io::Error::other)?;
        aterm_json::from_value(value["priorities"].clone()).map_err(io::Error::other)
    }

    fn replace(root: &Path) -> io::Result<()> {
        // SAFETY: only lower this owned process's scheduling priority, retaining
        // the Nice=5 an old subscriber unit supplies. No escalation is attempted.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 5) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // This process really entered the Background task band. Ask for default
        // before SETEXEC, so the record proves the old band rather than a QoS request.
        aterm_gui::hold_process_as_housekeeping();
        let _lift = aterm_uds::qos::lift();
        let nice = usage(std::process::id())?
            .scheduling
            .and_then(|scheduling| scheduling.nice.ok());
        std::fs::write(
            root.join("before-exec.json"),
            aterm_json::to_vec(&json!({
                "pid":std::process::id(), "priorities":priorities(std::process::id())?,"nice":nice
            }))
            .map_err(io::Error::other)?,
        )?;
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe()?);
        command
            .args([
                OsString::from("--after-replace"),
                root.as_os_str().to_owned(),
            ])
            .leave_background_band(true);
        Err(command.exec())
    }

    fn after_replace(root: &Path) -> io::Result<()> {
        // The primitive control isolates SETEXEC from any new watch-flag contract.
        // Install the identical housekeeping policy before starting another thread.
        super::subscriber_scheduling::install();
        let at_rest = priorities(std::process::id())?;
        let _lift = aterm_uds::qos::lift();
        let raised = priorities(std::process::id())?;
        let reading = usage(std::process::id())?;
        let mut stream = UnixStream::connect(root.join("replace.sock"))?;
        stream.set_read_timeout(Some(HANG))?;
        stream.set_write_timeout(Some(HANG))?;
        send(
            &mut stream,
            &json!({"pid":std::process::id(),"at_rest":at_rest,
            "lifted":raised,"nice":reading.scheduling.and_then(|scheduling|scheduling.nice.ok())}),
        )?;
        let mut release = [0];
        stream.read_exact(&mut release)?;
        Ok(())
    }

    fn watch() -> io::Result<()> {
        let root = own_root()?;
        let mut stream = UnixStream::connect(root.join("control.sock"))?;
        stream.set_read_timeout(Some(HANG))?;
        stream.set_write_timeout(Some(HANG))?;
        let exe = std::env::current_exe()?;
        let ordinary = child_read(aterm_uds::spawnfd::command(&exe))?;
        let escaped = child_read(aterm_secrets::converge::runner(
            &exe,
            aterm_secrets::converge::SUBSCRIBER,
            aterm_secrets::converge::RepositoryWork::Refresh,
        ))?;
        let outside: i32 = std::env::var(DEFAULT_PRIORITY)
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        if ordinary.iter().any(|priority| *priority != 4) || escaped != [outside] {
            return Err(io::Error::other(format!(
                "child policy differs: ordinary={ordinary:?}, escaped={escaped:?}"
            )));
        }

        let (ordinary_release, ordinary_wait) = mpsc::channel();
        let (ordinary_ready, ordinary_started) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            ordinary_ready.send(()).expect("ordinary observer is held");
            ordinary_wait
                .recv_timeout(HANG)
                .expect("ordinary observer released");
        });
        ordinary_started
            .recv_timeout(HANG)
            .map_err(io::Error::other)?;
        let baseline = priorities(std::process::id())?;
        if baseline.iter().any(|priority| *priority != 4) {
            return Err(io::Error::other(format!(
                "ordinary threads escaped housekeeping: {baseline:?}"
            )));
        }

        let fleet = root.join("fleet");
        std::fs::DirBuilder::new().mode(0o700).create(&fleet)?;
        let (held, mint_waiting) = mpsc::channel();
        let (release, mint_release) = mpsc::channel();
        let mint_dir = fleet.clone();
        let mint = std::thread::spawn(move || {
            aterm_net::fleet_member::node_identity_during_mint(&mint_dir, move || {
                held.send(()).expect("mint observer is held");
                mint_release.recv_timeout(HANG).expect("cold mint released");
            })
        });
        mint_waiting.recv_timeout(HANG).map_err(io::Error::other)?;
        if aterm_net::fleet_member::read_node_identity(&fleet)?.is_some() {
            return Err(io::Error::other(
                "the cold-mint barrier already published an identity",
            ));
        }
        send(
            &mut stream,
            &json!({
                "stage":"mint-held", "pid":std::process::id(),
                "ordinary_threads":baseline, "ordinary_child":ordinary, "escaped_child":escaped
            }),
        )?;
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        release.send(()).map_err(io::Error::other)?;
        let minted = mint
            .join()
            .map_err(|_| io::Error::other("mint thread panicked"))??;
        let kept = aterm_net::fleet_member::read_node_identity(&fleet)?
            .ok_or_else(|| io::Error::other("the completed mint is unreadable"))?;
        if minted.node() != kept.node() {
            return Err(io::Error::other("the readback names a different mint"));
        }
        ordinary_release.send(()).map_err(io::Error::other)?;
        thread
            .join()
            .map_err(|_| io::Error::other("ordinary thread panicked"))?;

        // The controller already admitted this exact account to its pinned signed
        // loopback vault. The measured child owns only the actual subscriber runtime.
        let paths = aterm_secrets::client::Paths::for_home(&root.join("home"));
        let status = aterm_secrets::client::status(&paths).map_err(io::Error::other)?;
        if !matches!(
            status.membership,
            aterm_secrets::client::Membership::In { .. }
        ) || !matches!(
            status.config.as_ref().map(|config| &config.source),
            Some(aterm_secrets::transport::Source::Url(_))
        ) || !aterm_secrets::client::materialization_ready(&paths).map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "signed HTTP fixture membership is not materialized",
            ));
        }
        send(&mut stream, &json!({"stage":"joined-idle"}))?;
        aterm_secrets::client::watch(&paths, &|_| {}).map_err(io::Error::other)
    }

    fn entry_contract() -> io::Result<()> {
        let root = own_root()?;
        let exe = std::env::current_exe()?;
        let legacy = root.join(LEGACY);
        let mut old = aterm_uds::spawnfd::command(&legacy);
        old.args(["secrets", super::subscriber_scheduling::WATCH_REEXEC]);
        let refused =
            aterm_uds::deadline::output_within(&mut old, HANG).map_err(io::Error::other)?;
        if refused.status.code() != Some(2)
            || std::env::var_os("HOME").is_none_or(|home| Path::new(&home).exists())
        {
            return Err(io::Error::other(
                "the old parser did not refuse the runtime entry before Paths",
            ));
        }
        let args = vec!["secrets".to_owned(), "watch".to_owned()];
        let unit = |name: &str, program: &Path| -> io::Result<String> {
            let path = aterm_secrets::service::write_plist(
                &root.join(name),
                aterm_secrets::service::Job::Subscriber,
                program,
                &args,
            )
            .map_err(io::Error::other)?;
            std::fs::read_to_string(path)
        };
        let new = unit("new-unit", &exe)?;
        let old_unit = unit("old-unit", &legacy)?;
        for unit in [&new, &old_unit] {
            let argv = aterm_secrets::service::plist_argv(unit)
                .ok_or_else(|| io::Error::other("unit arguments are unavailable"))?;
            if !unit.contains("<key>ProcessType</key>\n\t<string>Background</string>")
                || !unit.contains("<key>Nice</key>\n\t<integer>5</integer>")
                || argv.get(1..) != Some(["secrets".to_owned(), "watch".to_owned()].as_slice())
            {
                return Err(io::Error::other(
                    "the unchanged unit no longer restarts plain Background watch",
                ));
            }
        }
        println!(
            "{}",
            json!({"old_parser_exit":2,"current_unit":"Background Nice=5",
            "old_unit":"Background Nice=5","restart_arguments":["secrets","watch"],"query_created_paths":false})
        );
        Ok(())
    }

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            aterm_uds::deadline::kill_within(&mut self.0, HANG);
        }
    }

    fn launchctl(args: &[String]) -> io::Result<std::process::Output> {
        let mut command = aterm_uds::spawnfd::command("/bin/launchctl");
        command.args(args);
        aterm_uds::deadline::output_within(&mut command, HANG).map_err(io::Error::other)
    }

    /// Only a fresh unique label created by this controller is ever stopped.
    struct PrivateJob {
        target: Option<String>,
        guardian: Option<Child>,
    }

    impl Drop for PrivateJob {
        fn drop(&mut self) {
            if let Some(target) = &self.target {
                let _ = launchctl(&["bootout".into(), target.clone()]);
            }
            self.release_guardian();
        }
    }

    impl PrivateJob {
        fn new(target: String, plist: &Path) -> io::Result<Self> {
            let (mut ready, peer) = UnixStream::pair()?;
            ready.set_read_timeout(Some(HANG))?;
            let mut command = aterm_uds::spawnfd::command(std::env::current_exe()?);
            command
                .arg("--guard-job")
                .arg(&target)
                .arg(plist)
                .stdin(Stdio::piped())
                .stdout(Stdio::from(std::os::fd::OwnedFd::from(peer)))
                .stderr(Stdio::null())
                .setsid(true);
            let guardian = command.spawn()?;
            let mut owner = Self {
                target: Some(target),
                guardian: Some(guardian),
            };
            let mut word = [0; 5];
            if let Err(error) = ready.read_exact(&mut word) {
                owner.release_guardian();
                return Err(error);
            }
            if &word != b"ready" {
                return Err(io::Error::other("private-job guardian did not start"));
            }
            Ok(owner)
        }

        fn release_guardian(&mut self) {
            if let Some(mut guardian) = self.guardian.take() {
                guardian.stdin.take();
                let until = Instant::now() + HANG;
                while guardian.try_wait().ok().flatten().is_none() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                aterm_uds::deadline::kill_within(&mut guardian, HANG);
            }
        }

        fn finish(mut self) -> io::Result<()> {
            let target = self.target.as_ref().expect("owned job is loaded");
            let output = launchctl(&["bootout".into(), target.clone()])?;
            if !output.status.success()
                || launchctl(&["print".into(), target.clone()])?
                    .status
                    .success()
            {
                return Err(io::Error::other(
                    "the owned private scheduler job did not unload",
                ));
            }
            self.target = None;
            Ok(())
        }
    }

    fn guard_job(target: &str, plist: &Path) -> io::Result<()> {
        let uid = aterm_uds::peer::our_uid();
        // SAFETY: reads this guardian's live parent id; it takes no pointers.
        let parent = unsafe { libc::getppid() };
        let prefix = format!("systems.alab.aterm.scheduler-probe-{parent}-");
        let label = target
            .strip_prefix(&format!("gui/{uid}/"))
            .or_else(|| target.strip_prefix(&format!("user/{uid}/")))
            .and_then(|label| label.strip_prefix(&prefix));
        if label.is_none_or(|suffix| {
            suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(io::Error::other(
                "refusing a job outside the controller's private label",
            ));
        }
        aterm_gui::hold_process_as_housekeeping();
        let domain = target
            .rsplit_once('/')
            .map(|(domain, _)| domain)
            .ok_or_else(|| io::Error::other("private job has no domain"))?;
        // This detached owner serializes bootstrap with cleanup. Losing the
        // controller while bootstrap is in flight cannot publish a job after it.
        let loaded = launchctl(&[
            "bootstrap".into(),
            domain.to_owned(),
            plist.to_string_lossy().into_owned(),
        ]);
        let result = match loaded {
            Ok(output) if output.status.success() => aterm_uds::stdout::write_all(b"ready")
                .and_then(|()| io::copy(&mut io::stdin().lock(), &mut io::sink()).map(|_| ())),
            Ok(output) => Err(io::Error::other(format!(
                "private scheduler bootstrap failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))),
            Err(error) => Err(error),
        };
        // EOF is custody loss, including SIGKILL of the controller. No vault or
        // system unit is named: this exact fresh private label was supplied by it.
        let _ = launchctl(&["bootout".into(), target.to_owned()]);
        result
    }

    fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
        let until = Instant::now() + HANG;
        let (stream, _) = aterm_uds::accept(
            listener,
            aterm_uds::AcceptWait::Until(until),
            aterm_uds::AcceptedMode::Blocking,
        )?;
        stream.set_read_timeout(Some(HANG))?;
        stream.set_write_timeout(Some(HANG))?;
        Ok(stream)
    }

    fn setexec_arm(exe: &Path, root: &Path, manager: bool) -> io::Result<Value> {
        use aterm_secrets::service::{Domain, Launchctl};
        std::fs::DirBuilder::new().mode(0o700).create(root)?;
        let listener = UnixListener::bind(root.join("replace.sock"))?;
        let mut child = None;
        let mut job = None;
        if manager {
            let uid = aterm_uds::peer::our_uid();
            let domain = if launchctl(&aterm_secrets::service::launchctl_argv(
                uid,
                Launchctl::PrintDomain(Domain::Gui),
            ))?
            .status
            .success()
            {
                Domain::Gui
            } else {
                Domain::User
            };
            let label = format!(
                "systems.alab.aterm.scheduler-probe-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_nanos()
            );
            let target = format!("{}/{}", domain.target(uid), label);
            if launchctl(&["print".into(), target.clone()])?
                .status
                .success()
            {
                return Err(io::Error::other(
                    "the fresh private scheduler label already exists",
                ));
            }
            let args = vec!["--replace".to_owned(), root.to_string_lossy().into_owned()];
            let text = aterm_secrets::service::plist_text_in(
                &label,
                exe,
                &args,
                &root.join("probe.log"),
                domain,
                None,
            );
            if !text.contains("<key>ProcessType</key>\n\t<string>Background</string>")
                || !text.contains("<key>Nice</key>\n\t<integer>5</integer>")
            {
                return Err(io::Error::other(
                    "the primitive unit is not Background plus Nice=5",
                ));
            }
            let plist = root.join("probe.plist");
            std::fs::write(&plist, text)?;
            let owned = PrivateJob::new(target, &plist)?;
            job = Some(owned);
        } else {
            let mut command = aterm_uds::spawnfd::command(exe);
            command.arg("--replace").arg(root).enter_background_band();
            command.stdout(Stdio::null()).stderr(Stdio::inherit());
            child = Some(Owned(command.spawn()?));
        }
        let stream = accept(&listener)?;
        let mut reader = io::BufReader::new(stream.try_clone()?);
        let after = read(&mut reader)?;
        let before: Value = aterm_json::from_slice(&std::fs::read(root.join("before-exec.json"))?)
            .map_err(io::Error::other)?;
        if before["pid"] != after["pid"] || before["priorities"] != json!([4]) {
            return Err(io::Error::other(
                "SETEXEC did not retain the actual Background process",
            ));
        }
        if before["nice"] != json!(5) || after["nice"] != before["nice"] {
            return Err(io::Error::other("SETEXEC did not retain Nice=5"));
        }
        let report = json!({"manager":if manager {"private launchd job"} else {"posix_spawn task type"},
            "before":before,"after":after,
            "setexec_escaped":after["lifted"].as_array().is_some_and(|p|p.iter().any(|p|p.as_i64().is_some_and(|p|p>4)))});
        // The job stays blocked until bootout, so KeepAlive cannot relaunch it between
        // the measurement and cleanup. The spawned arm is killed and reaped by Owned.
        if let Some(job) = job {
            job.finish()?;
        }
        drop(child);
        Ok(report)
    }

    fn keepalive_fallback(exe: &Path, root: &Path) -> io::Result<Value> {
        use aterm_secrets::service::{Domain, Launchctl};
        std::fs::DirBuilder::new().mode(0o700).create(root)?;
        let listener = UnixListener::bind(root.join("fallback.sock"))?;
        let uid = aterm_uds::peer::our_uid();
        let domain = if launchctl(&aterm_secrets::service::launchctl_argv(
            uid,
            Launchctl::PrintDomain(Domain::Gui),
        ))?
        .status
        .success()
        {
            Domain::Gui
        } else {
            Domain::User
        };
        let label = format!(
            "systems.alab.aterm.scheduler-probe-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        );
        let target = format!("{}/{}", domain.target(uid), label);
        if launchctl(&["print".into(), target.clone()])?
            .status
            .success()
        {
            return Err(io::Error::other("the fresh fallback label already exists"));
        }
        let args = vec!["secrets".to_owned(), "watch".to_owned()];
        let mut text = aterm_secrets::service::plist_text_in(
            &label,
            exe,
            &args,
            &root.join("probe.log"),
            domain,
            None,
        );
        if text.contains("<key>EnvironmentVariables</key>") {
            return Err(io::Error::other("unexpected fixture environment"));
        }
        let escape = aterm_codec::xml::escape;
        text=text.replacen("\t<key>RunAtLoad</key>",&format!(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>{ROOT}</key>\n\t\t<string>{}</string>\n\t\t<key>{FALLBACK}</key>\n\t\t<string>1</string>\n\t\t<key>HOME</key>\n\t\t<string>{}</string>\n\t</dict>\n\t<key>RunAtLoad</key>",
            escape(&root.to_string_lossy()),escape(&root.join("home").to_string_lossy())),1);
        let plist = root.join("fallback.plist");
        std::fs::write(&plist, text)?;
        let job = PrivateJob::new(target, &plist)?;
        let stream = accept(&listener)?;
        let mut reader = io::BufReader::new(stream);
        let restarted = read(&mut reader)?;
        let refused: Value =
            aterm_json::from_slice(&std::fs::read(root.join("marker-refused.json"))?)
                .map_err(io::Error::other)?;
        if restarted["stage"].as_str() != Some("plain-restarted")
            || restarted["pid"] == refused["pid"]
            || restarted["args"] != json!(["secrets", "watch"])
            || restarted["priorities"] != json!([4])
            || restarted["nice"] != json!(5)
            || root.join("home").exists()
        {
            return Err(io::Error::other(
                "old-parser refusal did not return to untouched plain Background watch",
            ));
        }
        job.finish()?;
        Ok(json!({"marker_refused":refused,"plain_restart":restarted,
            "created_vault_paths":false,"private_job_unloaded":true}))
    }

    fn usage(pid: u32) -> io::Result<aterm_sysprobe::procs::ProcessReading> {
        aterm_sysprobe::procs::processes()
            .and_then(|sweep| sweep.procs.into_iter().find(|process| process.pid == pid))
            .ok_or_else(|| {
                io::Error::other("the owned subscriber's resource reading is unavailable")
            })
    }

    fn delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
        before
            .zip(after)
            .and_then(|(before, after)| after.checked_sub(before))
    }

    struct HttpWorld(Option<aterm_secrets::server::Running>);

    impl Drop for HttpWorld {
        fn drop(&mut self) {
            if let Some(server) = self.0.take() {
                server.stop();
            }
        }
    }

    impl HttpWorld {
        fn start(root: &Path) -> io::Result<Self> {
            use aterm_secrets::client::{self, Paths};
            let source = root.join("scheduler-fixture");
            let import = root.join("import");
            std::fs::create_dir(&import)?;
            let entry = import.join("subscriber-probe");
            std::fs::write(&entry, b"inert_fixture_value")?;
            std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o600))?;
            let admin_paths = Paths::for_home(&root.join("admin"));
            let created = client::create(
                &admin_paths,
                source.to_string_lossy().as_ref(),
                Some(&import),
                Some("fixture-admin"),
            )
            .map_err(io::Error::other)?;
            let first = std::fs::read(&source)?;
            let admin = client::admin_open(&admin_paths).map_err(io::Error::other)?;
            let mut doc = admin.doc.clone();
            let mut recipients = Vec::new();
            for name in ["baseline", "reexec"] {
                let paths = Paths::for_home(&root.join(name).join("home"));
                let identity = client::machine_identity(&paths).map_err(io::Error::other)?;
                let recipient = identity.recipient().to_bech32();
                doc.add_machine(name, &recipient)
                    .map_err(io::Error::other)?;
                recipients.push(recipient);
            }
            client::admin_publish_opened(&admin_paths, &admin, &doc, &recipients)
                .map_err(io::Error::other)?;
            let latest = std::fs::read(&source)?;
            let envelope = aterm_secrets::envelope::parse(&latest).map_err(io::Error::other)?;
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let options = aterm_secrets::server::Options {
                listen: listener.local_addr()?,
                store: root.join("http-store"),
                advertise: None,
                pins: vec![(created.name.clone(), envelope.signer)],
                hold: Duration::from_secs(600),
                keep_awake: false,
            };
            let server = Self(Some(
                aterm_secrets::server::start(listener, &options).map_err(io::Error::other)?,
            ));
            let url = format!("http://{}/v1/{}", server.running().addr, created.name);
            let target = aterm_secrets::transport::Source::Url(url.clone());
            aterm_secrets::transport::publish(&target, &first).map_err(io::Error::other)?;
            // A fresh explicit non-ALab vault has a signed envelope and Commons
            // foundation, but no separately seeded membership-record log. Replicate
            // every retained record if one exists; an empty stream is no POST.
            let records = client::load_records(&admin_paths)
                .map_err(io::Error::other)?
                .encode_held();
            if !records.is_empty() {
                aterm_secrets::transport::post_records(&url, &records).map_err(io::Error::other)?;
            }
            aterm_secrets::transport::publish(&target, &latest).map_err(io::Error::other)?;
            for name in ["baseline", "reexec"] {
                let paths = Paths::for_home(&root.join(name).join("home"));
                let outcome = client::join(
                    &paths,
                    &client::JoinOptions {
                        source: format!("{url}#{}", created.signer),
                        host: Some(name.into()),
                        interactive: false,
                        wait: Duration::ZERO,
                        confirm: &|_| false,
                        password: client::JoinPassword::Given(None),
                        fetched: None,
                        requested: &|| {},
                        say: &|_| {},
                    },
                )
                .map_err(io::Error::other)?;
                if !matches!(outcome, client::JoinOutcome::Joined { .. })
                    || !client::materialization_ready(&paths).map_err(io::Error::other)?
                {
                    return Err(io::Error::other(
                        "the signed loopback fixture did not admit and materialize its subscriber",
                    ));
                }
            }
            server.wait_held(false)?;
            Ok(server)
        }

        fn running(&self) -> &aterm_secrets::server::Running {
            self.0.as_ref().expect("owned HTTP fixture is running")
        }

        fn wait_held(&self, wants: bool) -> io::Result<usize> {
            let until = Instant::now() + HANG;
            loop {
                let held = self.running().test_waiting();
                if (held != 0) == wants {
                    return Ok(held);
                }
                if Instant::now() >= until {
                    return Err(io::Error::other(
                        "the actual held-long-poll barrier did not change",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn arm(
        exe: &Path,
        root: &Path,
        kind: &str,
        outside: i32,
        server: &HttpWorld,
    ) -> io::Result<Value> {
        // The fixture setup already created this private account and admitted it.
        server.wait_held(false)?;
        let listener = UnixListener::bind(root.join("control.sock"))?;
        let mut command = aterm_uds::spawnfd::command(exe);
        command
            .env(ROOT, root)
            .env("HOME", root.join("home"))
            .env(DEFAULT_PRIORITY, outside.to_string());
        command.args(["secrets", "watch"]);
        command.enter_background_band();
        if kind == "baseline" {
            command.env(BASELINE, "1");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let child = Owned(command.spawn()?);
        let mut stream = accept(&listener)?;
        let mut reader = io::BufReader::new(stream.try_clone()?);
        let held = read(&mut reader)?;
        if held["stage"].as_str() != Some("mint-held")
            || held["pid"].as_u64() != Some(u64::from(child.0.id()))
        {
            return Err(io::Error::other(
                "the private cold-mint barrier was not reached",
            ));
        }
        let priorities = priorities(child.0.id())?;
        let lifted = priorities
            .iter()
            .filter(|priority| **priority == outside)
            .count()
            == 1
            && priorities
                .iter()
                .all(|priority| *priority == 4 || *priority == outside);
        let background = priorities.iter().all(|priority| *priority == 4);
        if (kind == "reexec" && !lifted) || (kind == "baseline" && !background) {
            return Err(io::Error::other(format!(
                "{kind}: wrong held cold-mint priorities {priorities:?}"
            )));
        }
        stream.write_all(b"release")?;
        let idle = read(&mut reader)?;
        if idle["stage"].as_str() != Some("joined-idle") {
            return Err(io::Error::other(
                "the actual local subscriber did not settle",
            ));
        }
        // The actual signed HTTP request must be held at the server before this
        // sample, so readiness and a parked stand-in cannot manufacture the arm.
        let held_before = server.wait_held(true)?;
        let before = usage(child.0.id())?;
        let began = Instant::now();
        std::thread::sleep(IDLE);
        let after = usage(child.0.id())?;
        let held_after = server.running().test_waiting();
        if held_after == 0 {
            return Err(io::Error::other(
                "the signed HTTP long-poll was not held after the sample",
            ));
        }
        let elapsed_ms = began.elapsed().as_millis();
        if before.start != after.start {
            return Err(io::Error::other(
                "subscriber identity changed during idle measurement",
            ));
        }
        let qos = |read: &aterm_sysprobe::procs::ProcessReading| {
            read.scheduling
                .and_then(|scheduling| scheduling.qos_cpu_ns)
                .and_then(Result::ok)
        };
        let qos_delta = qos(&before).zip(qos(&after)).and_then(|(before, after)| {
            let mut delta = [0; 7];
            for index in 0..7 {
                delta[index] = after[index].checked_sub(before[index])?;
            }
            Some(delta)
        });
        Ok(json!({"arm":kind,"held":held,"held_priorities":priorities,
            "mint_lifted":lifted,
            "actual_joined_subscriber_idle":{"elapsed_ms":elapsed_ms,
                "signed_loopback_long_polls_before":held_before,"signed_loopback_long_polls_after":held_after,
                "cpu_ns":delta(before.cpu_ns,after.cpu_ns),
                "energy_nj":delta(before.energy_nj,after.energy_nj),
                "disk_read":delta(before.disk_read,after.disk_read),
                "disk_written":delta(before.disk_written,after.disk_written),
                "qos_cpu_ns":qos_delta,"footprint_before":before.footprint,
                "footprint_after":after.footprint}}))
    }

    fn controller(setexec_only: bool) -> io::Result<()> {
        let base = Path::new("/tmp/.ubd");
        std::fs::create_dir_all(base)?;
        let root = base.join(format!(
            "subscriber-scheduling-{}-{}.noindex",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        let exe = std::env::current_exe()?;
        if setexec_only {
            let task = setexec_arm(&exe, &root.join("task-type"), false)?;
            let job = setexec_arm(&exe, &root.join("launchd"), true)?;
            println!("{}", json!({"schema":1,"task":task,"job":job}));
            return std::fs::remove_dir_all(root);
        }
        std::os::unix::fs::symlink(&exe, root.join(LEGACY))?;
        let mut contract = aterm_uds::spawnfd::command(&exe);
        contract
            .arg("--entry-contract")
            .env(ROOT, &root)
            .env("HOME", root.join("query-home"));
        let output =
            aterm_uds::deadline::output_within(&mut contract, HANG).map_err(io::Error::other)?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "runtime entry contract fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let contract: Value = aterm_json::from_slice(&output.stdout).map_err(io::Error::other)?;
        let fallback = keepalive_fallback(&exe, &root.join("fallback"))
            .map_err(|error| io::Error::other(format!("old-image KeepAlive restart: {error}")))?;
        let server = HttpWorld::start(&root)
            .map_err(|error| io::Error::other(format!("signed HTTP fixture admission: {error}")))?;
        let mut outside = aterm_uds::spawnfd::command(&exe);
        outside.leave_background_band(true);
        let outside = child_read(outside)?;
        if outside.len() != 1 || outside[0] <= 4 {
            return Err(io::Error::other(
                "the Nice=5 outside-band default reference is unavailable",
            ));
        }
        let mut arms = Vec::new();
        for kind in ["baseline", "reexec"] {
            arms.push(
                arm(&exe, &root.join(kind), kind, outside[0], &server)
                    .map_err(|error| io::Error::other(format!("{kind} subscriber arm: {error}")))?,
            );
        }
        server.wait_held(false)?;
        drop(server);
        println!(
            "{}",
            json!({"schema":1,"entry_contract":contract,"keepalive_fallback":fallback,"outside_band_default":outside,"arms":arms,
            "isolation":"owned scratch nodes and explicitly pinned signed loopback HTTP vault; no LAN dials or advertisements"})
        );
        std::fs::remove_dir_all(root)
    }
}

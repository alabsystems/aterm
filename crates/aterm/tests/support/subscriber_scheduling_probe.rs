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
    const HANG: Duration = Duration::from_secs(60);
    // A sampling interval, never an acceptance ceiling or a first-use delay.
    const IDLE: Duration = Duration::from_secs(5);

    pub(super) fn enter(argv: Vec<OsString>) -> ExitCode {
        // The production entry applies only to exact plain watch, before any worker.
        // No replacement image, hidden verb or priority adjustment is part of this probe.
        super::subscriber_scheduling::enter(&argv);
        let args = argv.get(1..).unwrap_or_default();
        let result = if args == ["--probe"] {
            controller()
        } else if std::env::var_os(CHILD).is_some() {
            child_priority()
        } else if args.len() == 3 && args[0] == "--guard-job" {
            guard_job(args[1].to_string_lossy().as_ref(), Path::new(&args[2]))
        } else if args == ["secrets", "watch"] {
            watch()
        } else if args.first().is_some_and(|arg| arg == "init") {
            // The signed loopback subscriber may start its ordinary converge runner;
            // this fixture has no settings to apply and dispatches no live front door.
            Ok(())
        } else {
            Err(io::Error::other("use --probe"))
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

    fn thread_id() -> io::Result<u64> {
        unsafe extern "C" {
            fn pthread_threadid_np(thread: *mut core::ffi::c_void, id: *mut u64) -> libc::c_int;
        }
        let mut id = 0;
        // SAFETY: null names this thread, and id is a writable u64 slot.
        let rc = unsafe { pthread_threadid_np(std::ptr::null_mut(), &mut id) };
        if rc != 0 || id == 0 {
            return Err(io::Error::other("the controlled thread id is unavailable"));
        }
        Ok(id)
    }

    fn thread_priority(pid: u32, thread: u64) -> io::Result<i32> {
        // The existing UDS band fixture uses this exact by-thread-id flavor.
        const PROC_PIDTHREADID64INFO: libc::c_int = 15;
        let pid = i32::try_from(pid).map_err(io::Error::other)?;
        let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::uninit();
        let size =
            libc::c_int::try_from(size_of::<libc::proc_threadinfo>()).map_err(io::Error::other)?;
        // SAFETY: info is size writable bytes; the caller retains the owned worker
        // at a channel barrier, so its exact kernel thread id remains alive.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDTHREADID64INFO,
                thread,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if read != size {
            return Err(io::Error::other(
                "the controlled thread's kernel priority is unavailable",
            ));
        }
        // SAFETY: the exact-size read initialized every integer field.
        Ok(unsafe { info.assume_init() }.pth_priority)
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
        aterm_gui::hold_process_as_housekeeping();
        let _lift = aterm_uds::qos::lift();
        let priority = thread_priority(std::process::id(), thread_id()?)?;
        println!("{}", json!({"priorities":[priority]}));
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
        if ordinary != [4] || escaped.len() != 1 || escaped[0] <= 4 {
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
        let (ended, mint_ended) = mpsc::channel();
        let (finish, mint_finish) = mpsc::channel();
        let mint_dir = fleet.clone();
        let mint = std::thread::spawn(move || -> io::Result<_> {
            let id = thread_id()?;
            let before = thread_priority(std::process::id(), id)?;
            let identity =
                aterm_net::fleet_member::node_identity_during_mint(&mint_dir, move || {
                    held.send((id, before)).expect("mint observer is held");
                    mint_release.recv_timeout(HANG).expect("cold mint released");
                })?;
            let after = thread_priority(std::process::id(), id)?;
            ended.send((id, after)).map_err(io::Error::other)?;
            mint_finish.recv_timeout(HANG).map_err(io::Error::other)?;
            Ok(identity)
        });
        let (mint_thread, before) = mint_waiting.recv_timeout(HANG).map_err(io::Error::other)?;
        if aterm_net::fleet_member::read_node_identity(&fleet)?.is_some() {
            return Err(io::Error::other(
                "the cold-mint barrier already published an identity",
            ));
        }
        send(
            &mut stream,
            &json!({
                "stage":"mint-held", "pid":std::process::id(),
                "mint_thread":mint_thread,"before_priority":before,
                "ordinary_threads":baseline, "ordinary_child":ordinary, "escaped_child":escaped
            }),
        )?;
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        release.send(()).map_err(io::Error::other)?;
        let (after_thread, after) = mint_ended.recv_timeout(HANG).map_err(io::Error::other)?;
        send(
            &mut stream,
            &json!({"stage":"mint-released","mint_thread":after_thread,
            "after_priority":after}),
        )?;
        stream.read_exact(&mut byte)?;
        finish.send(()).map_err(io::Error::other)?;
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
            for name in ["baseline", "current"] {
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
            for name in ["baseline", "current"] {
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
        use aterm_secrets::service::{Domain, Job, Launchctl};
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
        let args = vec!["secrets".to_owned(), "watch".to_owned()];
        // Select policy using the REAL canonical Subscriber label. Only afterwards
        // rename this fixture's job, retaining the production scheduling definition.
        let policy = match kind {
            "current" => aterm_secrets::service::SubscriberProcessType::Interactive,
            "baseline" => aterm_secrets::service::SubscriberProcessType::Background,
            _ => return Err(io::Error::other("unknown private subscriber arm")),
        };
        let mut text = aterm_secrets::service::plist_text_in_with_subscriber_process_type(
            Job::Subscriber.label(),
            exe,
            &args,
            &root.join("probe.log"),
            domain,
            None,
            policy,
        );
        // Record the produced policy, but let the actual kernel-thread oracle decide.
        // A renderer fault must reach the held mint rather than stop at an XML assertion.
        let rendered_interactive =
            text.contains("<key>ProcessType</key>\n\t<string>Interactive</string>");
        let rendered_nice = text.contains("<key>Nice</key>");
        let canonical_label = format!("<string>{}</string>", Job::Subscriber.label());
        text = text.replacen(
            canonical_label.as_str(),
            &format!("<string>{label}</string>"),
            1,
        );
        if text.contains("<key>EnvironmentVariables</key>") {
            return Err(io::Error::other("unexpected fixture environment"));
        }
        let escape = aterm_codec::xml::escape;
        text = text.replacen("\t<key>RunAtLoad</key>", &format!(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>{ROOT}</key>\n\t\t<string>{}</string>\n\t\t<key>HOME</key>\n\t\t<string>{}</string>\n\t</dict>\n\t<key>RunAtLoad</key>",
            escape(&root.to_string_lossy()), escape(&root.join("home").to_string_lossy())), 1);
        let plist = root.join("probe.plist");
        std::fs::write(&plist, text)?;
        let job = PrivateJob::new(target.clone(), &plist)?;
        let mut stream = accept(&listener)?;
        let mut reader = io::BufReader::new(stream.try_clone()?);
        let held = read(&mut reader)?;
        let loaded = launchctl(&["print".into(), target])?;
        let loaded_pid =
            aterm_secrets::service::print_pid(&String::from_utf8_lossy(&loaded.stdout));
        let pid = held["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| Some(*pid) == loaded_pid)
            .ok_or_else(|| io::Error::other("the mint worker is not the owned private job"))?;
        let mint_thread = held["mint_thread"]
            .as_u64()
            .ok_or_else(|| io::Error::other("the held mint has no exact kernel thread id"))?;
        let held_priority = thread_priority(pid, mint_thread)?;
        let expected = if kind == "current" { outside } else { 4 };
        if held["stage"].as_str() != Some("mint-held")
            || held["before_priority"].as_i64() != Some(4)
            || held_priority != expected
        {
            return Err(io::Error::other(format!(
                "{kind}: exact mint worker priority {held_priority}, expected {expected}"
            )));
        }
        stream.write_all(&[1])?;
        let released = read(&mut reader)?;
        let after_priority = thread_priority(pid, mint_thread)?;
        if released["stage"].as_str() != Some("mint-released")
            || released["mint_thread"].as_u64() != Some(mint_thread)
            || released["after_priority"].as_i64() != Some(4)
            || after_priority != 4
        {
            return Err(io::Error::other(
                "the exact mint worker did not return to housekeeping",
            ));
        }
        stream.write_all(&[1])?;
        let idle = read(&mut reader)?;
        if idle["stage"].as_str() != Some("joined-idle") {
            return Err(io::Error::other(
                "the actual local subscriber did not settle",
            ));
        }
        // The actual signed HTTP request must be held at the server before this
        // sample, so readiness and a parked stand-in cannot manufacture the arm.
        let held_before = server.wait_held(true)?;
        let before = usage(pid)?;
        let began = Instant::now();
        std::thread::sleep(IDLE);
        let after = usage(pid)?;
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
        job.finish()?;
        server.wait_held(false)?;
        Ok(
            json!({"arm":kind,"rendered_interactive":rendered_interactive,
            "rendered_nice":rendered_nice,"held":held,"released":released,
            "effective_mint_priority":{"before":4,"inside":held_priority,"after":after_priority},
            "mint_lifted":held_priority == outside,"private_job_unloaded":true,
            "actual_joined_subscriber_idle":{"elapsed_ms":elapsed_ms,
                "signed_loopback_long_polls_before":held_before,"signed_loopback_long_polls_after":held_after,
                "cpu_ns":delta(before.cpu_ns,after.cpu_ns),
                "energy_nj":delta(before.energy_nj,after.energy_nj),
                "disk_read":delta(before.disk_read,after.disk_read),
                "disk_written":delta(before.disk_written,after.disk_written),
                "qos_cpu_ns":qos_delta,"footprint_before":before.footprint,
                "footprint_after":after.footprint}}),
        )
    }

    fn controller() -> io::Result<()> {
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
        let server = HttpWorld::start(&root)
            .map_err(|error| io::Error::other(format!("signed HTTP fixture admission: {error}")))?;
        let mut outside = aterm_uds::spawnfd::command(&exe);
        outside.leave_background_band(true);
        let outside = child_read(outside)?;
        if outside.len() != 1 || outside[0] <= 4 {
            return Err(io::Error::other(
                "the outside-band exact-thread default reference is unavailable",
            ));
        }
        let mut arms = Vec::new();
        for kind in ["baseline", "current"] {
            arms.push(
                arm(&exe, &root.join(kind), kind, outside[0], &server)
                    .map_err(|error| io::Error::other(format!("{kind} subscriber arm: {error}")))?,
            );
        }
        server.wait_held(false)?;
        drop(server);
        println!(
            "{}",
            json!({"schema":2,"outside_band_default":outside,"arms":arms,
            "isolation":"owned unique launchd jobs, scratch nodes and pinned signed loopback HTTP; no LAN dials or advertisements"})
        );
        std::fs::remove_dir_all(root)
    }
}

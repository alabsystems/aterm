//! Standalone native actors: a sole main thread, no fake Host/Root/SDK owners.
//! This qualifies a fixed kernel restriction and physical input checks only.
//! Parent retains SAME actual descriptions across the fork and both execs;
//! there is no production prelude transfer or operation-completion assertion.

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("closed_exec_native: native macOS arm64 controls unavailable");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> std::io::Result<()> {
    native::run()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod native {
    use std::ffi::CString;
    use std::io::{self, Write as _};
    use std::os::unix::{
        ffi::OsStrExt as _,
        fs::{PermissionsExt as _, symlink},
    };
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use trust_resource_provenance::program::{
        HeldExecImage, HeldExecImageSet, HeldProgram, HeldSystemCommand,
    };

    fn end() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }
    fn program(owner: &Arc<HeldSystemCommand>, end: Instant) -> io::Result<&HeldProgram> {
        owner.program_input(end)
    }
    fn input(owner: &Arc<HeldSystemCommand>, end: Instant) -> HeldExecImage {
        HeldExecImage::retain(Box::new(Arc::clone(owner)), end, program)
    }
    fn fixture() -> io::Result<(tempfile::TempDir, PathBuf)> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let image = root.join("image");
        std::fs::copy(std::env::current_exe()?, &image)?;
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o555))?;
        Ok((temp, image))
    }
    fn held(path: &Path) -> Arc<HeldSystemCommand> {
        Arc::new(HeldSystemCommand::open(path, end()).unwrap())
    }

    pub fn run() -> io::Result<()> {
        let mut args = std::env::args_os().skip(1);
        match args.next().as_deref().and_then(|arg| arg.to_str()) {
            Some("--closed-inherited-service-ports") => {
                return inherited_service_ports_actor();
            }
            Some("--closed-install-once") | Some("--closed-reject-old-seal") => {
                let old = std::env::args_os().any(|arg| arg == "--closed-reject-old-seal");
                println!("closed-exec-wrapper-ran");
                io::stdout().flush()?;
                let original = held(&std::fs::canonicalize(std::env::current_exe()?)?);
                let set = HeldExecImageSet::capture(vec![input(&original, end())], end()).unwrap();
                if old {
                    trust_exec_policy::ExecutionSeal::NoFork.install_current()?;
                }
                // SAFETY: a standalone sole main thread and genuine current
                // originals retained until exit. This actor never self-execs;
                // there is no transfer or child-completion assertion.
                let installed = unsafe { set.install_current() };
                if old {
                    assert!(installed.is_err(), "an old seal cannot be tightened");
                } else {
                    installed?;
                    assert!(
                        unsafe { set.install_current() }.is_err(),
                        "only one attempted installation"
                    );
                }
                assert!(
                    set.check_current().is_err(),
                    "refusal permanently quarantines originals"
                );
                assert!(trust_exec_policy::fork_denied(std::process::id())?);
                return Ok(());
            }
            Some("--closed-initial") => {
                println!("closed-exec-initial-ran");
                io::stdout().flush()?;
                denied_actions()?;
                let later = args.next().expect("owned later image");
                let error = aterm_uds::spawnfd::command(later)
                    .arg("--closed-later")
                    .exec();
                panic!("allowed later exec returned: {error}");
            }
            Some("--closed-later") => {
                println!("closed-exec-later-ran");
                io::stdout().flush()?;
                denied_actions()?;
                std::process::exit(72);
            }
            _ => {}
        }
        capture_bounds_and_original_end()?;
        whole_owner_and_checked_projection_remain_retained()?;
        observed_original_drift_is_sticky()?;
        alias_inputs_cannot_supply_originals()?;
        preparation_transfer_keeps_same_whole_original_and_refuses_metadata_equal_alias()?;
        actual_wrapper_installation_is_one_use_and_cannot_tighten_old_seal()?;
        actual_allowed_initial_and_later_exec_deny_unowned_exec_and_fork()?;
        actual_registered_bootstrap_aliases_cannot_survive_preparation()?;
        println!("closed_exec_native: eight controls passed");
        Ok(())
    }

    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_get_special_port(task: u32, which: i32, port: *mut u32) -> i32;
        fn mach_ports_register(task: u32, ports: *const u32, count: u32) -> i32;
        fn mach_ports_lookup(task: u32, ports: *mut *mut u32, count: *mut u32) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        fn vm_deallocate(task: u32, address: usize, bytes: usize) -> i32;
    }

    fn inherited_service_ports_actor() -> io::Result<()> {
        println!("closed-exec-service-port-actor-ran");
        io::stdout().flush()?;
        let original = held(&std::fs::canonicalize(std::env::current_exe()?)?);
        let deadline = end();
        let set = HeldExecImageSet::capture(vec![input(&original, deadline)], deadline).unwrap();
        // SAFETY: own-task handle and a complete native output. This obtains
        // one actual send ref, not a fabricated service, Root or source owner.
        let task = unsafe { mach_task_self_ };
        let mut bootstrap = 0;
        assert_eq!(unsafe { task_get_special_port(task, 4, &mut bootstrap) }, 0);
        assert_ne!(bootstrap, 0, "hostile fixture needs a real bootstrap input");
        let aliases = [bootstrap; 3];
        // SAFETY: this dedicated sole-main child owns its disposable registered
        // stash. The fixed three entries are copies of its actual bootstrap.
        assert_eq!(unsafe { mach_ports_register(task, aliases.as_ptr(), 3) }, 0);
        // SAFETY: release only the query's one new local send reference.
        assert_eq!(unsafe { mach_port_deallocate(task, bootstrap) }, 0);
        // SAFETY: genuine current image and whole original remain held in this
        // sole trusted child until exit. Its bootstrap/stash refs are disposable;
        // every error is terminal. No self-exec or external completion follows.
        unsafe { set.install_current()? };
        let mut ports = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: own-task observation; successful API returns its owned array
        // and a new local SEND ref for every nonzero returned element.
        assert_eq!(
            unsafe { mach_ports_lookup(task, &mut ports, &mut count) },
            0
        );
        if count > 3 || count != 0 && ports.is_null() {
            return Err(io::Error::other(
                "native registered-port fixture observation is unbounded",
            ));
        }
        let mut remained = false;
        for index in 0..count as usize {
            // SAFETY: the complete successful native result is bounded above.
            let name = unsafe { *ports.add(index) };
            if name != 0 {
                remained = true;
                // SAFETY: release exactly this lookup element's new send ref,
                // including a separate user ref when elements share one name.
                assert_eq!(unsafe { mach_port_deallocate(task, name) }, 0);
            }
        }
        if count != 0 {
            // SAFETY: release only the exact out-of-line array from this API.
            assert_eq!(
                unsafe { vm_deallocate(task, ports as usize, count as usize * 4) },
                0
            );
        }
        if remained {
            return Err(io::Error::other(
                "actual bootstrap aliases survived in registered inheritance",
            ));
        }
        println!("actual registered bootstrap aliases were revoked");
        Ok(())
    }

    fn actual_registered_bootstrap_aliases_cannot_survive_preparation() -> io::Result<()> {
        let (temp, path) = fixture()?;
        let mut retained = aterm_uds::deadline::KeptUntilRan::new(temp.keep());
        let mut command = aterm_uds::spawnfd::command(&path);
        command.arg("--closed-inherited-service-ports");
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        if String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "closed-exec-service-port-actor-ran")
        {
            retained.ran();
        }
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("actual registered bootstrap aliases were revoked")
        );
        println!("actual_registered_bootstrap_aliases_cannot_survive_preparation ok");
        Ok(())
    }

    fn capture_bounds_and_original_end() -> io::Result<()> {
        let (_tmp, path) = fixture()?;
        let original = held(&path);
        assert!(HeldExecImageSet::capture(Vec::new(), end()).is_err());
        let population = (0..17).map(|_| input(&original, end())).collect();
        let refused = match HeldExecImageSet::capture(population, end()) {
            Err(refused) => refused,
            Ok(_) => panic!("oversized population accepted"),
        };
        assert_eq!(refused.retained_image_count(), 17);
        assert!(
            HeldExecImageSet::capture(
                vec![input(&original, end()), input(&original, end())],
                end()
            )
            .is_err()
        );
        let early = end();
        let set = HeldExecImageSet::capture(
            vec![input(&original, early)],
            early + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(set.original_deadline(), early);
        assert!(HeldExecImageSet::capture(vec![input(&original, Instant::now())], end()).is_err());
        // A foreign fork must refuse before touching any inherited projector.
        retained_wait()?;
        let child = unsafe { libc::fork() };
        if child == 0 {
            let refused = set
                .check_current()
                .is_err_and(|error| error.raw_os_error() == Some(libc::EPERM));
            unsafe { libc::_exit(if refused { 0 } else { 91 }) };
        }
        if child < 0 {
            return Err(io::Error::last_os_error());
        }
        assert_eq!(wait(child, end())?, 0);
        set.check_current()?;
        println!("capture_bounds_and_original_end ok");
        Ok(())
    }

    struct Whole {
        command: Arc<HeldSystemCommand>,
        alternate: Arc<HeldSystemCommand>,
        switch: Arc<AtomicBool>,
        panic: Arc<AtomicBool>,
        checks: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for Whole {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn checked_whole(owner: &Whole, end: Instant) -> io::Result<&HeldProgram> {
        owner.checks.fetch_add(1, Ordering::SeqCst);
        assert!(
            !owner.panic.load(Ordering::SeqCst),
            "hostile checked projection unwind"
        );
        if owner.switch.load(Ordering::SeqCst) {
            owner.alternate.program_input(end)
        } else {
            owner.command.program_input(end)
        }
    }
    fn whole_owner_and_checked_projection_remain_retained() -> io::Result<()> {
        let (_one, path) = fixture()?;
        let (_two, alternate) = fixture()?;
        let checks = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let switch = Arc::new(AtomicBool::new(false));
        let panic = Arc::new(AtomicBool::new(false));
        let original = Box::new(Whole {
            command: held(&path),
            alternate: held(&alternate),
            switch: Arc::clone(&switch),
            panic: Arc::clone(&panic),
            checks: Arc::clone(&checks),
            drops: Arc::clone(&drops),
        });
        let set = HeldExecImageSet::capture(
            vec![HeldExecImage::retain(original, end(), checked_whole)],
            end(),
        )
        .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let prior = checks.load(Ordering::SeqCst);
        set.check_current()?;
        assert!(checks.load(Ordering::SeqCst) > prior);
        switch.store(true, Ordering::SeqCst);
        assert!(set.check_current().is_err());
        switch.store(false, Ordering::SeqCst);
        assert!(set.check_current().is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(set);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let original = Box::new(Whole {
            command: held(&path),
            alternate: held(&alternate),
            switch,
            panic: Arc::clone(&panic),
            checks,
            drops: Arc::clone(&drops),
        });
        let set = HeldExecImageSet::capture(
            vec![HeldExecImage::retain(original, end(), checked_whole)],
            end(),
        )
        .unwrap();
        panic.store(true, Ordering::SeqCst);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| set.check_current())).is_err()
        );
        panic.store(false, Ordering::SeqCst);
        assert!(
            set.check_current().is_err(),
            "unwind cannot revive original custody"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(set);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        println!("whole_owner_and_checked_projection_remain_retained ok");
        Ok(())
    }

    fn observed_original_drift_is_sticky() -> io::Result<()> {
        let (_tmp, path) = fixture()?;
        let original = held(&path);
        let set = HeldExecImageSet::capture(vec![input(&original, end())], end()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        assert!(set.check_current().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555))?;
        assert!(
            set.check_current().is_err(),
            "restored input cannot revive observed drift"
        );
        println!("observed_original_drift_is_sticky ok");
        Ok(())
    }
    fn alias_inputs_cannot_supply_originals() -> io::Result<()> {
        let (_tmp, path) = fixture()?;
        let alias = path.with_file_name("alias");
        symlink(&path, &alias)?;
        assert!(HeldSystemCommand::open(&alias, end()).is_err());
        println!("alias_inputs_cannot_supply_originals ok");
        Ok(())
    }

    fn preparation_transfer_keeps_same_whole_original_and_refuses_metadata_equal_alias()
    -> io::Result<()> {
        let (_tmp, path) = fixture()?;
        let original = held(&path);
        let weak = Arc::downgrade(&original);
        let first_end = end();
        let set = Arc::new(
            HeldExecImageSet::capture(vec![input(&original, first_end)], first_end).unwrap(),
        );
        let transfer = set.prepare_transfer(
            original.program_input(end())?,
            first_end + Duration::from_secs(60),
        )?;
        assert_eq!(transfer.original_deadline(), first_end);
        assert_eq!(transfer.initial_program()?, path);
        assert_eq!(
            transfer.names()?,
            vec![path.as_os_str().as_bytes().to_vec()]
        );
        // The same held population can serve distinct sequential native
        // preludes, without replacing its whole original or extending its end.
        let later = set.prepare_transfer(original.program_input(end())?, first_end)?;
        later.check_current()?;
        let reopened = held(&path);
        assert_eq!(original.observation(), reopened.observation());
        assert!(
            set.prepare_transfer(reopened.program_input(end())?, first_end)
                .is_err(),
            "same bytes/path/metadata do not create the same original program"
        );
        assert!(transfer.check_current().is_err());
        assert!(later.names().is_err());
        drop((set, original));
        assert!(
            weak.upgrade().is_some(),
            "transfers retain the same whole original"
        );
        drop((transfer, later));
        assert!(weak.upgrade().is_none());
        println!(
            "preparation_transfer_keeps_same_whole_original_and_refuses_metadata_equal_alias ok"
        );
        Ok(())
    }

    fn denied_actions() -> io::Result<()> {
        assert!(trust_exec_policy::fork_denied(std::process::id())?);
        let forked = unsafe { libc::fork() };
        if forked == 0 {
            unsafe { libc::_exit(99) };
        }
        if forked > 0 {
            let _ = wait(forked, end());
            panic!("fixed policy allowed fork");
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        let error = aterm_uds::spawnfd::command("/usr/bin/true").exec();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EPERM),
            "unowned exec must fail: {error}"
        );
        Ok(())
    }
    fn actual_wrapper_installation_is_one_use_and_cannot_tighten_old_seal() -> io::Result<()> {
        let (temp, path) = fixture()?;
        let mut retained = aterm_uds::deadline::KeptUntilRan::new(temp.keep());
        for mode in ["--closed-install-once", "--closed-reject-old-seal"] {
            let mut command = aterm_uds::spawnfd::command(&path);
            command.arg(mode);
            let output =
                aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "closed-exec-wrapper-ran")
            {
                retained.ran();
            }
            assert!(output.status.success(), "{mode}: {output:?}");
        }
        println!("actual_wrapper_installation_is_one_use_and_cannot_tighten_old_seal ok");
        Ok(())
    }
    fn actual_allowed_initial_and_later_exec_deny_unowned_exec_and_fork() -> io::Result<()> {
        let (temp, first) = fixture()?;
        let later = first.with_file_name("later-\"quoted\\image");
        std::fs::copy(&first, &later)?;
        std::fs::set_permissions(&later, std::fs::Permissions::from_mode(0o555))?;
        let original_first = held(&first);
        let original_later = held(&later);
        let original_end = end();
        let set = HeldExecImageSet::capture(
            vec![
                input(&original_first, original_end),
                input(&original_later, original_end),
            ],
            original_end,
        )
        .unwrap();
        let names = [
            CString::new(first.as_os_str().as_bytes())?,
            CString::new(later.as_os_str().as_bytes())?,
        ];
        let names = names.iter().map(|name| name.as_c_str()).collect::<Vec<_>>();
        let mut retained = aterm_uds::deadline::KeptUntilRan::new(temp.keep());
        retained_wait()?;
        set.check_current()?;
        let child = unsafe { libc::fork() };
        if child == 0 {
            // SAFETY: this standalone actor has exactly one thread. Its actual
            // outside parent retains SAME original descriptions/whole owners
            // under original_end until wait; no names are written meanwhile.
            // No Host, native scope, chain semantics or Seed is fabricated.
            unsafe { trust_exec_policy::install_no_fork_closed_exec(&names) }.unwrap();
            let error = aterm_uds::spawnfd::command(&first)
                .arg("--closed-initial")
                .arg(&later)
                .exec();
            panic!("allowed initial exec returned: {error}");
        }
        if child < 0 {
            return Err(io::Error::last_os_error());
        }
        let code = wait(child, original_end)?;
        assert_eq!(
            code, 72,
            "both actual exec stages and hostile actions must finish"
        );
        // Only that terminal path follows both images' actual ran/flush proof.
        retained.ran();
        set.check_current()?;
        println!("actual_allowed_initial_and_later_exec_deny_unowned_exec_and_fork ok");
        Ok(())
    }

    fn retained_wait() -> io::Result<()> {
        // The parent must also be sole-threaded before fork: merely being the
        // main thread would not permit child Rust allocation after fork.
        let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let count = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                task.as_mut_ptr().cast(),
                size,
            )
        };
        if count != size
            || unsafe { task.assume_init() }.pti_threadnum != 1
            || unsafe { libc::pthread_main_np() } != 1
        {
            return Err(io::Error::other(
                "native actor requires sole original main thread",
            ));
        }
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let action = unsafe { action.assume_init() };
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(io::Error::other(
                "native actor requires retained original child wait",
            ));
        }
        Ok(())
    }
    fn wait(pid: libc::pid_t, end: Instant) -> io::Result<i32> {
        loop {
            let mut status = 0;
            let got = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if got == pid {
                return if libc::WIFEXITED(status) {
                    Ok(libc::WEXITSTATUS(status))
                } else {
                    Err(io::Error::other("native actor did not exit ordinarily"))
                };
            }
            if got < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if Instant::now() >= end {
                // The preceding wait retains our original unreaped child. No
                // observed foreign PID or process group becomes signal rights.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                loop {
                    if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
                        break;
                    }
                    if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                        break;
                    }
                }
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real private GNU loan/group retirement under the separate kernel constraint.
//! This is no account admission, SDK, source/config owner or Seed permission.

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use aterm_capacity::build_cpu::{Owner, RetainedPool};
    use aterm_capacity::cohort::Lifetime;
    use aterm_capacity::cohort_namespace::Namespace;
    use aterm_capacity::leadership::Fence;
    use aterm_capacity::ledger::{CohortClaimState, Ledger};
    use aterm_capacity::retained_cohort::{GnuLoanClosure, RetainedCohort};
    use aterm_capacity::{Accounting, Fixed, Need, Polled, Reading, Waiter};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::spawnfd::{self, Stdio};
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::num::NonZeroU32;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::Path;
    use std::time::Instant;
    use trust_exec_policy::ForkingGroupRestriction;

    const MEMBER_INPUT: i32 = 16;

    fn original_sigchld() {
        let mut desired = unsafe { std::mem::zeroed::<libc::sigaction>() };
        desired.sa_sigaction = libc::SIG_DFL;
        assert_eq!(unsafe { libc::sigemptyset(&mut desired.sa_mask) }, 0);
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, &desired, std::ptr::null_mut()) },
            0
        );
        let mut current = unsafe { std::mem::zeroed::<libc::sigaction>() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut current) },
            0
        );
        assert_eq!(current.sa_sigaction, libc::SIG_DFL);
        assert_eq!(current.sa_flags & libc::SA_NOCLDWAIT, 0);
    }

    /// Fresh harness=false main, before any test runner, thread or descendant.
    fn leader(world: &Path) {
        original_sigchld();
        let pid = std::process::id() as i32;
        assert_eq!(unsafe { libc::getsid(0) }, pid);
        assert_eq!(unsafe { libc::getpgrp() }, pid);
        // SAFETY: the actual outside owner launched this original session/group
        // leader and retains its unreaped child, cohort and actual GNU loan.
        // No descendant or untrusted callback has preceded this fixed entry.
        let restriction = unsafe { ForkingGroupRestriction::install_current() }.unwrap();
        let (mut ack, mut ready) = aterm_uds::pipe().unwrap();
        // SAFETY: this exact target slot was delivered by our original parent's
        // fixed native mapping, consumed once here and never used as authority.
        let original_input = unsafe { File::from_raw_fd(MEMBER_INPUT) };
        let handing =
            aterm_uds::spawn_lock::handing(&[original_input.as_raw_fd(), ready.as_raw_fd()]);
        // SAFETY: single-threaded trusted entry, with its handed descriptions
        // held across the actual fork. The child never drops inherited locks.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            std::mem::forget(handing);
            // The kernel's CLOFORK already closed this unhanded reader. Its
            // copied Rust wrapper must not close that now-invalid number.
            std::mem::forget(ack);
            assert_eq!(unsafe { libc::getsid(0) }, pid);
            assert_eq!(unsafe { libc::getpgrp() }, pid);
            assert_eq!(unsafe { libc::setsid() }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(unsafe { libc::setpgid(0, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(unsafe { libc::getsid(0) }, pid);
            assert_eq!(unsafe { libc::getpgrp() }, pid);
            std::fs::write(
                world.join("member-entered"),
                b"actual member denied both escapes\n",
            )
            .unwrap();
            ready.write_all(b"+").unwrap();
            drop(ready);
            let mut input = original_input;
            let mut byte = [0];
            assert_eq!(input.read(&mut byte).unwrap(), 0);
            // SAFETY: terminate this actual fork child without running inherited
            // owner destructors or unlocking the parent's copied spawn lock.
            unsafe { libc::_exit(0) };
        }
        drop(handing);
        drop(ready);
        drop(original_input);
        let mut byte = [0];
        ack.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"+");
        restriction.check_current().unwrap();
        println!("actual restricted leader leaving a live same-group member");
        // Its member is held by the OUTSIDE writer, not this leader's lifetime.
    }

    fn cat() -> spawnfd::Command {
        let mut command = spawnfd::command("/bin/cat");
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        command
    }

    pub(super) fn run() {
        let args = std::env::args_os().collect::<Vec<_>>();
        if args.get(1).is_some_and(|arg| arg == "restricted-leader") {
            assert_eq!(args.len(), 3);
            leader(Path::new(&args[2]));
            return;
        }
        assert_eq!(args.len(), 1);
        original_sigchld();
        let world = aterm_tempfile::tempdir().unwrap().keep();
        eprintln!("preserved native group-loan world {}", world.display());
        let home = world.as_path().join("private-home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(world.as_path(), &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        // Actual private claim/CPU locks and GNU transport; the fixed reading
        // supplies no grant from the account's live ledger or observed context.
        let machine = Fixed::new(Reading::of_gib(64, 40, 8));
        let claim = match Waiter::enter(
            &ledger,
            &machine,
            Need::new("forking-group-loan").mem_gib(1),
        )
        .unwrap()
        .poll(&machine)
        {
            Polled::Admitted(claim) => claim,
            other => panic!("private native claim refused: {other:?}"),
        };
        let memory = claim.cohort_closure_ref().unwrap();
        let width = NonZeroU32::new(2).unwrap();
        let pool =
            RetainedPool::create_private(&world.as_path().join("cpu.noindex"), width).unwrap();
        let owner = Owner::current(Some(123));
        let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
        let leases = (0..2)
            .map(|_| pool.try_acquire(owner).unwrap().unwrap())
            .collect();
        let mut retained = RetainedCohort::new_bundle(&fence, claim, leases).unwrap();
        let (descriptions, _) = retained.prepare_gnu(&fence, width).unwrap().into_parts();
        let _original_gnu = descriptions;
        let mut root = retained.launch(&fence, &mut cat()).unwrap();
        let root_input = aterm_uds::deadline::cat_echoes(root.child_mut());
        let mut loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
        assert!(retained.try_loan_gnu(&fence).unwrap().is_none());

        let (member_input, member_writer) = aterm_uds::pipe().unwrap();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .arg("restricted-leader")
            .arg(world.as_path())
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
            .inherit_guards_at(vec![(member_input.into(), MEMBER_INPUT)])
            .unwrap();
        let mut subgroup = retained
            .launch_loaned_subgroup(&fence, &mut root, &mut command, &mut loan)
            .unwrap();
        assert!(command.starts_suspended());
        assert!(
            retained
                .retire_loaned_subgroup(&fence, &mut subgroup, &mut loan)
                .is_err()
        );
        subgroup.child_mut().resume().unwrap();
        until_within(MUST_HAPPEN, "restricted actual member entry", || {
            std::fs::read(world.as_path().join("member-entered")).ok()
        });
        until_within(MUST_HAPPEN, "same actual leader exit without reap", || {
            subgroup.child_mut().exited().unwrap().then_some(())
        });
        assert!(!subgroup.child_mut().is_reaped());
        assert_eq!(subgroup.cohort().lifetime(), Lifetime::Present);
        assert!(
            retained
                .retire_loaned_subgroup(&fence, &mut subgroup, &mut loan)
                .is_err()
        );
        assert!(pool.try_acquire(owner).unwrap().is_none());
        assert!(retained.try_loan_gnu(&fence).unwrap().is_none());

        assert!(subgroup.child_mut().wait().unwrap().success());
        assert_eq!(subgroup.cohort().lifetime(), Lifetime::Present);
        assert!(
            retained
                .retire_loaned_subgroup(&fence, &mut subgroup, &mut loan)
                .is_err()
        );
        assert!(retained.try_finish(&fence).is_err());
        assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Whole);
        // Only the original outside writer can release this still-live member.
        drop(member_writer);
        until_within(MUST_HAPPEN, "same original subgroup drains", || {
            (subgroup.cohort().lifetime() == Lifetime::Drained).then_some(())
        });
        let retired = retained
            .retire_loaned_subgroup(&fence, &mut subgroup, &mut loan)
            .unwrap();
        assert!(retained.try_loan_gnu(&fence).unwrap().is_none());
        retained
            .return_gnu_loan(&fence, &mut loan, GnuLoanClosure::Retired(retired))
            .unwrap();
        let mut actual_returned = retained.try_loan_gnu(&fence).unwrap().unwrap();
        assert!(retained.try_loan_gnu(&fence).unwrap().is_none());
        let unused = retained
            .cancel_unused_gnu_loan(&fence, &mut actual_returned)
            .unwrap();
        retained
            .return_gnu_loan(&fence, &mut actual_returned, unused)
            .unwrap();
        drop(root_input);
        assert!(root.child_mut().wait().unwrap().success());
        retained.try_finish(&fence).unwrap();
        assert!(retained.is_settled());
        let returned: Vec<_> = (0..2)
            .map(|_| pool.try_acquire(owner).unwrap().unwrap())
            .collect();
        assert!(pool.try_acquire(owner).unwrap().is_none());
        for original in returned {
            original.settle().unwrap();
        }
        drop(retained);
        assert_eq!(
            ledger.cohort_claim_state(&memory).unwrap(),
            CohortClaimState::Closed
        );
        assert!(ledger.list().unwrap().is_empty());
        eprintln!(
            "same original GNU byte returned only after actual reap and restricted group drain"
        );
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() {
    native::run();
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() {
    eprintln!("forking-group loan: native macOS debug control unavailable on this target");
}

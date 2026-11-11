// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A scratch prefix, removed when the test ends however it ends: the
/// directory goes when its `TempDir`, held only to be dropped, does.
struct Prefix(
    PathBuf,
    #[allow(dead_code, reason = "held for its drop")] aterm_tempfile::TempDir,
);

impl std::ops::Deref for Prefix {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for &Prefix {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

fn prefix(label: &str) -> Prefix {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("atc-lease-{label}-"))
        .tempdir()
        .expect("a scratch prefix");
    Prefix(dir.path().to_path_buf(), dir)
}

fn trust(build: u64) -> Subject {
    Subject::build("trust", build).unwrap()
}

/// The file names every lease reader and writer agree on.
#[test]
fn subjects_spell_and_read_back() {
    assert_eq!(trust(9192).stem(), "store-trust-9192");
    assert_eq!(
        Subject::build("trust-mc", 7).unwrap().stem(),
        "store-trust-mc-7"
    );
    assert_eq!(Subject::view("trust").unwrap().stem(), "rustup-trust");
    for s in [
        trust(9192),
        Subject::build("trust-mc", 7).unwrap(),
        Subject::view("trust-dev").unwrap(),
    ] {
        assert_eq!(Subject::parse_stem(&s.stem()), Some(s));
    }
    for bad in [
        "store-trust",
        "store--9",
        "store-trust-x9",
        "store-trust-0",
        "store-trust-007",
        "rustup-",
        "other-x",
    ] {
        assert_eq!(Subject::parse_stem(bad), None, "{bad}");
    }
    assert!(Subject::build("a/b", 1).is_none());
    assert!(Subject::build("a.b", 1).is_none());
    assert!(Subject::build("trust", 0).is_none());
    assert!(Subject::view("").is_none());
}

#[test]
fn a_toolchain_dir_names_its_subject() {
    let p = prefix("of-dir");
    let bin = p.join("store/trust/9192/bin");
    std::fs::create_dir_all(&bin).unwrap();
    assert_eq!(Subject::of_dir(&p, &bin), Some(trust(9192)));
    let root = p.join("compat/trust/9192/bin");
    assert_eq!(Subject::of_dir(&p, &root), Some(trust(9192)));
    let view = p.join("rustup/trust/bin");
    assert_eq!(Subject::of_dir(&p, &view), Subject::view("trust"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(p.join("store/trust/9192"), p.join("store/trust/current"))
            .unwrap();
        assert_eq!(
            Subject::of_dir(&p, &p.join("store/trust/current/bin")),
            Some(trust(9192))
        );
        // A build laid aside (`crate::layout`): through its link, and as the compiler
        // reports its canonical sysroot.
        let aside = p.join("store/trust/15698.noindex/bin");
        std::fs::create_dir_all(&aside).unwrap();
        std::os::unix::fs::symlink("15698.noindex", p.join("store/trust/15698")).unwrap();
        for dir in [aside, p.join("store/trust/15698/bin")] {
            assert_eq!(Subject::of_dir(&p, &dir), Some(trust(15698)), "{dir:?}");
        }
        // A view laid aside, as rustc reports it and through its link.
        let view_aside = p.join("rustup/trust-dev.noindex/bin");
        std::fs::create_dir_all(&view_aside).unwrap();
        std::os::unix::fs::symlink("trust-dev.noindex", p.join("rustup/trust-dev")).unwrap();
        for dir in [view_aside, p.join("rustup/trust-dev/bin")] {
            assert_eq!(
                Subject::of_dir(&p, &dir),
                Subject::view("trust-dev"),
                "{dir:?}"
            );
        }
    }
    assert_eq!(
        Subject::of_dir(&p, Path::new("/opt/trust/stage2/bin")),
        None
    );
    assert_eq!(Subject::of_dir(&p, &p.join("store/trust")), None);
    assert_eq!(Subject::of_dir(&p, &p.join("store/trust/0/bin")), None);
    assert_eq!(Subject::of_dir(&p, &p.join("store/trust/007/bin")), None);
    assert_eq!(
        Subject::of_dir(&p, &p.join("store/trust/9192/../../../outside")),
        None,
        "parent-dir traversal must not match a store subject"
    );
    let _ = std::fs::remove_dir_all(&p);
}

/// A live lease is seen with its holder's words; dropping it frees the subject, and a
/// read never writes.
#[test]
fn a_lease_is_held_until_dropped() {
    let p = prefix("held");
    assert_eq!(holders(&p, &trust(1)), Holders::Free);
    let lease = take(&p, &trust(1), "aterm-release (pid 7) in /w", DEFAULT_WAIT).unwrap();
    assert_eq!(lease.subject(), &trust(1));
    assert_eq!(
        holders(&p, &trust(1)),
        Holders::Held(vec!["aterm-release (pid 7) in /w".to_string()])
    );
    assert_eq!(
        holders(&p, &trust(2)),
        Holders::Free,
        "another build is free"
    );
    let second = take(&p, &trust(1), "aterm-release cut", DEFAULT_WAIT).unwrap();
    let Holders::Held(who) = holders(&p, &trust(1)) else {
        panic!("two holders");
    };
    assert_eq!(who.len(), 2);
    assert_eq!(live(&p).unwrap().len(), 1);
    lease.release();
    second.release();
    assert_eq!(holders(&p, &trust(1)), Holders::Free);
    assert!(live(&p).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&p);
}

/// A lease never offered to a child is let go by `LOCK_UN` at its drop, so a child forked by
/// another thread, holding a copy of its descriptor until its `exec`, does not keep it held.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_dropped_lease_is_let_go_while_a_forked_child_holds_its_copy() {
    let root = aterm_tempfile::tempdir().unwrap();
    let subject = trust(92);
    let lease = take(root.path(), &subject, "forked sibling", Duration::ZERO).unwrap();
    let path = lease.path().to_path_buf();
    let parked = aterm_uds::fork_window::Parked::park();
    drop(lease);
    assert_eq!(let_go_within(&path, Duration::ZERO), Ok(()));
    assert!(parked.release().success());
    assert_eq!(holders(root.path(), &subject), Holders::Free);
    let again = take(root.path(), &subject, "after", Duration::ZERO).unwrap();
    again.release();
    assert_eq!(holders(root.path(), &subject), Holders::Free);
}

/// The original owner's Drop cannot free a real shared-description copy.
#[cfg(unix)]
#[test]
fn sdk_lease_stays_visible_until_the_last_actual_description_closes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let subject = trust(91);
    let lease = take(root.path(), &subject, "retained SDK user", Duration::ZERO).unwrap();
    let path = lease.path().to_path_buf();
    let duplicate = lease.try_clone_descriptor().unwrap();
    use std::os::fd::AsRawFd as _;
    // SAFETY: F_GETFD only reads flags of the live owned descriptor.
    let flags = unsafe { libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);

    drop(lease);

    assert!(
        matches!(holders(root.path(), &subject), Holders::Held(_)),
        "a live duplicate became invisible to SDK reclamation"
    );
    assert!(matches!(reclaim(root.path(), &subject), Reclaim::Keep(_)));
    assert!(
        path.exists(),
        "a live description must keep its rendezvous named"
    );
    drop(duplicate);
    // A sibling test's fork holds its own copy until that child's `exec` ([`Lease::release`]).
    assert_eq!(let_go_within(&path, LET_GO_HANG), Ok(()));
    assert_eq!(holders(root.path(), &subject), Holders::Free);
    let Reclaim::Clear(_guard) = reclaim(root.path(), &subject) else {
        panic!("the final actual close must permit SDK reclamation");
    };
    assert!(
        !path.exists(),
        "reclaim removes the positively unlocked slot"
    );
}

#[cfg(unix)]
fn inherited(fd: std::os::fd::OwnedFd) -> std::os::fd::OwnedFd {
    use std::os::fd::AsRawFd as _;

    let raw = fd.as_raw_fd();
    // SAFETY: the test exclusively owns this candidate and changes only its inheritance
    // flag, exactly as the controlled exec boundary does.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    // SAFETY: the live owned descriptor remains exclusively held by `fd`.
    assert_eq!(
        unsafe { libc::fcntl(raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );
    fd
}

#[cfg(unix)]
fn lease_leaf(lease: &Lease) -> String {
    lease
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("generated lease leaf")
        .to_owned()
}

/// An inherited offer retains the exact original subject after its sender goes away.
/// Mutating the independently derived subject or substituting a fresh open refuses while
/// consuming each refused owned offer exactly once (the negative controls). It runs in a
/// re-exec of its own: a parallel test's open can take a consumed number, which would read
/// as still open.
#[cfg(unix)]
#[test]
fn inherited_lease_adopts_only_the_selected_subjects_original_description() {
    use std::os::fd::AsRawFd as _;

    const NAME: &str =
        "lease::tests::inherited_lease_adopts_only_the_selected_subjects_original_description";
    const ISOLATED: &str = "ATERM_TOOLCHAIN_LEASE_ADOPT_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let mut isolated = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        isolated
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ISOLATED, "1");
        let output =
            aterm_uds::deadline::output_within(&mut isolated, std::time::Duration::from_secs(60))
                .expect("run the adoption fixture in its own process");
        assert!(
            output.status.success(),
            "isolated adoption fixture failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let bin = root.path().join("store/trust/9192/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let original = take_for_dir(root.path(), &bin, "exec sender")
        .unwrap()
        .expect("managed store toolchain");
    let path = original.path().to_owned();
    let leaf = lease_leaf(&original);

    let wrong_dir = root.path().join("store/trust/9193/bin");
    std::fs::create_dir_all(&wrong_dir).unwrap();
    let wrong_subject = inherited(original.try_clone_descriptor().unwrap());
    let wrong_raw = wrong_subject.as_raw_fd();
    // SAFETY: this is an exclusive inherited-form offer. The deliberately wrong subject
    // refuses while consuming the owned offer.
    assert!(
        unsafe { Lease::adopt_inherited(wrong_subject, root.path(), &wrong_dir, &leaf) }.is_err()
    );
    // SAFETY: F_GETFD only observes that the refused OwnedFd was consumed.
    assert_eq!(unsafe { libc::fcntl(wrong_raw, libc::F_GETFD) }, -1);

    let independently_opened = std::fs::File::open(&path).unwrap();
    let independent = inherited(independently_opened.into());
    let independent_raw = independent.as_raw_fd();
    // SAFETY: this known-ineligible independent description is supplied only for checked
    // refusal; the boundary consumes it.
    assert!(unsafe { Lease::adopt_inherited(independent, root.path(), &bin, &leaf) }.is_err());
    // SAFETY: F_GETFD only observes that the refused OwnedFd was consumed.
    assert_eq!(unsafe { libc::fcntl(independent_raw, libc::F_GETFD) }, -1);

    let offered = inherited(original.try_clone_descriptor().unwrap());
    let offered_raw = offered.as_raw_fd();
    // SAFETY: the actual shared description is exclusively offered before any thread can
    // reuse its number, and both paths were selected independently of the offer.
    let adopted = unsafe { Lease::adopt_inherited(offered, root.path(), &bin, &leaf) }.unwrap();
    // SAFETY: successful adoption consumed and closed the advertised raw number.
    assert_eq!(unsafe { libc::fcntl(offered_raw, libc::F_GETFD) }, -1);
    assert_eq!(adopted.subject(), &trust(9192));
    assert_eq!(adopted.path(), path);
    drop(original);
    assert!(
        matches!(holders(root.path(), &trust(9192)), Holders::Held(_)),
        "the adopted description alone retains the selected toolchain"
    );
    drop(adopted);
    assert_eq!(let_go_within(&path, LET_GO_HANG), Ok(()));
    assert_eq!(holders(root.path(), &trust(9192)), Holders::Free);
}

/// A refusal after the lower lock boundary has consumed the inherited raw slot must never
/// close that numeric slot again. An isolated re-exec keeps parallel tests out of this
/// process's descriptor table; there the callback replaces the named lease after adoption
/// and deliberately reuses the former number for an unrelated sentinel before
/// `check_current` refuses. NEGATIVE CONTROL: closing the advertised raw number for every
/// outer error closes the sentinel and makes its final write fail.
#[cfg(unix)]
#[test]
fn a_post_boundary_refusal_never_closes_a_reused_descriptor() {
    use std::cell::RefCell;
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    const NAME: &str = "lease::tests::a_post_boundary_refusal_never_closes_a_reused_descriptor";
    const ISOLATED: &str = "ATERM_TOOLCHAIN_LEASE_REUSED_FD_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let mut isolated = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        isolated
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ISOLATED, "1");
        let output =
            aterm_uds::deadline::output_within(&mut isolated, std::time::Duration::from_secs(60))
                .expect("run the descriptor-custody fixture in its own process");
        assert!(
            output.status.success(),
            "isolated descriptor-custody fixture failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let bin = root.path().join("store/trust/9192/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let original = take_for_dir(root.path(), &bin, "exec sender")
        .unwrap()
        .expect("managed store toolchain");
    let leaf = lease_leaf(&original);
    let moved = original.path().with_extension("lease.moved");
    let sentinel_path = root.path().join("unrelated-sentinel");
    let offered = inherited(original.try_clone_descriptor().unwrap());
    let former_raw = offered.as_raw_fd();
    let sentinel = RefCell::new(None);

    // SAFETY: this single-threaded fixture owns the inherited-form descriptor and selects
    // both paths independently. The callback runs only after the lower boundary consumed the
    // raw slot and retains the unrelated file that deliberately reuses its number.
    let error = unsafe {
        Lease::adopt_inherited_with(offered, root.path(), &bin, &leaf, |slot| {
            std::fs::rename(slot, &moved).unwrap();
            std::fs::write(slot, b"replacement").unwrap();
            let unrelated = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&sentinel_path)
                .unwrap();
            assert_eq!(
                unrelated.as_raw_fd(),
                former_raw,
                "the former offer number is reused before the outer refusal"
            );
            *sentinel.borrow_mut() = Some(unrelated);
        })
    }
    .unwrap_err();
    assert!(
        error.to_string().contains("original lease slot changed"),
        "{error}"
    );

    let mut unrelated = sentinel.into_inner().expect("callback retained sentinel");
    unrelated
        .write_all(b"still-open")
        .expect("a post-boundary refusal did not close the reused number");
    #[expect(
        clippy::disallowed_methods,
        reason = "the descriptor-reuse fixture must publish its unrelated bytes before inspection"
    )]
    unrelated.sync_all().unwrap();
    assert_eq!(std::fs::read(&sentinel_path).unwrap(), b"still-open");
}

/// The release handoff's actual native exec keeps the original lease description visible
/// to the kernel until startup adopts it. A corrupted leaf is refused while consuming the
/// inherited slot exactly once (the negative control), while the exact leaf remains held after
/// the sender's image and all of its close-on-exec descriptions are gone.
#[cfg(target_vendor = "apple")]
#[test]
fn a_toolchain_lease_is_visible_to_lsof_and_adopts_across_a_real_exec() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::time::Duration;

    const NAME: &str =
        "lease::tests::a_toolchain_lease_is_visible_to_lsof_and_adopts_across_a_real_exec";
    const STAGE: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_STAGE";
    const PREFIX: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_PREFIX";
    const TOOLCHAIN: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_TOOLCHAIN";
    const LEAF: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_LEAF";
    const FD: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_FD";
    const PID: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_PID";
    const REFUSE: &str = "ATERM_TOOLCHAIN_LEASE_EXEC_REFUSE";

    let path = |name: &str| PathBuf::from(std::env::var_os(name).expect("fixture path"));
    match std::env::var(STAGE).as_deref() {
        Ok("observer") => {
            let prefix = path(PREFIX);
            let toolchain = path(TOOLCHAIN);
            let fd: std::os::fd::RawFd = std::env::var(FD).unwrap().parse().unwrap();
            let pid = std::env::var(PID).unwrap();
            assert_eq!(pid, std::process::id().to_string(), "exec changed the pid");
            let leaf = std::env::var(LEAF).unwrap();
            let lease_path = dir(&prefix).join(&leaf);
            let kernel_path = std::fs::canonicalize(&lease_path).unwrap();

            let mut lsof = aterm_uds::spawnfd::command("/usr/sbin/lsof");
            lsof.args(["-a", "-p", &pid, "-d", &fd.to_string(), "-Fn"]);
            let seen = aterm_uds::deadline::output_within(&mut lsof, Duration::from_secs(60))
                .expect("lsof the inherited lease");
            assert!(
                seen.status.success(),
                "lsof failed: {}",
                String::from_utf8_lossy(&seen.stderr)
            );
            let lines = String::from_utf8_lossy(&seen.stdout);
            assert!(
                lines.lines().any(|line| line == format!("f{fd}")),
                "lsof did not see descriptor {fd}: {lines}"
            );
            assert!(
                lines
                    .lines()
                    .any(|line| line.strip_prefix('n')
                        == Some(kernel_path.to_string_lossy().as_ref())),
                "lsof did not name {}: {lines}",
                lease_path.display()
            );
            // SAFETY: observer startup exclusively owns the inherited raw number and creates
            // its one Rust owner before any other code can close or reuse it.
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };

            if std::env::var_os(REFUSE).is_some() {
                // SAFETY: startup exclusively owns the actual inherited description. The
                // deliberately corrupted leaf is refused and the OwnedFd is consumed.
                assert!(
                    unsafe {
                        Lease::adopt_inherited(fd, &prefix, &toolchain, "wrong-subject@1-1.lease")
                    }
                    .is_err()
                );
                println!("toolchain-lease-exec-refused-mutated-leaf");
                return;
            }

            // SAFETY: this single-threaded startup owns the actual inherited description;
            // the store paths came from the parent fixture, independently of its fd.
            let lease = unsafe { Lease::adopt_inherited(fd, &prefix, &toolchain, &leaf) }
                .expect("adopt the lease handed through exec");
            assert_eq!(lease.subject(), &trust(9192));
            assert!(matches!(
                holders(&prefix, lease.subject()),
                Holders::Held(_)
            ));
            println!("toolchain-lease-exec-adopted-original-description");
            return;
        }
        Ok("actor") => {
            let prefix = path(PREFIX);
            let toolchain = path(TOOLCHAIN);
            let lease = take_for_dir(&prefix, &toolchain, "exec sender")
                .unwrap()
                .expect("managed store toolchain");
            let leaf = lease_leaf(&lease);
            let descriptor = lease.try_clone_descriptor().unwrap();
            let fd = descriptor.as_raw_fd();
            let mut observer = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            observer
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(STAGE, "observer")
                .env(PREFIX, &prefix)
                .env(TOOLCHAIN, &toolchain)
                .env(LEAF, leaf)
                .env(FD, fd.to_string())
                .env(PID, std::process::id().to_string());
            if std::env::var_os(REFUSE).is_some() {
                observer.env(REFUSE, "1");
            }
            observer.inherit_exec_guards(vec![descriptor]).unwrap();
            panic!("lease handoff exec returned: {:?}", observer.exec());
        }
        _ => {}
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let toolchain = root.path().join("store/trust/9192/bin");
    std::fs::create_dir_all(&toolchain).unwrap();
    for refused in [true, false] {
        let mut actor = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        actor
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(STAGE, "actor")
            .env(PREFIX, root.path())
            .env(TOOLCHAIN, &toolchain);
        if refused {
            actor.env(REFUSE, "1");
        }
        let output = aterm_uds::deadline::output_within(&mut actor, Duration::from_secs(60))
            .expect("run the lease exec actor");
        assert!(
            output.status.success(),
            "lease exec actor failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8_lossy(&output.stdout);
        let expected = if refused {
            "toolchain-lease-exec-refused-mutated-leaf"
        } else {
            "toolchain-lease-exec-adopted-original-description"
        };
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
}

#[cfg(unix)]
#[test]
fn lease_identity_drift_refuses_without_releasing_or_deleting_a_replacement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let subject = trust(92);
    let lease = take(root.path(), &subject, "original SDK user", Duration::ZERO).unwrap();
    lease.check_current().unwrap();
    let path = lease.path().to_path_buf();
    let old = root.path().join("original-slot");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::rename(&path, &old).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let replacement = crate::record_lock::RecordLock::try_lock(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap(),
        WHO_MAX,
    )
    .unwrap();
    assert_eq!(
        lease.check_current().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        lease.try_clone_descriptor().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        probe_file(&old, false).unwrap().is_some(),
        "a failed check must retain the original lock"
    );
    drop(lease);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "Drop must preserve the replacement"
    );
    assert!(matches!(
        reclaim(root.path(), &subject),
        Reclaim::Keep(Holders::Held(_))
    ));
    drop(replacement);
    // A sibling test's fork holds its own copy until that child's `exec` ([`Lease::release`]).
    assert_eq!(let_go_within(&path, LET_GO_HANG), Ok(()));
    assert!(matches!(reclaim(root.path(), &subject), Reclaim::Clear(_)));
}

#[cfg(unix)]
#[test]
fn a_colliding_publication_never_hides_a_live_sdk_lease() {
    let root = aterm_tempfile::tempdir().unwrap();
    let subject = trust(93);
    let lease = take(root.path(), &subject, "live SDK user", Duration::ZERO).unwrap();
    let path = lease.path().to_path_buf();
    let bytes = std::fs::read(&path).unwrap();
    let taking = dir(root.path()).join("colliding.taking");
    std::fs::write(&taking, b"replacement user\n").unwrap();
    assert_eq!(
        publish_lease_file(&taking, &path).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    lease.check_current().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(matches!(
        reclaim(root.path(), &subject),
        Reclaim::Keep(Holders::Held(_))
    ));
    lease.release();
    assert!(matches!(reclaim(root.path(), &subject), Reclaim::Clear(_)));
}

/// A holder that died without dropping (its lock released by the kernel, its file left
/// behind) holds nothing: a reader reads it free, and the reclaimer reaps the file.
#[test]
fn a_dead_holders_file_holds_nothing_and_is_reaped() {
    let p = prefix("dead");
    let lease = take(&p, &trust(3), "gone", DEFAULT_WAIT).unwrap();
    let path = lease.path().to_path_buf();
    #[cfg(unix)]
    lease.release();
    #[cfg(not(unix))]
    {
        lease.file.unlock();
        std::mem::forget(lease);
    }
    assert!(path.exists());
    assert_eq!(holders(&p, &trust(3)), Holders::Free);
    assert!(path.exists(), "a read never reaps");
    let Reclaim::Clear(guard) = reclaim(&p, &trust(3)) else {
        panic!("a dead holder keeps nothing");
    };
    assert!(!path.exists(), "the reclaim reaped it");
    drop(guard);
    let _ = std::fs::remove_dir_all(&p);
}

/// The race the gate exists for: while a reclaim holds it, a holder waits (and gives up
/// at its bound); while a holder holds a lease, a reclaim keeps the subject.
#[test]
fn the_gate_orders_a_holder_against_a_reclaim() {
    let p = prefix("gate");
    let lease = take(&p, &trust(4), "gate run", DEFAULT_WAIT).unwrap();
    match reclaim(&p, &trust(4)) {
        Reclaim::Keep(Holders::Held(who)) => assert_eq!(who, ["gate run"]),
        other => panic!("a leased build is kept: {other:?}"),
    }
    lease.release();
    let Reclaim::Clear(guard) = reclaim(&p, &trust(4)) else {
        panic!("free once released");
    };
    match take(&p, &trust(4), "late", Duration::from_millis(250)) {
        Err(LeaseError::Busy(s)) => assert_eq!(s, trust(4)),
        other => panic!("a holder waits out a reclaim: {other:?}"),
    }
    assert!(matches!(reclaim(&p, &trust(4)), Reclaim::Keep(_)));
    drop(guard);
    let after = take(&p, &trust(4), "after", aterm_spec::wait::MUST_HAPPEN).unwrap();
    drop(after);
    let _ = std::fs::remove_dir_all(&p);
}

/// The sweep takes dead holders' files, `.taking` debris and the gates of subjects that
/// are gone — and nothing a live holder or a live subject owns.
#[test]
fn the_sweep_takes_only_the_dead() {
    let p = prefix("sweep");
    let kept = take(&p, &trust(6), "live", DEFAULT_WAIT).unwrap();
    let dead = take(&p, &trust(7), "dead", DEFAULT_WAIT).unwrap();
    let dead_path = dead.path().to_path_buf();
    #[cfg(unix)]
    dead.release();
    #[cfg(not(unix))]
    {
        dead.file.unlock();
        std::mem::forget(dead);
    }
    let debris = dir(&p).join("store-trust-8@1-1.taking");
    std::fs::write(&debris, b"x\n").unwrap();
    let exists = |s: &Subject| *s == trust(6) || *s == trust(8);
    sweep(&p, &exists);
    assert!(kept.path().exists(), "a live holder's file stays");
    assert!(!dead_path.exists(), "a dead holder's goes");
    assert!(!debris.exists(), "a dead `.taking` goes");
    let names: Vec<String> = std::fs::read_dir(dir(&p))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(GATE_SUFFIX))
        .collect();
    assert_eq!(
        names,
        ["store-trust-6.gate"],
        "only a live subject's gate stays"
    );
    drop(kept);
    let _ = std::fs::remove_dir_all(&p);
}

/// A holder MID-LAY — its subject's gate held shared, its `.taking` created and not yet
/// locked, the instant between `take`'s `create_new` and its `try_lock` — is not reaped
/// by a sweep: the file is a live holder's, and reaping it failed that holder's rename,
/// so its run went on without a lease.
#[test]
fn the_sweep_never_takes_a_lease_being_laid() {
    let p = prefix("sweep-laying");
    let d = ensure_dir(&p).unwrap();
    let gate = open_rendezvous(&d.join("store-trust-6.gate")).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's stand-in holder: it lets go of the gate by unlock() below"
    )]
    gate.try_lock_shared().unwrap();
    let laying = d.join("store-trust-6@1-1.taking");
    std::fs::write(&laying, b"").unwrap();
    sweep(&p, &|_| true);
    assert!(
        laying.exists(),
        "a `.taking` under a held gate is a live holder's"
    );
    gate.unlock().unwrap();
    sweep(&p, &|_| true);
    assert!(!laying.exists(), "with the gate free it is a dead holder's");
    let orphan = d.join("store-trust-9@1-1.taking");
    std::fs::write(&orphan, b"").unwrap();
    sweep(&p, &|_| true);
    assert!(!orphan.exists());
    let _ = std::fs::remove_dir_all(&p);
}

/// The who line is one line, cut on a character boundary under the reader's bound.
#[test]
fn the_who_line_is_one_bounded_line() {
    assert_eq!(who_line("a\nb"), "a");
    let long = "é".repeat(600);
    let line = who_line(&long);
    assert!(line.len() < usize::try_from(WHO_MAX).unwrap());
    assert!(line.chars().all(|c| c == 'é'));
}

/// An unreadable lease directory is never read as free.
#[cfg(unix)]
#[test]
fn an_unreadable_lease_directory_is_not_free() {
    use std::os::unix::fs::PermissionsExt as _;
    let p = prefix("unreadable");
    let l = take(&p, &trust(9), "x", DEFAULT_WAIT).unwrap();
    drop(l);
    std::fs::set_permissions(dir(&p), std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(dir(&p)).is_ok();
    let seen = holders(&p, &trust(9));
    std::fs::set_permissions(dir(&p), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !readable,
        "NOT RUN as written: this process reads a 0000 directory (root?)"
    );
    assert!(matches!(seen, Holders::Unknown(_)), "{seen:?}");
    assert!(seen.in_use());
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn non_directory_at_leases_path_and_unbounded_wait_are_handled_cleanly() {
    let p = prefix("nondir-and-max-wait");
    let ok = take(&p, &trust(1), "max-wait", Duration::MAX).expect("unbounded wait succeeds");
    drop(ok);
    let _ = std::fs::remove_dir_all(&p);

    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(dir(&p), b"not a directory").unwrap();
    let err = take(&p, &trust(1), "blocked", Duration::ZERO).expect_err("file at leases path");
    assert!(matches!(err, LeaseError::Io(ref e) if e.kind() == io::ErrorKind::AlreadyExists));
    let _ = std::fs::remove_dir_all(&p);
}

/// NEGATIVE CONTROL: a pre-existing permissive `leases` directory is tightened to `0700` on
/// Unix, and a symlink at `<prefix>/leases` is refused rather than followed.
#[cfg(unix)]
#[test]
fn ensure_dir_narrows_permissive_directory_and_refuses_symlink() {
    use std::os::unix::fs::PermissionsExt as _;
    let p = prefix("narrow-and-symlink");
    std::fs::create_dir_all(dir(&p)).unwrap();
    std::fs::set_permissions(dir(&p), std::fs::Permissions::from_mode(0o755)).unwrap();
    let lease = take(&p, &trust(1), "holder", DEFAULT_WAIT).unwrap();
    let mode = std::fs::symlink_metadata(dir(&p))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    drop(lease);
    std::fs::remove_dir_all(dir(&p)).unwrap();

    let target = p.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, dir(&p)).unwrap();
    let err = take(&p, &trust(1), "blocked", Duration::ZERO).expect_err("symlink at leases path");
    assert!(matches!(err, LeaseError::Io(ref e) if e.kind() == io::ErrorKind::AlreadyExists));
    let _ = std::fs::remove_dir_all(&p);
}

/// NEGATIVE CONTROL: non-UTF-8 bytes in a live holder's `who` line are decoded lossily
/// rather than causing `probe_file` to discard the holder identity.
#[test]
fn probe_file_decodes_non_utf8_who_bytes_lossily() {
    let p = prefix("lossy-who");
    let lease = take(&p, &trust(5), "placeholder", DEFAULT_WAIT).unwrap();
    #[cfg(unix)]
    {
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(lease.path())
            .unwrap();
        writer.write_all(b"targo in /w/\xff/repo\n").unwrap();
    }
    #[cfg(not(unix))]
    let mut lease = lease;
    #[cfg(not(unix))]
    {
        use std::io::Seek as _;
        lease.file.seek(io::SeekFrom::Start(0)).unwrap();
        lease.file.set_len(0).unwrap();
        lease.file.write_all(b"targo in /w/\xff/repo\n").unwrap();
    }
    assert_eq!(
        holders(&p, &trust(5)),
        Holders::Held(vec!["targo in /w/\u{fffd}/repo".to_string()])
    );
    drop(lease);
    let _ = std::fs::remove_dir_all(&p);
}

/// NEGATIVE CONTROL: a non-regular `<stem>@<tag>.lease` entry (directory or symlink) is
/// reported as `Holders::Unknown` (`in_use() == true`) rather than skipped as `Holders::Free`,
/// so `reclaim` keeps the build instead of deleting over an anomalous lease slot.
#[test]
fn non_regular_lease_entry_is_reported_unknown_not_free() {
    let p = prefix("non-regular-lease");
    let d = ensure_dir(&p).unwrap();
    let bogus_dir = d.join("store-trust-11@1-1.lease");
    std::fs::create_dir(&bogus_dir).unwrap();
    let seen = holders(&p, &trust(11));
    assert!(matches!(seen, Holders::Unknown(_)), "{seen:?}");
    assert!(seen.in_use());
    assert!(matches!(
        reclaim(&p, &trust(11)),
        Reclaim::Keep(Holders::Unknown(_))
    ));
    std::fs::remove_dir(&bogus_dir).unwrap();

    #[cfg(unix)]
    {
        let target = p.join("outside.lease");
        std::fs::write(&target, b"holder\n").unwrap();
        let bogus_link = d.join("store-trust-11@1-1.lease");
        std::os::unix::fs::symlink(&target, &bogus_link).unwrap();
        let seen = holders(&p, &trust(11));
        assert!(matches!(seen, Holders::Unknown(_)), "{seen:?}");
        assert!(seen.in_use());
        assert!(matches!(
            reclaim(&p, &trust(11)),
            Reclaim::Keep(Holders::Unknown(_))
        ));
    }

    let _ = std::fs::remove_dir_all(&p);
}

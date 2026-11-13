// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
unsafe extern "C" {
    fn fcntl(fd: std::ffi::c_int, cmd: std::ffi::c_int, ...) -> std::ffi::c_int;
}
#[cfg(unix)]
const F_GETFD: std::ffi::c_int = 1;
#[cfg(unix)]
const F_SETFD: std::ffi::c_int = 2;
#[cfg(unix)]
const FD_CLOEXEC: std::ffi::c_int = 1;

#[cfg(unix)]
fn inherited(fd: std::os::fd::OwnedFd) -> std::os::fd::OwnedFd {
    use std::os::fd::AsRawFd as _;

    let raw = fd.as_raw_fd();
    // SAFETY: the test exclusively owns this candidate and changes only the inheritance
    // flag, exactly as the controlled exec boundary does.
    let flags = unsafe { fcntl(raw, F_GETFD) };
    assert!(flags >= 0 && flags & FD_CLOEXEC != 0);
    // SAFETY: the live owned descriptor remains exclusively held by `fd`.
    assert_eq!(unsafe { fcntl(raw, F_SETFD, flags & !FD_CLOEXEC) }, 0);
    fd
}

#[test]
fn a_store_lease_io_failure_is_not_reported_as_an_unmanaged_compiler() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bin = root.path().join("store/trust/9192/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(root.path().join(atpkg::lease::DIR), "blocked").unwrap();
    let mut held = std::collections::BTreeMap::new();
    assert!(
        retain_cut_lease(&mut held, root.path(), &bin)
            .unwrap_err()
            .contains("lease refused")
    );
    assert!(held.is_empty(), "a transient refusal is not retained");
    assert!(
        take_cut_lease_checked(root.path(), &root.path().join("external"))
            .unwrap()
            .is_none()
    );
    fs::remove_file(root.path().join(atpkg::lease::DIR)).unwrap();
    retain_cut_lease(&mut held, root.path(), &bin).unwrap();
    retain_cut_lease(&mut held, root.path(), &bin).unwrap();
    assert_eq!(held.len(), 1, "the acquired lease is retained once");
    let subject = atpkg::lease::Subject::build("trust", 9192).unwrap();
    let atpkg::lease::Holders::Held(who) = atpkg::lease::holders(root.path(), &subject) else {
        panic!("the successful retry holds the compiler");
    };
    assert_eq!(who.len(), 1);
}

#[test]
fn a_cut_leases_the_toolchain_it_pinned_and_nothing_else_does() {
    assert!(
        !LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed),
        "no test arms the cut's lease"
    );
    let root = crate::test_scratch_dir("lease");
    let prefix = root.path();
    let bin = prefix.join("store/trust/9192/bin");
    fs::create_dir_all(&bin).unwrap();
    let lease = take_cut_lease(prefix, &bin).expect("a store toolchain is leased");
    let subject = atpkg::lease::Subject::build("trust", 9192).unwrap();
    assert_eq!(lease.subject(), &subject);
    let atpkg::lease::Holders::Held(who) = atpkg::lease::holders(prefix, &subject) else {
        panic!("atpkg reads the cut's lease");
    };
    assert_eq!(who.len(), 1, "{who:?}");
    drop(lease);
    assert!(take_cut_lease(prefix, &std::env::temp_dir()).is_none());
}

/// The gate exports one close-on-exec copy and retains an adopted original description
/// after the sender map disappears. Selecting another managed subject is the negative
/// control: its valid-looking environment leaf cannot redirect adoption, and the refused offer
/// is consumed rather than retained, so the toolchain is let go once the adopted lease drops.
#[cfg(unix)]
#[test]
fn a_cut_lease_offer_adopts_only_for_its_independently_selected_toolchain() {
    let root = aterm_tempfile::tempdir().unwrap();
    let prefix = root.path().canonicalize().unwrap();
    let bin = prefix.join("store/trust/9192/bin");
    let other = prefix.join("store/trust/9193/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&other).unwrap();
    let mut sender = ToolchainLeases::new();
    retain_cut_lease(&mut sender, &prefix, &bin).unwrap();
    let path = sender.get(&bin).unwrap().path().to_owned();
    let subject = atpkg::lease::Subject::build("trust", 9192).unwrap();

    let wrong = offer_cut_lease(&sender, &bin).unwrap().unwrap();
    assert_eq!(
        wrong.leaf,
        path.file_name().unwrap().to_str().unwrap(),
        "only the generated leaf crosses the handover"
    );
    let wrong_leaf = wrong.leaf;
    let wrong = inherited(wrong.descriptor);
    let mut receiver = ToolchainLeases::new();
    // SAFETY: this exclusive inherited-form offer is intentionally paired with a different
    // independently selected subject. The adoption boundary consumes it on refusal.
    assert!(
        unsafe { adopt_cut_lease(&mut receiver, &prefix, &other, wrong, &wrong_leaf) }
            .unwrap_err()
            .contains("selected toolchain subject")
    );
    assert!(receiver.is_empty());

    let offer = offer_cut_lease(&sender, &bin).unwrap().unwrap();
    let leaf = offer.leaf;
    let offer = inherited(offer.descriptor);
    // SAFETY: this test exclusively offers the actual retained description before any
    // thread can reuse it; prefix and bin are independently selected canonical paths.
    unsafe { adopt_cut_lease(&mut receiver, &prefix, &bin, offer, &leaf) }.unwrap();
    assert_eq!(receiver.get(&bin).unwrap().subject(), &subject);
    drop(sender);
    assert!(
        matches!(
            atpkg::lease::holders(&prefix, &subject),
            atpkg::lease::Holders::Held(_)
        ),
        "the adopted gate lease alone retains the toolchain"
    );
    drop(receiver);
    // Nothing else holds the description: neither the refused offer nor the adopted one
    // outlives its owner. (A descriptor number would not say so; any thread may reuse it.)
    assert_eq!(
        atpkg::lease::let_go_within(&path, aterm_spec::wait::MUST_HAPPEN),
        Ok(())
    );
    assert_eq!(
        atpkg::lease::holders(&prefix, &subject),
        atpkg::lease::Holders::Free
    );
}

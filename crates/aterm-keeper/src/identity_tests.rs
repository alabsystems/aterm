// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WITHIN: Duration = Duration::from_secs(60);

const TRUSTD_SILENT: &str =
    "COULD NOT RUN: the process trustd gate was silent — the machine's, not the code's";

fn verdict<T>(r: Result<T, CheckError>) -> Result<T, CheckError> {
    if let Err(CheckError::Unanswered(why)) = &r {
        panic!("{TRUSTD_SILENT}: {why}");
    }
    r
}

fn answered(r: Result<(), String>) -> Result<(), String> {
    if let Err(why) = &r
        && why.starts_with("macOS's code-signing service (trustd) is not answering")
    {
        panic!("{TRUSTD_SILENT}: {why}");
    }
    r
}

#[test]
fn a_process_satisfies_its_own_requirement() {
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    assert!(
        id.describe().starts_with("designated requirement: "),
        "{}",
        id.describe()
    );
    let (a, _b) = CtlStream::pair().expect("pair");
    verdict(id.check(&a, WITHIN)).expect("self passes");
    assert_eq!(Identity::same_uid().describe(), "same-uid");
    Identity::same_uid()
        .check(&a, WITHIN)
        .expect("same uid passes");
}

#[test]
fn a_check_regardless_of_uid_still_checks_code_and_the_floor_keeps_its_uid() {
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_self(IdentityPolicy::Designated)
        .expect("own requirement")
        .regardless_of_uid();
    assert!(id.any_uid);
    let (a, _b) = CtlStream::pair().expect("pair");
    verdict(id.check(&a, WITHIN)).expect("self passes by code");
    assert!(!Identity::same_uid().regardless_of_uid().any_uid);
    // A test binary is signed ad hoc by its linker: no team, and no strict check without a
    // requirement in force.
    assert_eq!(own_team(), Ok(None));
    assert!(matches!(
        Identity::same_uid().admits_code_at_strictly(Path::new("/bin/ls")),
        Err(CheckError::Unchecked(_))
    ));
    assert!(matches!(
        CheckError::from(GateRefusal::Unanswered("silent".into())),
        CheckError::Unanswered(_)
    ));
    assert!(matches!(
        CheckError::from(GateRefusal::Failed("could not run".into())),
        CheckError::Unchecked(_)
    ));
}

// Native status classification controls live beside the shared implementation.

#[test]
fn a_held_token_is_checked_as_the_process_it_named() {
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    let (a, _b) = CtlStream::pair().expect("pair");
    let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
    verdict(id.check_named(&a, Some(token), WITHIN))
        .expect("the token read from the connection names this process");
    let mut later = token;
    later.0[7] = later.0[7].wrapping_add(1);
    let stale = verdict(id.check_named(&a, Some(later), WITHIN));
    assert!(
        matches!(stale, Err(CheckError::Unchecked(_))),
        "a token for another generation of this pid was looked up as the live process: \
         {stale:?}"
    );
    let unnamed = verdict(id.check_named(&a, None, WITHIN));
    assert!(
        matches!(unnamed, Err(CheckError::Unchecked(_))),
        "a peer the kernel did not name was checked: {unnamed:?}"
    );
    Identity::same_uid()
        .check_named(&a, None, WITHIN)
        .expect("the floor reads no token");
}

/// The live law of [`CheckCost`]: each real code check is counted once, on the helper thread
/// that ran it, as it returns, and a check settled without a helper is not counted. The checks
/// are macOS's own (`SecCodeCopyGuestWithAttributes`, `SecCodeCheckValidity`) of this process's
/// real token. It counts checks; `cpu_ns` is a clock's reading and is asserted nowhere.
///
/// Negative control: delete the `checks` add in `costed` (the first assertion reads 0, wants 1).
#[test]
fn a_costed_check_is_counted_on_the_helper_that_ran_it() {
    use std::sync::atomic::Ordering;
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    let (a, _b) = CtlStream::pair().expect("pair");
    let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
    let cost = std::sync::Arc::new(CheckCost::default());
    verdict(id.check_named_costed(&a, Some(token), WITHIN, &cost))
        .expect("the costed check admits what the plain check admits");
    assert_eq!(cost.checks.load(Ordering::Relaxed), 1);
    // A second real check is a second count.
    verdict(id.check_named_costed(&a, Some(token), WITHIN, &cost))
        .expect("the same peer is admitted again");
    assert_eq!(cost.checks.load(Ordering::Relaxed), 2);
    // Another generation of this pid: the token does not name the connected peer, so the check
    // settles before any helper runs and is not counted.
    let mut later = token;
    later.0[7] = later.0[7].wrapping_add(1);
    let stale = verdict(id.check_named_costed(&a, Some(later), WITHIN, &cost));
    assert!(matches!(stale, Err(CheckError::Unchecked(_))), "{stale:?}");
    assert_eq!(cost.checks.load(Ordering::Relaxed), 2);
    // No token and no requirement: each settles with no helper, so neither is counted.
    let unnamed = verdict(id.check_named_costed(&a, None, WITHIN, &cost));
    assert!(
        matches!(unnamed, Err(CheckError::Unchecked(_))),
        "{unnamed:?}"
    );
    Identity::same_uid()
        .check_named_costed(&a, Some(token), WITHIN, &cost)
        .expect("the floor runs no code check");
    assert_eq!(cost.checks.load(Ordering::Relaxed), 2);
    // The plain check adds to no cost.
    verdict(id.check_named(&a, Some(token), WITHIN)).expect("the plain check");
    assert_eq!(cost.checks.load(Ordering::Relaxed), 2);
}

#[test]
fn code_on_disk_is_checked_against_the_requirement() {
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    let exe = std::env::current_exe().expect("test binary");
    answered(id.admits_code_at(&exe)).expect("this binary meets its own requirement");
    let scratch = crate::test_scratch_dir("code");
    let copy = scratch.path().join("copy");
    std::fs::copy(&exe, &copy).expect("copy");
    answered(id.admits_code_at(&copy)).expect("a byte-for-byte copy is the same code");
    let other = answered(id.admits_code_at(Path::new("/usr/bin/true")));
    assert!(other.is_err(), "another program admitted: {other:?}");
    let mut bytes = std::fs::read(&copy).expect("read");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&copy, &bytes).expect("write");
    let broken = answered(id.admits_code_at(&copy));
    assert!(broken.is_err(), "a changed copy admitted: {broken:?}");
    Identity::same_uid()
        .admits_code_at(Path::new("/usr/bin/true"))
        .expect("the floor checks no code");
}

#[test]
fn a_stated_requirement_refuses_code_that_does_not_meet_it() {
    aterm_uds::deadline::require_trust_service();
    let id = Identity::for_requirement(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and \
         certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
         leaf[subject.OU] = \"A66A9P66Z7\"",
    )
    .expect("the Developer ID requirement compiles");
    assert!(
        id.describe()
            .starts_with("requirement: anchor apple generic"),
        "{}",
        id.describe()
    );
    assert_eq!(id.policy(), IdentityPolicy::Designated);
    let (a, _b) = CtlStream::pair().expect("pair");
    let refused = verdict(id.check(&a, WITHIN));
    assert!(
        matches!(refused, Err(CheckError::Refused(_))),
        "an ad-hoc test binary met a Developer ID requirement: {refused:?}"
    );
    assert!(Identity::for_requirement("this is not a requirement").is_err());
}

#[test]
fn the_code_at_a_path_admits_exactly_its_own_processes() {
    aterm_uds::deadline::require_trust_service();
    let exe = std::env::current_exe().expect("test binary");
    let own = Identity::for_code_at(&exe).expect("this binary's requirement");
    assert!(
        own.describe().starts_with("designated requirement of "),
        "{}",
        own.describe()
    );
    let (a, _b) = CtlStream::pair().expect("pair");
    verdict(own.check(&a, WITHIN)).expect("a process running the code at the path passes");
    let other = Identity::for_code_at(Path::new("/usr/bin/true")).expect("a platform binary");
    let refused = verdict(other.check(&a, WITHIN));
    assert!(
        matches!(refused, Err(CheckError::Refused(_))),
        "this test binary met /usr/bin/true's requirement: {refused:?}"
    );
    let scratch = crate::test_scratch_dir("unsigned");
    let unsigned = scratch.path().join("script");
    std::fs::write(&unsigned, "#!/bin/sh\nexit 0\n").expect("write");
    assert!(
        Identity::for_code_at(&unsigned).is_err(),
        "an unsigned file has no designated requirement"
    );
}

#[test]
fn another_program_fails_the_designated_check() {
    aterm_uds::deadline::require_trust_service();
    let scratch = crate::test_scratch_dir("id");
    let path = scratch.path().join("s");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let mut nc = aterm_uds::spawnfd::command("/usr/bin/nc")
        .arg("-U")
        .arg(&path)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("nc");
    let (stream, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    let refused = verdict(id.check(&stream, WITHIN));
    assert!(
        matches!(refused, Err(CheckError::Refused(_))),
        "nc was not refused as other code: {refused:?}"
    );
    Identity::same_uid()
        .check(&stream, WITHIN)
        .expect("nc is our uid");
    let _ = nc.kill();
    let _ = nc.wait();
}
